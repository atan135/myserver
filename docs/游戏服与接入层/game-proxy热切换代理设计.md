# game-proxy 热切换代理设计

## 1. 文档定位

本文描述 `apps/game-proxy` 当前的接入代理、上游路由、drain/rollout 基础能力，以及后续滚动重启 / 灰度发布的边界。

统一口径：

- 本文讨论服务端滚动重启、灰度发布、连接接入和上游路由。
- 生产公网暴露边界、多实例路线、room ownership 和同连接迁移目标态见 [生产拓扑与 Room 迁移设计](../后台与运维/生产拓扑与Room迁移设计.md)。
- room 内 CSV 或运行时配置更新属于 [game-server 更新策略拆分](./游戏服更新策略拆分.md)。
- 空房接管式灰度的完整目标规范见 [空房接管式灰度规范](./空房接管式灰度规范.md)。
- 任务状态见 [空房接管式灰度任务清单](./空房接管式灰度任务清单.md)。
- 可重复演练入口见 [old/new/proxy 三进程 rollout 演练入口](../后台与运维/三进程灰度演练手册.md)。

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
- proxy 鉴权前消息白名单：未认证连接只允许 `AuthReq` 与 `PingReq`；其它消息返回 `PREAUTH_MESSAGE_NOT_ALLOWED`，不会触发上游选择。
- proxy 单连接预鉴权失败阈值：默认 `PROXY_MAX_PREAUTH_FAILURES=3`，非法预鉴权消息或鉴权失败累计达到阈值后关闭连接。
- proxy 总前端连接上限：`PROXY_MAX_CONNECTIONS` 默认 `0` 表示不限制，配置为正整数时拒绝超限新连接。
- proxy 静态 IP denylist：`PROXY_IP_DENYLIST` 支持逗号分隔的精确 IP 和 CIDR，命中后在 session 建立初期拒绝。
- proxy 单 IP 本地连接上限：`PROXY_MAX_CONNECTIONS_PER_IP` 默认 `0` 表示不限制，配置为正整数时按来源 IP 限制本实例并发连接，连接关闭时释放。
- proxy 单玩家本地连接上限：`PROXY_MAX_CONNECTIONS_PER_PLAYER` 默认 `0` 表示不限制，配置为正整数时在 `AuthReq` 本地鉴权成功后登记已鉴权玩家连接；超限返回 `AuthRes(ok=false, error_code=PLAYER_CONNECTION_LIMIT_EXCEEDED)`，连接关闭或重复鉴权切换玩家时释放。
- proxy 维护模式判断：`AuthReq` 阶段同时检查本进程 admin 开关和 Redis 共享状态 `${REDIS_KEY_PREFIX}maintenance:global`；命中后返回 `AuthRes(ok=false, error_code=MAINTENANCE_MODE)`，不选择上游。Redis 状态带短 TTL 缓存，默认 `PROXY_MAINTENANCE_CACHE_TTL_MS=2000`。
- `game-server` 仍执行最终鉴权。
- admin HTTP 口，默认 `PROXY_ADMIN_PORT=7101`。
- admin 操作级 scoped token RBAC：兼容全权限写 token、只读 token，并可通过 scoped token 限制维护、rollout 或 route 写入。
- 本进程维护模式开关：`/maintenance/on`、`/maintenance/off`。
- admin 写操作 `X-Admin-Actor` 操作人解析、结构化日志审计与 JSONL 持久审计。
- upstream 操作状态：`Active`、`Draining`、`Disabled`。
- upstream 健康状态：`Healthy`、`Unavailable`。
- rollout session、room route、player route 的 route store；默认内存态，本地开发无需 Redis，生产可通过 `PROXY_ROUTE_STORE_BACKEND=redis` 持久化到 Redis。
- 根据 `RoomJoinReq`、`RoomJoinAsObserverReq`、`RoomReconnectReq` 做最小协议感知路由。
- 成功加入 / 重连 / 观战后绑定 room owner 和 player route。
- room route 的版本、epoch、checksum 校验。

已推进到第一阶段闭环：

