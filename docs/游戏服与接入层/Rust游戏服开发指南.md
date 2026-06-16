# game-server Rust 阅读指南

## 1. 文档定位

本文是面向熟悉服务端 / C++、但刚开始读 Rust 的协作者的阅读指南，只解释 `apps/game-server` 当前代码如何组织，以及读代码时会遇到的 Rust 写法。

本文不作为整体架构或协议的主口径：

- 整体服务边界以 [整体架构](../总览/整体架构.md) 为准
- 消息号、包头和 proto 字段以 [协议设计](../协议与客户端/协议设计.md) 与 `packages/proto/` 为准
- 房间运行时细节以 [帧同步与房间生命周期设计](./帧同步与房间生命周期设计.md) 为准

## 2. 当前职责

`game-server` 是 Rust + Tokio 实现的游戏逻辑服，当前承担：

- 玩家协议包解析与最终鉴权
- TCP 直连入口、本地 socket 入口、内部 socket 入口和 admin TCP 管理口
- 房间生命周期、帧推进、断线恢复、观战和输入历史
- `RoomManager + RoomRuntimePolicy + RoomLogic` 的房间运行时框架
- CSV 配置表加载和热更新
- 背包、属性、外观、移动同步、战斗 demo 等游戏侧服务
- 匹配服务回调、服务注册、metrics 上报、PostgreSQL 审计和玩家数据持久化

客户端正式入口优先走 `game-proxy`。`game-server` 仍保留玩家 TCP 监听口，主要用于本地调试、兼容和直接联调。

## 3. 启动流程

入口在 `apps/game-server/src/main.rs`。

启动顺序大致是：

1. `dotenvy::dotenv()` 读取 `.env`
2. `Config::from_env()` 读取端口、Redis、PostgreSQL、CSV、日志、注册中心等配置
3. `init_logging(&config)` 初始化 `tracing`
4. `ConfigTableRuntime::load(...)` 加载 CSV 配置表
5. 按配置注册到 Redis service registry，并启动 heartbeat
6. 按配置启动 CSV hot reload task
7. 初始化 `MySqlAuditStore`
8. 启动 metrics 上报任务
9. 调用 `server::run(...)` 启动主服务
10. 退出时注销服务、停止任务并关闭 PostgreSQL 连接池

对 C++ 服务端的直觉映射：

- `#[tokio::main]` 相当于创建异步运行时并进入 main
- `async fn main() -> Result<...>` 允许主流程直接 `await`
- `?` 用于错误向上传播，类似检查返回值后立刻 return

## 4. server.rs 现在做什么

`apps/game-server/src/server.rs` 是运行编排层，不再直接承载大部分房间业务规则。

它启动四类入口：

- 玩家 TCP：`GAME_HOST:GAME_PORT`，默认 `127.0.0.1:7000`
- admin TCP：`ADMIN_HOST:ADMIN_PORT`，默认 `127.0.0.1:7500`
- local socket：供 `game-proxy` 转发玩家连接
- internal socket：供 `match-service` 发起内部建房请求

它还会创建共享运行时对象：

- `RoomManager`
- `RuntimeConfig`
- `PlayerManager`
- `ConfigTableRuntime`
- `PlayerRegistry`
- `ServiceContext`

连接处理仍是典型 Tokio 模式：

1. accept 到连接
2. 为连接分配 `session_id`
3. 拆分读写半边
4. 创建容量由 `OUTBOUND_QUEUE_CAPACITY` 控制的有界 `mpsc::Sender<OutboundMessage>` 作为写队列；未配置、解析失败或配置为 `0` 时使用 `DEFAULT_OUTBOUND_QUEUE_CAPACITY=1024`
5. 为连接创建共享关闭状态，`ConnectionContext`、玩家注册表和房间成员出站句柄共用它；当 `try_send` 因队列满返回 `Full` 时会记录 warning、标记 `outbound_queue_full`，读循环写入连接审计并进入断线清理
6. 单独 spawn 一个 writer task 串行写出 socket
7. 读循环解析包头和 body，再交给 service handler 分发，同时监听 kick 和服务端关闭信号

