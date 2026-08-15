//! Sanitized, short-lived projection of a proxy's active registry upstreams.
//!
//! This is deliberately separate from any player/room routing store. It is
//! suitable for a narrowly scoped read-only observer: no endpoint, socket,
//! player, room, ticket, or route-store data is serialised here.

use std::collections::BTreeSet;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::{SystemTime, UNIX_EPOCH};

use redis::AsyncCommands;
use serde::{Deserialize, Serialize};

pub const PROXY_ROUTE_OBSERVATION_SCHEMA_VERSION: u32 = 1;
pub const PROXY_ROUTE_OBSERVATION_TTL_SECS: u64 = 30;
pub const PROXY_ROUTE_OBSERVATION_KEY_NAMESPACE: &str = "route-observation:game-proxy:";

/// Returns the one projection key owned by a game-proxy service instance.
/// The instance id is sourced from the already validated service-registry
/// identity and is never derived from player-controlled input.
pub fn proxy_route_observation_key(key_prefix: &str, proxy_instance_id: &str) -> String {
    format!("{key_prefix}{PROXY_ROUTE_OBSERVATION_KEY_NAMESPACE}{proxy_instance_id}")
}

/// Versioned, sanitized read model for the active upstream set consumed by a
/// single game-proxy instance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyRouteObservation {
    pub schema_version: u32,
    pub proxy_instance_id: String,
    pub target_service: String,
    pub eligible_upstream_instance_ids: Vec<String>,
    pub eligible_upstream_count: u32,
    pub revision: u64,
    pub observed_at_unix_ms: u64,
    pub ttl_secs: u64,
    pub expires_at_unix_ms: u64,
}

impl ProxyRouteObservation {
    pub fn new(
        proxy_instance_id: impl Into<String>,
        target_service: impl Into<String>,
        upstream_instance_ids: impl IntoIterator<Item = String>,
        revision: u64,
        observed_at_unix_ms: u64,
    ) -> Result<Self, RouteObservationError> {
        let proxy_instance_id = proxy_instance_id.into();
        let target_service = target_service.into();
        let upstream_instance_ids: Vec<_> = upstream_instance_ids
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let observation = Self {
            schema_version: PROXY_ROUTE_OBSERVATION_SCHEMA_VERSION,
            proxy_instance_id,
            target_service,
            eligible_upstream_count: u32::try_from(upstream_instance_ids.len())
                .map_err(|_| RouteObservationError::InvalidProjection)?,
            eligible_upstream_instance_ids: upstream_instance_ids,
            revision,
            observed_at_unix_ms,
            ttl_secs: PROXY_ROUTE_OBSERVATION_TTL_SECS,
            expires_at_unix_ms: observed_at_unix_ms
                .saturating_add(PROXY_ROUTE_OBSERVATION_TTL_SECS.saturating_mul(1_000)),
        };
        observation.validate()?;
        Ok(observation)
    }

    pub fn validate(&self) -> Result<(), RouteObservationError> {
        let ids = &self.eligible_upstream_instance_ids;
        let expected_expiry = self
            .observed_at_unix_ms
            .saturating_add(self.ttl_secs.saturating_mul(1_000));
        if self.schema_version != PROXY_ROUTE_OBSERVATION_SCHEMA_VERSION
            || self.proxy_instance_id.trim().is_empty()
            || self.target_service.trim().is_empty()
            || self.revision == 0
            || self.observed_at_unix_ms == 0
            || self.ttl_secs != PROXY_ROUTE_OBSERVATION_TTL_SECS
            || self.expires_at_unix_ms != expected_expiry
            || self.eligible_upstream_count as usize != ids.len()
            || ids.len() > 64
            || ids.iter().any(|id| id.trim().is_empty())
            || ids.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(RouteObservationError::InvalidProjection);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteObservationError {
    InvalidProjection,
    RedisUnavailable,
    SerializationFailed,
}

impl std::fmt::Display for RouteObservationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidProjection => "route observation projection is invalid",
            Self::RedisUnavailable => "route observation projection store is unavailable",
            Self::SerializationFailed => "route observation projection serialization failed",
        })
    }
}

impl std::error::Error for RouteObservationError {}

