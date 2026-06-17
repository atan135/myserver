use serde::{Deserialize, Serialize};

pub const SERVICE_INSTANCE_SCHEMA_VERSION: u32 = 2;

/// 服务实例信息
#[derive(Clone, Serialize, Debug)]
pub struct ServiceInstance {
    /// 服务发现 schema 版本
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// 实例唯一标识
    pub id: String,
    /// 服务名称
    pub name: String,
    /// 主机地址
    #[serde(default)]
    pub host: String,
    /// 服务端口
    #[serde(default)]
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
    /// 统一服务端点
    #[serde(default)]
    pub endpoints: Vec<ServiceEndpoint>,
    #[serde(skip)]
    endpoints_provided: bool,
    /// 注册时间戳
    #[serde(default)]
    pub registered_at: i64,
    /// 是否健康
    #[serde(default = "default_healthy")]
    pub healthy: bool,
}

/// 服务访问端点
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct ServiceEndpoint {
    /// 端点名称，如 client/admin/local_socket
    pub name: String,
    /// 协议，如 tcp/http/grpc/local_socket
    #[serde(default)]
    pub protocol: String,
    /// 主机地址
    #[serde(default)]
    pub host: String,
    /// TCP/UDP 端口
    #[serde(default)]
    pub port: u16,
    /// Unix Domain Socket 名称或路径
    #[serde(default)]
    pub socket: String,
    /// 可见性，如 public/internal/admin/local
    #[serde(default)]
    pub visibility: String,
    /// 端点级元数据
    #[serde(default)]
    pub metadata: serde_json::Value,
    /// 端点是否健康
    #[serde(default = "default_healthy")]
    pub healthy: bool,
}

fn default_schema_version() -> u32 {
    1
}

fn default_weight() -> u32 {
    100
}

fn default_healthy() -> bool {
    true
}

impl ServiceInstance {
    /// 创建新的服务实例
    pub fn new(id: String, name: String, host: String, port: u16) -> Self {
        let mut instance = Self {
            schema_version: 1,
            id,
            name,
            host,
            port,
            admin_port: 0,
            local_socket: String::new(),
            tags: Vec::new(),
            weight: 100,
            metadata: serde_json::Value::Object(serde_json::Map::new()),
            endpoints: Vec::new(),
            endpoints_provided: false,
            registered_at: chrono_timestamp(),
            healthy: true,
        };
        instance.normalize();
        instance
    }

    /// 设置管理端口
    pub fn with_admin_port(mut self, admin_port: u16) -> Self {
        self.admin_port = admin_port;
        self.normalize();
        self
    }

    /// 设置 UDS 名称
    pub fn with_local_socket(mut self, local_socket: String) -> Self {
        self.local_socket = local_socket;
        self.normalize();
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

    /// 设置统一服务端点
    pub fn with_endpoints(mut self, endpoints: Vec<ServiceEndpoint>) -> Self {
        self.endpoints = endpoints;
        self.endpoints_provided = true;
        self.normalize();
        self
    }

    /// 返回归一化后的实例副本
    pub fn normalized(mut self) -> Self {
        self.normalize();
        self
    }

    /// 归一化 v1/v2 字段：显式 v2 endpoints 只保留合法端点；未提供 endpoints 的 legacy
    /// payload 才从 v1 字段补齐端点。
    pub fn normalize(&mut self) {
        let should_backfill_legacy_endpoints = self.schema_version < SERVICE_INSTANCE_SCHEMA_VERSION
            && !self.endpoints_provided
            && self.endpoints.is_empty();

        self.schema_version = SERVICE_INSTANCE_SCHEMA_VERSION;
        if self.registered_at == 0 {
            self.registered_at = chrono_timestamp();
        }
        if self.metadata.is_null() {
            self.metadata = serde_json::Value::Object(serde_json::Map::new());
        }

        self.endpoints = self
            .endpoints
            .drain(..)
            .filter_map(|mut endpoint| {
                endpoint.normalize();
                endpoint.is_valid().then_some(endpoint)
            })
            .collect();

        if !should_backfill_legacy_endpoints {
            return;
        }

        if !self.host.trim().is_empty() && self.port > 0 {
            self.endpoints.push(ServiceEndpoint::tcp(
                "client", &self.host, self.port, "public",
            ));
        }

        if !self.host.trim().is_empty() && self.admin_port > 0 {
            self.endpoints.push(ServiceEndpoint::tcp(
                "admin",
                &self.host,
                self.admin_port,
                "admin",
            ));
        }

        if !self.local_socket.trim().is_empty() {
            self.endpoints
                .push(ServiceEndpoint::socket("local_socket", &self.local_socket));
        }
    }
}

impl<'de> Deserialize<'de> for ServiceInstance {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct ServiceInstanceWire {
            #[serde(default = "default_schema_version")]
            schema_version: u32,
            id: String,
            name: String,
            #[serde(default)]
            host: String,
            #[serde(default)]
            port: u16,
            #[serde(default)]
            admin_port: u16,
            #[serde(default)]
            local_socket: String,
            #[serde(default)]
            tags: Vec<String>,
            #[serde(default = "default_weight")]
            weight: u32,
            #[serde(default)]
            metadata: serde_json::Value,
            endpoints: Option<Vec<ServiceEndpoint>>,
            #[serde(default)]
            registered_at: i64,
            #[serde(default = "default_healthy")]
            healthy: bool,
        }

