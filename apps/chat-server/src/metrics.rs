//! Chat Server Metrics Module
//!
//! 监控指标收集与 Redis 上报

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;
use std::sync::Mutex;
use std::time::Duration;

use deadpool_redis::{Config as RedisConfig, Runtime as RedisRuntime};
use deadpool_redis::redis::AsyncCommands;
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

    /// 设置扩展字段
    pub fn set_extra(&self, key: impl Into<String>, value: impl Into<String>) {
        let mut extra = self.extra.lock().unwrap();
        extra.insert(key.into(), value.into());
    }

    /// 启动指标上报任务
    ///
    /// # Arguments
    /// * `redis_url` - Redis 连接 URL
    /// * `interval_secs` - 上报间隔（秒）
    pub async fn start_reporting(&'static self, redis_url: &str, interval_secs: u64) {
        let redis_config = RedisConfig::from_url(redis_url);
        let pool = match redis_config.create_pool(Some(RedisRuntime::Tokio1)) {
            Ok(p) => p,
            Err(e) => {
                error!(error = %e, "failed to create redis pool for metrics");
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

                // 计算聚合延迟
                let latency_ms = if latency_count > 0 {
                    latency_sum / latency_count
                } else {
                    0
                };

                let bucket = current_bucket();
                let metrics_key = format!("metrics:{}:{}", service_name, bucket);
                let heartbeat_key = format!("metrics:heartbeat:{}", service_name);

                // 收集扩展字段
                let extra = {
                    let guard = self.extra.lock().unwrap();
                    guard.clone()
                };

                // 上报到 Redis
                let mut con = match pool.get().await {
                    Ok(c) => c,
                    Err(e) => {
                        error!(error = %e, "failed to get redis connection for metrics");
                        continue;
                    }
                };

                // 使用 HSET 写入指标
                let mut fields: Vec<(String, String)> = vec![
                    ("qps".to_string(), qps.to_string()),
                    ("latency_ms".to_string(), latency_ms.to_string()),
                    ("online_players".to_string(), online_players.to_string()),
                ];

                for (k, v) in extra {
                    fields.push((k, v));
                }

                // 写入指标 Hash，TTL 7天
                if let Err(e) = con.hset_multiple::<_, _, _, ()>(&metrics_key, &fields).await {
                    error!(error = %e, metrics_key = %metrics_key, "failed to write metrics to redis");
                }

                // 设置 TTL
                if let Err(e) = con.expire::<_, ()>(&metrics_key, 604800).await {
                    error!(error = %e, metrics_key = %metrics_key, "failed to set TTL");
                }

                // 更新心跳
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
                    .to_string();

                if let Err(e) = con.set_ex::<_, _, ()>(&heartbeat_key, now, 30).await {
                    error!(error = %e, "failed to update heartbeat");
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
    }
}
