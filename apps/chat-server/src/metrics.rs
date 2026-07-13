//! Chat Server Metrics Module
//!
//! 监控指标收集与 NATS 上报

use std::collections::HashMap;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::json;
use tokio::time::interval;
use tracing::{error, info};

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
    /// 收到的邮件通知数
    mail_notification_received: AtomicU64,
    /// 邮件通知解析或契约校验失败数
    mail_notification_parse_failed: AtomicU64,
    /// 因未知版本拒绝的邮件通知数
    mail_notification_version_rejected: AtomicU64,
    /// 按 event_id 命中的邮件通知去重数
    mail_notification_deduplicated: AtomicU64,
    /// 成功进入当前在线 session 队列的邮件通知数
    mail_notification_pushed: AtomicU64,
    /// 玩家离线而跳过的邮件通知数
    mail_notification_offline_skipped: AtomicU64,
    /// session 队列已满或关闭导致的邮件通知失败数
    mail_notification_queue_failed: AtomicU64,
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
            mail_notification_received: AtomicU64::new(0),
            mail_notification_parse_failed: AtomicU64::new(0),
            mail_notification_version_rejected: AtomicU64::new(0),
            mail_notification_deduplicated: AtomicU64::new(0),
            mail_notification_pushed: AtomicU64::new(0),
            mail_notification_offline_skipped: AtomicU64::new(0),
            mail_notification_queue_failed: AtomicU64::new(0),
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

    pub fn record_mail_notification_received(&self) {
        self.mail_notification_received
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_mail_notification_parse_failed(&self) {
        self.mail_notification_parse_failed
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_mail_notification_version_rejected(&self) {
        self.mail_notification_version_rejected
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_mail_notification_deduplicated(&self) {
        self.mail_notification_deduplicated
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_mail_notification_pushed(&self) {
        self.mail_notification_pushed
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_mail_notification_offline_skipped(&self) {
        self.mail_notification_offline_skipped
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_mail_notification_queue_failed(&self) {
        self.mail_notification_queue_failed
            .fetch_add(1, Ordering::Relaxed);
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

        let service_name = "chat-server";

        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(interval_secs));

            loop {
                ticker.tick().await;

                // 读取并归零计数器
                let qps = self.qps_counter.swap(0, Ordering::Relaxed);
                let latency_sum = self.latency_sum.swap(0, Ordering::Relaxed);
                let latency_count = self.latency_count.swap(0, Ordering::Relaxed);
                let online_players = self.online_players.load(Ordering::Relaxed);
                let mail_notification_received =
                    self.mail_notification_received.swap(0, Ordering::Relaxed);
                let mail_notification_parse_failed = self
                    .mail_notification_parse_failed
                    .swap(0, Ordering::Relaxed);
                let mail_notification_version_rejected = self
                    .mail_notification_version_rejected
                    .swap(0, Ordering::Relaxed);
                let mail_notification_deduplicated = self
                    .mail_notification_deduplicated
                    .swap(0, Ordering::Relaxed);
                let mail_notification_pushed =
                    self.mail_notification_pushed.swap(0, Ordering::Relaxed);
                let mail_notification_offline_skipped = self
                    .mail_notification_offline_skipped
                    .swap(0, Ordering::Relaxed);
                let mail_notification_queue_failed = self
                    .mail_notification_queue_failed
                    .swap(0, Ordering::Relaxed);

                // 计算聚合延迟
                let latency_ms = if latency_count > 0 {
                    latency_sum / latency_count
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
                    (
                        "mail_notification_received".to_string(),
                        mail_notification_received.to_string(),
                    ),
                    (
                        "mail_notification_parse_failed".to_string(),
                        mail_notification_parse_failed.to_string(),
                    ),
                    (
                        "mail_notification_version_rejected".to_string(),
                        mail_notification_version_rejected.to_string(),
                    ),
                    (
                        "mail_notification_deduplicated".to_string(),
                        mail_notification_deduplicated.to_string(),
                    ),
                    (
                        "mail_notification_pushed".to_string(),
                        mail_notification_pushed.to_string(),
                    ),
                    (
                        "mail_notification_offline_skipped".to_string(),
                        mail_notification_offline_skipped.to_string(),
                    ),
                    (
                        "mail_notification_queue_failed".to_string(),
                        mail_notification_queue_failed.to_string(),
                    ),
                ];

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

        // 验证计数器工作正常
        assert_eq!(collector.qps_counter.load(Ordering::Relaxed), 1);
        assert_eq!(collector.latency_sum.load(Ordering::Relaxed), 100);
        assert_eq!(collector.latency_count.load(Ordering::Relaxed), 1);
        assert_eq!(collector.online_players.load(Ordering::Relaxed), 10);

        collector.record_mail_notification_received();
        collector.record_mail_notification_parse_failed();
        collector.record_mail_notification_version_rejected();
        collector.record_mail_notification_deduplicated();
        collector.record_mail_notification_pushed();
        collector.record_mail_notification_offline_skipped();
        collector.record_mail_notification_queue_failed();
        assert_eq!(
            collector.mail_notification_received.load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            collector
                .mail_notification_parse_failed
                .load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            collector
                .mail_notification_version_rejected
                .load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            collector
                .mail_notification_deduplicated
                .load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            collector.mail_notification_pushed.load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            collector
                .mail_notification_offline_skipped
                .load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            collector
                .mail_notification_queue_failed
                .load(Ordering::Relaxed),
            1
        );
    }
}
