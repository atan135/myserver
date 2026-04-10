# 服务注册中心设计方案

## 1. 方案概述

### 1.1 什么是服务注册中心

服务注册中心是分布式系统中管理服务元信息的核心组件，提供服务注册、注销、心跳、发现等能力，使服务间可以动态感知彼此的位置和状态。

### 1.2 当前问题

| 问题 | 描述 |
|------|------|
| 端口配置分散 | 各服务端口配置在各自的 `.env` 文件中，无统一管理 |
| 客户端硬编码 | `simple-client` 硬编码 `gameHost = "127.0.0.1"` 和 `gamePort = 7000` |
| 依赖硬编码 | 服务间依赖通过环境变量硬编码，缺乏动态发现 |
| 手动维护 | `port.txt` + `update-ports.js` 仅支持 GAME_PORT 手动更新 |
| 扩缩容困难 | 无法动态增减服务实例 |

### 1.3 服务注册中心的价值

- **动态发现**：proxy-server 可查询可用 game-server 地址，无需硬编码
- **健康检查**：自动移除不健康实例，避免请求发往已下线服务
- **配置统一**：game-server 端口信息集中管理
- **扩缩容支持**：支持多 game-server 实例部署，自动负载均衡
- **故障恢复**：game-server 异常时自动从注册表移除

---

## 2. 服务角色划分

### 2.1 单实例服务（固定地址）

| 服务 | 地址 | 说明 |
|------|------|------|
| auth-http | 127.0.0.1:3000 | 登录认证服务，单实例足够 |
| proxy-server | 127.0.0.1:7002 | 网关代理，单实例足够 |

**这些服务地址硬编码在客户端配置中，保持不变。**

### 2.2 多实例服务（动态发现）

| 服务 | 地址 | 说明 |
|------|------|------|
| game-server | 127.0.0.1:7000+n | 支持多实例，端口动态分配 |

**game-server 通过服务注册中心实现动态发现。**

### 2.3 简化后的数据流

```
1. Client -> auth-http (登录)
2. auth-http 返回: { ticket, proxyHost, proxyPort }
   - proxyHost: 固定配置 (127.0.0.1)
   - proxyPort: 固定配置 (7002)

3. Client -> proxy-server (proxyHost:proxyPort)
   - proxy-server 通过 Redis 查询活跃的 game-server
   - proxy-server 路由到选中的 game-server 实例
```

---

## 3. 技术选型

### 3.1 候选方案对比

| 特性 | Consul | etcd | Redis | ZooKeeper |
|------|--------|------|-------|-----------|
| 多语言支持 | 优秀 (HTTP API) | 良好 (HTTP/gRPC) | 优秀 | 一般 (Java/C绑定) |
| 部署复杂度 | 中等 | 中等 | **低** (项目已用Redis) | 高 |
| 健康检查 | 内置 | 需自行实现 | 需自行实现 | 需自行实现 |
| 持久化 | Raft | Raft | RDB/AOF | 事务日志 |
| 性能 | 高 | 高 | **极高** | 中等 |
| 生态 | 优秀 | 优秀 | **完善** | 成熟 |
| 客户端复杂度 | 低 | 中等 | **低** | 高 |

### 3.2 推荐方案：Redis

**推荐理由：**

1. **项目已集成 Redis**：auth-http、game-server、chat-server 均已使用 Redis，无需引入新组件
2. **多语言客户端成熟**：Node.js (ioredis)、Rust (redis-rs)、C# (StackExchange.Redis) 均有稳定客户端
3. **部署简单**：开发/生产环境已有 Redis 实例
4. **性能足够**：当前规模（5-10人房间）下 Redis 性能远超需求
5. **功能可扩展**：可利用 Redis Pub/Sub 实现变更通知

**潜在缺点：**
- 非专门服务注册组件，需要自行实现健康检查
- 需要处理 Redis 单点故障（可通过 Redis Sentinel 解决）

### 3.3 备选方案：Consul

如果未来需要更严格的服务注册功能（如 ACL、意图感知等），可迁移到 Consul。

---

## 3. 架构设计

### 3.1 整体架构