        let wire = ServiceInstanceWire::deserialize(deserializer)?;
        Ok(Self {
            schema_version: wire.schema_version,
            id: wire.id,
            name: wire.name,
            host: wire.host,
            port: wire.port,
            admin_port: wire.admin_port,
            local_socket: wire.local_socket,
            tags: wire.tags,
            weight: wire.weight,
            metadata: wire.metadata,
            endpoints_provided: wire.endpoints.is_some(),
            endpoints: wire.endpoints.unwrap_or_default(),
            registered_at: wire.registered_at,
            healthy: wire.healthy,
        })
    }
}

impl ServiceEndpoint {
    pub fn tcp(name: &str, host: &str, port: u16, visibility: &str) -> Self {
        let mut endpoint = Self {
            name: name.to_string(),
            protocol: "tcp".to_string(),
            host: host.to_string(),
            port,
            socket: String::new(),
            visibility: visibility.to_string(),
            metadata: serde_json::Value::Object(serde_json::Map::new()),
            healthy: true,
        };
        endpoint.normalize();
        endpoint
    }

    pub fn socket(name: &str, socket: &str) -> Self {
        let mut endpoint = Self {
            name: name.to_string(),
            protocol: "local_socket".to_string(),
            host: String::new(),
            port: 0,
            socket: socket.to_string(),
            visibility: "local".to_string(),
            metadata: serde_json::Value::Object(serde_json::Map::new()),
            healthy: true,
        };
        endpoint.normalize();
        endpoint
    }

    pub fn is_valid(&self) -> bool {
        if self.name.trim().is_empty() {
            return false;
        }
        if !is_supported_visibility(&self.visibility) {
            return false;
        }
        if !is_supported_protocol(&self.protocol) {
            return false;
        }
        if self.protocol == "local_socket" {
            return !self.socket.trim().is_empty()
                && self.host.trim().is_empty()
                && self.port == 0;
        }
        !self.host.trim().is_empty() && self.port > 0 && self.socket.trim().is_empty()
    }

    fn normalize(&mut self) {
        self.name = self.name.trim().to_string();
        self.protocol = self.protocol.trim().to_string();
        self.host = self.host.trim().to_string();
        self.socket = self.socket.trim().to_string();
        self.visibility = self.visibility.trim().to_string();
        if self.metadata.is_null() {
            self.metadata = serde_json::Value::Object(serde_json::Map::new());
        }
        if self.protocol.is_empty() {
            self.protocol = if self.socket.is_empty() {
                "tcp".to_string()
            } else {
                "local_socket".to_string()
            };
        }
        if self.visibility.is_empty() {
            self.visibility = if self.socket.is_empty() {
                "internal".to_string()
            } else {
                "local".to_string()
            };
        }
    }
}

fn is_supported_protocol(protocol: &str) -> bool {
    matches!(
        protocol,
        "http" | "tcp" | "udp" | "kcp" | "grpc" | "local_socket"
    )
}

fn is_supported_visibility(visibility: &str) -> bool {
    matches!(visibility, "public" | "internal" | "admin" | "local")
}

fn chrono_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
