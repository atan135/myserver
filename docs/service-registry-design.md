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

### 2.1 服务类型分类

| 类型 | 说明 | 示例 |
|------|------|------|
| 入口服务 | 客户端直连，固定端口 | auth-http, game-proxy, admin-api |
| 内部服务 | 通过注册中心发现，支持多实例 | game-server, chat-server, match-service, mail-service |
| 前端资源 | 静态托管，不监听端口 | admin-web |

### 2.2 端口分配表

#### 入口服务（固定端口，客户端硬编码）

| 服务 | 端口 | 协议 | 说明 |
|------|------|------|------|
| auth-http | 3000 | HTTP | 玩家登录，签发 ticket，下发服务地址 |
| admin-api | 3001 | HTTP | 运营后台 API |
| game-proxy | 7002 | KCP | 客户端游戏入口 |

#### 内部服务（动态端口，通过 Redis 注册）

| 服务 | 端口 | 协议 | 发现方式 |
|------|------|------|----------|
| game-server | 7000-7499 | TCP | Redis 注册中心 |
| game-server (admin) | **7500** | TCP | 固定端口，admin-api 调用 |
| chat-server | 9001 | TCP | Redis 注册中心 |
| match-service | 9002 | gRPC | Redis 注册中心 |
| mail-service | 9003 | HTTP | Redis 注册中心 |

#### 前端资源

| 服务 | 部署方式 | 说明 |
|------|----------|------|
| admin-web | Nginx 静态托管 | 不监听端口 |

### 2.3 服务独立架构

各服务独立运行，不相互依赖，**故障隔离**：

```
┌─────────────────────────────────────────────────────────────┐
│                         客户端                              │
│                                                             │
│   直连: auth-http (3000)  →  登录、获取服务地址             │
│   直连: game-proxy (7002) →  游戏流量                       │
│   直连: chat-server (9001)→  聊天                          │
│   直连: mail-service (9003)→ 邮件（HTTP CRUD）             │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
                    ┌─────────────────┐
                    │  Redis 注册中心   │
                    │                  │
                    │ service:game-server:instances:* │
                    │ service:chat-server:instances:*  │
                    │ service:mail-service:instances:* │
                    └─────────────────┘
```

### 2.4 登录流程

```http
POST /api/login
Content-Type: application/json

{
  "username": "test001",
  "password": "Passw0rd!"
}
```

```json
{
  "ok": true,
  "player_id": "player_001",
  "ticket": "xxx",
  "expires_at": 1713000000,
  "services": {
    "game": {
      "host": "127.0.0.1",
      "port": 7002,
      "protocol": "kcp"
    },
    "chat": {
      "host": "127.0.0.1",
      "port": 9001,
      "protocol": "tcp"
    },
    "mail": {
      "host": "127.0.0.1",
      "port": 9003,
      "protocol": "http"
    }
  }
}
```

客户端根据返回的 `services` 直连各服务，无需硬编码。

### 2.5 服务职责边界

| 服务 | 职责 | 独立扩缩容 |
|------|------|-----------|
| auth-http | 登录、ticket 签发、服务地址下发 | ✅ |
| game-proxy | 玩家游戏流量入口、路由到 game-server | ✅ |
| game-server | 游戏逻辑、房间、帧同步 | ✅ |
| chat-server | 私聊、群聊、聊天历史 | ✅ |
| match-service | 匹配逻辑（MOBA/天梯等） | ✅ |
| mail-service | 邮件 CRUD、附件领取 | ✅ |
| admin-api | 运营管理（玩家、房间、审计） | ✅ |

### 2.6 服务间通信

服务间通信通过 **Redis Pub/Sub** 解耦，不直接调用：

```
mail-service 收到新邮件
       ↓
   Redis Pub/Sub: mail:notify:{player_id}
       ↓
   chat-server / game-server 订阅该频道
       ↓
   在已有 TCP 连接上推送通知给客户端
```

**好处**：
- 服务间无直接依赖
- game-server 负载高不影响邮件通知
- 新增通知渠道只需修改订阅方

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
                         +---------------------+
                         |      Redis          |
                         |   注册中心 + 心跳    |
                         +-----------+---------+
                                     ^
                                     │
        +----------------------------+----------------------------+
        │                            │                            │
        ▼                            ▼                            ▼