```
                                    +------------------+
                                    |  Service         |
                                    |  Registry        |
                                    |  (Redis)         |
                                    |                  |
                                    |  只注册          |
                                    |  game-server     |
                                    +--------+---------+
                                             ^
                                             |
                    +------------------------+------------------------+
                    |                        |                        |
                    |                        |                        |
           +----------------+       +----------------+       +----------------+
           |   auth-http    |       |  game-server   |       |  game-proxy    |
           |   (Node.js)   |       |   (Rust) x N   |       |    (Rust)     |
           |  单实例固定地址 |       |  多实例动态注册 |       |  单实例固定地址 |
           +----------------+       +----------------+       +----------------+
                    |                        ^                        |
                    |                        |                        |
                    +------------------------+------------------------+
                                             |
                                             v
                                    +----------------+
                                    |  simple-client |
                                    |  (Unity C#)    |
                                    +----------------+
```

### 3.2 服务注册中心位置

```
apps/
├── auth-http/          # 注册 auth-http 服务
├── game-server/        # 注册 game-server 服务
├── chat-server/        # 注册 chat-server 服务
├── game-proxy/         # 注册 game-proxy 服务，查询 game-server
packages/
└── service-registry/   # [新增] Redis 注册中心封装库
```

### 3.3 数据流

1. **服务启动**：服务从环境变量读取基础配置，连接 Redis，注册自身
2. **心跳维持**：服务定期向 Redis 发送心跳，更新 TTL
3. **服务发现**：客户端/服务向 Redis 查询目标服务地址列表
4. **服务下线**：服务正常关闭时主动注销；心跳超时则自动移除

---

## 4. 数据结构设计

### 4.1 服务注册表结构

使用 Redis Hash 存储服务注册信息，Key 格式为 `service:{service_name}:instances:{instance_id}`

```text
# game-server 实例
service:game-server:instances:game-server-001 = {
    "id": "game-server-001",
    "name": "game-server",
    "host": "127.0.0.1",
    "port": 7000,
    "admin_port": 7001,
    "local_socket": "myserver-game-server.sock",
    "tags": ["game", "tcp"],
    "weight": 100,
    "metadata": {},
    "registered_at": 1712736000000,
    "healthy": true
}

# game-proxy 实例
service:game-proxy:instances:game-proxy-001 = {
    "id": "game-proxy-001",
    "name": "game-proxy",
    "host": "127.0.0.1",
    "port": 7002,
    "tags": ["game", "kcp", "proxy"],
    "weight": 100,
    "metadata": {},
    "registered_at": 1712736000000,
    "healthy": true
}
```

### 4.2 心跳机制

使用 Redis Key 的 TTL 实现心跳：

```text
# 心跳 Key，TTL 30 秒
heartbeat:game-server:game-server-001 = 1

# 服务需每 10 秒更新一次心跳
# 如果心跳 Key 过期（30 秒未更新），则视为不健康
```

**健康检查流程：**
1. 服务启动时创建心跳 Key：`SET heartbeat:{service}:{id} 1 EX 30`
2. 服务每 10 秒更新心跳：`EXPIRE heartbeat:{service}:{id} 30`
3. 注册表清理任务定期扫描，过滤无心跳实例

---

## 5. 各服务改造点

### 5.1 game-server 注册（核心改造）

game-server 启动时注册到 Redis，关闭时注销。

#### 启动参数（环境变量）

```bash
# 每个 game-server 实例启动时传入
SERVICE_NAME=game-server
SERVICE_INSTANCE_ID=game-server-001    # 唯一标识
GAME_PORT=7000                          # 该实例的端口
UPSTREAM_LOCAL_SOCKET_NAME=myserver-game-server-001.sock  # 该实例的 UDS 文件
```

#### Rust 实现

