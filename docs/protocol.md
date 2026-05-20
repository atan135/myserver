# 协议设计

## 1. 适用范围

本文描述当前仓库已经落地的协议现状，覆盖：

- `game-server` 玩家 TCP 协议
- `chat-server` 聊天 TCP 协议
- `game-server` admin 控制通道的消息号占用
- `game-proxy` 透传和路由所需的玩家协议消息号

额外协议说明：

- `match-service` 对外接口和 `game-server -> match-service` 回调使用 gRPC，定义在 `packages/proto/match.proto`
- `match-service -> game-server` 的 matched room 创建复用 `packages/proto/game.proto` 中的 `CreateMatchedRoomReq/CreateMatchedRoomRes`，通过 `game-server` 内部 socket 承载
- 管理控制消息结构定义在 `packages/proto/admin.proto`，消息号见本文 admin 段；它们通过独立 TCP admin 通道承载，不走玩家连接

代码基准：

- `packages/proto/game.proto`
- `apps/game-server/src/protocol/message_type.rs`
- `apps/game-proxy/src/protocol.rs`
- `apps/chat-server/src/proto/chat.proto`
- `apps/chat-server/src/chat_server.rs`

---

## 2. 通用 TCP 包结构

`game-server`、`game-proxy` 与 `chat-server` 共享同一套包头格式。`game-proxy` 主要解析首包认证、房间路由相关请求和部分 rollout 消息，其余业务包按原始包头与 body 透传到上游 `game-server`：

```text
| magic(2) | version(1) | flags(1) | msgType(2) | seq(4) | bodyLen(4) | body(N) |
```

字段说明：

- `magic`: 固定值 `0xCAFE`
- `version`: 当前版本 `1`
- `flags`: 当前必须为 `0`
- `msgType`: 消息号
- `seq`: 请求序号，响应通常回填同一序号
- `bodyLen`: 消息体长度
- `body`: Protobuf 编码内容

---

## 3. 协议号分段规则

为避免不同服务继续复用同一段消息号，并为 `game-server` 后续功能模块预留足够空间，当前采用“按服务划大段，再按功能划 100 号段”的规则。

约束说明：

- `game-server` 后续新增功能模块，默认申请一个独立的 `xx00-xx99` 数据段
- `9000-9099` 是跨服务共享的通用错误段，属于全局保留例外

| 段 | 用途 | 当前状态 |
|----|------|----------|
| `1000-19999` | `game-server` 玩家 TCP、admin / 内部控制以及后续功能模块扩展 | 已使用/保留 |
| `20000-20999` | `chat-server` TCP 协议 | 已使用 |
| `21000-29999` | 预留给未来独立 TCP 服务 | 保留 |
| `9000-9099` | 通用错误响应 | 已使用 |

当前细分约定：

| 段 | 用途 |
|----|------|
| `1000-1099` | 游戏服鉴权/心跳 |
| `1100-1199` | 房间/对局请求响应 |
| `1200-1299` | 房间/对局推送 |
| `1300-1399` | 查询类消息 |
| `1400-1499` | 背包/属性/外观请求响应 |
| `1500-1599` | 背包/属性/外观推送 |
| `1600-1699` | room rollout / drain / transfer 控制消息 |
| `1700-1999` | 预留给 `game-server` 后续功能模块，按每模块 `100` 号段继续划分 |
| `2000-2099` | admin 基础控制消息 |
| `3000-3099` | GM 控制消息 |
| `2100-2999`、`3100-8999` | 预留给 `game-server` 后续功能模块，按每模块 `100` 号段继续划分 |
| `9000-9099` | 通用错误响应 |
| `9100-19999` | 预留给 `game-server` 后续功能模块，按每模块 `100` 号段继续划分 |
| `20000-20099` | chat 认证 |
| `20100-20199` | chat 发送/推送 |
| `20200-20299` | 群组与历史查询 |
| `20300-20399` | chat 异步通知推送 |

后续新增消息时遵循：

- 新服务必须先申请独立段位，再定义消息号
- 不再允许通过“不同端口复用同一消息号”的方式扩展协议
- 同一服务内，请求/响应尽量成对分配，推送消息单独留段

---

## 4. game-server / game-proxy 玩家协议消息号

### 4.1 当前消息号

#### 鉴权与心跳

