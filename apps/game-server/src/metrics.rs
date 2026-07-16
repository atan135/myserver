//! Game Server Metrics Module
//!
//! 监控指标收集与 NATS 上报

use std::collections::HashMap;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::json;
use service_registry::collect_discovery_metric_fields;
use tokio::time::interval;
use tracing::{error, info};

use crate::protocol_version_policy::ClientProtocolVersionMetric;

/// 计算当前 bucket 时间戳（5秒对齐）
fn current_bucket() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        / 5
        * 5
}

fn subject_token(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

async fn publish_metrics(
    client: &async_nats::Client,
    service_name: &str,
    service_instance_id: &str,
    bucket: u64,
    fields: Vec<(String, String)>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let metrics = fields
        .into_iter()
        .map(|(key, value)| (key, serde_json::Value::String(value)))
        .collect::<serde_json::Map<_, _>>();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let payload = json!({
        "service": service_name,
        "instance_id": service_instance_id,
        "bucket": bucket,
        "timestamp": timestamp,
        "metrics": metrics,
    });
    let subject = format!(
        "myserver.metrics.{}.{}",
        service_name,
        subject_token(service_instance_id)
    );

    client
        .publish(subject, serde_json::to_vec(&payload)?.into())
        .await?;
    client.flush().await?;
    Ok(())
}

/// MetricsCollector 结构体
pub struct MetricsCollector {
    /// QPS 计数器
    qps_counter: AtomicU64,
    /// 延迟总和（毫秒）
    latency_sum: AtomicU64,
    /// 延迟计数
    latency_count: AtomicU64,
    /// 在线玩家数
    online_players: AtomicU64,
    /// 房间数
    room_count: AtomicU64,
    inventory_grant_first_success: AtomicU64,
    inventory_grant_idempotent_hit: AtomicU64,
    inventory_grant_fingerprint_conflict: AtomicU64,
    inventory_grant_transaction_failure: AtomicU64,
    inventory_grant_push_failure: AtomicU64,
    asset_transaction_duration_ms: AtomicU64,
    asset_transaction_count: AtomicU64,
    asset_version_conflict: AtomicU64,
    asset_capacity_fallback: AtomicU64,
    reward_mail_created: AtomicU64,
    client_protocol_auth_accepted_legacy: AtomicU64,
    client_protocol_auth_accepted_current: AtomicU64,
    client_protocol_auth_accepted_supported_older: AtomicU64,
    client_protocol_auth_rejected_too_old: AtomicU64,
    client_protocol_auth_rejected_too_new: AtomicU64,
    /// 扩展字段
    extra: Mutex<HashMap<String, String>>,
}

impl MetricsCollector {
    /// 创建新的 MetricsCollector
    pub fn new() -> Self {
        Self {
            qps_counter: AtomicU64::new(0),
            latency_sum: AtomicU64::new(0),
            latency_count: AtomicU64::new(0),
            online_players: AtomicU64::new(0),
            room_count: AtomicU64::new(0),
            inventory_grant_first_success: AtomicU64::new(0),
            inventory_grant_idempotent_hit: AtomicU64::new(0),
            inventory_grant_fingerprint_conflict: AtomicU64::new(0),
            inventory_grant_transaction_failure: AtomicU64::new(0),
            inventory_grant_push_failure: AtomicU64::new(0),
            asset_transaction_duration_ms: AtomicU64::new(0),
            asset_transaction_count: AtomicU64::new(0),
            asset_version_conflict: AtomicU64::new(0),
            asset_capacity_fallback: AtomicU64::new(0),
            reward_mail_created: AtomicU64::new(0),
            client_protocol_auth_accepted_legacy: AtomicU64::new(0),
            client_protocol_auth_accepted_current: AtomicU64::new(0),
            client_protocol_auth_accepted_supported_older: AtomicU64::new(0),
            client_protocol_auth_rejected_too_old: AtomicU64::new(0),
            client_protocol_auth_rejected_too_new: AtomicU64::new(0),
            extra: Mutex::new(HashMap::new()),
        }
    }

    /// 记录一次请求（QPS +1）
    pub fn record_request(&self) {
        self.qps_counter.fetch_add(1, Ordering::Relaxed);
    }

    /// 记录延迟（毫秒）
    pub fn record_latency(&self, duration_ms: u64) {
        self.latency_sum.fetch_add(duration_ms, Ordering::Relaxed);
        self.latency_count.fetch_add(1, Ordering::Relaxed);
    }

    /// 设置在线玩家数
    pub fn set_online_players(&self, val: u64) {
        self.online_players.store(val, Ordering::Relaxed);
    }

    /// 设置房间数
    pub fn set_room_count(&self, val: u64) {
        self.room_count.store(val, Ordering::Relaxed);
    }

    pub fn record_inventory_grant_first_success(&self) {
        self.inventory_grant_first_success
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_inventory_grant_idempotent_hit(&self) {
        self.inventory_grant_idempotent_hit
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_inventory_grant_fingerprint_conflict(&self) {
        self.inventory_grant_fingerprint_conflict
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_inventory_grant_transaction_failure(&self) {
        self.inventory_grant_transaction_failure
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_inventory_grant_push_failure(&self) {
        self.inventory_grant_push_failure
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_asset_transaction_duration(&self, duration_ms: u64) {
        self.asset_transaction_duration_ms
            .fetch_add(duration_ms, Ordering::Relaxed);
        self.asset_transaction_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_asset_version_conflict(&self) {
        self.asset_version_conflict.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_asset_capacity_fallback(&self) {
        self.asset_capacity_fallback.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_reward_mail_created(&self) {
        self.reward_mail_created.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_client_protocol_version(&self, metric: ClientProtocolVersionMetric) {
        let counter = match metric {
            ClientProtocolVersionMetric::AcceptedLegacy => {
                &self.client_protocol_auth_accepted_legacy
            }
            ClientProtocolVersionMetric::AcceptedCurrent => {
                &self.client_protocol_auth_accepted_current
            }
            ClientProtocolVersionMetric::AcceptedSupportedOlder => {
                &self.client_protocol_auth_accepted_supported_older
            }
            ClientProtocolVersionMetric::RejectedTooOld => {
                &self.client_protocol_auth_rejected_too_old
            }
            ClientProtocolVersionMetric::RejectedTooNew => {
                &self.client_protocol_auth_rejected_too_new
            }
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// 设置扩展字段
    pub fn set_extra(&self, key: impl Into<String>, value: impl Into<String>) {
        let mut extra = self.extra.lock().unwrap();
        extra.insert(key.into(), value.into());
    }

    /// 启动指标上报任务
    ///
    /// # Arguments
    /// * `nats_url` - NATS 连接 URL
    /// * `service_instance_id` - 服务实例 ID
    /// * `interval_secs` - 上报间隔（秒）
    pub async fn start_reporting(
        &'static self,
        nats_url: &str,
        service_instance_id: String,
        interval_secs: u64,
    ) {
        let client = match async_nats::connect(nats_url).await {
            Ok(client) => client,
            Err(e) => {
                error!(error = %e, "failed to connect nats for metrics");
                return;
            }
        };

        let service_name = "game-server";

        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(interval_secs));

            loop {
                ticker.tick().await;

                // 读取并归零计数器
                let qps = self.qps_counter.swap(0, Ordering::Relaxed);
                let latency_sum = self.latency_sum.swap(0, Ordering::Relaxed);
                let latency_count = self.latency_count.swap(0, Ordering::Relaxed);
                let online_players = self.online_players.load(Ordering::Relaxed);
                let room_count = self.room_count.load(Ordering::Relaxed);
                let inventory_grant_first_success = self
                    .inventory_grant_first_success
                    .swap(0, Ordering::Relaxed);
                let inventory_grant_idempotent_hit = self
                    .inventory_grant_idempotent_hit
                    .swap(0, Ordering::Relaxed);
                let inventory_grant_fingerprint_conflict = self
                    .inventory_grant_fingerprint_conflict
                    .swap(0, Ordering::Relaxed);
                let inventory_grant_transaction_failure = self
                    .inventory_grant_transaction_failure
                    .swap(0, Ordering::Relaxed);
                let inventory_grant_push_failure =
                    self.inventory_grant_push_failure.swap(0, Ordering::Relaxed);
                let asset_transaction_duration_ms = self
                    .asset_transaction_duration_ms
                    .swap(0, Ordering::Relaxed);
                let asset_transaction_count =
                    self.asset_transaction_count.swap(0, Ordering::Relaxed);
                let asset_version_conflict = self.asset_version_conflict.swap(0, Ordering::Relaxed);
                let asset_capacity_fallback =
                    self.asset_capacity_fallback.swap(0, Ordering::Relaxed);
                let reward_mail_created = self.reward_mail_created.swap(0, Ordering::Relaxed);
                let client_protocol_auth_accepted_legacy = self
                    .client_protocol_auth_accepted_legacy
                    .swap(0, Ordering::Relaxed);
                let client_protocol_auth_accepted_current = self
                    .client_protocol_auth_accepted_current
                    .swap(0, Ordering::Relaxed);
                let client_protocol_auth_accepted_supported_older = self
                    .client_protocol_auth_accepted_supported_older
                    .swap(0, Ordering::Relaxed);
                let client_protocol_auth_rejected_too_old = self
                    .client_protocol_auth_rejected_too_old
                    .swap(0, Ordering::Relaxed);
                let client_protocol_auth_rejected_too_new = self
                    .client_protocol_auth_rejected_too_new
                    .swap(0, Ordering::Relaxed);

                // 计算聚合延迟
                let latency_ms = if latency_count > 0 {
                    latency_sum / latency_count
                } else {
                    0
                };
                let asset_transaction_latency_ms = if asset_transaction_count > 0 {
                    asset_transaction_duration_ms / asset_transaction_count
                } else {
                    0
                };

                let bucket = current_bucket();
                // 收集扩展字段
                let extra = {
                    let guard = self.extra.lock().unwrap();
                    guard.clone()
                };

                let mut fields: Vec<(String, String)> = vec![
                    ("qps".to_string(), qps.to_string()),
                    ("latency_ms".to_string(), latency_ms.to_string()),
                    ("online_players".to_string(), online_players.to_string()),
                    ("room_count".to_string(), room_count.to_string()),
                    (
                        "inventory_grant_first_success_total".to_string(),
                        inventory_grant_first_success.to_string(),
                    ),
                    (
                        "inventory_grant_idempotent_hit_total".to_string(),
                        inventory_grant_idempotent_hit.to_string(),
                    ),
                    (
                        "inventory_grant_fingerprint_conflict_total".to_string(),
                        inventory_grant_fingerprint_conflict.to_string(),
                    ),
                    (
                        "inventory_grant_transaction_failure_total".to_string(),
                        inventory_grant_transaction_failure.to_string(),
                    ),
                    (
                        "inventory_grant_push_failure_total".to_string(),
                        inventory_grant_push_failure.to_string(),
                    ),
                    (
                        "asset_transaction_latency_ms".to_string(),
                        asset_transaction_latency_ms.to_string(),
                    ),
                    (
                        "asset_transaction_count".to_string(),
                        asset_transaction_count.to_string(),
                    ),
                    (
                        "asset_version_conflict_total".to_string(),
                        asset_version_conflict.to_string(),
                    ),
                    (
                        "asset_capacity_fallback_total".to_string(),
                        asset_capacity_fallback.to_string(),
                    ),
                    (
                        "reward_mail_created_total".to_string(),
                        reward_mail_created.to_string(),
                    ),
                    (
                        "client_protocol_auth_accepted_legacy_total".to_string(),
                        client_protocol_auth_accepted_legacy.to_string(),
                    ),
                    (
                        "client_protocol_auth_accepted_current_total".to_string(),
                        client_protocol_auth_accepted_current.to_string(),
                    ),
                    (
                        "client_protocol_auth_accepted_supported_older_total".to_string(),
                        client_protocol_auth_accepted_supported_older.to_string(),
                    ),
                    (
                        "client_protocol_auth_rejected_too_old_total".to_string(),
                        client_protocol_auth_rejected_too_old.to_string(),
                    ),
                    (
                        "client_protocol_auth_rejected_too_new_total".to_string(),
                        client_protocol_auth_rejected_too_new.to_string(),
                    ),
                ];

                fields.extend(collect_discovery_metric_fields(true));

                for (k, v) in extra {
                    fields.push((k, v));
                }

                if let Err(e) =
                    publish_metrics(&client, service_name, &service_instance_id, bucket, fields)
                        .await
                {
                    error!(error = %e, "failed to publish metrics to nats");
                }

                info!(
                    bucket = bucket,
                    qps = qps,
                    latency_ms = latency_ms,
                    online_players = online_players,
                    room_count = room_count,
                    "metrics reported"
                );
            }
        });
    }
}

impl Default for MetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}

/// 全局 MetricsCollector 实例
pub static METRICS: LazyLock<MetricsCollector, fn() -> MetricsCollector> =
    LazyLock::new(MetricsCollector::new);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_current_bucket() {
        let bucket = current_bucket();
        // Bucket 应该是 5 的倍数
        assert_eq!(bucket % 5, 0);
    }

    #[test]
    fn test_metrics_collector() {
        let collector = MetricsCollector::new();

        collector.record_request();
        collector.record_latency(100);
        collector.set_online_players(10);
        collector.set_room_count(5);
        collector.record_inventory_grant_first_success();
        collector.record_inventory_grant_idempotent_hit();
        collector.record_inventory_grant_fingerprint_conflict();
        collector.record_inventory_grant_transaction_failure();
        collector.record_inventory_grant_push_failure();
        collector.record_asset_transaction_duration(12);
        collector.record_asset_version_conflict();
        collector.record_asset_capacity_fallback();
        collector.record_reward_mail_created();
        collector.record_client_protocol_version(ClientProtocolVersionMetric::AcceptedLegacy);
        collector
            .record_client_protocol_version(ClientProtocolVersionMetric::AcceptedSupportedOlder);
        collector.record_client_protocol_version(ClientProtocolVersionMetric::RejectedTooNew);

        // 验证计数器工作正常
        assert_eq!(collector.qps_counter.load(Ordering::Relaxed), 1);
        assert_eq!(collector.latency_sum.load(Ordering::Relaxed), 100);
        assert_eq!(collector.latency_count.load(Ordering::Relaxed), 1);
        assert_eq!(collector.online_players.load(Ordering::Relaxed), 10);
        assert_eq!(collector.room_count.load(Ordering::Relaxed), 5);
        assert_eq!(
            collector
                .inventory_grant_first_success
                .load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            collector
                .inventory_grant_idempotent_hit
                .load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            collector
                .inventory_grant_fingerprint_conflict
                .load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            collector
                .inventory_grant_transaction_failure
                .load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            collector
                .inventory_grant_push_failure
                .load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            collector
                .asset_transaction_duration_ms
                .load(Ordering::Relaxed),
            12
        );
        assert_eq!(collector.asset_transaction_count.load(Ordering::Relaxed), 1);
        assert_eq!(collector.asset_version_conflict.load(Ordering::Relaxed), 1);
        assert_eq!(collector.asset_capacity_fallback.load(Ordering::Relaxed), 1);
        assert_eq!(collector.reward_mail_created.load(Ordering::Relaxed), 1);
        assert_eq!(
            collector
                .client_protocol_auth_accepted_legacy
                .load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            collector
                .client_protocol_auth_rejected_too_new
                .load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            collector
                .client_protocol_auth_accepted_supported_older
                .load(Ordering::Relaxed),
            1
        );
    }
}
