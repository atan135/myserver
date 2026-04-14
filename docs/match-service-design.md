# Match-Service 设计文档

## 1. 概述

Match-Service 是独立的匹配服务，负责玩家匹配逻辑，与 game-server 通过 gRPC 通信。

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
│  │ (按mode/    │  │ (撮合逻辑)   │  │ (玩家状态管理)   │    │
│  │  rank分离)  │  │             │  │                 │    │
│  └─────────────┘  └─────────────┘  └─────────────────┘    │
│         │                │                  │              │
│         └────────────────┼──────────────────┘              │
│                          │                                  │
│  ┌─────────────────────────────────────────────────────┐   │
│  │              GrpcServer ( tonic )                   │   │
│  │  - MatchStart / MatchCancel / MatchStatus           │   │
│  │  - CreateRoomAndJoin / PlayerJoined / PlayerLeft   │   │
│  └─────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────┘
                            │ gRPC
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
    │   ├── mod.rs            # myserver.match 模块导出
    │   └── myserver.match.rs # 自动生成
    ├── server.rs             # gRPC 服务器
    ├── service/
    │   ├── mod.rs
    │   ├── match_service.rs  # MatchService 实现（对外接口）
    │   └── admin_service.rs  # 管理接口（可选）
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

package myserver.match;

service MatchService {
  // 客户端发起匹配
  rpc MatchStart(MatchStartReq) returns (MatchStartRes);

  // 客户端取消匹配
  rpc MatchCancel(MatchCancelReq) returns (MatchCancelRes);

  // 客户端查询匹配状态
  rpc MatchStatus(MatchStatusReq) returns (MatchStatusRes);
}

// 推送消息（服务端通过 gRPC stream 推送）
service MatchPush {
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
  pools: HashMap<Mode, ModePool>
}

ModePool {
  mode: String,                    // 模式标识
  team_size: usize,               // 每队人数
  total_size: usize,              // 总人数 = team_size * 2
  candidates: Vec<MatchCandidate>, // 等待撮合的候选人
  matcher: Box<dyn Matcher>,      // 撮合器
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
    pub stream_sender: ChannelSender<MatchEvent>,  // 推送通道
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
4. 发布 MatchMatched 事件
5. 调用 game-server.CreateRoomAndJoin
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
4. MatchService → GameServer.CreateRoomAndJoin
5. GameServer 创建房间，返回 room_id
6. MatchService → 客户端 (stream) MatchEvent { matched, room_id, token }
7. 客户端 → GameServer 携带 token 进入房间
8. GameServer → MatchService.PlayerJoined
9. 所有玩家都 joined 后，MatchService 标记 match 为 InRoom
```

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

---

## 8. 配置

```rust
struct Config {
    bind_addr: String,           // gRPC 监听地址
    match_timeout_secs: u64,     // 匹配超时（默认 30s）
    max_concurrent_matches: usize, // 最大并发匹配数

    // 各模式配置
    mode_1v1: ModeConfig,
    mode_3v3: ModeConfig,
    mode_5v5: ModeConfig,
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
- [x] 简单撮合器
- [x] 匹配超时处理

### Phase 3: 房间联动 ✅ 已完成
- [x] MatchService 接口定义
- [x] GameServer gRPC Client（`apps/game-server/src/match_client.rs`）
- [x] CreateRoomAndJoin 流程
- [x] PlayerJoined / PlayerLeft 回调

### Phase 4: 完善 ⚠️ 部分完成
- [x] MatchEventStream 推送
- [x] MatchStatus 查询
- [x] MatchCancel 取消
- [x] 错误码与日志完善

---

## 11.1 当前实现状态

### 已完成
- `apps/match-service/` 项目结构
- `packages/proto/match.proto` - gRPC 接口定义
- `MatchService` 对外接口（MatchStart/Cancel/Status/EventStream）
- `MatchInternal` 对内接口（CreateRoomAndJoin/PlayerJoined/PlayerLeft/MatchEnd）
- 玩家状态机（Idle/Matching/Matched/InRoom）
- 匹配池（按模式分离，人齐即开）
- 简单撮合器（SimpleMatcher）
- GameServer MatchClient（`apps/game-server/src/match_client.rs`）
- gRPC Server（tonic + reflection）

### 待完成
- GameServer 作为 client 调用 MatchService
- 实际的房间创建回调（当前为 mock）
- 撮合定时器（当前只在 MatchStart 时尝试撮合）
- 匹配超时清理（cleanup_timeout 未被调用）

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
