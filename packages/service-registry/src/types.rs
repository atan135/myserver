use serde::{Deserialize, Serialize};

/// 服务实例信息
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct ServiceInstance {
    /// 实例唯一标识
    pub id: String,
    /// 服务名称
    pub name: String,
    /// 主机地址
    pub host: String,
    /// 服务端口
    pub port: u16,
    /// 管理端口（可选）
    #[serde(default)]
    pub admin_port: u16,
    /// Unix Domain Socket 名称（用于 Rust 服务间通信）
    #[serde(default)]
    pub local_socket: String,
    /// 服务标签
    #[serde(default)]
    pub tags: Vec<String>,
    /// 权重（用于负载均衡）
    #[serde(default = "default_weight")]
    pub weight: u32,
    /// 额外元数据
    #[serde(default)]
    pub metadata: serde_json::Value,
    /// 注册时间戳
    pub registered_at: i64,
    /// 是否健康
    #[serde(default = "default_healthy")]
    pub healthy: bool,
}

fn default_weight() -> u32 {
    100
}

fn default_healthy() -> bool {
    true
}

impl ServiceInstance {
    /// 创建新的服务实例
    pub fn new(
        id: String,
        name: String,
        host: String,
        port: u16,
    ) -> Self {
        Self {
            id,
            name,
            host,
            port,
            admin_port: 0,
            local_socket: String::new(),
            tags: Vec::new(),
            weight: 100,
            metadata: serde_json::Value::Object(serde_json::Map::new()),
            registered_at: chrono_timestamp(),
            healthy: true,
        }
    }

    /// 设置管理端口
    pub fn with_admin_port(mut self, admin_port: u16) -> Self {
        self.admin_port = admin_port;
        self
    }

    /// 设置 UDS 名称
    pub fn with_local_socket(mut self, local_socket: String) -> Self {
        self.local_socket = local_socket;
        self
    }

    /// 设置标签
    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    /// 设置权重
    pub fn with_weight(mut self, weight: u32) -> Self {
        self.weight = weight;
        self
    }

    /// 设置元数据
    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = metadata;
        self
    }
}

fn chrono_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