- Redis route store 持久化已形成第一阶段多 proxy 闭环：启用后启动加载已有 rollout session、room route、player route，变更后以单 key 快照级 CAS 写回 Redis，并在 CAS 成功后通过 Redis pub/sub 发布最新 `store_revision`；其它 proxy 收到比本地更新的 revision 后会 reload Redis 快照，解决单 proxy 重启丢 route，并缩短多 proxy 本地缓存陈旧窗口。
- `game-proxy` 已支持基于当前 rollout route store 的自动收尾：控制面可检查当前 epoch 内是否仍有 old owner / 迁移中 room route 或指向 old 的 player route，排空后自动结束 rollout 并清理当前 epoch 和空 epoch route metadata。
- `game-proxy` 的 `POST /rollout/complete-if-drained` 可选启用旧服真实 drain status 校验：当 `PROXY_ROLLOUT_DRAIN_STATUS_CHECK_ENABLED=true` 时，route store 先判定 `Drained` 后还会通过 `auth-http` 内部接口查询旧服真实状态，只有 HTTP 2xx、JSON `ok=true` 且 `ownedRoomCount == 0`、`migratingRoomCount == 0`、`connectionCount == 0` 时才结束 rollout；失败、超时、非 2xx、JSON 异常或字段不满足都会返回 `409` 并保留 rollout session。该校验默认关闭，保持本地开发和既有测试行为。
- `auth-http` 内部控制接口已可查询旧服 `game-server` 真实 rollout drain 状态：`GET /api/v1/internal/game-server/rollout-drain-status` 会转发 `GetRolloutDrainStatusReq/Res`，返回 `drain_mode_enabled`、`drain_mode_entered_at_ms`、`drain_mode_reason`、`drain_mode_source`、`connection_count`、`owned_room_count`、`migrating_room_count`、`retired_room_count`、`transferable_empty_room_count`、`RoomRouteStatus` 样本与可接管空房样本；`tools/mock-client` 可用 `rollout-drain-status` 场景打印这些字段。`game-proxy` 的旧服真实状态校验会透传 `retiredRoomCount` / `retired_room_count` 作为观测字段，但通过条件仍只看 `ok`、`ownedRoomCount`、`migratingRoomCount` 和 `connectionCount`。
- `game-server` 已通过已鉴权 admin/internal 通道提供 `RequestServerShutdownReq/Res` 受控 graceful shutdown 入口，并由 `auth-http` 暴露为 `POST /api/v1/internal/game-server/shutdown-if-drained`；入口会再次校验旧服 `drain_mode_enabled`、`connection_count == 0`、`owned_room_count == 0`、`migrating_room_count == 0`，通过后触发 game-server 自身 graceful shutdown 信号，`retired_room_count` 只作为观测字段。`tools/mock-client` 可用 `request-server-shutdown` 场景人工演练该入口。
- `game-server` 已支持通过已鉴权 admin/internal 通道触发 `ServerRedirectPush`；push 成功进入目标连接出站队列后，旧服会以 `server_redirect_reconnect_required` 主动请求关闭旧连接。mock-client 已能认证进房后监听该 push，也已有 `server-redirect-reconnect` 场景用于收到 push 后主动断线、连接目标入口、重新 `AuthReq` 并优先 `RoomReconnectReq`。
- `FreezeRoomForTransfer` / `ExportRoomTransfer` / `ImportRoomTransfer` / `ConfirmRoomOwnership` / `RetireTransferredRoom` 已在 `game-server` 已鉴权 internal/admin 通道形成最小闭环，并已有显式编排入口。
- `scripts/rollout-three-process-drill.ps1` 已提供 old/new/proxy 第一阶段演练入口。默认 dry-run，只做工具检查、端口探测和步骤命令输出；显式 `-ExecuteSteps` 才调用已运行服务的 rollout start、old drain、transfer、drain status 和 complete-if-drained，旧服 shutdown 请求还需要额外 `-AllowShutdownRequest`。2026-06-13 已在真实 old/new/proxy/auth 环境中人工执行 `movement_demo` 空房迁移控制面并通过，覆盖 freeze/export、import/confirm、route upsert、retire 和 `complete-if-drained`。

仍未完整落地：

- 真实 old/new/proxy 多进程联调已完成一轮人工空房迁移控制面验收，但尚未纳入自动测试准入；mybevy 适配、真实 route metadata 丢失恢复演练和生产部署平台 stop hook 接入仍未完成。`complete-if-drained` 已具备可选旧服真实状态联动，`game-server` 已有受控 graceful shutdown 安全闸入口，mock-client / 三进程演练脚本已能等待指定旧服 PID 退出；完整生产部署编排还需要平台侧实例管理、权限和审计接入。
- proxy 不做同一连接内换 upstream。
- proxy 不保存玩法状态，不做 room transfer payload 权威存储。
- 当前 Redis route store 仍不是完整多 proxy 强一致方案；它已有单 key 快照级 revision/CAS 和 pub/sub 本地缓存失效第一阶段能力，但仍缺统一控制面 owner、真实 Redis 多 proxy 压测和更细粒度冲突合并。

