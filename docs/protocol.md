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

`game-server`、`game-proxy` 与 `chat-server` 共享同一套包头格式。`game-proxy` 主要解析接入认证、鉴权前心跳、房间路由相关请求和部分 rollout 消息。连接完成 `AuthReq` 且代理本地校验成功前，其余业务包不会被转发到上游 `game-server`；鉴权成功并绑定上游后，其余业务包按原始包头与 body 透传：

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
| `1611` | `TriggerServerRedirectReq` |
| `1612` | `TriggerServerRedirectRes` |

说明：

- 这些消息结构已在 `packages/proto/game.proto` 定义；`game-server` 消息号枚举包含完整控制面消息，`game-proxy` 仅包含自身转发和路由判定需要识别的子集。
- `GetRolloutDrainStatusRes` 返回旧服真实 `drain_mode_enabled`、`drain_mode_entered_at_ms`、`connection_count`、`owned_room_count`、`migrating_room_count` 与 `RoomRouteStatus` 样本；未进入 drain mode 时 `drain_mode_entered_at_ms=0`。
- 当前代码已落地 `drain_mode` 对新建房的拦截、`game-proxy` 侧 rollout 路由状态、`game-server` 房间 freeze/export/import/retire 最小闭环，以及 `TriggerServerRedirectReq/Res` 可控 redirect push 下发入口；真实多进程联调和自动灰度收尾仍以 `docs/game-server-room-rollout-spec.md`、任务清单和实际代码为准。
- `ServerRedirectPush` 已作为第一阶段显式重连协议使用。服务端可下发 push，push 成功入队后旧服会以 `server_redirect_reconnect_required` 请求关闭旧连接；`tools/mock-client` 已有收到 push 后主动重连目标入口并优先 `RoomReconnectReq` 的验证场景。

#### 通用错误

| msgType | 名称 |
|---------|------|
| `9000` | `ErrorRes` |

当前 `game-proxy` 和 `game-server` 都会在鉴权成功前拒绝非 `AuthReq` / `PingReq` 消息，并返回 `ErrorRes`，`error_code=PREAUTH_MESSAGE_NOT_ALLOWED`。代理侧错误在本地产生，不会触发上游选择、鉴权 replay 或 upstream 连接；游戏服侧错误在 dispatch 层产生，不会进入房间、移动、背包等业务 handler。

`game-proxy` 与 `game-server` 都支持单连接消息频率限制。proxy 侧使用 `PROXY_MSG_RATE_WINDOW_MS` / `PROXY_MSG_RATE_MAX`，在读到完整 packet 后、进入本地鉴权 / 预鉴权白名单 / 上游转发前检查；game-server 侧使用 `MSG_RATE_WINDOW_MS` / `MSG_RATE_MAX`，在业务 dispatch 前检查。对应最大值为 `0` 时关闭；启用后超限返回 `ErrorRes`，`error_code=MSG_RATE_EXCEEDED`，并继续保持连接。

`game-proxy` 可通过 `PROXY_REDIS_BLOCKLIST_ENABLED=true` 启用 Redis 动态黑名单。IP 命中 `${REDIS_KEY_PREFIX}security:blocklist:ip:<ip>` 时会在连接建立早期拒绝，通常不会产生协议包；玩家命中 `${REDIS_KEY_PREFIX}security:blocklist:player:<player_id>` 时会在本地 ticket 校验成功后返回 `AuthRes(ok=false,error_code=PLAYER_BLOCKED)`。启用后 Redis 查询失败按 fail-closed 处理，连接早期拒绝或在 `AuthReq` 返回 `AuthRes(ok=false,error_code=BLOCKLIST_UNAVAILABLE)`。

`auth-http` 可通过 `AUTH_REDIS_BLOCKLIST_ENABLED=true` 启用同一套 Redis 动态黑名单。登录入口 IP 命中时 HTTP 返回 `IP_BLOCKED`；登录成功但玩家命中时不创建 session / ticket，返回 `PLAYER_BLOCKED`；`/api/v1/game-ticket/issue` 玩家命中时不签发新 ticket。启用后 Redis 查询失败按 fail-closed 返回 `BLOCKLIST_UNAVAILABLE`。

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