| msgType | 名称 |
|---------|------|
| `1001` | `AuthReq` |
| `1002` | `AuthRes` |
| `1003` | `PingReq` |
| `1004` | `PingRes` |

#### 房间与对局请求响应

| msgType | 名称 |
|---------|------|
| `1101` | `RoomJoinReq` |
| `1102` | `RoomJoinRes` |
| `1103` | `RoomLeaveReq` |
| `1104` | `RoomLeaveRes` |
| `1105` | `RoomReadyReq` |
| `1106` | `RoomReadyRes` |
| `1107` | `RoomStartReq` |
| `1108` | `RoomStartRes` |
| `1111` | `PlayerInputReq` |
| `1112` | `PlayerInputRes` |
| `1113` | `RoomEndReq` |
| `1114` | `RoomEndRes` |
| `1115` | `RoomReconnectReq` |
| `1116` | `RoomReconnectRes` |
| `1117` | `RoomJoinAsObserverReq` |
| `1118` | `RoomJoinAsObserverRes` |
| `1119` | `CreateMatchedRoomReq` |
| `1120` | `CreateMatchedRoomRes` |
| `1121` | `MoveInputReq` |
| `1122` | `MoveInputRes` |

#### 房间与对局推送

| msgType | 名称 |
|---------|------|
| `1201` | `RoomStatePush` |
| `1202` | `GameMessagePush` |
| `1203` | `FrameBundlePush` |
| `1204` | `RoomFrameRatePush` |
| `1205` | `RoomMemberOfflinePush` |
| `1206` | `MovementSnapshotPush` |
| `1207` | `MovementRejectPush` |
| `1208` | `ServerRedirectPush` |
| `1209` | `SessionKickPush` |

#### 查询类消息

| msgType | 名称 |
|---------|------|
| `1301` | `GetRoomDataReq` |
| `1302` | `GetRoomDataRes` |

#### 背包/属性/外观

| msgType | 名称 |
|---------|------|
| `1401` | `ItemEquipReq` |
| `1402` | `ItemEquipRes` |
| `1403` | `ItemUseReq` |
| `1404` | `ItemUseRes` |
| `1405` | `ItemDiscardReq` |
| `1406` | `ItemDiscardRes` |
| `1407` | `ItemAddReq` |
| `1408` | `ItemAddRes` |
| `1409` | `WarehouseAccessReq` |
| `1410` | `WarehouseAccessRes` |
| `1411` | `GetInventoryReq` |
| `1412` | `GetInventoryRes` |
| `1501` | `InventoryUpdatePush` |
| `1502` | `AttrChangePush` |
| `1503` | `VisualChangePush` |
| `1504` | `ItemObtainPush` |

#### Room rollout / drain / transfer

| msgType | 名称 |
|---------|------|
| `1601` | `FreezeRoomForTransferReq` |
| `1602` | `FreezeRoomForTransferRes` |
| `1603` | `ExportRoomTransferReq` |
| `1604` | `ExportRoomTransferRes` |
| `1605` | `ImportRoomTransferReq` |
| `1606` | `ImportRoomTransferRes` |
| `1607` | `RetireTransferredRoomReq` |
| `1608` | `RetireTransferredRoomRes` |
| `1609` | `GetRolloutDrainStatusReq` |
| `1610` | `GetRolloutDrainStatusRes` |

说明：

- 这些消息结构已在 `packages/proto/game.proto` 和 `game-server` / `game-proxy` 消息号枚举中定义。
- 当前代码已落地 `drain_mode` 对新建房的拦截，以及 `game-proxy` 侧 rollout 路由状态；完整房间冻结、导出、导入、迁移退休链路仍以 `docs/game-server-room-rollout-spec.md` 和实际代码为准。
- `ServerRedirectPush` 已作为协议结构和代理路由语义保留，当前主要用于 rollout 重连链路的后续补齐。

#### 通用错误

| msgType | 名称 |
|---------|------|
| `9000` | `ErrorRes` |

### 4.2 关键消息结构

#### `AuthReq`

- `ticket: string`

#### `AuthRes`

- `ok: bool`
- `player_id: string`
- `error_code: string`

#### `RoomJoinReq`

- `room_id: string`
- `policy_id: string`

#### `RoomReconnectReq`