## 5. 模块分层

当前 `apps/game-server/src` 的主要分层如下。

基础入口与横切模块：

- `config.rs`：环境变量配置
- `protocol/`：包头、消息号和 Protobuf 编解码辅助
- `session.rs`：单连接状态
- `ticket.rs`：ticket 签名、过期校验和 hash
- `db_store.rs`：PostgreSQL 连接与房间事件审计
- `metrics.rs`：Core NATS metrics 上报
- `kick_subscriber.rs`：订阅 `myserver.session.kick.*`，处理并发登录、改密等踢旧连接通知

服务入口：

- `server.rs`：玩家 TCP / local socket 接入与消息分发
- `admin_server.rs`：内部管理口，支持状态查询、运行时配置、drain mode、GM 发物品、广播、踢人和在线封禁处置
- `core/context.rs` 中的 `PlayerRegistry` 记录当前实例已鉴权连接，用于同服踢旧连接、NATS session kick 和 GM 踢人/封禁；跨实例踢人仍依赖外部路由或 NATS 事件
- `internal_server.rs`：内部建房入口，当前服务于 `match-service`
- `match_client.rs`：调用 `match-service` 的 gRPC 回调

框架层：

- `core/context.rs`：连接上下文、服务上下文、共享状态
- `core/runtime/room_manager.rs`：房间生命周期、帧推进、恢复、清理和广播
- `core/runtime/room_policy.rs`：房间策略，包含 fps、销毁、离线 TTL、输入等待策略等
- `core/room/`：房间内存模型、成员状态、输入历史和快照
- `core/logic/`：`RoomLogic` 与 `RoomLogicFactory` 抽象
- `core/service/`：协议请求到框架能力的业务入口
- `core/system/`：移动、场景、战斗等可复用系统
- `core/config_table/`：通用 CSV runtime
- `core/player/`、`core/inventory/`：玩家数据与背包模型

游戏侧装配：

- `gameroom/`：具体房间逻辑实现，如 `movement_demo`、`combat_demo`、`persistent_world`
- `gameservice/`：游戏业务消息入口，例如配置查询和调试能力
- `gameconfig/`：具体 CSV 表注册与装配
- `csv_code/`：由 CSV codegen 生成的表结构代码

## 6. 协议与鉴权链路

包头格式由 `protocol/mod.rs` 维护：

```text
| magic(2) | version(1) | flags(1) | msgType(2) | seq(4) | bodyLen(4) | body(N) |
```

关键点：

- `MessageType::from_u16` 把外部整数转换为受控枚举，未知值返回 `None`
- `Packet::decode_body::<T>()` 用 `prost` 解 Protobuf
- 当前 `game-proxy` 和 `game-server` 都会校验 ticket，`game-server` 是最终会话建立点
- `game-server` 在 `dispatch_packet` 层强制鉴权前白名单：未认证连接只允许 `AuthReq` 和 `PingReq`，其它业务消息直接返回 `PREAUTH_MESSAGE_NOT_ALLOWED`，不会进入房间或背包 handler
- 玩家连接读到完整 packet 后、业务 dispatch 前会先做单连接消息频率检查，再对已鉴权连接做当前 `game-server` 实例内的单玩家消息频率检查；两个限制默认关闭，启用后超频都会返回 `MSG_RATE_EXCEEDED` 并继续保持连接
- `chat-server` 复用 ticket 签名校验，并已检查 Redis `ticket:<sha256(ticket)>` 归属与 `player-ticket-version:<playerId>`

鉴权主流程：

1. `auth-http` 签发 ticket，并把 `ticket:<sha256(ticket)> -> playerId` 写入 Redis
2. 客户端连接 `game-proxy`，发送 `AuthReq`
3. `game-proxy` 校验签名、过期时间和 Redis ticket 记录，先向客户端返回 `AuthRes`
4. `game-proxy` 选定上游后，把认证包 replay 到 `game-server`
5. `game-server` 再次校验签名、过期时间和 Redis ticket 记录，成功后进入 `Authenticated`