+------------------+         +------------------+         +------------------+
|   auth-http      |         |  game-server x N  |         |  game-proxy      |
|   :3000          |         |  :7000 (游戏端口) |         |  :7002           |
|   (Node.js)      |         |  :7001 (管理端口)  |         |  (Rust)          |
+------------------+         +------------------+         +------------------+
        ▲                            ▲                            ▲
        │                            │                            │
        │   /login 返回 services     │   Redis 注册               │   Redis 发现
        │───────────────────────────│───────────────────────────│
                                     │
                                     ▼
                         +------------------+
                         | chat-server      |
                         | :9001 (TCP)      |
                         +------------------+
                                     │
                                     ▼
                         +------------------+
                         | mail-service     |
                         | :9003 (HTTP)     |
                         +------------------+


┌─────────────────────────────────────────────────────────────────┐
│                          客户端                                  │
│                                                                  │
│  auth-http:3000 (登录)                                           │
│  game-proxy:7002 (游戏)                                          │
│  chat-server:9001 (聊天)                                         │
│  mail-service:9003 (邮件)                                        │
└─────────────────────────────────────────────────────────────────┘
```

### 3.2 服务注册中心职责

所有**内部服务**启动时注册到 Redis，关闭时注销：

```
apps/
├── game-server/        # 注册 game-server 服务
├── chat-server/        # 注册 chat-server 服务
├── match-service/      # 注册 match-service 服务
├── mail-service/      # 注册 mail-service 服务
├── game-proxy/         # 查询 game-server（消费者）
├── auth-http/          # 查询服务列表用于登录响应
packages/
└── service-registry/   # Redis 注册中心封装库
```

### 3.3 数据流

1. **服务启动**：从环境变量读取配置，连接 Redis，注册自身（含端口、实例ID）
2. **心跳维持**：服务定期向 Redis 发送心跳，更新 TTL（30s）
3. **服务发现**：auth-http 登录时查询可用服务地址，下发给客户端
4. **服务下线**：正常关闭主动注销；心跳超时（30s 未更新）则自动移除

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
    "local_socket": "myserver-game-server-001.sock",
    "tags": ["game", "tcp"],
    "weight": 100,
    "metadata": {},
    "registered_at": 1712736000000,
    "healthy": true
}

# chat-server 实例
service:chat-server:instances:chat-server-001 = {
    "id": "chat-server-001",
    "name": "chat-server",
    "host": "127.0.0.1",
    "port": 9001,
    "tags": ["chat", "tcp"],
    "weight": 100,
    "metadata": {},
    "registered_at": 1712736000000,
    "healthy": true
}

# mail-service 实例
service:mail-service:instances:mail-001 = {
    "id": "mail-001",
    "name": "mail-service",
    "host": "127.0.0.1",
    "port": 9003,
    "tags": ["mail", "http"],
    "weight": 100,
    "metadata": {},
    "registered_at": 1712736000000,
    "healthy": true
}

# match-service 实例
service:match-service:instances:match-001 = {
    "id": "match-001",
    "name": "match-service",
    "host": "127.0.0.1",
    "port": 9002,
    "protocol": "grpc",
    "tags": ["match", "grpc"],
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

### 5.3 其他服务注册（chat-server / match-service / mail-service）

所有内部服务遵循相同的注册模式：

#### chat-server 注册

```rust
// 启动参数
SERVICE_NAME=chat-server
SERVICE_INSTANCE_ID=chat-server-001
CHAT_PORT=9001
```

#### match-service 注册

```rust
// 启动参数
SERVICE_NAME=match-service
SERVICE_INSTANCE_ID=match-001
MATCH_PORT=9002
MATCH_PROTOCOL=grpc
```

#### mail-service 注册

```rust
// 启动参数
SERVICE_NAME=mail-service
SERVICE_INSTANCE_ID=mail-001
MAIL_PORT=9003
```

**统一注册流程**：
1. 服务启动时调用 `RegistryClient::register()`
2. 启动心跳任务 `RegistryClient::start_heartbeat(10)`
3. 服务关闭时调用 `RegistryClient::deregister()`

### 5.4 auth-http 服务发现（登录响应）

auth-http 登录时需要查询各服务地址，组装到登录响应中：

```rust
async fn handle_login(&self, username: &str, password: &str) -> Result<LoginResponse, Error> {
    // 1. 验证账号
    let player = self.verify_player(username, password)?;

    // 2. 查询可用服务
    let services = self.discover_services().await?;

    // 3. 签发 ticket
    let ticket = self.sign_ticket(&player.id)?;

    Ok(LoginResponse {
        ok: true,
        player_id: player.id,
        ticket,
        expires_at: now() + 86400,
        services,
    })
}