## 3. 职责边界

### 3.1 auth-http

`auth-http` 负责：

- 登录。
- session、access token、game ticket。
- 维护模式下拦截普通玩家登录和新 game ticket 签发。
- 账号安全、限流和审计。
- 下发客户端连接所需的 proxy 地址、资源列表和版本信息。

客户端资源热更新、强更判断和资源清单不属于 `game-proxy`。

### 3.2 game-proxy

`game-proxy` 负责：

- 客户端游戏接入入口。
- ticket 的最小接入鉴权。
- 维护模式下拒绝新 `AuthReq` 接入。
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
4. `game-proxy` 先检查本进程维护开关和 Redis 共享维护模式状态；维护开启时返回 `AuthRes(ok=false, error_code=MAINTENANCE_MODE)`。
5. `game-proxy` 校验 ticket 签名、过期时间和 Redis ticket 记录。
6. `game-proxy` 返回 `AuthRes`。
7. 如果鉴权失败，连接仍保持未认证；后续非 `AuthReq` / `PingReq` 消息会被本地拒绝，不会选择 upstream。
8. 鉴权成功后，客户端发起首个业务请求，如 `RoomJoinReq`。
9. `game-proxy` 根据请求类型选择 upstream。
10. `game-proxy` 建立到 `game-server` 的 local socket 连接。
11. `game-proxy` replay 原始 `AuthReq` 到上游。
12. `game-server` 鉴权成功后，proxy 转发首个业务请求和后续双向流量。

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

后续目标态允许设计同连接 upstream swap，但前提是 proxy 从透明 `copy_bidirectional` 演进为 L7 session relay，具备暂停 old upstream、冻结 room、重放鉴权/重连或 resume、输入缓冲/排序/去重以及失败回滚能力。该目标态不改变当前实现事实，也不改变第一阶段先走 redirect/reconnect 的结论。

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

启用 Redis route store 时，rollout session 会随 route store 快照一起以 serde JSON 保存，proxy 重启后先恢复该 session，再继续同步静态或注册中心 upstream。

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

### 6.5 Route Store 持久化

`ProxyRouteStore` 当前支持两种 backend：

- `memory`：默认值，所有 rollout session、room route、player route 仅保存在 proxy 进程内，适合本地开发和不启 Redis 的单机调试。
- `redis`：启动时从 Redis 加载已有 route store 快照，后续在 begin/end rollout、rollout state 更新、room route/player route upsert、成功 join/reconnect/observer 后的 `bind_room_owner` 中以乐观并发控制写回快照。

Redis backend 使用结构化 serde JSON 保存一个快照 key，当前 key 为：

```text
{PROXY_ROUTE_STORE_KEY_PREFIX}proxy:route-store:state
```

Redis backend 还使用一个 pub/sub 更新频道：

```text
{PROXY_ROUTE_STORE_KEY_PREFIX}proxy:route-store:updates
```

快照字段包含 `store_revision`、`rollout_session`、`room_routes`、`player_routes`。旧 JSON 没有 `store_revision` 时按 `0` 兼容加载。Redis backend 保存时使用 Lua compare-and-set：只有 Redis 当前快照 revision 等于本地 expected revision 时才写入，成功后 `store_revision + 1` 并发布包含 `store_revision` 的 JSON 通知；其它 proxy 订阅更新频道，只有收到的 revision 比本地更新时才 reload Redis 快照，旧消息或自身发布导致的同 revision 消息会被忽略。冲突时会重新加载 Redis 最新快照。admin 写入路径会返回明确错误码（如 `ROUTE_STORE_REVISION_CONFLICT`），玩家 join/reconnect/observer 触发的 `bind_room_owner` 元数据更新只记录 warning 并 reload，不中断玩家链路。

配置优先级：

- `PROXY_ROUTE_STORE_BACKEND=memory|redis`，默认 `memory`。
- `PROXY_ROUTE_STORE_REDIS_URL` 优先；未设置时依次复用 `REGISTRY_URL`、`REDIS_URL`，最后默认 `redis://127.0.0.1:6379`。
- `PROXY_ROUTE_STORE_KEY_PREFIX` 优先；未设置时复用 `REDIS_KEY_PREFIX`，最后为空。

