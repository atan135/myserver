# game-proxy 热切换代理设计

## 1. 文档定位

本文描述 `apps/game-proxy` 当前的接入代理、上游路由、drain/rollout 基础能力，以及后续滚动重启 / 灰度发布的边界。

统一口径：

- 本文讨论服务端滚动重启、灰度发布、连接接入和上游路由。
- room 内 CSV 或运行时配置更新属于 [game-server 更新策略拆分](./game-server-update-strategy.md)。
- 空房接管式灰度的完整目标规范见 [空房接管式灰度规范](./game-server-room-rollout-spec.md)。
- 任务状态见 [空房接管式灰度任务清单](./game-server-room-rollout-task-list.md)。

当前代码优先于本文。如果本文与 `apps/game-proxy/src` 冲突，应以代码为准并同步修正文档。

## 2. 当前实现结论

`game-proxy` 当前已经落地为 Rust + Tokio 接入代理。

已经落地：

- 客户端 KCP 入口，默认 `PROXY_PORT=4000`。
- TCP fallback 入口，默认 `PROXY_TCP_FALLBACK_PORT=PROXY_PORT+10000`，用于本地调试和兼容。
- 上游 `game-server` 使用本地 socket 连接。
- 静态上游配置：`UPSTREAM_SERVER_ID`、`UPSTREAM_LOCAL_SOCKET_NAME`。
- 可选 Redis service registry 发现：`REGISTRY_ENABLED`、`REGISTRY_URL`、`UPSTREAM_SERVICE_NAME`。
- proxy 本地 ticket 鉴权：校验签名、过期时间和 Redis ticket 记录。
- proxy 鉴权成功后先返回 `AuthRes`，选定上游后再 replay 原始 `AuthReq` 到 `game-server`。
- `game-server` 仍执行最终鉴权。
- admin HTTP 口，默认 `PROXY_ADMIN_PORT=7101`。
- 维护模式开关：`/maintenance/on`、`/maintenance/off`。
- upstream 操作状态：`Active`、`Draining`、`Disabled`。
- upstream 健康状态：`Healthy`、`Unavailable`。
- rollout session、room route、player route 的内存态存储。
- 根据 `RoomJoinReq`、`RoomJoinAsObserverReq`、`RoomReconnectReq` 做最小协议感知路由。
- 成功加入 / 重连 / 观战后绑定 room owner 和 player route。
- room route 的版本、epoch、checksum 校验。

仍未完整落地：

- proxy route store 当前是进程内内存态，尚未持久化。
- 自动灰度结束检测尚未完整闭环。
- `ServerRedirectPush` 由旧服下发、客户端显式重连、proxy 重新路由的完整链路尚未完成。
- `FreezeRoomForTransfer` / `ExportRoomTransfer` / `ImportRoomTransfer` / `RetireTransferredRoom` 只是协议和目标流程，尚未在 `game-server` 闭环。
- proxy 不做同一连接内换 upstream。
- proxy 不保存玩法状态，不做 room transfer payload 权威存储。

## 3. 职责边界

### 3.1 auth-http

`auth-http` 负责：

- 登录。
- session、access token、game ticket。
- 账号安全、限流和审计。
- 下发客户端连接所需的 proxy 地址、资源列表和版本信息。

客户端资源热更新、强更判断和资源清单不属于 `game-proxy`。

### 3.2 game-proxy

`game-proxy` 负责：

- 客户端游戏接入入口。
- ticket 的最小接入鉴权。
- 根据默认 upstream、room route 或 player route 选择 `game-server`。
- 建立到目标 `game-server` 的本地 socket 上游连接。
- 鉴权 replay 与后续双向转发。
- 维护模式、摘流和基础 rollout route metadata。
- 记录路由、连接和 rollout 相关日志。

`game-proxy` 不负责：

- 玩家登录和 ticket 签发。
- 客户端资源版本决策。
- 玩法逻辑。
- 房间内部状态计算。
- NPC、怪物、战斗、背包等业务状态。
- 跨服迁移 payload 的权威存储。

### 3.3 game-server

`game-server` 负责：

- 最终 ticket 鉴权和会话建立。
- 玩家协议处理。
- 房间生命周期、帧推进、输入、观战和重连。
- 玩法逻辑、状态快照、移动、战斗等游戏运行时。
- drain mode 下拒绝新建房、保留旧房运行。
- 后续 room freeze/export/import/retire 的实现主体。

