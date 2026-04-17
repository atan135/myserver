# Match-Service 设计文档

## 1. 概述

Match-Service 是独立的匹配服务，负责玩家匹配逻辑；对客户端暴露 gRPC，对 `game-server` 通过 gRPC 接收回调，并通过内部 local socket + Protobuf 请求 `game-server` 创建 matched room。

### 1.1 核心职责

- 维护匹配池（按模式分离）
- 撮合算法（人齐即开，预留扩展）
- 匹配状态管理（Idle / Matching / Matched / InRoom）
- 房间创建回调
- 匹配取消与超时处理

### 1.2 非职责

- 不直接管理房间（由 game-server 负责）
- 不维护玩家会话
- 不处理帧同步

---

## 2. 架构

```
┌─────────────────────────────────────────────────────────────┐
│                        Match-Service                         │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────┐    │
│  │ MatchPool   │  │ Matcher     │  │ StateMachine    │    │
│  │ (按 mode    │  │ (撮合逻辑)   │  │ (玩家状态管理)   │    │
│  │  分离)      │  │             │  │                 │    │
│  └─────────────┘  └─────────────┘  └─────────────────┘    │
│         │                │                  │              │
│         └────────────────┼──────────────────┘              │
│                          │                                  │
│  ┌─────────────────────────────────────────────────────┐   │
│  │              GrpcServer ( tonic )                   │   │
│  │  - MatchStart / MatchCancel / MatchStatus           │   │
│  │  - MatchEventStream / CreateRoomAndJoin             │   │
│  │  - PlayerJoined / PlayerLeft / MatchEnd             │   │
│  └─────────────────────────────────────────────────────┘   │
│                          │                                  │
│  ┌─────────────────────────────────────────────────────┐   │
│  │           GameServerClient (local socket)           │   │
│  │  - CreateMatchedRoomReq / CreateMatchedRoomRes      │   │
│  └─────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────┘
                            │ gRPC / local socket
                            ▼
┌─────────────────────────────────────────────────────────────┐
│                      Game-Server                             │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────┐    │
│  │ RoomManager │  │ Room        │  │ MatchClient     │    │
│  │             │  │             │  │ (调用matchsvc)   │    │
│  └─────────────┘  └─────────────┘  └─────────────────┘    │
└─────────────────────────────────────────────────────────────┘
```

---

## 3. 目录结构

```
apps/match-service/
├── Cargo.toml
├── build.rs                  # proto 编译
└── src/
    ├── main.rs               # 入口、日志初始化
    ├── config.rs             # 配置读取
    ├── proto/
    │   ├── mod.rs                   # myserver.matchservice / myserver.game 模块导出
    │   ├── myserver.matchservice.rs # 自动生成
    │   └── myserver.game.rs         # 自动生成
    ├── server.rs             # gRPC 服务器
    ├── game_server_client.rs # 通过内部 socket 请求 game-server 建房
    ├── service/
    │   ├── mod.rs
    │   ├── match_service.rs  # MatchService 实现（对外接口）
    ├── pool/
    │   ├── mod.rs
    │   ├── match_pool.rs     # 匹配池
    │   └── candidate.rs      # 候选人结构
    ├── matcher/
    │   ├── mod.rs
    │   └── simple_matcher.rs # 简单撮合器
    ├── state/
    │   ├── mod.rs
    │   └── player_state.rs   # 玩家匹配状态机
    └── error.rs              # 错误定义

packages/proto/
└── match.proto               # MatchService 接口定义（与其他 proto 放一起）
```

---

## 4. Proto 接口定义

### 4.1 MatchService 对外接口（客户端调用）

```protobuf
// packages/proto/match.proto
syntax = "proto3";

package myserver.matchservice;

service MatchService {
  // 客户端发起匹配
  rpc MatchStart(MatchStartReq) returns (MatchStartRes);

  // 客户端取消匹配
  rpc MatchCancel(MatchCancelReq) returns (MatchCancelRes);

  // 客户端查询匹配状态
  rpc MatchStatus(MatchStatusReq) returns (MatchStatusRes);

  // 客户端订阅匹配事件推送
  rpc MatchEventStream(MatchEventStreamReq) returns (stream MatchEvent);
}
```

### 4.2 MatchService 对内接口（GameServer 调用）

```protobuf
service MatchInternal {
  // GameServer 创建房间成功后回调
  rpc CreateRoomAndJoin(CreateRoomAndJoinReq) returns (CreateRoomAndJoinRes);

  // GameServer 通知玩家已进入房间
  rpc PlayerJoined(PlayerJoinedReq) returns (PlayerJoinedRes);

  // GameServer 通知玩家已离开房间
  rpc PlayerLeft(PlayerLeftReq) returns (PlayerLeftRes);

  // GameServer 通知对局结束
  rpc MatchEnd(MatchEndReq) returns (MatchEndRes);
}
```