- `player_id: string`

#### `RoomJoinAsObserverReq`

- `room_id: string`

#### `CreateMatchedRoomReq`

- `match_id: string`
- `room_id: string`
- `player_ids: repeated string`
- `mode: string`

说明：

- 当前该消息既可由已鉴权玩家 TCP 请求触发，也可由 `match-service` 通过 `game-server` 内部 socket 发送
- `CreateMatchedRoomRes` 会返回实际创建后的 `room_id` 与初始 `RoomSnapshot`

#### `MoveInputReq`

- `frame_id: uint32`
- `input_type: MoveInputType`
- `dir_x: float`
- `dir_y: float`
- `has_client_state: bool`
- `client_x: float`
- `client_y: float`
- `client_frame_id: uint32`

说明：

- `MOVE_INPUT_TYPE_MOVE_DIR` 和 `MOVE_INPUT_TYPE_MOVE_STOP` 是移动控制输入，客户端移动期间应持续发送。
- `MOVE_INPUT_TYPE_FACE_TO` 只表示朝向变化，不会让服务端继续保持移动。
- 服务端会拒绝非有限数值或超出安全范围的方向、客户端位置字段。
- 连续缺少真实移动控制输入达到房间策略阈值后，服务端会强制停步，并通过 `MovementSnapshotPush.reason_code = MOVEMENT_CORRECTION_REASON_CONTROL_TIMEOUT` 下发权威状态。

#### `MovementSnapshotPush`

- `room_id: string`
- `frame_id: uint32`
- `entities: repeated EntityTransform`
- `full_sync: bool`
- `reason: string`
- `correction_kind: MovementCorrectionKind`
- `reason_code: MovementCorrectionReason`
- `target_player_ids: repeated string`
- `reference_frame_id: uint32`

#### `MovementRejectPush`

- `room_id: string`
- `frame_id: uint32`
- `player_id: string`
- `error_code: string`
- `corrected: EntityTransform`
- `correction_kind: MovementCorrectionKind`
- `reason_code: MovementCorrectionReason`
- `reference_frame_id: uint32`
- `has_client_state: bool`
- `client_x: float`
- `client_y: float`
- `server_x: float`
- `server_y: float`

#### `ServerRedirectPush`

- `reason: string`
- `room_id: string`
- `rollout_epoch: string`
- `reconnect_required: bool`
- `retry_after_ms: uint32`

#### `SessionKickPush`

- `reason: string`
- `timestamp: int64`

### 4.3 房间快照结构

#### `RoomMember`

- `player_id: string`
- `ready: bool`
- `is_owner: bool`
- `offline: bool`
- `role: MemberRole`

#### `RoomSnapshot`

- `room_id: string`
- `owner_player_id: string`
- `state: string`
- `members: repeated RoomMember`
- `current_frame_id: uint32`
- `game_state: string`

### 4.4 房间状态约束

- 未鉴权连接仅允许 `AuthReq`、`PingReq`
- 一个连接同一时刻只能处于一个房间上下文
- `PlayerInputReq` / `MoveInputReq` 只应在允许的对局状态中发送
- 重连和观战走独立消息，不复用普通 `RoomJoinReq`

---

## 5. chat-server TCP 协议

### 5.1 当前消息号

#### 认证

| msgType | 名称 |
|---------|------|
| `20001` | `ChatAuthReq` |
| `20002` | `ChatAuthRes` |

#### 聊天收发

| msgType | 名称 |
|---------|------|
| `20101` | `ChatPrivateReq` |
| `20102` | `ChatPrivateRes` |
| `20103` | `ChatGroupReq` |
| `20104` | `ChatGroupRes` |
| `20105` | `ChatPush` |

#### 群组与历史查询

| msgType | 名称 |
|---------|------|
| `20201` | `GroupCreateReq` |
| `20202` | `GroupCreateRes` |
| `20203` | `GroupJoinReq` |
| `20204` | `GroupJoinRes` |
| `20205` | `GroupLeaveReq` |
| `20206` | `GroupLeaveRes` |
| `20207` | `GroupDismissReq` |
| `20208` | `GroupDismissRes` |
| `20209` | `GroupListReq` |
| `20210` | `GroupListRes` |
| `20211` | `ChatHistoryReq` |
| `20212` | `ChatHistoryRes` |