```rust
// src/registry.rs
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct ServiceInstance {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub admin_port: u16,
    pub local_socket: String,
    pub tags: Vec<String>,
    pub weight: u32,
    #[serde(default)]
    pub metadata: serde_json::Value,
    pub registered_at: i64,
    pub healthy: bool,
}

pub struct RegistryClient {
    redis: redis::Client,
    instance_id: String,
    service_name: String,
}

impl RegistryClient {
    pub async fn register(&self, info: &ServiceInstance) -> Result<(), Box<dyn std::error::Error>> {
        let mut conn = self.redis.get_multiplexed_async_connection().await?;
        let key = format!("service:{}:instances:{}", self.service_name, self.instance_id);
        let json = serde_json::to_string(info)?;
        conn.hset(&key, "data", &json).await?;
        Ok(())
    }

    pub async fn start_heartbeat(&self, interval_secs: u64) {
        let heartbeat_key = format!("heartbeat:{}:{}", self.service_name, self.instance_id);

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(interval_secs));
            loop {
                interval.tick().await;
                if let Ok(mut c) = self.redis.get_multiplexed_async_connection().await {
                    let _: Result<(), _> = c.set_ex(&heartbeat_key, "1", 30).await;
                }
            }
        });
    }
}
```

### 5.2 game-proxy 发现 game-server

proxy-server 通过 Redis 查询活跃的 game-server 实例。

```rust
// src/upstream_discovery.rs
use redis::AsyncCommands;

pub struct UpstreamDiscovery {
    redis: redis::Client,
}

impl UpstreamDiscovery {
    pub async fn discover_game_server(&self) -> Result<UpstreamRoute, Box<dyn std::error::Error>> {
        let mut conn = self.redis.get_multiplexed_async_connection().await?;

        // 获取所有 game-server 实例
        let pattern = "service:game-server:instances:*";
        let keys: Vec<String> = redis::cmd("KEYS").arg(pattern).query_async(&mut conn).await?;

        for key in keys {
            let instance_id = key.split(':').last().unwrap();
            let heartbeat_key = format!("heartbeat:game-server:{}", instance_id);
            let exists: bool = conn.exists(&heartbeat_key).await?;

            if exists {
                let data: String = conn.hget(&key, "data").await?;
                let info: ServiceInstance = serde_json::from_str(&data)?;

                return Ok(UpstreamRoute {
                    server_id: info.id,
                    local_socket_name: info.local_socket,
                    state: UpstreamState::Active,
                });
            }
        }

        Err("No healthy game-server found".into())
    }
}
```

### 5.3 客户端流程（无需改造）

simple-client 不需要服务发现，保持原有流程：

```csharp
// MyServerClientConfig.cs - 固定配置
public sealed class MyServerClientConfig
{
    public string httpBaseUrl = "http://127.0.0.1:3000";  // auth-http
    public string gameHost = "127.0.0.1";                  // proxy-server
    public int gamePort = 7002;                            // proxy-server
}
```

登录后直接使用 proxy-server 地址连接。```

---

## 6. API 设计

### 6.1 服务注册

```
POST /api/v1/registry/services
Content-Type: application/json

{
    "name": "game-server",
    "host": "127.0.0.1",
    "port": 7000,
    "admin_port": 7001,
    "local_socket": "myserver-game-server.sock",
    "tags": ["game", "tcp"],
    "weight": 100
}
```

### 6.2 服务注销

```
DELETE /api/v1/registry/services/:serviceName/:instanceId
```

### 6.3 心跳

```
PUT /api/v1/registry/services/:serviceName/:instanceId/heartbeat
```

### 6.4 服务发现

```
GET /api/v1/registry/services/:serviceName
```

响应：
```json
{
    "host": "127.0.0.1",
    "port": 7000
}
```

---

## 7. 实现步骤

### 阶段一：创建 service-registry 包（0.5天）

在 `packages/` 下创建服务注册中心封装：

```
packages/
└── service-registry/
    ├── src/
    │   ├── mod.rs
    │   ├── client.rs      # 注册中心客户端
    │   └── types.rs       # 数据结构定义
    ├── Cargo.toml
    └── lib.rs
```

**里程碑**：`service-registry` 包可被 Rust 项目引用

### 阶段二：game-server 集成（1天）

1. 引入 `service-registry` 包
2. 启动时注册到 Redis
3. 关闭时注销
4. 启动心跳续期任务
5. 注册时携带 UDS socket 名称

**里程碑**：game-server 启动时自动注册到 Redis

### 阶段三：game-proxy 改造（1天）

1. 移除静态 `UPSTREAM_LOCAL_SOCKET_NAME` 配置
2. 引入 `service-registry` 包
3. 实现 `UpstreamDiscovery` 动态发现 game-server
4. 启动时查询 Redis 获取活跃实例

**里程碑**：game-proxy 可动态路由到任意活跃的 game-server

### 阶段四：测试验证（0.5天）

1. 启动多个 game-server 实例
2. 验证 proxy-server 正确路由
3. 验证心跳超时移除
4. 验证优雅关闭注销

---

## 8. 环境变量配置

新增注册中心相关环境变量：

```bash
# service-registry 配置 (所有服务)
REGISTRY_ENABLED=true
REGISTRY_URL=redis://127.0.0.1:6379
REGISTRY_HEARTBEAT_INTERVAL=10
REGISTRY_HEARTBEAT_TTL=30