生产建议启用 `PROXY_ROUTE_STORE_BACKEND=redis`，并为不同环境配置独立 key prefix。显式选择 Redis 后，启动加载失败会让 `game-proxy` 启动失败，避免生产静默退回内存态。

当前持久化解决的是单 proxy 重启恢复 rollout session、room route、player route 的最低风险，并提供单 key 快照级 CAS 和 pub/sub reload 来避免直接最后写覆盖、缩短跨 proxy 缓存失效窗口。它不持久化 upstream health/operation state，也不提供统一控制面 owner 或冲突合并；多 proxy 生产场景仍需要补控制面仲裁、锁/owner 策略和真实 Redis 集成压测。

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

鉴权前只有 `AuthReq` 和 `PingReq` 会被处理；`RoomJoinReq`、`RoomReconnectReq`、业务包、admin/GM 消息或未知 `msgType` 都会在 proxy 本地返回 `ErrorRes(PREAUTH_MESSAGE_NOT_ALLOWED)`。`AuthReq` 失败不会提升连接状态，不能通过后续业务包触发上游绑定。鉴权成功后，proxy 才允许进入上游选择和 auth replay 流程。

绑定上游后，proxy 不继续解析玩法消息。

## 8. Admin 接口

当前 `game-proxy` admin 口是轻量 HTTP，默认监听 `PROXY_ADMIN_HOST:PROXY_ADMIN_PORT`。所有 admin 请求都需要 admin token 鉴权；`PROXY_ADMIN_TOKEN` 是全权限写 token，兼容所有 GET/POST；可选 `PROXY_ADMIN_READ_TOKEN` 是只读 token，仅允许 GET；`PROXY_ADMIN_SCOPED_TOKENS` 可配置额外受限 token。当前兼容两种 header 形式：

- `Authorization: Bearer <token>`
- `X-Admin-Token: <token>`

URL query 中不支持传 token，避免 token 进入访问日志。开发环境未设置时会使用 `dev-only-change-this-proxy-admin-token`；`NODE_ENV=production` 或 `APP_ENV=production` 时，`PROXY_ADMIN_TOKEN` 为空或仍为明显默认值会导致配置加载失败。生产环境如果设置 `PROXY_ADMIN_READ_TOKEN`，也必须是非空、非明显默认值，且不能与 `PROXY_ADMIN_TOKEN` 相同。

`PROXY_ADMIN_SCOPED_TOKENS` 使用分号分隔 token，冒号后用逗号列权限，例如：

```env
PROXY_ADMIN_SCOPED_TOKENS=maint-token:proxy.maintenance.write;rollout-token:proxy.rollout.write;route-token:proxy.route.write,proxy.read
```

当前权限含义：

| 权限 | 允许操作 |
|------|----------|
| `proxy.read` | `GET /status`、`/instances`、`/rollout`、`/room-routes`、`/player-routes` |
| `proxy.maintenance.write` | `POST /maintenance/on`、`/maintenance/off` |
| `proxy.rollout.write` | `POST /rollout/start`、`/rollout/end`、`/rollout/state`、`/rollout/complete-if-drained` |
| `proxy.route.write` | `POST /room-route/upsert`、`/player-route/upsert`、`/switch/<server_id>` |
| `proxy.write` | 已知和未知 POST 写路径的通用写权限；不包含只读 GET |
| `*` | 全部读写权限 |

scoped token 配置会拒绝空 token、明显默认 token、重复 token、未知权限和空权限；生产环境下还会拒绝明显弱 token。scoped token 不会写入日志、响应或审计事件。