/// Redis publisher for the sanitized projection. It only issues `SET ... EX`
/// for the publishing proxy's exact key; it never reads or mutates route-store
/// data. Failures are intentionally reported without Redis error details.
#[derive(Clone)]
pub struct ProxyRouteObservationPublisher {
    redis: redis::Client,
    key_prefix: String,
    proxy_instance_id: String,
    next_revision: Arc<AtomicU64>,
}

impl ProxyRouteObservationPublisher {
    pub fn new_lazy(
        redis_url: &str,
        key_prefix: impl Into<String>,
        proxy_instance_id: impl Into<String>,
    ) -> Result<Self, RouteObservationError> {
        let proxy_instance_id = proxy_instance_id.into();
        if proxy_instance_id.trim().is_empty() {
            return Err(RouteObservationError::InvalidProjection);
        }
        let redis =
            redis::Client::open(redis_url).map_err(|_| RouteObservationError::RedisUnavailable)?;
        Ok(Self {
            redis,
            key_prefix: key_prefix.into(),
            proxy_instance_id,
            next_revision: Arc::new(AtomicU64::new(0)),
        })
    }

    pub fn key(&self) -> String {
        proxy_route_observation_key(&self.key_prefix, &self.proxy_instance_id)
    }

    pub async fn publish(
        &self,
        target_service: &str,
        upstream_instance_ids: impl IntoIterator<Item = String>,
    ) -> Result<ProxyRouteObservation, RouteObservationError> {
        let revision = self
            .next_revision
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        let observation = ProxyRouteObservation::new(
            self.proxy_instance_id.clone(),
            target_service.to_string(),
            upstream_instance_ids,
            revision,
            unix_now_ms(),
        )?;
        let encoded = serde_json::to_string(&observation)
            .map_err(|_| RouteObservationError::SerializationFailed)?;
        let mut connection = self
            .redis
            .get_multiplexed_async_connection()
            .await
            .map_err(|_| RouteObservationError::RedisUnavailable)?;
        let _: () = connection
            .set_ex(self.key(), encoded, PROXY_ROUTE_OBSERVATION_TTL_SECS)
            .await
            .map_err(|_| RouteObservationError::RedisUnavailable)?;
        Ok(observation)
    }
}

fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_is_sorted_deduplicated_and_contains_no_route_details() {
        let projection = ProxyRouteObservation::new(
            "proxy-1",
            "game-server",
            ["game-b".into(), "game-a".into(), "game-a".into()],
            7,
            10_000,
        )
        .unwrap();

        assert_eq!(
            projection.eligible_upstream_instance_ids,
            ["game-a", "game-b"]
        );
        assert_eq!(projection.eligible_upstream_count, 2);
        assert_eq!(projection.expires_at_unix_ms, 40_000);
        let encoded = serde_json::to_string(&projection).unwrap();
        for forbidden in [
            "socket",
            "room",
            "character",
            "player",
            "ticket",
            "endpoint",
        ] {
            assert!(!encoded.contains(forbidden), "unexpected field {forbidden}");
        }
    }

    #[test]
    fn projection_rejects_unknown_or_inconsistent_fields() {
        let valid =
            ProxyRouteObservation::new("proxy-1", "game-server", ["game-a".into()], 1, 1_000)
                .unwrap();
        assert!(valid.validate().is_ok());
        assert!(serde_json::from_str::<ProxyRouteObservation>(
            r#"{"schema_version":1,"proxy_instance_id":"proxy-1","target_service":"game-server","eligible_upstream_instance_ids":[],"eligible_upstream_count":0,"revision":1,"observed_at_unix_ms":1000,"ttl_secs":30,"expires_at_unix_ms":31000,"socket":"private.sock"}"#
        )
        .is_err());

        let mut invalid = valid;
        invalid.eligible_upstream_count = 2;
        assert_eq!(
            invalid.validate(),
            Err(RouteObservationError::InvalidProjection)
        );
    }

    #[test]
    fn projection_key_is_separate_from_route_store_state() {
        assert_eq!(
            proxy_route_observation_key("test:", "proxy-1"),
            "test:route-observation:game-proxy:proxy-1"
        );
        assert!(!PROXY_ROUTE_OBSERVATION_KEY_NAMESPACE.contains("route-store"));
    }
}