### 4.3 消息定义

```protobuf
// --- MatchStart ---
message MatchStartReq {
  string player_id = 1;
  string mode = 2;           // "1v1", "3v3", "5v5"
  // 预留扩展字段（当前不使用）
  int32 rank_tier = 3;       // 预留：段位（暂不使用）
}

message MatchStartRes {
  bool ok = 1;
  string match_id = 2;
  string error_code = 3;
}

// --- MatchCancel ---
message MatchCancelReq {
  string player_id = 1;
  string match_id = 2;
}

message MatchCancelRes {
  bool ok = 1;
  string error_code = 2;
}

// --- MatchStatus ---
message MatchStatusReq {
  string player_id = 1;
}

message MatchStatusRes {
  string status = 1;          // "idle", "matching", "matched", "in_room"
  string match_id = 2;
  string room_id = 3;         // matched/in_room 时有效
  string token = 4;          // 进入房间的临时凭证
  int64 estimated_wait_secs = 5;  // 预计等待时间
}

// --- MatchEventStream ---
message MatchEventStreamReq {
  string player_id = 1;
}

message MatchEvent {
  string event = 1;           // "matched", "match_failed", "match_cancelled"
  string match_id = 2;
  string room_id = 3;
  string token = 4;
  string error_code = 5;
}

// --- CreateRoomAndJoin (GameServer -> MatchService) ---
message CreateRoomAndJoinReq {
  string match_id = 1;
  string room_id = 2;
  repeated string player_ids = 3;
  string mode = 4;
}

message CreateRoomAndJoinRes {
  bool ok = 1;
  string error_code = 2;
}

// --- PlayerJoined (GameServer -> MatchService) ---
message PlayerJoinedReq {
  string match_id = 1;
  string player_id = 2;
  string room_id = 3;
}

message PlayerJoinedRes {
  bool ok = 1;
  string error_code = 2;
}

// --- PlayerLeft (GameServer -> MatchService) ---
message PlayerLeftReq {
  string match_id = 1;
  string player_id = 2;
  string reason = 3;         // "normal", "disconnect", "kicked"
}

message PlayerLeftRes {
  bool ok = 1;
  bool match_should_abort = 2;  // 所有人都离开，是否 abort 匹配
  string error_code = 3;
}

// --- MatchEnd (GameServer -> MatchService) ---
message MatchEndReq {
  string match_id = 1;
  string room_id = 2;
  string reason = 3;        // "game_over", "aborted", "timeout"
}

message MatchEndRes {
  bool ok = 1;
  string error_code = 2;
}
```

---

## 5. 匹配池设计

### 5.1 匹配池结构

```
MatchPool {
  pools: HashMap<Mode, ModePool>,
  matches: HashMap<MatchId, MatchTask>
}

ModePool {
  mode: String,                    // 模式标识
  config: ModeConfig,             // total_size / timeout 等模式配置
  candidates: Vec<MatchCandidate>, // 等待撮合的候选人
}

MatchTask {
  match_id: String,
  mode: String,
  players: Vec<String>,
  room_id: Option<String>,
  joined_players: HashSet<String>,
  active_players: HashSet<String>,
}
```

### 5.2 MatchCandidate 结构

```rust
pub struct MatchCandidate {
    pub player_id: String,
    pub match_id: String,
    pub mode: String,
    pub created_at: Instant,
    pub timeout_at: Instant,
}
```

### 5.3 匹配模式（简化版：人齐即开）

| 模式 | 人数 | 说明 |
|-----|------|------|
| 1v1 | 2 | 凑齐 2 人即开 |
| 3v3 | 6 | 凑齐 6 人即开 |
| 5v5 | 10 | 凑齐 10 人即开 |

### 5.4 撮合算法（SimpleMatcher）

```
1. 收集同一 mode 的候选人
2. 按等待时间排序（早来先配）
3. 凑齐 total_size 人后，生成 match_id
4. 创建 match task，并把候选玩家从 Matching 推进到 Matched 上下文
5. 通过 `GameServerClient` 调用 `game-server` 内部 local socket，请求创建 matched room
6. 收到 `CreateRoomAndJoin` 回调后，写入 room_id / token 并推送 `matched`
```

> **扩展预留**：后续可在 `Matcher` trait 中实现按段位/技能匹配等复杂算法

---

## 6. 玩家状态机