# 服务自身标识
SERVICE_NAME=game-server
SERVICE_INSTANCE_ID=game-server-001
```

---

## 9. 关键文件参考

| 文件 | 用途 |
|------|------|
| `apps/game-proxy/src/route_store.rs` | 现有路由存储，可改造为动态发现 |
| `apps/game-server/src/config.rs` | Rust 环境变量配置模式 |
| `apps/game-proxy/src/upstream.rs` | 现有 UDS 连接逻辑 |
| `packages/` | 新建 service-registry 包 |

---

## 10. 简化后的服务发现流程

```
┌─────────────────┐
│   game-server   │ 启动时注册
│   实例 001      │ host: 127.0.0.1
│   port: 7000    │ local_socket: myserver-game-server-001.sock
└────────┬────────┘
         │ heartbeat (每10秒)
         ▼
┌─────────────────┐
│     Redis       │
│                 │
│ service:game-server:instances:game-server-001
│ heartbeat:game-server:game-server-001 (TTL 30s)
└────────┬────────┘
         │ 查询
         ▼
┌─────────────────┐
│  game-proxy     │ 路由时查询
│                 │ select_active() -> game-server-001
└────────┬────────┘
         │ UDS
         ▼
┌─────────────────┐
│   game-server   │
│   实例 001      │
└─────────────────┘
```

---

## 11. 未来扩展

### 11.1 迁移到 Consul

如果未来需要更专业的服务注册功能，可平滑迁移：

1. 保持 `ServiceRegistryClient` 接口不变
2. 底层从 Redis 实现切换到 Consul 实现
3. 服务代码无需改动

### 11.2 支持多环境

```bash
REGISTRY_NAMESPACE=production  # 或 development/staging
```

### 11.3 服务分组

支持游戏房间服务分组：
```json
{
    "tags": ["game", "room-1"],
    "group": "room-1"
}
```

---

## 12. 实际实现说明

### 12.1 已完成的实现

1. **service-registry 包** (`packages/service-registry/`)
   - `RegistryClient` 实现服务注册、注销、心跳、发现
   - 使用 Redis Hash 存储服务实例信息
   - 心跳 TTL 30秒，每10秒续期

2. **game-server 集成**
   - 启动时注册到 Redis，关闭时注销
   - 通过环境变量 `SERVICE_INSTANCE_ID` 区分不同实例
   - UDS socket 文件名包含实例ID，避免冲突

3. **game-proxy 动态发现**
   - 支持静态配置（向后兼容）
   - 启用 Registry 时自动从 Redis 发现活跃实例
   - 支持 KCP 和 TCP 两种前端协议

4. **管理接口** (`game-proxy`)

| 接口 | 方法 | 说明 |
|------|------|------|
| `/status` | GET | 查看连接数、维护模式、当前上游 |
| `/instances` | GET | 查看所有已注册的游戏服实例 |
| `/maintenance/on` | POST | 开启维护模式 |
| `/maintenance/off` | POST | 关闭维护模式 |
| `/switch/:server_id` | POST | 切换到指定服务器 |

### 12.2 启动脚本

- `scripts/dev-game.ps1` - 启动 game-server 实例
  ```powershell
  .\dev-game.ps1 -InstanceId "game-server-001" -Port 7000
  ```

- `scripts/dev-proxy.ps1` - 启动 game-proxy（启用服务发现）
  ```powershell
  .\dev-proxy.ps1
  ```

### 12.3 测试验证

1. 启动多个 game-server 实例
2. 访问 `GET /instances` 确认所有实例已注册
3. 关闭其中一个实例
4. 验证 proxy 自动路由到剩余实例
