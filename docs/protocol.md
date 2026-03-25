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

## 第一版基础消息

- `1001 AUTH_REQ`
- `1002 AUTH_RES`
- `1003 PING_REQ`
- `1004 PING_RES`
- `1101 ROOM_JOIN_REQ`
- `1102 ROOM_JOIN_RES`
- `9000 ERROR_RES`

## 第一版消息体

`AUTH_REQ`

- `ticket: string`

`AUTH_RES`

- `ok: bool`
- `player_id: string`
- `error_code: string`

`PING_REQ`

- `client_time: int64`

`PING_RES`

- `server_time: int64`

`ROOM_JOIN_REQ`

- `room_id: string`

`ROOM_JOIN_RES`

- `ok: bool`
- `room_id: string`
- `error_code: string`

`ERROR_RES`

- `error_code: string`
- `message: string`

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

`payload_json` 当前包含：

- `playerId`
- `nonce`
- `exp`

签名算法：

- `HMAC-SHA256`

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