```
              ┌─────────┐
              │  Idle   │
              └────┬────┘
                   │ MatchStart
                   ▼
              ┌──────────┐
    ┌────────│ Matching │────────┐
    │        └────┬─────┘         │
    │ MatchCancel │               │ Matched
    ▼             ▼               ▼
┌────────┐   ┌─────────┐   ┌─────────┐
│  Idle  │   │ Matched │   │ InRoom  │
└────────┘   └────┬────┘   └────┬────┘
                  │              │
                  │ MatchFailed │ MatchEnd
                  ▼              ▼
              ┌────────┐   ┌────────┐
              │  Idle  │   │  Idle  │
              └────────┘   └────────┘
```

### 6.1 状态说明

| 状态 | 说明 | 允许的操作 |
|-----|------|----------|
| Idle | 未匹配 | MatchStart |
| Matching | 匹配中 | MatchCancel |
| Matched | 匹配成功，等待进入房间 | - |
| InRoom | 已进入房间 | - |

---

## 7. gRPC 通信流程

### 7.1 正常匹配流程

```
1. 客户端 → MatchService.MatchStart
2. MatchService → 匹配池添加候选人
3. 撮合器检测到人数够，生成 match_id
4. MatchService → GameServer 内部 socket `CreateMatchedRoomReq`
5. GameServer 创建房间，返回 room_id，并通过 `CreateRoomAndJoin` 回调 MatchService
6. MatchService → 客户端 (stream) MatchEvent { matched, room_id, token }
7. 客户端根据 room_id 进入房间；当前 token 已生成并回传，但尚未接入 `game-server` 的入房校验
8. GameServer → MatchService.PlayerJoined
9. 所有玩家都 joined 后，MatchService 标记 match 为 InRoom
```

> 当前实现备注：`MatchStart` 已会在入池后立即触发 `try_match_mode()`；`try_match_mode()` 会创建 match task、通过内部 socket 请求 `game-server` 建房，并在 `CreateRoomAndJoin` 回调落地后向玩家推送 `matched` 事件。

### 7.2 取消匹配流程

```
1. 客户端 → MatchService.MatchCancel
2. MatchService 从匹配池移除候选人
3. 如果已经 matched 但没人进入，通知 GameServer abort
4. MatchService → 客户端 MatchEvent { match_cancelled }
```

### 7.3 玩家断线流程

```
1. GameServer 检测到玩家断线（心跳超时）
2. GameServer → MatchService.PlayerLeft { reason: disconnect }
3. MatchService 判断是否所有人都离开了
4. 如果是，标记 match_should_abort = true
5. GameServer 收到后 abort 房间创建或通知房间内其他人
```

> 当前实现备注：`player_left()` 现在会维护 `active_players`，当最后一个活跃玩家离开时返回 `match_should_abort = true`；`game-server` 收到后会进一步上报 `MatchEnd { reason: "aborted" }` 清理匹配状态。

---

## 8. 配置

```rust
struct Config {
    bind_addr: String,           // gRPC 监听地址
    public_host: String,         // 注册中心使用的对外地址
    port: u16,                   // 从 bind_addr 推导出的端口
    match_timeout_secs: u64,     // 匹配超时（默认 30s）
    max_concurrent_matches: usize, // 最大并发匹配数
    modes: HashMap<String, ModeConfig>,
    match_cleanup_interval_secs: u64,
    game_server_service_name: String,
    game_server_internal_socket_name: String,
    registry_enabled: bool,
    registry_url: String,
}

struct ModeConfig {
    team_size: usize,            // 每队人数
    total_size: usize,           // 总人数 = team_size * 2
    match_timeout_secs: u64,    // 模式特定超时
}
```

---

## 9. 错误码

| 错误码 | 说明 |
|-------|------|
| INVALID_MODE | 不支持的匹配模式 |
| ALREADY_MATCHING | 已经在匹配中 |
| NOT_MATCHING | 当前不在匹配状态 |
| MATCH_NOT_FOUND | 匹配ID不存在 |
| MATCH_TIMEOUT | 匹配超时 |
| ROOM_CREATE_FAILED | 房间创建失败 |
| PLAYER_NOT_FOUND | 玩家不存在 |
| INTERNAL_ERROR | 服务内部错误 |

---