#### 异步通知

| msgType | 名称 |
|---------|------|
| `20301` | `MailNotifyPush` |

#### 通用错误

| msgType | 名称 |
|---------|------|
| `9000` | `ErrorRes` |

### 5.2 关键消息结构

#### `ChatAuthReq`

- `player_id: string`
- `token: string`

说明：

- 当前实现主要使用 `token`
- `token` 复用 `auth-http` 签发的 game ticket 校验逻辑
- `chat-server` 现在要求首包 `msgType` 必须是 `20001`

#### `ChatAuthRes`

- `ok: bool`
- `error_code: string`

#### `ChatPrivateReq`

- `target_id: string`
- `content: string`

#### `ChatGroupReq`

- `group_id: string`
- `content: string`

#### `ChatPush`

- `msg_id: string`
- `chat_type: int32`
- `sender_id: string`
- `sender_name: string`
- `content: string`
- `timestamp: int64`
- `target_id: string`
- `group_id: string`

#### `GroupCreateRes`

- `ok: bool`
- `group_id: string`
- `error_code: string`

#### `ChatHistoryReq`

- `chat_type: int32`
- `target_id: string`
- `before_time: int64`
- `limit: int32`

#### `MailNotifyPush`

- `mail_id: string`
- `title: string`
- `from_player_id: string`
- `mail_type: string`
- `created_at: int64`

### 5.3 chat 状态约束

- 连接建立后的首个业务包必须是 `ChatAuthReq`
- 认证成功后才允许发送私聊、群聊、群管理、历史查询消息
- `MailNotifyPush` 由服务端异步推送，不是客户端请求响应的一部分

---

## 6. admin 控制消息号

`game-server` 的 admin 通道当前占用 `2000-2099` 与 `3000-3099` 段：

| msgType | 名称 |
|---------|------|
| `2001` | `AdminServerStatusReq` |
| `2002` | `AdminServerStatusRes` |
| `2003` | `AdminUpdateConfigReq` |
| `2004` | `AdminUpdateConfigRes` |
| `3001` | `GmBroadcastReq` |
| `3002` | `GmBroadcastRes` |
| `3003` | `GmSendItemReq` |
| `3004` | `GmSendItemRes` |
| `3005` | `GmKickPlayerReq` |
| `3006` | `GmKickPlayerRes` |
| `3007` | `GmBanPlayerReq` |
| `3008` | `GmBanPlayerRes` |

消息结构定义位于：

- `packages/proto/admin.proto`

说明：

- admin 消息不走玩家 TCP 通道
- 但为了保持统一封包方式，仍使用同一套包头和独立消息号段
- `AdminServerStatusReq/Res` 与 `AdminUpdateConfigReq/Res` 使用 `packages/proto/admin.proto`
- `GmSendItemReq/Res` 当前复用 `GrantItemsReq/GrantItemsRes`，并保留 JSON 旧格式兼容
- `GmBroadcast`、`GmKickPlayer`、`GmBanPlayer` 目前有消息号和后台入口占位，但 `game-server` admin handler 尚未完整实现，收到后会按不支持消息处理

---

## 7. ticket 设计

`auth-http` 签发的 ticket 格式：

```text
base64url(payload_json).base64url(hmac_sha256_signature)
```

当前校验规则：

- `game-server` 使用同一 `TICKET_SECRET` 校验签名
- `game-proxy` 会先校验签名并检查 Redis ticket 记录，随后把认证包 replay 到 `game-server`
- `chat-server` 也复用同一套签名校验逻辑
- `exp` 过期则拒绝
- `game-server` 还会继续检查 Redis 中是否存在对应 ticket 记录
- `chat-server` 当前只校验 ticket 签名和过期时间，不查询 Redis ticket 记录

Redis 相关键：

- `session:<accessToken>`
- `ticket:<sha256(ticket)>`
- `player-session:<playerId>`
- `session:kick:<playerId>`

---

## 8. 维护原则

后续维护协议时，按以下顺序处理：

1. 先确认新消息属于哪个服务。
2. 在该服务所属段位内分配新的 `msgType`。
3. 同步更新协议定义、服务端枚举、客户端常量和文档。
4. 如果需要新增独立 TCP 服务，先在本文中为它预留新段位，再写代码。