async fn discover_services(&self) -> Result<Services, Error> {
    let mut conn = self.redis.get_multiplexed_async_connection().await?;

    Ok(Services {
        game: self.find_one_service("game-proxy").await?,
        chat: self.find_one_service("chat-server").await?,
        mail: self.find_one_service("mail-service").await?,
    })
}
```

### 5.5 客户端流程（改造）

客户端从 auth-http 获取所有服务地址后直连各服务：

```csharp
// 1. 登录获取服务地址
var loginResponse = await authClient.LoginAsync(username, password);
// loginResponse.services 包含所有服务地址

// 2. 直连各服务
var gameClient = new GameClient(loginResponse.services.game);
// gameClient.Connect();

var chatClient = new ChatClient(loginResponse.services.chat);
// chatClient.Connect();

var mailClient = new MailClient(loginResponse.services.mail);
// 邮件查询直接 HTTP 请求即可
```

**优势**：
- 客户端无需硬编码服务地址
- 新增服务只需修改 auth-http 登录响应
- 各服务独立扩缩容，不影响客户端

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

### 阶段四：chat-server 集成（1天）

1. 引入 `service-registry` 包
2. 启动时注册到 Redis，关闭时注销
3. 启动心跳续期任务
4. auth-http 登录响应添加 chat-server 地址

**里程碑**：客户端可从登录响应获取 chat-server 地址并连接

### 阶段五：mail-service 集成（1天）

1. 新建 mail-service（Node.js/Go）
2. 引入 `service-registry` 包，注册到 Redis
3. 实现邮件 CRUD API
4. 通过 Redis Pub/Sub 接收新邮件通知
5. auth-http 登录响应添加 mail-service 地址

**里程碑**：客户端可从登录响应获取 mail-service 地址，邮件通知通过 chat-server 推送

### 阶段六：match-service 集成（1天）

1. 完善 match-service gRPC 接口
2. 引入 `service-registry` 包
3. 实现匹配逻辑（房间、队列、规则）
4. 匹配完成后通知 game-server 创建房间

**里程碑**：客户端可通过 match-service 加入匹配队列

### 阶段七：测试验证（0.5天）

1. 启动所有服务实例
2. 验证登录响应包含所有服务地址
3. 验证各服务独立运行
4. 验证心跳超时移除
5. 验证优雅关闭注销

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

### 12.2 待集成服务

| 服务 | 端口 | 状态 | 说明 |
|------|------|------|------|
| chat-server | 9001 | 待集成 | 需引入 service-registry 包 |
| mail-service | 9003 | 待实现 | 新建服务，实现邮件 CRUD + Redis Pub/Sub 通知 |
| match-service | 9002 | 开发中 | gRPC 接口已定义，需完善业务逻辑 |

### 12.3 启动脚本

- `scripts/dev-game.ps1` - 启动 game-server 实例
  ```powershell
  .\dev-game.ps1 -InstanceId "game-server-001" -Port 7000
  ```

- `scripts/dev-proxy.ps1` - 启动 game-proxy（启用服务发现）
  ```powershell
  .\dev-proxy.ps1
  ```

### 12.4 服务地址统一返回

auth-http 登录接口已规划返回所有服务地址：

```json
{
  "ok": true,
  "player_id": "player_001",
  "ticket": "xxx",
  "services": {
    "game": { "host": "127.0.0.1", "port": 7002, "protocol": "kcp" },
    "chat": { "host": "127.0.0.1", "port": 9001, "protocol": "tcp" },
    "mail": { "host": "127.0.0.1", "port": 9003, "protocol": "http" }
  }
}
```

### 12.5 测试验证

1. 启动所有服务实例
2. 调用登录接口验证返回的服务地址
3. 验证客户端可直连各服务
4. 验证心跳超时移除
5. 验证优雅关闭注销
