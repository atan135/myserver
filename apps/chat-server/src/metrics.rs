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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MetricTransport {
    Tcp,
    WebSocket,
}

pub(crate) struct ConnectionMetricGuard<'a> {
    collector: &'a MetricsCollector,
    transport: MetricTransport,
}

impl Drop for ConnectionMetricGuard<'_> {
    fn drop(&mut self) {
        self.collector.connection_closed(self.transport);
    }
}

/// Tracks every accepted transport connection, including a WebSocket while it
/// is still completing its HTTP Upgrade. This is the per-instance capacity
/// gauge rather than the post-upgrade WebSocket session gauge.
pub(crate) struct ConnectionCapacityMetricGuard<'a> {
    collector: &'a MetricsCollector,
}

impl Drop for ConnectionCapacityMetricGuard<'_> {
    fn drop(&mut self) {
        self.collector.connection_capacity_closed();
    }
}

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
    /// 当前实例已接受、尚未释放的 TCP/WSS 连接数
    connection_capacity_current: AtomicU64,
    /// 因当前实例总连接数达到容量上限而拒绝的连接数
    connection_capacity_rejected: AtomicU64,
    /// 收到的邮件通知数
    mail_notification_received: AtomicU64,
    /// 邮件通知解析或契约校验失败数
    mail_notification_parse_failed: AtomicU64,
    /// 因未知版本拒绝的邮件通知数
    mail_notification_version_rejected: AtomicU64,
    /// 兼容开关关闭或截止时间到期后拒绝的旧格式通知数
    mail_notification_legacy_rejected: AtomicU64,
    /// 按 event_id 命中的邮件通知去重数
    mail_notification_deduplicated: AtomicU64,
    /// 成功进入当前在线 session 队列的邮件通知数
    mail_notification_pushed: AtomicU64,
    /// 玩家离线而跳过的邮件通知数
    mail_notification_offline_skipped: AtomicU64,
    /// session 队列已满或关闭导致的邮件通知失败数
    mail_notification_queue_failed: AtomicU64,
    /// Redis 查询失败导致跳过的跨实例聊天在线 push 数
    chat_push_route_lookup_failed: AtomicU64,
    /// 路由不存在导致跳过的跨实例聊天在线 push 数
    chat_push_route_unavailable: AtomicU64,
    /// 成功进入有界跨实例 publish 队列的聊天 push 数
    chat_push_remote_queued: AtomicU64,
    /// 跨实例 publish 队列满或关闭导致的聊天 push 数
    chat_push_remote_queue_failed: AtomicU64,
    /// 已由 Core NATS 接收的跨实例聊天 push 数
    chat_push_published: AtomicU64,
    /// Core NATS 发布失败的跨实例聊天 push 数
    chat_push_publish_failed: AtomicU64,
    /// 当前实例收到的跨实例聊天 push 数
    chat_push_received: AtomicU64,
    /// 无效或超限跨实例聊天 push payload 数
    chat_push_payload_rejected: AtomicU64,
    /// 陈旧路由、实例不匹配或 session 已迁移时跳过的聊天 push 数
    chat_push_stale_skipped: AtomicU64,
    /// 成功进入当前有效 session 队列的聊天 push 数
    chat_push_delivered: AtomicU64,
    /// 当前 session 队列满或关闭导致的聊天 push 数
    chat_push_session_queue_failed: AtomicU64,
    /// 当前 TCP 连接数
    tcp_connections_current: AtomicU64,
    /// 当前已完成握手的 WebSocket 连接数
    websocket_connections_current: AtomicU64,
    /// WebSocket 握手成功数
    websocket_handshake_success: AtomicU64,
    /// WebSocket 握手失败数（含握手容量拒绝）
    websocket_handshake_failure: AtomicU64,
    /// 超过 WebSocket 全实例握手速率窗口而拒绝的连接数
    websocket_handshake_rate_limited: AtomicU64,
    /// WebSocket message/frame 契约拒绝数
    websocket_frame_rejected: AtomicU64,
    /// WebSocket 非正常关闭数
    websocket_abnormal_close: AtomicU64,
    /// TCP 出站队列失败数
    tcp_outbound_queue_failure: AtomicU64,
    /// WebSocket 出站队列失败数
    websocket_outbound_queue_failure: AtomicU64,
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
            connection_capacity_current: AtomicU64::new(0),
            connection_capacity_rejected: AtomicU64::new(0),
            mail_notification_received: AtomicU64::new(0),
            mail_notification_parse_failed: AtomicU64::new(0),
            mail_notification_version_rejected: AtomicU64::new(0),
            mail_notification_legacy_rejected: AtomicU64::new(0),
            mail_notification_deduplicated: AtomicU64::new(0),
            mail_notification_pushed: AtomicU64::new(0),
            mail_notification_offline_skipped: AtomicU64::new(0),
            mail_notification_queue_failed: AtomicU64::new(0),
            chat_push_route_lookup_failed: AtomicU64::new(0),
            chat_push_route_unavailable: AtomicU64::new(0),
            chat_push_remote_queued: AtomicU64::new(0),
            chat_push_remote_queue_failed: AtomicU64::new(0),
            chat_push_published: AtomicU64::new(0),
            chat_push_publish_failed: AtomicU64::new(0),
            chat_push_received: AtomicU64::new(0),
            chat_push_payload_rejected: AtomicU64::new(0),
            chat_push_stale_skipped: AtomicU64::new(0),
            chat_push_delivered: AtomicU64::new(0),
            chat_push_session_queue_failed: AtomicU64::new(0),
            tcp_connections_current: AtomicU64::new(0),
            websocket_connections_current: AtomicU64::new(0),
            websocket_handshake_success: AtomicU64::new(0),
            websocket_handshake_failure: AtomicU64::new(0),
            websocket_handshake_rate_limited: AtomicU64::new(0),
            websocket_frame_rejected: AtomicU64::new(0),
            websocket_abnormal_close: AtomicU64::new(0),
            tcp_outbound_queue_failure: AtomicU64::new(0),
            websocket_outbound_queue_failure: AtomicU64::new(0),
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

    pub(crate) fn track_connection_capacity(&self) -> ConnectionCapacityMetricGuard<'_> {
        self.connection_capacity_current
            .fetch_add(1, Ordering::Relaxed);
        ConnectionCapacityMetricGuard { collector: self }
    }

    pub(crate) fn record_connection_capacity_rejected(&self) {
        self.connection_capacity_rejected
            .fetch_add(1, Ordering::Relaxed);
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

    pub fn record_mail_notification_legacy_rejected(&self) {
        self.mail_notification_legacy_rejected
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

    pub fn record_chat_push_route_lookup_failed(&self) {
        self.chat_push_route_lookup_failed
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_chat_push_route_unavailable(&self) {
        self.chat_push_route_unavailable
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_chat_push_remote_queued(&self) {
        self.chat_push_remote_queued.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_chat_push_remote_queue_failed(&self) {
        self.chat_push_remote_queue_failed
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_chat_push_published(&self) {
        self.chat_push_published.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_chat_push_publish_failed(&self) {
        self.chat_push_publish_failed
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_chat_push_received(&self) {
        self.chat_push_received.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_chat_push_payload_rejected(&self) {
        self.chat_push_payload_rejected
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_chat_push_stale_skipped(&self) {
        self.chat_push_stale_skipped.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_chat_push_delivered(&self) {
        self.chat_push_delivered.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_chat_push_session_queue_failed(&self) {
        self.chat_push_session_queue_failed
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn track_connection(&self, transport: MetricTransport) -> ConnectionMetricGuard<'_> {
        self.connection_counter(transport)
            .fetch_add(1, Ordering::Relaxed);
        ConnectionMetricGuard {
            collector: self,
            transport,
        }
    }

    pub(crate) fn record_websocket_handshake_success(&self) {
        self.websocket_handshake_success
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_websocket_handshake_failure(&self) {
        self.websocket_handshake_failure
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_websocket_handshake_rate_limited(&self) {
        self.websocket_handshake_rate_limited
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_websocket_frame_rejected(&self) {
        self.websocket_frame_rejected
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_websocket_abnormal_close(&self) {
        self.websocket_abnormal_close
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_outbound_queue_failure(&self, transport: MetricTransport) {
        match transport {
            MetricTransport::Tcp => &self.tcp_outbound_queue_failure,
            MetricTransport::WebSocket => &self.websocket_outbound_queue_failure,
        }
        .fetch_add(1, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn outbound_queue_failure_count(&self, transport: MetricTransport) -> u64 {
        match transport {
            MetricTransport::Tcp => &self.tcp_outbound_queue_failure,
            MetricTransport::WebSocket => &self.websocket_outbound_queue_failure,
        }
        .load(Ordering::Relaxed)
    }

    fn connection_counter(&self, transport: MetricTransport) -> &AtomicU64 {
        match transport {
            MetricTransport::Tcp => &self.tcp_connections_current,
            MetricTransport::WebSocket => &self.websocket_connections_current,
        }
    }

    fn connection_closed(&self, transport: MetricTransport) {
        let counter = self.connection_counter(transport);
        let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            Some(current.saturating_sub(1))
        });
    }

    fn connection_capacity_closed(&self) {
        let _ = self.connection_capacity_current.fetch_update(
            Ordering::Relaxed,
            Ordering::Relaxed,
            |current| Some(current.saturating_sub(1)),
        );
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
        let client = match nats_client::connect(nats_url).await {
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
                let connection_capacity_current =
                    self.connection_capacity_current.load(Ordering::Relaxed);
                let connection_capacity_rejected =
                    self.connection_capacity_rejected.swap(0, Ordering::Relaxed);
                let mail_notification_received =
                    self.mail_notification_received.swap(0, Ordering::Relaxed);
                let mail_notification_parse_failed = self
                    .mail_notification_parse_failed
                    .swap(0, Ordering::Relaxed);
                let mail_notification_version_rejected = self
                    .mail_notification_version_rejected
                    .swap(0, Ordering::Relaxed);
                let mail_notification_legacy_rejected = self
                    .mail_notification_legacy_rejected
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
                let chat_push_route_lookup_failed = self
                    .chat_push_route_lookup_failed
                    .swap(0, Ordering::Relaxed);
                let chat_push_route_unavailable =
                    self.chat_push_route_unavailable.swap(0, Ordering::Relaxed);
                let chat_push_remote_queued =
                    self.chat_push_remote_queued.swap(0, Ordering::Relaxed);
                let chat_push_remote_queue_failed = self
                    .chat_push_remote_queue_failed
                    .swap(0, Ordering::Relaxed);
                let chat_push_published = self.chat_push_published.swap(0, Ordering::Relaxed);
                let chat_push_publish_failed =
                    self.chat_push_publish_failed.swap(0, Ordering::Relaxed);
                let chat_push_received = self.chat_push_received.swap(0, Ordering::Relaxed);
                let chat_push_payload_rejected =
                    self.chat_push_payload_rejected.swap(0, Ordering::Relaxed);
                let chat_push_stale_skipped =
                    self.chat_push_stale_skipped.swap(0, Ordering::Relaxed);
                let chat_push_delivered = self.chat_push_delivered.swap(0, Ordering::Relaxed);
                let chat_push_session_queue_failed = self
                    .chat_push_session_queue_failed
                    .swap(0, Ordering::Relaxed);
                let tcp_connections_current = self.tcp_connections_current.load(Ordering::Relaxed);
                let websocket_connections_current =
                    self.websocket_connections_current.load(Ordering::Relaxed);
                let websocket_handshake_success =
                    self.websocket_handshake_success.swap(0, Ordering::Relaxed);
                let websocket_handshake_failure =
                    self.websocket_handshake_failure.swap(0, Ordering::Relaxed);
                let websocket_handshake_rate_limited = self
                    .websocket_handshake_rate_limited
                    .swap(0, Ordering::Relaxed);
                let websocket_frame_rejected =
                    self.websocket_frame_rejected.swap(0, Ordering::Relaxed);
                let websocket_abnormal_close =
                    self.websocket_abnormal_close.swap(0, Ordering::Relaxed);
                let tcp_outbound_queue_failure =
                    self.tcp_outbound_queue_failure.swap(0, Ordering::Relaxed);
                let websocket_outbound_queue_failure = self
                    .websocket_outbound_queue_failure
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
                        "connection_capacity_current".to_string(),
                        connection_capacity_current.to_string(),
                    ),
                    (
                        "connection_capacity_rejected_total".to_string(),
                        connection_capacity_rejected.to_string(),
                    ),
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
                        "mail_notification_legacy_rejected".to_string(),
                        mail_notification_legacy_rejected.to_string(),
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
                    (
                        "chat_push_route_lookup_failed".to_string(),
                        chat_push_route_lookup_failed.to_string(),
                    ),
                    (
                        "chat_push_route_unavailable".to_string(),
                        chat_push_route_unavailable.to_string(),
                    ),
                    (
                        "chat_push_remote_queued".to_string(),
                        chat_push_remote_queued.to_string(),
                    ),
                    (
                        "chat_push_remote_queue_failed".to_string(),
                        chat_push_remote_queue_failed.to_string(),
                    ),
                    (
                        "chat_push_published".to_string(),
                        chat_push_published.to_string(),
                    ),
                    (
                        "chat_push_publish_failed".to_string(),
                        chat_push_publish_failed.to_string(),
                    ),
                    (
                        "chat_push_received".to_string(),
                        chat_push_received.to_string(),
                    ),
                    (
                        "chat_push_payload_rejected".to_string(),
                        chat_push_payload_rejected.to_string(),
                    ),
                    (
                        "chat_push_stale_skipped".to_string(),
                        chat_push_stale_skipped.to_string(),
                    ),
                    (
                        "chat_push_delivered".to_string(),
                        chat_push_delivered.to_string(),
                    ),
                    (
                        "chat_push_session_queue_failed".to_string(),
                        chat_push_session_queue_failed.to_string(),
                    ),
                    (
                        "tcp_connections_current".to_string(),
                        tcp_connections_current.to_string(),
                    ),
                    (
                        "websocket_connections_current".to_string(),
                        websocket_connections_current.to_string(),
                    ),
                    (
                        "websocket_handshake_success_total".to_string(),
                        websocket_handshake_success.to_string(),
                    ),
                    (
                        "websocket_handshake_failure_total".to_string(),
                        websocket_handshake_failure.to_string(),
                    ),
                    (
                        "websocket_handshake_rate_limited_total".to_string(),
                        websocket_handshake_rate_limited.to_string(),
                    ),
                    (
                        "websocket_frame_rejected_total".to_string(),
                        websocket_frame_rejected.to_string(),
                    ),
                    (
                        "websocket_abnormal_close_total".to_string(),
                        websocket_abnormal_close.to_string(),
                    ),
                    (
                        "tcp_outbound_queue_failure_total".to_string(),
                        tcp_outbound_queue_failure.to_string(),
                    ),
                    (
                        "websocket_outbound_queue_failure_total".to_string(),
                        websocket_outbound_queue_failure.to_string(),
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

        {
            let _capacity = collector.track_connection_capacity();
            assert_eq!(
                collector
                    .connection_capacity_current
                    .load(Ordering::Relaxed),
                1
            );
        }
        assert_eq!(
            collector
                .connection_capacity_current
                .load(Ordering::Relaxed),
            0
        );
        collector.record_connection_capacity_rejected();
        assert_eq!(
            collector
                .connection_capacity_rejected
                .load(Ordering::Relaxed),
            1
        );

        collector.record_mail_notification_received();
        collector.record_mail_notification_parse_failed();
        collector.record_mail_notification_version_rejected();
        collector.record_mail_notification_legacy_rejected();
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
                .mail_notification_legacy_rejected
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

        {
            let _tcp = collector.track_connection(MetricTransport::Tcp);
            let _websocket = collector.track_connection(MetricTransport::WebSocket);
            assert_eq!(collector.tcp_connections_current.load(Ordering::Relaxed), 1);
            assert_eq!(
                collector
                    .websocket_connections_current
                    .load(Ordering::Relaxed),
                1
            );
        }
        assert_eq!(collector.tcp_connections_current.load(Ordering::Relaxed), 0);
        assert_eq!(
            collector
                .websocket_connections_current
                .load(Ordering::Relaxed),
            0
        );

        collector.record_websocket_handshake_success();
        collector.record_websocket_handshake_failure();
        collector.record_websocket_handshake_rate_limited();
        collector.record_websocket_frame_rejected();
        collector.record_websocket_abnormal_close();
        collector.record_outbound_queue_failure(MetricTransport::Tcp);
        collector.record_outbound_queue_failure(MetricTransport::WebSocket);
        assert_eq!(
            collector
                .websocket_handshake_success
                .load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            collector
                .websocket_handshake_failure
                .load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            collector
                .websocket_handshake_rate_limited
                .load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            collector.websocket_frame_rejected.load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            collector.websocket_abnormal_close.load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            collector.tcp_outbound_queue_failure.load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            collector
                .websocket_outbound_queue_failure
                .load(Ordering::Relaxed),
            1
        );
    }

    #[test]
    fn connection_gauge_drop_is_saturating() {
        let collector = MetricsCollector::new();
        collector.connection_closed(MetricTransport::Tcp);
        collector.connection_closed(MetricTransport::WebSocket);
        collector.connection_capacity_closed();

        assert_eq!(collector.tcp_connections_current.load(Ordering::Relaxed), 0);
        assert_eq!(
            collector
                .websocket_connections_current
                .load(Ordering::Relaxed),
            0
        );
        assert_eq!(
            collector
                .connection_capacity_current
                .load(Ordering::Relaxed),
            0
        );
    }
}
