use std::time::Duration;

use tokio::sync::watch;
use tracing::{debug, warn};

const SET_ROUTE_WITH_OWNER_SCRIPT: &str = r#"
redis.call('SET', KEYS[1], ARGV[1], 'EX', ARGV[3])
redis.call('SET', KEYS[2], ARGV[2], 'EX', ARGV[3])
return 1
"#;

const DELETE_IF_OWNER_SCRIPT: &str = r#"
if redis.call('GET', KEYS[2]) == ARGV[1] then
    redis.call('DEL', KEYS[1], KEYS[2])
    return 1
end
return 0
"#;

const RENEW_IF_OWNER_SCRIPT: &str = r#"
if redis.call('GET', KEYS[1]) and redis.call('GET', KEYS[2]) == ARGV[1] then
    redis.call('EXPIRE', KEYS[1], ARGV[2])
    redis.call('EXPIRE', KEYS[2], ARGV[2])
    return 1
end
return 0
"#;

const GET_ROUTE_WITH_OWNER_SCRIPT: &str = r#"
local instance_id = redis.call('GET', KEYS[1])
local connection_token = redis.call('GET', KEYS[2])
if not instance_id or not connection_token then
    return nil
end
return { instance_id, connection_token }
"#;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OnlineRoute {
    pub instance_id: String,
    pub connection_token: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalDeliveryDecision {
    Deliver,
    RouteMissing,
    RouteInstanceMismatch,
    RouteOwnerMismatch,
    SessionUnavailable,
    SessionOwnerMismatch,
}

pub fn online_route_key(prefix: &str, player_id: &str) -> String {
    format!("{}chat:online:{}", prefix, player_id)
}

pub fn online_route_owner_key(prefix: &str, player_id: &str) -> String {
    format!("{}chat:online-owner:{}", prefix, player_id)
}

/// Gets a consistent route/owner pair. A route missing either value is treated
/// as unavailable so consumers fail closed instead of targeting an unknown
/// session during connection migration.
pub async fn get_online_route(
    redis_url: &str,
    key_prefix: &str,
    player_id: &str,
) -> Result<Option<OnlineRoute>, redis::RedisError> {
    let client = redis::Client::open(redis_url)?;
    let mut redis = client.get_multiplexed_async_connection().await?;
    let key = online_route_key(key_prefix, player_id);
    let owner_key = online_route_owner_key(key_prefix, player_id);
    let values: Option<Vec<String>> = redis::Script::new(GET_ROUTE_WITH_OWNER_SCRIPT)
        .key(&key)
        .key(&owner_key)
        .invoke_async(&mut redis)
        .await?;

    Ok(values.and_then(|values| match values.as_slice() {
        [instance_id, connection_token]
            if !instance_id.is_empty() && !connection_token.is_empty() =>
        {
            Some(OnlineRoute {
                instance_id: instance_id.clone(),
                connection_token: connection_token.clone(),
            })
        }
        _ => None,
    }))
}

/// Pure acceptance policy shared by NATS consumers. Callers supply the route
/// observed from Redis and the token held by the local session, which keeps
/// route migration tests independent of Redis/NATS processes.
pub fn evaluate_local_delivery(
    current_route: Option<&OnlineRoute>,
    expected_instance_id: &str,
    expected_connection_token: &str,
    local_instance_id: &str,
    local_session_connection_token: Option<&str>,
) -> LocalDeliveryDecision {
    let Some(current_route) = current_route else {
        return LocalDeliveryDecision::RouteMissing;
    };
    if current_route.instance_id != expected_instance_id
        || current_route.instance_id != local_instance_id
    {
        return LocalDeliveryDecision::RouteInstanceMismatch;
    }
    if current_route.connection_token != expected_connection_token {
        return LocalDeliveryDecision::RouteOwnerMismatch;
    }
    let Some(local_session_connection_token) = local_session_connection_token else {
        return LocalDeliveryDecision::SessionUnavailable;
    };
    if local_session_connection_token != expected_connection_token {
        return LocalDeliveryDecision::SessionOwnerMismatch;
    }
    LocalDeliveryDecision::Deliver
}

pub async fn set_online_route(
    redis_url: &str,
    key_prefix: &str,
    player_id: &str,
    instance_id: &str,
    connection_token: &str,
    ttl_secs: u64,
) -> Result<(), redis::RedisError> {
    let client = redis::Client::open(redis_url)?;
    let mut redis = client.get_multiplexed_async_connection().await?;
    let key = online_route_key(key_prefix, player_id);
    let owner_key = online_route_owner_key(key_prefix, player_id);
    let _: i32 = redis::Script::new(SET_ROUTE_WITH_OWNER_SCRIPT)
        .key(&key)
        .key(&owner_key)
        .arg(instance_id)
        .arg(connection_token)
        .arg(ttl_secs)
        .invoke_async(&mut redis)
        .await?;
    debug!(instance_id = %instance_id, ttl_secs, "chat online route set");
    Ok(())
}