#### `PlayerInputReq`

- `frame_id: uint32`
- `action: string`
- `payload_json: string`
- `client_timestamp_ms: int64`

说明：

- `client_timestamp_ms` 与 `MoveInputReq` 使用同一套 `game-server` 可配置窗口校验；默认兼容旧客户端。
- `game-server` 会对同一玩家短窗口内的重复输入、过期帧、未来帧和时间戳异常做本实例内计数与结构化日志；默认只观测不额外拒绝，配置阈值后返回 `INPUT_ANOMALY_BLOCKED`。

#### `MoveInputReq`

- `frame_id: uint32`
- `input_type: MoveInputType`
- `dir_x: float`
- `dir_y: float`
- `has_client_state: bool`
- `client_x: float`
- `client_y: float`
- `client_frame_id: uint32`
- `client_timestamp_ms: int64`

说明：

- `MOVE_INPUT_TYPE_MOVE_DIR` 和 `MOVE_INPUT_TYPE_MOVE_STOP` 是移动控制输入，客户端移动期间应持续发送。
- `MOVE_INPUT_TYPE_FACE_TO` 只表示朝向变化，不会让服务端继续保持移动。
- 服务端会拒绝非有限数值或超出安全范围的方向、客户端位置字段。
- `client_timestamp_ms` 已在 `game-server` 落地可配置窗口校验，默认兼容旧客户端；字段缺失或为 `0` 时默认跳过校验，`INPUT_TIMESTAMP_REQUIRED=true` 时拒绝并返回 `INPUT_TIMESTAMP_REQUIRED`。
- 当 `client_timestamp_ms > 0` 且 `INPUT_TIMESTAMP_MAX_SKEW_MS > 0` 时，服务端会按当前 Unix 毫秒时间校验绝对偏差，超出窗口返回 `INPUT_TIMESTAMP_SKEW`；`INPUT_TIMESTAMP_MAX_SKEW_MS=0` 表示只要求字段存在、不做偏差窗口校验。
- 同一玩家同一房间连续重复上报同一 `frame_id` 且输入内容完全相同时，会记录为 `INPUT_FRAME_DUPLICATE` 异常；同帧不同内容仍保留现有替换语义。`RoomManager` 对已过期帧和超前帧仍返回 `INPUT_FRAME_EXPIRED` / `INPUT_FRAME_TOO_FAR`，这些结果会进入同一异常计数窗口。
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
- `target_host: string`
- `target_port: uint32`
- `target_server_id: string`
- `transport: string`

第一阶段 redirect 语义是显式重连，不是同连接迁移。客户端收到 `ServerRedirectPush` 后应断开当前游戏连接，重新连接 `target_host:target_port` 指向的 `game-proxy`，再发送 `AuthReq` 和 `RoomReconnectReq` / `RoomJoinReq`。

#### `SessionKickPush`

- `reason: string`
- `timestamp: int64`

当前 `reason` 会用于并发登录踢旧连接、改密踢旧连接，以及 GM 踢人/封禁的在线连接处置。GM 操作未填写原因时，`game-server` 分别使用 `gm_kick` / `gm_ban`。

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