## 10. 依赖

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
tonic = "0.12"
prost = "0.13"
prost-build = "0.13"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["fmt", "env-filter"] }
tracing-appender = "0.2"
uuid = { version = "1", features = ["v4"] }
chrono = { version = "0.4", default-features = false, features = ["clock", "std"] }
dotenvy = "0.15"
thiserror = "2"
```

---

## 11. 开发阶段

### Phase 1: 核心框架 ✅ 已完成
- [x] 项目结构搭建
- [x] proto 定义与编译
- [x] gRPC server 基础框架
- [x] 玩家状态机

### Phase 2: 匹配逻辑 ✅ 已完成
- [x] 匹配池实现
- [x] 简单撮合器核心逻辑
- [x] 自动触发撮合流程
- [x] 匹配超时清理调度

### Phase 3: 房间联动 ✅ 已完成
- [x] MatchService / MatchInternal 接口定义
- [x] GameServer gRPC Client（`apps/game-server/src/match_client.rs`）
- [x] GameServer → MatchService 回调链路（`CreateRoomAndJoin` / `PlayerJoined` / `PlayerLeft` / `MatchEnd`）
- [x] MatchService → GameServer 主动创建房间链路
- [x] `CreateRoomAndJoin` 服务端状态落地

### Phase 4: 完善 ✅ 基本完成
- [x] MatchEventStream 推送
- [x] MatchStatus 查询
- [x] MatchCancel 取消
- [x] 错误码、日志与基础指标
- [x] 离房后的中止判定与失败补偿

---

## 11.1 当前实现状态

### 已实现
- `apps/match-service/` 服务骨架已落地，`server.rs` 同时挂载了 `MatchService`、`MatchInternal` 和 tonic reflection
- `packages/proto/match.proto` 已生成并接入当前服务；同时引入 `packages/proto/game.proto` 的 `CreateMatchedRoomReq/CreateMatchedRoomRes`，用于 MatchService 主动请求 `game-server` 建立 matched room
- 外部接口包含 `MatchStart` / `MatchCancel` / `MatchStatus` / `MatchEventStream`，内部接口包含 `CreateRoomAndJoin` / `PlayerJoined` / `PlayerLeft` / `MatchEnd`
- 玩家状态机、玩家上下文、事件 stream 注册与推送链路已实现，状态包括 `Idle / Matching / Matched / InRoom`
- 匹配池与匹配任务存储已实现，支持按模式分池、候选人增删、创建 match task、记录房间号、joined 玩家集合和 active 玩家集合
- `SimpleMatcher` 已实现 `start_match`、`cancel_match`、`get_status`、`player_joined`、`player_left`、`match_end`、`try_match_mode`
- `MatchStart` 会在玩家入池后立即触发 `try_match_mode()`；`server.rs` 还会启动后台定时任务调用 `cleanup_timeout()`
- `GameServerClient` 已实现，通过 `GAME_SERVER_INTERNAL_SOCKET_NAME` 或注册中心元数据中的 `internal_socket` 连接 `game-server` 内部通道发起 `CreateMatchedRoomReq`
- GameServer 侧的 `MatchClient` 和房间回调链路已落地；`RoomManager` 会在建房、进房、离房、重连、对局结束时调用 MatchService
- `CreateRoomAndJoin` 会把房间号、token 和玩家状态写回匹配上下文，并向玩家推送 `matched`
- `player_left()` 会基于剩余 active 玩家数量返回 `match_should_abort`；最后一人离开时，`game-server` 会继续上报 `MatchEnd { reason: "aborted" }`
- 监控指标已接入，包含 QPS、延迟、池子大小和 `metrics:heartbeat:match-service`

### 当前限制 / 后续补充点
- `MatchEvent.token` 和 `MatchStatus.token` 已生成并回传，但当前 `game-server` 入房流程尚未消费这个 token，客户端仍主要依赖已有登录态 + `room_id` 入房
- 文档中的“调用 game-server.CreateRoomAndJoin”在当前实现里落地为内部 local socket `CreateMatchedRoomReq`，与最初纯 gRPC 设想存在实现偏差
- `match_timeout_secs` 仍主要由各 mode 的 `ModeConfig.match_timeout_secs` 决定；顶层 `MATCH_TIMEOUT_SECS` 更偏向默认配置入口
- 当前仓库没有把“收到 matched 事件后继续自动 RoomJoin”做成单条现成脚本，验证时通常拆为 gRPC 探针和 mock-client 场景两段执行

### 已验证的关键链路
- `MatchStart -> try_match_mode -> GameServerClient.create_matched_room -> CreateRoomAndJoin -> MatchEventStream(matched)`
- `MatchStart -> cleanup_timeout -> MatchEventStream(match_failed/MATCH_TIMEOUT) -> MatchStatus(idle)`
- `PlayerJoined -> InRoom`、`PlayerLeft -> match_should_abort`、`MatchEnd -> Idle`

---

## 12. 待讨论事项

以下问题已确认：

1. **MatchEventStream 推送方式**：gRPC Server Streaming
2. **段位系统**：预留字段，暂不实现；匹配算法简化为"人齐即开"
3. **安全信任**：内网服务互信，不验证 ticket

剩余待讨论：

4. **MatchService 是否需要持久化匹配记录？**
   - 建议先不做，后续通过 MySQL 记录

5. **是否需要匹配池的动态扩缩容？**
   - 建议先不做，单实例足够

6. **是否需要反作弊/防恶意匹配？**
   - 建议后续单独设计