已实现接口：

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/status` | 活跃前端连接数、维护状态、active upstream、rollout、route 数量 |
| `GET` | `/instances` | upstream 列表 |
| `GET` | `/rollout` | 当前 rollout session 和 route store 排空评估摘要 |
| `GET` | `/room-routes` | room route 列表 |
| `GET` | `/player-routes` | player route 列表 |
| `POST` | `/maintenance/on` | 开启本进程维护模式，拒绝新 `AuthReq` |
| `POST` | `/maintenance/off` | 关闭维护模式 |
| `POST` | `/rollout/start?rollout_epoch=...&old_server_id=...&new_server_id=...` | 开始 rollout |
| `POST` | `/rollout/end` | 在 route store 已排空时结束 rollout 并清理相关 route；仍有 old owner / 迁移中 room route 或指向 old 的 player route 时返回 `409 ROLLOUT_NOT_DRAINED` |
| `POST` | `/rollout/state?state=Active|Ending|Interrupted` | 更新 rollout 状态 |
| `POST` | `/rollout/complete-if-drained` | 自动收尾：若当前 epoch 内仍有 old owner / 迁移中 room route 或指向 old 的 player route，返回 `409` 和阻塞计数/示例；若已排空则结束 rollout 并返回清理摘要 |
| `POST` | `/room-route/upsert?...` | 手动 upsert room route；校验必填字段、迁移状态枚举、成员数、版本号、checksum 长度和 upstream 存在性 |
| `POST` | `/player-route/upsert?...` | 手动 upsert player route；校验 player/room/server id、upstream 存在性和 rollout epoch |
| `POST` | `/switch/<server_id>` | 将目标 upstream 置为 active，其余置为 draining |

当前 admin 修改接口会记录结构化日志审计，包含 `action`、操作人、关键目标（`server_id` / `room_id` / `player_id` / `rollout_epoch`）和 `result=ok|error`，不会记录 token。权限不足返回 `403 insufficient admin permission`；写操作权限拒绝会尽量写入 JSONL 审计，`error=insufficient_permission`，审计写入失败时至少记录 structured warn。启用 Redis route store 时，admin 写入会同步更新 route store 快照；持久审计第一阶段采用本地 JSONL 文件，尚未接入 PostgreSQL 审计查询或集中留存。

当前 admin 写接口同时支持轻量 JSONL 持久审计，默认开启：

- 操作人来自 `X-Admin-Actor` header，允许字母、数字、`-`、`_`、`.`、`@`，最大 128 字节。
- 未提供或格式非法时记录 `actor=unknown` 且 `actor_missing=true`；配置 `PROXY_ADMIN_AUDIT_REQUIRE_ACTOR=true` 后，缺失 actor 的写操作会返回 `400 missing X-Admin-Actor`。
- JSONL 文件路径由 `PROXY_ADMIN_AUDIT_PATH` 控制，默认 `logs/game-proxy/admin-audit.jsonl`；每行包含 `ts_ms`、`actor`、`actor_missing`、`method`、`path`、`action`、`result`、`error`、`server_id`、`room_id`、`player_id`、`rollout_epoch`。
- 审计文件创建或追加失败时，admin 写操作按安全优先返回 `500` 并记录 warning，不静默放行。
- 该审计是控制面补充，不是公网暴露依据；生产仍必须把 `PROXY_ADMIN_HOST:PROXY_ADMIN_PORT` 放在内网、VPN、堡垒机或安全组边界内。

仍未完成的生产化能力：

- 操作级 RBAC 仍是第一阶段 scoped token 模型，尚未数据库化，也没有集中策略、审批流、权限变更审计查询或按资源范围授权。
- 审计查询、集中留存和统一 trace/request id。
- 多 proxy 部署下 route store 已有 Redis pub/sub 本地缓存失效第一阶段能力；仍缺统一控制面 owner、真实 Redis 集成压测，以及必要的锁/冲突合并策略。
- 更完整的 HTTP parser、TLS 和管理网段访问控制，这些仍建议由部署侧限制。

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
- proxy 已能基于当前 rollout route store 判断是否可自动结束 rollout。当前判断范围是 proxy 已知的 room route / player route：当前 epoch 内 `owner_server_id == old_server_id`、迁移状态仍在 old/transfer 中，或 player route 仍指向 old server 时会阻止结束；已切到 `new_server_id` 的 route 不阻止结束。带其它 epoch 的陈旧 route 不阻止当前 rollout 自动收尾，结束时只清理当前 epoch 和空 epoch route；这些陈旧记录仍需要后续 TTL/巡检或控制面清理策略处理。
- `tools/mock-client` 提供显式 room transfer 编排入口，可在 new import 成功后调用 proxy admin `/room-route/upsert` 将 room route 切到 `OwnedByNew`，并带上 `rollout_epoch`、`last_transfer_checksum`、`room_version` 和 CAS 参数。

仍未闭环的目标行为：

- 自动收尾可选读取旧服真实 `connection_count` / drain status；启用 `PROXY_ROLLOUT_DRAIN_STATUS_CHECK_ENABLED=true` 后，proxy route store 排空只是第一道条件，旧服真实 `connectionCount`、`ownedRoomCount`、`migratingRoomCount` 也必须为 `0` 才会结束 rollout。旧服状态同时提供 `transferableEmptyRoomCount` 和样本，供控制面优先选择仍为 `Owned` 且在线成员数为 `0` 的 room 做 freeze/export。game-server 自身已有 `RequestServerShutdownReq/Res` 受控停服安全闸，本地编排脚本已能在安全闸通过后验证指定 PID 退出；生产停服前仍需要控制面 owner、权限、审计和部署平台 stop hook 确认。
- old server 可通过控制面主动下发 `ServerRedirectPush`，push 只发给当前 old server 上目标 room 的在线成员；push 成功排队后旧连接会由 old server 主动关闭，排队失败不覆盖已有关闭原因。
- mybevy 等真实客户端收到 redirect 后断线重连到 push 中的 proxy 目标地址。
- mock-client 重连后通过 proxy 进入 new owner 的端到端联调已完成；外部 mybevy 客户端仍未验收。

显式编排入口当前仍是第一阶段 redirect/reconnect 的前置控制流，不是同连接迁移。它只调用已鉴权 `game-server` admin TCP 包协议和 `game-proxy` admin HTTP；proxy 仍保持透明转发模型，不实现 L7 relay 或同连接 upstream swap。`tools/mock-client` 已有主动断线重连验证场景，旧服也会在 redirect push 成功排队后主动关闭旧连接；该链路已通过真实服务人工验收，但尚未成为自动测试准入，也不代表 mybevy 已适配。

## 10. 配置项

常用环境变量：

| 变量 | 说明 | 默认 |
|------|------|------|
| `PROXY_HOST` | KCP 监听 host | `127.0.0.1` |
| `PROXY_PORT` | KCP 监听端口 | `4000` |
| `PROXY_ADMIN_HOST` | admin 监听 host | 同 `PROXY_HOST` |
| `PROXY_ADMIN_PORT` | admin 监听端口 | `7101` |
| `PROXY_ADMIN_TOKEN` | admin HTTP 口鉴权 token；支持 Bearer 和 `X-Admin-Token` header；生产环境禁止空值或默认值 | 开发默认值 |
| `PROXY_ADMIN_READ_TOKEN` | 可选 admin 只读 token；仅允许 GET；支持 Bearer 和 `X-Admin-Token` header；生产环境设置时禁止空值、默认值或与写 token 相同 | 未设置 |
| `PROXY_ADMIN_SCOPED_TOKENS` | 可选 admin scoped token；格式 `token:permission1,permission2;token2:permission3`，支持 `proxy.read`、`proxy.maintenance.write`、`proxy.rollout.write`、`proxy.route.write`、`proxy.write`、`*` | 未设置 |
| `PROXY_ADMIN_AUDIT_ENABLED` | 是否启用 admin 写操作 JSONL 持久审计 | `true` |
| `PROXY_ADMIN_AUDIT_PATH` | admin 写操作 JSONL 审计文件路径 | `logs/game-proxy/admin-audit.jsonl` |
| `PROXY_ADMIN_AUDIT_REQUIRE_ACTOR` | 是否要求 admin 写操作携带合法 `X-Admin-Actor` header | `false` |
| `PROXY_TCP_FALLBACK_HOST` | TCP fallback host | 同 `PROXY_HOST` |
| `PROXY_TCP_FALLBACK_PORT` | TCP fallback 端口 | `PROXY_PORT + 10000` |
| `UPSTREAM_SERVER_ID` | 静态上游 server id | `game-server-1` |
| `UPSTREAM_LOCAL_SOCKET_NAME` | 静态上游 local socket | `myserver-game-server.sock` |
| `REGISTRY_ENABLED` | 是否启用服务发现 | `false` |
| `REGISTRY_URL` / `REDIS_URL` | 服务发现 Redis 地址；route store 未单独配置 URL 时也会按此顺序复用 | `redis://127.0.0.1:6379` |
| `REGISTRY_DISCOVER_INTERVAL_SECS` | 服务发现刷新间隔 | `5` |
| `UPSTREAM_SERVICE_NAME` | 要发现的服务名 | `game-server` |
| `TICKET_SECRET` | ticket HMAC secret | dev 默认值 |
| `REDIS_KEY_PREFIX` | Redis key 前缀 | 空 |
| `PROXY_ROUTE_STORE_BACKEND` | route store backend，`memory` 为本地默认，生产建议 `redis` | `memory` |
| `PROXY_ROUTE_STORE_REDIS_URL` | route store Redis 地址；未设置时依次复用 `REGISTRY_URL`、`REDIS_URL` | `redis://127.0.0.1:6379` |
| `PROXY_ROUTE_STORE_KEY_PREFIX` | route store Redis key 前缀；未设置时复用 `REDIS_KEY_PREFIX` | 空 |
| `PROXY_ROLLOUT_DRAIN_STATUS_CHECK_ENABLED` | `POST /rollout/complete-if-drained` 是否在 route store 判定排空后继续校验旧服真实 drain status | `false` |
| `PROXY_ROLLOUT_DRAIN_STATUS_URL` | 旧服真实 drain status 查询 URL，建议指向 `auth-http` 内部接口 | `http://127.0.0.1:3000/api/v1/internal/game-server/rollout-drain-status` |
| `PROXY_ROLLOUT_DRAIN_STATUS_TOKEN` | 调用旧服真实 drain status URL 时发送的 `X-Service-Token`；为空则不发送 | 空 |
| `PROXY_ROLLOUT_DRAIN_STATUS_CONNECT_TIMEOUT_MS` | 旧服真实 drain status HTTP 连接超时 | `3000` |
| `PROXY_ROLLOUT_DRAIN_STATUS_READ_TIMEOUT_MS` | 旧服真实 drain status HTTP 读取超时 | `3000` |
| `PROXY_ROLLOUT_DRAIN_STATUS_OVERALL_TIMEOUT_MS` | 旧服真实 drain status 单次请求总超时 | `3000` |
| `PROXY_ROLLOUT_DRAIN_STATUS_MAX_BODY_BYTES` | 旧服真实 drain status 最大响应体大小 | `1048576` |
| `PROXY_MAX_CONNECTIONS` | 总前端连接上限，`0` 表示不限制 | `0` |
| `PROXY_MAX_PREAUTH_FAILURES` | 同一连接鉴权成功前允许的非法消息或鉴权失败次数，`0` 表示不按次数断开 | `3` |
| `PROXY_IP_DENYLIST` | 静态来源 IP denylist，逗号分隔，支持精确 IP 和 CIDR；为空表示不启用 | 空 |
| `PROXY_MAX_CONNECTIONS_PER_IP` | 单来源 IP 本地并发连接上限，`0` 表示不限制 | `0` |
| `PROXY_MAX_CONNECTIONS_PER_PLAYER` | 单玩家本地已鉴权并发连接上限，`0` 表示不限制 | `0` |
| `LOG_LEVEL` / `LOG_ENABLE_CONSOLE` / `LOG_ENABLE_FILE` / `LOG_DIR` | 日志配置 | 见 `.env.example` |

