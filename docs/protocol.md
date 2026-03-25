# 协议设计 v0.1

## 玩家 TCP 包结构

```text
| magic(2) | version(1) | flags(1) | msgType(2) | seq(4) | bodyLen(4) | body(N) |
```

## 字段说明

- `magic`：固定值，快速识别非法连接
- `version`：协议版本
- `flags`：当前必须为 `0`
- `msgType`：消息号
- `seq`：消息序列号
- `bodyLen`：消息体长度
- `body`：Protobuf 序列化内容

## 当前消息号

- `1001 AUTH_REQ`
- `1002 AUTH_RES`
- `1003 PING_REQ`
- `1004 PING_RES`
- `1101 ROOM_JOIN_REQ`
- `1102 ROOM_JOIN_RES`
- `1103 ROOM_LEAVE_REQ`
- `1104 ROOM_LEAVE_RES`
- `1105 ROOM_READY_REQ`
- `1106 ROOM_READY_RES`
- `1201 ROOM_STATE_PUSH`
- `9000 ERROR_RES`

## 房间核心消息

`ROOM_JOIN_REQ`

- `room_id: string`

`ROOM_JOIN_RES`

- `ok: bool`
- `room_id: string`
- `error_code: string`

`ROOM_LEAVE_REQ`

- 空消息体

`ROOM_LEAVE_RES`

- `ok: bool`
- `room_id: string`
- `error_code: string`

`ROOM_READY_REQ`

- `ready: bool`

`ROOM_READY_RES`

- `ok: bool`
- `room_id: string`
- `ready: bool`
- `error_code: string`

`ROOM_STATE_PUSH`

- `event: string`
- `snapshot: RoomSnapshot`

`RoomSnapshot`

- `room_id: string`
- `owner_player_id: string`
- `state: string`
- `members: RoomMember[]`

`RoomMember`

- `player_id: string`
- `ready: bool`
- `is_owner: bool`

## 当前房间规则

- 一个连接同一时刻只能在一个房间中
- 房间最大人数为 `10`
- 第一个进入房间的玩家为 `owner`
- `owner` 离开后，房主转移给当前房间内的下一个成员
- 所有成员 `ready=true` 时，房间状态变为 `ready`
- 只要有成员未准备，房间状态为 `waiting`

## 房间事件广播

当前会广播以下事件：

- `member_joined`
- `ready_changed`
- `member_left`
- `member_disconnected`

## 状态约束

- 未鉴权连接只允许发送 `AUTH_REQ` 和 `PING_REQ`
- `flags` 当前必须为 `0`
- 服务端必须校验包长度、消息号和状态机
- 心跳超时连接需要断开
- 非法消息直接返回错误或断开

## ticket 设计

HTTP 登录服签发的 `ticket` 格式如下：

```text
base64url(payload_json).base64url(hmac_sha256_signature)
```

验证规则：

- `game-server` 使用相同的 `TICKET_SECRET` 计算签名
- 签名不匹配则拒绝
- `exp` 过期则拒绝
- Redis 中不存在对应 ticket 时也拒绝

## Redis 键设计

- `session:<accessToken>`
- `ticket:<sha256(ticket)>`

## 内部控制面

第一版不复用玩家 TCP 通道。内部控制命令后续通过独立协议实现，协议定义预留在 `packages/proto/admin.proto`。