## 4. 当前拓扑

```text
mybevy client / mock-client
  -> auth-http
  -> game-proxy(KCP, TCP fallback)
    -> game-server(local socket)
```

普通流程：

1. 客户端从 `auth-http` 获得 ticket 和 proxy 地址。
2. 客户端连接 `game-proxy`。
3. 客户端发送 `AuthReq`。
4. `game-proxy` 校验 ticket 签名、过期时间和 Redis ticket 记录。
5. `game-proxy` 返回 `AuthRes`。
6. 客户端发起首个业务请求，如 `RoomJoinReq`。
7. `game-proxy` 根据请求类型选择 upstream。
8. `game-proxy` 建立到 `game-server` 的 local socket 连接。
9. `game-proxy` replay 原始 `AuthReq` 到上游。
10. `game-server` 鉴权成功后，proxy 转发首个业务请求和后续双向流量。

## 5. 连接模型

当前实现是：

```text
client KCP/TCP session <-> proxy session <-> upstream game-server local socket stream
```

特点：

- 一个客户端 proxy 会话绑定一个上游连接。
- 绑定前 proxy 会缓存认证状态。
- 绑定后主要使用 `copy_bidirectional` 透明转发。
- proxy 只在绑定前和首个 room 相关响应上做最小协议解析。

当前不做：

- 同一连接内切换 upstream。
- 多路复用。
- 连接池复用业务连接。
- 深度玩法协议解析。

## 6. 路由模型

### 6.1 UpstreamRoute

当前 upstream 记录包含：

```text
UpstreamRoute {
  server_id,
  local_socket_name,
  operation_state,
  health_state,
}
```

`operation_state`：

- `Active`
- `Draining`
- `Disabled`

`health_state`：

- `Healthy`
- `Unavailable`

合成后的有效状态：

- `Active`
- `Draining`
- `Disabled`
- `Unavailable`

规则：

- `Active` 可接新 room。
- `Draining` 不接新 room，但允许已绑定 room / player session 回到该 upstream。
- `Disabled` 和 `Unavailable` 不应接入。

### 6.2 RolloutSession

当前 rollout session 包含：

```text
RolloutSession {
  rollout_epoch,
  old_server_id,
  new_server_id,
  state,
  started_at_ms,
}
```

`state` 当前支持：

- `Active`
- `Ending`
- `Interrupted`

rollout session 启动后，新 room 默认优先进入 `new_server_id` 对应 upstream。

### 6.3 RoomRouteRecord

当前 room route 包含：

```text
RoomRouteRecord {
  room_id,
  owner_server_id,
  migration_state,
  member_count,
  online_member_count,
  empty_since_ms,
  room_version,
  rollout_epoch,
  last_transfer_checksum,
  updated_at_ms,
}
```

route 更新时会校验：

- rollout epoch 是否匹配当前 rollout。
- 版本是否倒退。
- 同版本是否冲突。
- 是否跳过版本号。
- 需要 checksum 的迁移状态是否带 checksum。
- CAS 式 `expected_room_version` 和 `expected_last_transfer_checksum`。

### 6.4 PlayerRouteRecord

当前 player route 包含：

```text
PlayerRouteRecord {
  player_id,
  current_room_id,
  preferred_server_id,
  rollout_epoch,
  updated_at_ms,
}
```

`RoomReconnectReq` 会优先根据 player route 和 room route 选择 upstream。

## 7. 当前 proxy 协议感知范围

proxy 当前只解析最小接入和路由所需消息：

| 消息 | proxy 用途 |
|------|------------|
| `AuthReq` | 本地校验 ticket，缓存原始认证包 |
| `PingReq` | 绑定上游前本地响应 |
| `RoomJoinReq` | 根据 `room_id` 选择或创建 room route 归属 |
| `RoomJoinAsObserverReq` | 根据 `room_id` 路由到 room owner |
| `RoomReconnectReq` | 根据 `player_id` / room route 路由 |
| `RoomJoinRes` / `RoomJoinAsObserverRes` / `RoomReconnectRes` | 成功后绑定 room owner 和 player route |

绑定上游后，proxy 不继续解析玩法消息。

## 8. Admin 接口

当前 `game-proxy` admin 口是轻量 HTTP，默认监听 `PROXY_ADMIN_HOST:PROXY_ADMIN_PORT`。