相关配置位于 `apps/game-server/src/config.rs` 与 `.env.example`：

```env
HEARTBEAT_TIMEOUT_SECS=30
MAX_BODY_LEN=4096
MSG_RATE_WINDOW_MS=1000
MSG_RATE_MAX=0
PLAYER_MSG_RATE_WINDOW_MS=1000
PLAYER_MSG_RATE_MAX=0
```

其中 `MSG_RATE_MAX=0` 和 `PLAYER_MSG_RATE_MAX=0` 表示不限制。admin TCP 的 `AdminUpdateConfigReq` 可动态更新 `heartbeat_timeout_secs`、`max_body_len`、`msg_rate_window_ms`、`msg_rate_max`、`player_msg_rate_window_ms`、`player_msg_rate_max` 和 `drain_mode`；当前 `ServerStatusRes` 协议仍只返回 `max_body_len` 与 `heartbeat_timeout_secs` 等既有字段，尚未回显消息频率限制配置。

## 7. 房间运行时怎么读

读房间相关代码时建议从这几个点入手：

- `RoomRuntimePolicy` 决定房间人数、fps、销毁、离线保留、输入等待和缺帧策略
- `RoomManager::join_room` 负责创建或加入房间
- `RoomManager::start_game` 启动 tick task
- `RoomManager::accept_player_input` 收集玩家输入
- `RoomManager::process_room_tick` 按帧推进并广播 `FrameBundlePush`
- `RoomManager::disconnect_room_member` 标记离线但保留重连状态
- `RoomManager::reconnect_room` 和 `join_room_as_observer` 返回快照、最近输入和移动恢复信息

当前可用策略包括：

- `default_match`
- `persistent_world`
- `disposable_match`
- `sandbox`
- `movement_demo`
- `combat_demo`

未知策略会回退到 `TestRoomLogic`。

## 8. Rust 写法速记

读这个项目最常见的 Rust 概念：

- `Option<T>`：值可能不存在，类似 `std::optional<T>`
- `Result<T, E>`：成功或失败，`?` 会把错误向上传播
- `Arc<T>`：多任务共享所有权，类似线程安全引用计数
- `tokio::sync::Mutex/RwLock`：异步锁，拿锁要 `.await`
- `mpsc`：连接写队列，房间逻辑不直接写 socket
- `tokio::spawn`：启动异步任务，不等同于一连接一 OS 线程
- `clone()`：要看类型；很多 clone 是句柄复制，不一定是深拷贝
- `let Some(x) = value else { ... };`：模式匹配式解包

## 9. 推荐阅读顺序

如果目标是理解当前 `game-server`，建议按这个顺序：

1. `apps/game-server/src/protocol/mod.rs`
2. `apps/game-server/src/protocol/message_type.rs`
3. `apps/game-server/src/session.rs`
4. `apps/game-server/src/ticket.rs`
5. `apps/game-server/src/core/context.rs`
6. `apps/game-server/src/core/runtime/room_policy.rs`
7. `apps/game-server/src/core/room/mod.rs`
8. `apps/game-server/src/core/runtime/room_manager.rs`
9. `apps/game-server/src/core/service/room_service.rs`
10. `apps/game-server/src/server.rs`
11. `apps/game-server/src/admin_server.rs`
12. `apps/game-server/src/internal_server.rs`
13. `apps/game-server/src/main.rs`

需要看协议字段时，再回到：

- `packages/proto/game.proto`
- `packages/proto/admin.proto`
- `packages/proto/match.proto`

## 10. 保留为单独文档的原因

这份文档不建议合并进 `整体架构.md` 或 `协议设计.md`。

原因是它的读者和内容不同：

- `整体架构.md` 应保持服务边界和主链路说明，避免混入 Rust 教程
- `协议设计.md` 应保持消息号、包头和字段定义，避免混入代码阅读顺序
- 本文适合做 Rust 代码阅读入口，可选阅读，不是 AI 了解项目的必读主文档
