use redis::AsyncCommands;

use crate::types::ServiceInstance;

/// 服务注册中心客户端
pub struct RegistryClient {
    redis: redis::Client,
    instance_id: String,
    service_name: String,
    heartbeat_interval_secs: u64,
    heartbeat_ttl_secs: u64,
}

impl RegistryClient {
    /// 创建新的注册中心客户端
    pub async fn new(
        redis_url: &str,
        service_name: &str,
        instance_id: &str,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let redis = redis::Client::open(redis_url)?;
        // 测试连接
        let _conn = redis.get_multiplexed_async_connection().await?;

        Ok(Self {
            redis,
            instance_id: instance_id.to_string(),
            service_name: service_name.to_string(),
            heartbeat_interval_secs: 10,
            heartbeat_ttl_secs: 30,
        })
    }

    /// 设置心跳间隔（秒）
    pub fn with_heartbeat_interval(mut self, secs: u64) -> Self {
        self.heartbeat_interval_secs = secs;
        self
    }

    /// 设置心跳 TTL（秒）
    pub fn with_heartbeat_ttl(mut self, secs: u64) -> Self {
        self.heartbeat_ttl_secs = secs;
        self
    }

    /// 注册服务实例
    pub async fn register(&self, instance: &ServiceInstance) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut conn = self.redis.get_multiplexed_async_connection().await?;
        let key = self.instance_key();
        let json = serde_json::to_string(instance)?;

        // 使用 HSET 存储 JSON 数据
        let _: () = redis::cmd("HSET")
            .arg(&key)
            .arg("data")
            .arg(&json)
            .query_async(&mut conn)
            .await?;

        // 创建心跳 Key
        let heartbeat_key = self.heartbeat_key();
        let _: () = conn.set_ex(&heartbeat_key, "1", self.heartbeat_ttl_secs).await?;

        tracing::info!(
            service = %self.service_name,
            instance = %self.instance_id,
            "service registered"
        );

        Ok(())
    }

    /// 注销服务实例
    pub async fn deregister(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut conn = self.redis.get_multiplexed_async_connection().await?;
        let key = self.instance_key();
        let heartbeat_key = self.heartbeat_key();

        conn.del::<_, ()>(&key).await?;
        conn.del::<_, ()>(&heartbeat_key).await?;

        tracing::info!(
            service = %self.service_name,
            instance = %self.instance_id,
            "service deregistered"
        );

        Ok(())
    }

    /// 发送心跳
    pub async fn heartbeat(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut conn = self.redis.get_multiplexed_async_connection().await?;
        let heartbeat_key = self.heartbeat_key();

        let _: () = conn.set_ex(&heartbeat_key, "1", self.heartbeat_ttl_secs).await?;

        Ok(())
    }

    /// 启动心跳任务
    pub fn start_heartbeat_task(&self) -> tokio::task::JoinHandle<()> {
        let heartbeat_ttl = self.heartbeat_ttl_secs;
        let heartbeat_interval = self.heartbeat_interval_secs;
        let redis = self.redis.clone();
        let instance_id = self.instance_id.clone();
        let service_name = self.service_name.clone();

        tokio::spawn(async move {
            let heartbeat_key = format!("heartbeat:{}:{}", service_name, instance_id);
            let ttl = heartbeat_ttl;
            let interval = heartbeat_interval;

            // 立即发送一次心跳
            if let Ok(mut conn) = redis.get_multiplexed_async_connection().await {
                let _: Result<(), _> = conn.set_ex::<_, _, ()>(&heartbeat_key, "1", ttl).await;
            }

            let mut ticker = tokio::time::interval(tokio::time::Duration::from_secs(interval));
            loop {
                ticker.tick().await;

                if let Ok(mut conn) = redis.get_multiplexed_async_connection().await {
                    let result: Result<(), _> = conn.set_ex::<_, _, ()>(&heartbeat_key, "1", ttl).await;
                    if result.is_err() {
                        tracing::warn!("failed to send heartbeat");
                    }
                }
            }
        })
    }

    /// 发现服务实例（查询所有健康实例）
    pub async fn discover(&self, service_name: &str) -> Result<Vec<ServiceInstance>, Box<dyn std::error::Error + Send + Sync>> {
        let mut conn = self.redis.get_multiplexed_async_connection().await?;
        let pattern = format!("service:{}:instances:*", service_name);

        // 使用 SCAN 而不是 KEYS（生产环境更安全）
        let mut cursor = 0_isize;
        let mut keys = Vec::new();

        loop {
            let (new_cursor, batch): (isize, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(100)
                .query_async(&mut conn)
                .await?;

            keys.extend(batch);
            cursor = new_cursor;

            if cursor == 0 {
                break;
            }
        }

        let mut instances = Vec::new();

        for key in keys {
            let instance_id = key.split(':').last().unwrap_or("");
            let heartbeat_key = format!("heartbeat:{}:{}", service_name, instance_id);

            // 检查心跳是否存在
            let exists: bool = conn.exists(&heartbeat_key).await?;
            if !exists {
                continue;
            }

            // 获取实例数据
            let data: Option<String> = conn.hget(&key, "data").await?;
            if let Some(json) = data {
                if let Ok(instance) = serde_json::from_str::<ServiceInstance>(&json) {
                    instances.push(instance);
                }
            }
        }

        Ok(instances)
    }

    /// 发现单个健康实例（用于 proxy 路由）
    pub async fn discover_one(&self, service_name: &str) -> Result<Option<ServiceInstance>, Box<dyn std::error::Error + Send + Sync>> {
        let instances = self.discover(service_name).await?;

        if instances.is_empty() {
            return Ok(None);
        }

        // 简单策略：返回第一个健康的实例
        // 未来可扩展为加权随机、轮询等
        Ok(Some(instances[0].clone()))
    }

    /// 获取当前实例的 Key
    fn instance_key(&self) -> String {
        format!("service:{}:instances:{}", self.service_name, self.instance_id)
    }

    /// 获取心跳 Key
    fn heartbeat_key(&self) -> String {
        format!("heartbeat:{}:{}", self.service_name, self.instance_id)
    }

    /// 获取服务名称
    pub fn service_name(&self) -> &str {
        &self.service_name
    }

    /// 获取实例 ID
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_service_instance_creation() {
        let instance = ServiceInstance::new(
            "test-001".to_string(),
            "game-server".to_string(),
            "127.0.0.1".to_string(),
            7000,
        )
        .with_admin_port(7001)
        .with_local_socket("test.sock".to_string());

        assert_eq!(instance.id, "test-001");
        assert_eq!(instance.port, 7000);
        assert_eq!(instance.admin_port, 7001);
        assert_eq!(instance.local_socket, "test.sock");
    }
}
