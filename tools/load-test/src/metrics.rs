use std::collections::BTreeMap;
use std::io::Cursor;

use base64::Engine;
use hdrhistogram::Histogram;
use hdrhistogram::serialization::{
    Deserializer as HdrDeserializer, Serializer, V2DeflateSerializer,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer as SerdeSerializer};

const HISTOGRAM_ENCODING: &str = "hdr_v2_deflate_base64";
const LOWEST_DISCERNIBLE_VALUE: u64 = 1;
const HIGHEST_TRACKABLE_VALUE: u64 = 60_000;
const SIGNIFICANT_FIGURES: u8 = 3;

/// A serializable wrapper around an HdrHistogram V2+DEFLATE payload.
///
/// The in-memory histogram is retained for cheap recording and merging; serde
/// serializes its compact, mergeable HDR representation rather than percentile
/// snapshots or an unbounded exact-value map.
#[derive(Debug, Clone, PartialEq)]
pub struct HistogramSnapshot {
    histogram: Histogram<u64>,
}

impl Default for HistogramSnapshot {
    fn default() -> Self {
        Self {
            histogram: Histogram::new_with_bounds(
                LOWEST_DISCERNIBLE_VALUE,
                HIGHEST_TRACKABLE_VALUE,
                SIGNIFICANT_FIGURES,
            )
            .expect("stage-one HDR histogram bounds are valid"),
        }
    }
}

impl HistogramSnapshot {
    pub fn record(&mut self, value_ms: u64) {
        self.histogram
            .saturating_record(value_ms.clamp(LOWEST_DISCERNIBLE_VALUE, HIGHEST_TRACKABLE_VALUE));
    }

    pub fn count(&self) -> u64 {
        self.histogram.len()
    }

    pub fn percentile(&self, percentile: f64) -> u64 {
        if self.histogram.is_empty() {
            return 0;
        }
        self.histogram.value_at_quantile(percentile.clamp(0.0, 1.0))
    }

    pub fn max(&self) -> u64 {
        self.histogram.max()
    }

    pub fn merge(&mut self, other: &Self) {
        self.histogram
            .add(&other.histogram)
            .expect("HDR batches must use compatible latency bounds");
    }
}