已实现接口：

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/status` | 连接数、维护状态、active upstream、rollout、route 数量 |
| `GET` | `/instances` | upstream 列表 |
| `GET` | `/rollout` | 当前 rollout session |
| `GET` | `/room-routes` | room route 列表 |
| `GET` | `/player-routes` | player route 列表 |
| `POST` | `/maintenance/on` | 开启维护模式，拒绝新 session |
| `POST` | `/maintenance/off` | 关闭维护模式 |
| `POST` | `/rollout/start?rollout_epoch=...&old_server_id=...&new_server_id=...` | 开始 rollout |
| `POST` | `/rollout/end` | 结束 rollout 并清理相关 route |
| `POST` | `/rollout/state?state=Active|Ending|Interrupted` | 更新 rollout 状态 |
| `POST` | `/room-route/upsert?...` | 手动 upsert room route |
| `POST` | `/player-route/upsert?...` | 手动 upsert player route |
| `POST` | `/switch/<server_id>` | 将目标 upstream 置为 active，其余置为 draining |

当前 admin 接口偏本地联调和内部控制用途，生产化前还需要认证、权限、审计和更严格的输入校验。

## 9. 与 drain / rollout 的关系

当前最小 rollout 能力由两部分组成：

- `game-proxy`：上游状态、rollout session、room/player route。
- `game-server`：server 级 `drain_mode`，阻止旧服继续创建新房，但允许已有房间继续 join/reconnect/observer。

已经可验证的最小行为：

- `game-server` drain 开启后拒绝创建新默认房。
- `game-server` drain 开启后拒绝 `CreateMatchedRoomReq` 创建新房。
- `game-server` drain 开启后允许已有房 join。
- `game-server` drain 开启后允许已有房 reconnect。
- `game-server` drain 开启后允许 observer 加入已有房。
- `game-proxy` 可按 room route / player route 将相关请求送回旧 owner 或送到新 owner。

尚未闭环的目标行为：

- old server 主动下发 `ServerRedirectPush`。
- 客户端收到 redirect 后断线重连。
- old server freeze/export。
- new server import。
- proxy 在 transfer 成功后切换 room owner。
- old server retire room。
- proxy 自动判断 rollout 结束。

## 10. 配置项

常用环境变量：

| 变量 | 说明 | 默认 |
|------|------|------|
| `PROXY_HOST` | KCP 监听 host | `127.0.0.1` |
| `PROXY_PORT` | KCP 监听端口 | `4000` |
| `PROXY_ADMIN_HOST` | admin 监听 host | 同 `PROXY_HOST` |
| `PROXY_ADMIN_PORT` | admin 监听端口 | `7101` |
| `PROXY_TCP_FALLBACK_HOST` | TCP fallback host | 同 `PROXY_HOST` |
| `PROXY_TCP_FALLBACK_PORT` | TCP fallback 端口 | `PROXY_PORT + 10000` |
| `UPSTREAM_SERVER_ID` | 静态上游 server id | `game-server-1` |
| `UPSTREAM_LOCAL_SOCKET_NAME` | 静态上游 local socket | `myserver-game-server.sock` |
| `REGISTRY_ENABLED` | 是否启用服务发现 | `false` |
| `REGISTRY_URL` / `REDIS_URL` | 服务发现 Redis 地址 | `redis://127.0.0.1:6379` |
| `REGISTRY_DISCOVER_INTERVAL_SECS` | 服务发现刷新间隔 | `5` |
| `UPSTREAM_SERVICE_NAME` | 要发现的服务名 | `game-server` |
| `TICKET_SECRET` | ticket HMAC secret | dev 默认值 |
| `REDIS_KEY_PREFIX` | Redis key 前缀 | 空 |
| `LOG_LEVEL` / `LOG_ENABLE_CONSOLE` / `LOG_ENABLE_FILE` / `LOG_DIR` | 日志配置 | 见 `.env.example` |

## 11. 后续重点

短期建议优先补：

1. proxy admin 认证、权限和审计。
2. route store 持久化或接入统一控制面，避免重启丢失 rollout metadata。
3. 自动 rollout 结束检测。
4. old server `ServerRedirectPush` 下发与客户端重连链路。
5. room transfer 的 freeze/export/import/retire 最小闭环。
6. 多 proxy 场景下的 route 一致性与健康判定。

跨服状态迁移的完整一致性要求见 [空房接管式灰度规范](./game-server-room-rollout-spec.md)。