当前连接治理只作用于单个 `game-proxy` 进程内，不提供跨 proxy 的全局 IP / 玩家连接限额；多 proxy 生产部署仍需要网关层策略或 Redis 分布式计数 / 动态封禁能力。

## 11. 后续重点

短期建议优先补：

1. proxy admin scoped token RBAC 的集中策略、审批流、审计查询、集中留存和统一 trace/request id。
2. route store 多 proxy 一致性：在已有 Redis 单 key CAS + pub/sub reload 基础上补统一控制面 owner、真实 Redis 集成压测和必要的锁/冲突合并策略。
3. 跨 proxy 全局单 IP / 单玩家连接限额、消息频率限制和 Redis 动态黑名单。
4. 自动 rollout 结束检测已落地到 proxy route store 维度；旧服真实 drain/connection 状态已可通过 `auth-http` 内部接口和 mock-client 查询，game-server 受控 graceful shutdown 安全闸已可调用，本地脚本已能验证指定旧服 PID 退出；下一步补统一控制面 owner、生产部署平台 stop hook 接入和审计固化。
5. old server `ServerRedirectPush` 下发与客户端重连链路。
6. room transfer 编排入口的多进程联调自动测试准入和操作审计固化。
7. 多 proxy 场景下的 route 一致性、健康判定和自动收尾。

跨服状态迁移的完整一致性要求见 [空房接管式灰度规范](./空房接管式灰度规范.md)。