#[derive(Serialize)]
struct SerializedHistogram {
    encoding: &'static str,
    lowest_discernible_value: u64,
    highest_trackable_value: u64,
    significant_figures: u8,
    payload_base64: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeserializedHistogram {
    encoding: String,
    lowest_discernible_value: u64,
    highest_trackable_value: u64,
    significant_figures: u8,
    payload_base64: String,
}

impl Serialize for HistogramSnapshot {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: SerdeSerializer,
    {
        let mut payload = Vec::new();
        V2DeflateSerializer::new()
            .serialize(&self.histogram, &mut payload)
            .map_err(serde::ser::Error::custom)?;
        SerializedHistogram {
            encoding: HISTOGRAM_ENCODING,
            lowest_discernible_value: self.histogram.low(),
            highest_trackable_value: self.histogram.high(),
            significant_figures: self.histogram.sigfig(),
            payload_base64: base64::engine::general_purpose::STANDARD.encode(payload),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for HistogramSnapshot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = DeserializedHistogram::deserialize(deserializer)?;
        if wire.encoding != HISTOGRAM_ENCODING {
            return Err(serde::de::Error::custom(
                "unsupported HDR histogram encoding",
            ));
        }
        let payload = base64::engine::general_purpose::STANDARD
            .decode(wire.payload_base64)
            .map_err(serde::de::Error::custom)?;
        let histogram: Histogram<u64> = HdrDeserializer::new()
            .deserialize(&mut Cursor::new(payload))
            .map_err(serde::de::Error::custom)?;
        if histogram.low() != wire.lowest_discernible_value
            || histogram.high() != wire.highest_trackable_value
            || histogram.sigfig() != wire.significant_figures
        {
            return Err(serde::de::Error::custom(
                "HDR histogram metadata does not match payload",
            ));
        }
        Ok(Self { histogram })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MetricsSnapshot {
    pub counters: BTreeMap<String, u64>,
    pub histograms: BTreeMap<String, HistogramSnapshot>,
    /// Raw, boundary-stamped gauge/counter samples. Values from workers are
    /// merged at the same monotonic boundary; percentiles are never stored.
    #[serde(default)]
    pub time_series: BTreeMap<u64, BTreeMap<String, u64>>,
}

impl MetricsSnapshot {
    pub fn merge(&mut self, other: &Self) {
        for (key, value) in &other.counters {
            *self.counters.entry(key.clone()).or_default() += value;
        }
        for (key, histogram) in &other.histograms {
            self.histograms
                .entry(key.clone())
                .or_default()
                .merge(histogram);
        }
        for (boundary_ms, samples) in &other.time_series {
            let destination = self.time_series.entry(*boundary_ms).or_default();
            for (key, value) in samples {
                *destination.entry(key.clone()).or_default() += value;
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Metrics {
    snapshot: MetricsSnapshot,
}

impl Metrics {
    pub fn increment(&mut self, key: &str, amount: u64) {
        assert!(
            is_low_cardinality_key(key),
            "metric labels must be fixed low-cardinality categories"
        );
        *self.snapshot.counters.entry(key.to_string()).or_default() += amount;
    }
    pub fn observe_latency(&mut self, key: &str, milliseconds: u64) {
        assert!(
            is_latency_key(key),
            "latency dimensions must be fixed low-cardinality categories"
        );
        self.snapshot
            .histograms
            .entry(key.to_string())
            .or_default()
            .record(milliseconds);
    }
    pub fn merge_latency(&mut self, key: &str, histogram: &HistogramSnapshot) {
        assert!(
            is_latency_key(key),
            "latency dimensions must be fixed low-cardinality categories"
        );
        self.snapshot
            .histograms
            .entry(key.to_string())
            .or_default()
            .merge(histogram);
    }

    /// Record a raw time-series sample. The timestamp is a worker monotonic
    /// boundary supplied by the controller; aggregation sums samples that
    /// share that boundary across workers.
    pub fn record_time_series(&mut self, boundary_ms: u64, key: &str, value: u64) {
        assert!(
            is_low_cardinality_key(key),
            "time-series dimensions must be fixed low-cardinality categories"
        );
        *self
            .snapshot
            .time_series
            .entry(boundary_ms)
            .or_default()
            .entry(key.to_string())
            .or_default() += value;
    }
    pub fn snapshot(&self) -> MetricsSnapshot {
        self.snapshot.clone()
    }

    pub fn merge_snapshot(&mut self, snapshot: &MetricsSnapshot) {
        self.snapshot.merge(snapshot);
    }
}

pub fn is_low_cardinality_key(key: &str) -> bool {
    matches!(
        key,
        "virtual_players"
            | "started"
            | "completed"
            | "failed"
            | "requests"
            | "operations"
            | "bytes_sent"
            | "bytes_received"
            | "connections_opened"
            | "connections_active"
            | "scheduler_lag_ms"
            | "scheduler_queue_depth"
            | "metrics_dropped"
            | "late_response"
            | "timeouts"
            | "disconnects"
            | "business_errors"
            | "protocol_errors"
            | "http_2xx"
            | "http_4xx"
            | "http_5xx"
            | "socket_errors"
            | "push_out_of_order"
            | "auth_requests"
            | "auth_login_requests"
            | "auth_login_successes"
            | "auth_connection_failures"
            | "auth_ticket_attempts"
            | "auth_ticket_successes"
            | "auth_rate_limited"
            | "auth_potential_data_writes"
            | "game_sessions_completed"
            | "game_auth_requests"
            | "game_heartbeat_requests"
            | "room_create_or_join"
            | "room_leave"
            | "room_reconnect"
            | "gameplay_messages_sent"
            | "gameplay_bytes_sent"
            | "gameplay_bytes_received"
            | "frame_inputs_sent"
            | "frame_inputs_received"
            | "frame_bundles_received"
            | "frame_out_of_order"
            | "frame_timeouts"
            | "frame_late_response"
            | "frame_local_dropped"
            | "gameplay_business_errors"
            | "player_match_event_stream_connections"
            | "player_match_attempts"
            | "player_match_successes"
            | "player_match_statuses"
            | "player_match_cancellations"
            | "player_match_timeouts"
            | "player_match_business_errors"
            | "player_match_grpc_status_observation_holes"
            | "player_match_room_create_closed_unobserved"
            | "reconnect_burst_login_actions"
            | "reconnect_burst_forced_disconnects"
            | "reconnect_burst_new_connections"
            | "reconnect_burst_room_recoveries"
            | "reconnect_burst_backoff_ms"
            | "reconnect_burst_potential_data_writes"
            | "side_chat_operations"
            | "side_chat_success"
            | "side_chat_rate_limited"
            | "side_chat_slow"
            | "side_chat_timeout"
            | "side_chat_disconnect"
            | "side_chat_push_out_of_order"
            | "side_chat_push_duplicate"
            | "side_chat_slow_consumer"
            | "side_chat_business_error"
            | "chat_wss_handshakes"
            | "chat_wss_handshake_successes"
            | "chat_wss_auth_attempts"
            | "chat_wss_active_connections"
            | "chat_wss_messages_sent"
            | "chat_wss_pushes_received"
            | "chat_wss_push_duplicates"
            | "chat_wss_push_out_of_order"
            | "chat_wss_queue_backlog"
            | "chat_wss_slow_consumer_disconnects"
            | "chat_wss_disconnects"
            | "chat_wss_reconnects"
            | "chat_wss_reconnect_backoff_ms"
            | "match_grpc_queued_ms"
            | "match_grpc_attempts"
            | "match_grpc_successes"
            | "match_grpc_event_stream_connections"
            | "match_grpc_statuses"
            | "match_grpc_room_create_closed_ms"
            | "match_grpc_room_create_closed_unobserved"
            | "match_grpc_cancellations"
            | "match_grpc_timeouts"
            | "match_grpc_stream_disconnects"
            | "match_grpc_backpressure_pending_limit_rejections"
            | "match_grpc_backpressure_dropped_pending_messages"
            | "match_grpc_backpressure_stream_disconnects"
            | "match_internal_operations"
            | "match_internal_successes"
            | "match_internal_statuses"
            | "match_internal_connections"
            | "match_internal_messages"
            | "match_internal_writes"
            | "match_internal_timeouts"
            | "match_internal_grpc_errors"
            | "match_internal_business_errors"
            | "match_internal_room_create_closed_unobserved"
            | "side_mail_operations"
            | "side_mail_success"
            | "side_mail_rate_limited"
            | "side_mail_slow"
            | "side_mail_timeout"
            | "side_mail_disconnect"
            | "side_mail_push_out_of_order"
            | "side_mail_push_duplicate"
            | "side_mail_slow_consumer"
            | "side_mail_business_error"
            | "side_mail_writes"
            | "side_mail_notifications"
            | "side_mail_claim_successes"
            | "side_mail_claim_idempotent_replays"
            | "side_mail_claim_processing"
            | "side_mail_claim_reconciliation_pending"
            | "side_mail_claim_retryable_failures"
            | "side_mail_notification_observation_holes"
            | "side_announce_writes"
            | "side_http_writes"
            | "side_announce_operations"
            | "side_announce_success"
            | "side_announce_rate_limited"
            | "side_announce_slow"
            | "side_announce_timeout"
            | "side_announce_disconnect"
            | "side_announce_push_out_of_order"
            | "side_announce_push_duplicate"
            | "side_announce_slow_consumer"
            | "side_announce_business_error"
            | "side_match_operations"
            | "side_match_success"
            | "side_match_rate_limited"
            | "side_match_slow"
            | "side_match_timeout"
            | "side_match_disconnect"
            | "side_match_push_out_of_order"
            | "side_match_push_duplicate"
            | "side_match_slow_consumer"
            | "side_match_business_error"
            | "side_chat_queue_backlog"
            | "side_mail_queue_backlog"
            | "side_announce_queue_backlog"
            | "side_match_queue_backlog"
            | "side_chat_push_events"
            | "side_mail_push_events"
            | "side_announce_push_events"
            | "side_match_push_events"
            | "kcp_backpressure_pending_limit_rejections"
            | "kcp_backpressure_dropped_pending_requests"
            | "kcp_backpressure_disconnects"
    )
}

fn is_latency_key(key: &str) -> bool {
    matches!(
        key,
        "operation_ms"
            | "login_ms"
            | "connect_ms"
            | "auth_ms"
            | "first_frame_ms"
            | "room_join_ms"
            | "room_first_frame_ms"
            | "room_recovery_ms"
            | "room_exit_ms"
            | "gameplay_step_ms"
            | "scheduler_lag_ms"
            | "ticket_ms"
            | "auth_operation_ms"
            | "side_chat_ms"
            | "side_mail_ms"
            | "side_announce_ms"
            | "side_match_ms"
            | "match_internal_ms"
            | "match_grpc_queue_ms"
            | "match_grpc_room_create_ms"
            | "player_match_queue_ms"
            | "chat_wss_handshake_ms"
            | "chat_wss_message_ms"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hdr_histogram_serializes_merges_and_reports_percentiles() {
        let mut first = HistogramSnapshot::default();
        first.record(10);
        first.record(20);
        let mut second = HistogramSnapshot::default();
        second.record(100);
        second.record(1_000);

        let encoded = serde_json::to_string(&first).unwrap();
        assert!(encoded.contains(HISTOGRAM_ENCODING));
        let restored: HistogramSnapshot = serde_json::from_str(&encoded).unwrap();
        assert_eq!(restored, first);

        first.merge(&second);
        assert_eq!(first.count(), 4);
        assert_eq!(first.percentile(0.50), 20);
        assert_eq!(first.percentile(0.90), 1_000);
        assert_eq!(first.percentile(0.95), 1_000);
        assert_eq!(first.percentile(0.99), 1_000);
        assert_eq!(first.max(), 1_000);
    }

    #[test]
    #[should_panic(expected = "low-cardinality")]
    fn identity_cannot_be_used_as_metric_label() {
        Metrics::default().increment("account:alice@example.com", 1);
    }

    #[test]
    fn completed_game_session_keys_are_fixed_low_cardinality_categories() {
        for key in [
            "game_sessions_completed",
            "game_auth_requests",
            "game_heartbeat_requests",
        ] {
            assert!(is_low_cardinality_key(key));
        }
    }

    #[test]
    fn merge_combines_raw_time_series_at_shared_boundaries() {
        let mut first = Metrics::default();
        first.increment("requests", 2);
        first.observe_latency("operation_ms", 10);
        first.record_time_series(100, "virtual_players", 3);
        let mut second = Metrics::default();
        second.increment("requests", 5);
        second.observe_latency("operation_ms", 1_000);
        second.record_time_series(100, "virtual_players", 4);
        second.record_time_series(200, "virtual_players", 1);

        let mut merged = first.snapshot();
        merged.merge(&second.snapshot());
        assert_eq!(merged.counters["requests"], 7);
        assert_eq!(merged.histograms["operation_ms"].count(), 2);
        assert_eq!(merged.histograms["operation_ms"].percentile(0.99), 1_000);
        assert_eq!(merged.time_series[&100]["virtual_players"], 7);
        assert_eq!(merged.time_series[&200]["virtual_players"], 1);
        let encoded = serde_json::to_string(&merged).unwrap();
        assert!(!encoded.contains("percentile"));
    }
}