- 未鉴权连接仅允许 `AuthReq`、`PingReq`；`game-proxy` 与 `game-server` 都已强制该白名单
- `AuthReq` 失败后连接保持未认证状态，后续房间、移动、背包、GM、未知消息等会被鉴权前白名单拒绝
- `game-proxy` 默认 `PROXY_MAX_PREAUTH_FAILURES=3`，同一连接在鉴权成功前非法消息或鉴权失败累计达到阈值后关闭连接；配置为 `0` 表示不按失败次数断开
- `game-proxy` 默认 `PROXY_MSG_RATE_MAX=0` 不启用单连接入站消息频率限制；配置为正整数后，超频消息会收到 `MSG_RATE_EXCEEDED`，当前不断开连接，也不计入预鉴权失败次数
- `game-server` 默认 `MSG_RATE_MAX=0` 不启用单连接消息频率限制；配置为正整数后，超频消息会收到 `MSG_RATE_EXCEEDED`，当前不断开连接
- 一个连接同一时刻只能处于一个房间上下文
- `PlayerInputReq` / `MoveInputReq` 只应在允许的对局状态中发送；两者都带 `client_timestamp_ms`，`game-server` 默认兼容旧客户端，配置要求时间戳或窗口校验失败时不会进入玩法层
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
- `chat-server` 会检查 ticket 签名、过期时间、`${REDIS_KEY_PREFIX}ticket:<sha256(ticket)>` 归属和 `${REDIS_KEY_PREFIX}player-ticket-version:<playerId>`

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
- `TriggerServerRedirectReq/Res` 使用 `packages/proto/game.proto`，可走已鉴权 admin / internal 通道触发旧服向指定 room 当前在线成员下发 `ServerRedirectPush`
- `GmSendItemReq/Res` 当前复用 `GrantItemsReq/GrantItemsRes`，并保留 JSON 旧格式兼容
- `GmBroadcastReq`、`GmKickPlayerReq`、`GmBanPlayerReq` 当前由 `admin-api` 以 JSON body 调用；`game-server` 会返回对应 `Gm*Res` 消息号，失败时返回 `ErrorRes`
- `GmBroadcastReq` 字段为 `{ title, content, sender }`，`game-server` 校验非空与长度后，复用玩家通道 `GameMessagePush` 推送在线连接：`event="gm_broadcast"`、`action="broadcast"`、`payload_json={ title, content, sender, timestamp }`
- `GmKickPlayerReq` 字段为 `{ playerId, reason }`，只处理当前 `game-server` 实例上的已鉴权在线连接，触发 `SessionKickPush` 后断开；离线或不在本实例返回 `PLAYER_OFFLINE`
- `GmBanPlayerReq` 字段为 `{ playerId, durationSeconds, reason }`，`game-server` 侧只做在线连接处置，成功语义等同“该实例在线玩家已收到封禁原因并被踢下线”；持久账号封禁状态由 `admin-api` 的 GM HTTP 入口写入 `player_accounts.status=banned`

---

## 7. ticket 设计

`auth-http` 签发的 ticket 格式：

```text
base64url(payload_json).base64url(hmac_sha256_signature)
```

当前校验规则：

- `game-server` 使用同一 `TICKET_SECRET` 校验签名
- `game-proxy` 会先校验签名并检查 Redis ticket 记录，随后把认证包 replay 到 `game-server`
- `chat-server` 也复用同一套签名校验逻辑，并检查 Redis ticket 记录和 ticket payload 中的版本号
- `exp` 过期则拒绝
- `game-server` 还会继续检查 Redis 中是否存在对应 ticket 记录
- `game-proxy`、`game-server`、`chat-server` 都会检查 Redis 中的 `player-ticket-version:<playerId>`，从而感知 logout / 改密等玩家级 ticket version 失效
- `game-proxy`、`game-server`、`chat-server` 都会检查 Redis 中的 `ticket:<sha256(ticket)>`，从而感知单张 ticket revoke；其中 `chat-server` 对不存在或归属不匹配统一返回 `TICKET_REVOKED`
- `auth-http` 当前默认 `TICKET_TTL_SECONDS=900`。logout 和改密通过递增 `player-ticket-version:<playerId>` 统一失效该玩家未过期旧 ticket，不枚举删除所有 ticket key；logout body 中可选的 `ticket` 仍会走单张 revoke 路径并校验归属
- 这些 Redis key 都受 `REDIS_KEY_PREFIX` 影响

Redis 相关键：

- `session:<accessToken>`
- `ticket:<sha256(ticket)>`
- `player-session:<playerId>`
- `player-ticket-version:<playerId>`

并发登录/改密踢旧连接通知已迁移为 Core NATS subject：

- `myserver.session.kick.<player_id_token>`

---

## 8. 维护原则

后续维护协议时，按以下顺序处理：

1. 先确认新消息属于哪个服务。
2. 在该服务所属段位内分配新的 `msgType`。
3. 同步更新协议定义、服务端枚举、`tools/mock-client`、外部 `mybevy` 客户端协议绑定和文档。
4. 如果需要新增独立 TCP 服务，先在本文中为它预留新段位，再写代码。