pub async fn clear_online_route(
    redis_url: &str,
    key_prefix: &str,
    player_id: &str,
    instance_id: &str,
    connection_token: &str,
) -> Result<(), redis::RedisError> {
    let client = redis::Client::open(redis_url)?;
    let mut redis = client.get_multiplexed_async_connection().await?;
    let key = online_route_key(key_prefix, player_id);
    let owner_key = online_route_owner_key(key_prefix, player_id);
    let deleted: i32 = redis::Script::new(DELETE_IF_OWNER_SCRIPT)
        .key(&key)
        .key(&owner_key)
        .arg(connection_token)
        .invoke_async(&mut redis)
        .await?;

    if deleted > 0 {
        debug!(instance_id = %instance_id, "chat online route cleared");
    } else {
        warn!(
            instance_id = %instance_id,
            error_category = "online_route_not_owned",
            "skip clearing chat online route owned by another instance"
        );
    }

    Ok(())
}

/// Refreshes only the route still owned by this connection. A replaced
/// connection therefore cannot revive or overwrite the route written by its
/// successor on another instance.
pub async fn renew_online_route_if_owner(
    redis_url: &str,
    key_prefix: &str,
    player_id: &str,
    connection_token: &str,
    ttl_secs: u64,
) -> Result<bool, redis::RedisError> {
    let client = redis::Client::open(redis_url)?;
    let mut redis = client.get_multiplexed_async_connection().await?;
    let key = online_route_key(key_prefix, player_id);
    let owner_key = online_route_owner_key(key_prefix, player_id);
    let renewed: i32 = redis::Script::new(RENEW_IF_OWNER_SCRIPT)
        .key(&key)
        .key(&owner_key)
        .arg(connection_token)
        .arg(ttl_secs)
        .invoke_async(&mut redis)
        .await?;
    Ok(renewed > 0)
}

pub async fn renew_online_route_until_shutdown(
    redis_url: String,
    key_prefix: String,
    player_id: String,
    connection_token: String,
    ttl_secs: u64,
    mut shutdown: watch::Receiver<bool>,
) {
    let interval_secs = (ttl_secs / 2).max(1);
    loop {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(interval_secs)) => {},
            _ = shutdown.changed() => return,
        }
        match renew_online_route_if_owner(
            &redis_url,
            &key_prefix,
            &player_id,
            &connection_token,
            ttl_secs,
        )
        .await
        {
            Ok(true) => {}
            Ok(false) => {
                debug!("chat online route renewal stopped because ownership changed");
                return;
            }
            Err(_) => warn!(
                error_category = "online_route_renew_failed",
                "failed to renew chat online route"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LocalDeliveryDecision, OnlineRoute, evaluate_local_delivery, online_route_key,
        online_route_owner_key,
    };

    #[test]
    fn online_route_key_respects_prefix() {
        assert_eq!(online_route_key("", "p1"), "chat:online:p1");
        assert_eq!(online_route_key("dev:", "p1"), "dev:chat:online:p1");
        assert_eq!(
            online_route_owner_key("dev:", "p1"),
            "dev:chat:online-owner:p1"
        );
    }

    #[test]
    fn delivery_policy_rejects_stale_or_cross_instance_routes() {
        let route = OnlineRoute {
            instance_id: "chat-b".to_string(),
            connection_token: "token-b".to_string(),
        };

        assert_eq!(
            evaluate_local_delivery(Some(&route), "chat-b", "token-b", "chat-b", Some("token-b"),),
            LocalDeliveryDecision::Deliver
        );
        assert_eq!(
            evaluate_local_delivery(
                Some(&route),
                "chat-b",
                "token-old",
                "chat-b",
                Some("token-old"),
            ),
            LocalDeliveryDecision::RouteOwnerMismatch
        );
        assert_eq!(
            evaluate_local_delivery(Some(&route), "chat-b", "token-b", "chat-a", Some("token-b"),),
            LocalDeliveryDecision::RouteInstanceMismatch
        );
        assert_eq!(
            evaluate_local_delivery(None, "chat-b", "token-b", "chat-b", Some("token-b")),
            LocalDeliveryDecision::RouteMissing
        );
        assert_eq!(
            evaluate_local_delivery(
                Some(&route),
                "chat-b",
                "token-b",
                "chat-b",
                Some("token-new"),
            ),
            LocalDeliveryDecision::SessionOwnerMismatch
        );
    }
}
