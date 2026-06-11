# 限流与安全现状

本文对齐当前代码中的限流、安全校验与相关配置项，重点说明“已经实现的能力”和“仍属于设计目标的部分”，避免把旧配置名当成现网配置。

---

## 1. 总览

当前安全与限流相关服务的状态如下：

- `auth-http`：已经实现 IP 限流、Redis 动态 IP / 玩家黑名单、账号锁定、ticket 签发/撤销、维护模式入口拦截，以及安全审计写库；配置项以 `apps/auth-http/src/config.js` 为准
- `game-proxy`：当前已实现 `AuthReq` 本地 ticket 校验、鉴权前消息白名单、单连接预鉴权失败阈值、单连接入站消息频率限制、总前端连接上限、静态 IP denylist、Redis 动态 IP / 玩家黑名单、单 IP / 单玩家本地连接上限、接入转发、活跃前端连接数观测、本地开关 + Redis 共享状态的维护模式拦截与上游发现；文档里旧的 KCP 令牌桶限流配置项目前并不存在
- `game-server`：当前已实现 ticket 校验、鉴权前消息白名单、心跳超时、包体长度限制、单连接消息频率限制和本实例内单玩家消息频率限制；频率限制默认关闭，分别按 `MSG_RATE_WINDOW_MS` / `MSG_RATE_MAX` 与 `PLAYER_MSG_RATE_WINDOW_MS` / `PLAYER_MSG_RATE_MAX` 启用；生产环境拒绝默认或空的 ticket/admin/internal token
- `chat-server`：当前已实现首包鉴权、ticket 签名与过期校验、Redis ticket 归属校验、ticket version 校验、心跳超时、包体长度限制、单连接消息频率限制、单 IP / 单账号本实例连接数限制、有界出站写队列，以及生产环境拒绝默认或空的 `TICKET_SECRET`

---

## 2. auth-http（登录服）

### 2.1 已实现能力

- IP 限流：Redis 滑动窗口，命中后返回 `429`
- Redis 动态黑名单：`AUTH_REDIS_BLOCKLIST_ENABLED=true` 时，登录入口早期检查 `${REDIS_KEY_PREFIX}security:blocklist:ip:<ip>`，登录成功且拿到 `playerId` 后、创建 session / ticket 前检查 `${REDIS_KEY_PREFIX}security:blocklist:player:<player_id>`；`/api/v1/game-ticket/issue` 在签发前再次检查玩家黑名单
- 账号锁定：连续密码登录失败后锁定账号
- ticket 签发：使用 `TICKET_SECRET` 生成 HMAC 签名 ticket，并写入 Redis
- ticket 撤销：`/api/v1/game-ticket/revoke` 会删除 Redis 中对应 ticket
- 维护模式：读取 `${REDIS_KEY_PREFIX}maintenance:global`，开启时普通玩家登录和 `/api/v1/game-ticket/issue` 返回 `MAINTENANCE_MODE`
- 安全审计：登录失败、账号锁定、IP 限流、ticket 撤销等事件会写入 `security_audit_logs`（前提是启用了 MySQL 存储）
- 内部接口 token：配置 `INTERNAL_API_TOKEN` 后，`/api/v1/internal/game-server/status` 与 `/api/v1/internal/game-server/config` 要求 `X-Service-Token`
- 生产配置保护：`NODE_ENV=production` 或 `APP_ENV=production` 时，配置加载阶段拒绝默认或空的 `TICKET_SECRET`、默认或空的 `GAME_ADMIN_TOKEN`、空的 `INTERNAL_API_TOKEN`

### 2.2 当前实际配置项

以下配置名来自 `apps/auth-http/src/config.js` 与 `apps/auth-http/.env.example`：

```env
# Rate Limiting
RATELIMIT_ENABLED=true
RATELIMIT_WINDOW_MS=60000
RATELIMIT_MAX=60

# Account Lockout
ACCOUNT_LOCK_ENABLED=true
ACCOUNT_LOCK_MAX_ATTEMPTS=5
ACCOUNT_LOCK_WINDOW_SECONDS=900
ACCOUNT_LOCK_TTL_SECONDS=900

# Ticket
TICKET_SECRET=replace-with-a-long-random-string
TICKET_TTL_SECONDS=900
TICKET_VALIDATE_ENABLED=true

# Redis Dynamic Blocklist
AUTH_REDIS_BLOCKLIST_ENABLED=false
AUTH_REDIS_BLOCKLIST_CACHE_TTL_MS=2000

# Security Audit
SECURITY_AUDIT_ENABLED=true

# Internal API
INTERNAL_API_TOKEN=
AUTH_STRICT_SECURITY=false
```

### 2.3 与旧文档的关键差异

- 旧文档中的 `RATELIMIT_IP_WINDOW` / `RATELIMIT_IP_MAX` 并不存在，实际配置名是 `RATELIMIT_WINDOW_MS` / `RATELIMIT_MAX`
- 旧文档中的 `ACCOUNT_LOCK_MAX` / `ACCOUNT_LOCK_TTL` 并不存在，实际配置名是 `ACCOUNT_LOCK_MAX_ATTEMPTS` / `ACCOUNT_LOCK_TTL_SECONDS`
- `ACCOUNT_LOCK_WINDOW_SECONDS` 与 `ACCOUNT_LOCK_TTL_SECONDS` 是两个不同概念：
  - 前者用于统计失败次数窗口
  - 后者用于真正的锁定时长

### 2.4 当前实现备注

- 当前默认值是 `TICKET_TTL_SECONDS=900`，即 15 分钟；过期后 `game-proxy`、`game-server` 与 `chat-server` 都会拒绝
- `AUTH_REDIS_BLOCKLIST_ENABLED=false` 默认完全不查 Redis 动态黑名单；启用后 Redis 查询失败会按 fail-closed 返回 `503 BLOCKLIST_UNAVAILABLE`，避免登录入口在封禁状态不可用时继续放行
- auth-http 与 game-proxy 共用 Redis 动态黑名单 key：`${REDIS_KEY_PREFIX}security:blocklist:ip:<ip>` 和 `${REDIS_KEY_PREFIX}security:blocklist:player:<player_id>`；key 存在即封禁，JSON 值可用 `{"until":<unix_ms>}` 表示封禁过期时间，已过期则视为未封禁
- `AUTH_REDIS_BLOCKLIST_CACHE_TTL_MS=2000` 控制 auth-http 黑名单短缓存；当前只在登录入口 IP、登录后玩家和 game ticket issue 前查询，不对所有 HTTP 路由查询
- GM 限时封禁写入 `player_accounts.ban_expires_at`；`auth-http` 会在游客登录、密码登录和 game ticket issue 路径检查账号状态，过期封禁会惰性恢复为 `active`，未到期或永久封禁继续按 `ACCOUNT_DISABLED` 拒绝
- `TICKET_VALIDATE_ENABLED` 和 `SECURITY_AUDIT_ENABLED` 已进入配置结构，但当前代码没有完整用它们做开关控制：
  - ticket 校验实际发生在 `game-proxy`、`game-server` 与 `chat-server`
  - 安全审计当前由 `mysqlStore?.appendSecurityAudit?.(...)` 直接写库，未额外判断 `SECURITY_AUDIT_ENABLED`
- ticket 不是“使用后立即删除”的一次性票据；当前 `game-proxy`、`game-server` 与 `chat-server` 校验时都会检查签名和 Redis 中是否存在对应 ticket，成功认证后不会自动删除
- 当前同一张 ticket 会被 `game-proxy`、`game-server` 与 `chat-server` 复用；因此不能简单在首次校验成功后就删除 Redis 记录，否则会破坏多服务接入链路
- logout 和改密会递增 `${REDIS_KEY_PREFIX}player-ticket-version:<playerId>`，使该玩家已签发但未过期的旧 ticket 在后续鉴权时因版本不匹配失效；单张 ticket revoke 仍用于删除精确 `ticket:<sha256(ticket)>` 并保留审计路径
- 维护模式不拦截 logout、game ticket revoke 等清理操作，也不主动踢已有在线连接
- `AUTH_STRICT_SECURITY` 仍控制内部接口请求期缺少 `INTERNAL_API_TOKEN` 时是否返回 `INTERNAL_API_TOKEN_REQUIRED`；生产环境现在会提前在配置加载阶段拒绝空 `INTERNAL_API_TOKEN`

---

## 3. game-proxy（接入代理）

### 3.1 当前已实现能力

- KCP 前端监听
- TCP fallback 前端监听
- `AuthReq` 本地 ticket 校验：校验 HMAC 签名，并检查 Redis 中是否存在对应 ticket
- 鉴权通过后暂存认证包，绑定上游后向 `game-server` 重放 `AuthReq`
- 鉴权前消息白名单：未认证连接只允许 `AuthReq` 与 `PingReq`，其它消息返回 `ErrorRes(PREAUTH_MESSAGE_NOT_ALLOWED)`，不会触发上游选择或绑定
- 单连接预鉴权失败阈值：非法预鉴权消息或鉴权失败累计达到 `PROXY_MAX_PREAUTH_FAILURES` 后关闭连接
- 总前端连接上限：`PROXY_MAX_CONNECTIONS` 为正整数时，超过上限的新连接会被拒绝
- 静态 IP denylist：`PROXY_IP_DENYLIST` 命中的来源会在 session 建立初期拒绝，支持精确 IP 和 CIDR
- Redis 动态黑名单：`PROXY_REDIS_BLOCKLIST_ENABLED=true` 时，session 建立早期检查 `${REDIS_KEY_PREFIX}security:blocklist:ip:<ip>`，本地 ticket 校验成功后检查 `${REDIS_KEY_PREFIX}security:blocklist:player:<player_id>`；key 存在即封禁，JSON 值可用 `until` 表示过期时间
- 单 IP 本地连接上限：`PROXY_MAX_CONNECTIONS_PER_IP` 为正整数时限制同一来源 IP 在本 proxy 实例上的并发连接，连接关闭时释放
- 单玩家本地连接上限：`PROXY_MAX_CONNECTIONS_PER_PLAYER` 为正整数时，`AuthReq` 本地鉴权成功后登记已鉴权玩家连接；超过上限返回 `AuthRes(ok=false, error_code=PLAYER_CONNECTION_LIMIT_EXCEEDED)`，连接关闭或重复鉴权切换玩家时释放
- 动态上游发现或静态上游路由
- 活跃前端连接数统计与监控暴露，包含尚未完成 `AuthReq` 的预鉴权连接
- 维护模式：保留本进程 admin 开关，并在 `AuthReq` 阶段读取 `${REDIS_KEY_PREFIX}maintenance:global` 共享状态；任一开关开启都会返回 `AuthRes(ok=false, error_code=MAINTENANCE_MODE)`
- admin HTTP 口 token 鉴权，支持 `Authorization: Bearer <token>` 和 `X-Admin-Token: <token>`
- `NODE_ENV=production` 或 `APP_ENV=production` 时拒绝空的或明显默认的 `PROXY_ADMIN_TOKEN`
- admin 写接口基础输入校验与结构化日志审计，不记录 token
- 固定最大包体限制：`MAX_PROXY_BODY_LEN=1MiB`，当前不是环境变量

### 3.2 当前实际配置项

`game-proxy` 当前代码实际读取的是以下配置：

```env
PROXY_HOST=127.0.0.1
PROXY_PORT=4000
PROXY_ADMIN_HOST=127.0.0.1
PROXY_ADMIN_PORT=7101
PROXY_ADMIN_TOKEN=dev-only-change-this-proxy-admin-token
PROXY_TCP_FALLBACK_HOST=127.0.0.1
PROXY_TCP_FALLBACK_PORT=14000
PROXY_LOCAL_SOCKET_NAME=myserver-game-proxy.sock

REDIS_URL=redis://127.0.0.1:6379
REDIS_KEY_PREFIX=
TICKET_SECRET=replace-with-a-long-random-string
PROXY_MAX_CONNECTIONS=0
PROXY_MAX_PREAUTH_FAILURES=3
PROXY_MAINTENANCE_CACHE_TTL_MS=2000
PROXY_REDIS_BLOCKLIST_ENABLED=false
PROXY_REDIS_BLOCKLIST_CACHE_TTL_MS=2000
PROXY_MSG_RATE_WINDOW_MS=1000
PROXY_MSG_RATE_MAX=0
PROXY_IP_DENYLIST=
PROXY_MAX_CONNECTIONS_PER_IP=0
PROXY_MAX_CONNECTIONS_PER_PLAYER=0

REGISTRY_ENABLED=false
REGISTRY_URL=redis://127.0.0.1:6379
REGISTRY_DISCOVER_INTERVAL_SECS=5
UPSTREAM_SERVICE_NAME=game-server

# registry 关闭时的兼容配置
UPSTREAM_SERVER_ID=game-server-1
UPSTREAM_LOCAL_SOCKET_NAME=myserver-game-server.sock
```

### 3.3 当前实现备注

- 文档旧版中的以下配置项目前并不存在于 `apps/game-proxy/src/config.rs`：
  - `RATELIMIT_IP_RATE`
  - `RATELIMIT_IP_BURST`
  - `MAX_CONNECTIONS_PER_IP`
  - `MAX_CONNECTIONS_PER_ACCOUNT`
- 当前单 IP / 单玩家连接上限使用 `PROXY_MAX_CONNECTIONS_PER_IP` 和 `PROXY_MAX_CONNECTIONS_PER_PLAYER`，旧的无 `PROXY_` 前缀配置名不会被读取
- `PROXY_REDIS_BLOCKLIST_ENABLED=false` 默认完全不查 Redis 动态黑名单；启用后 Redis 查询失败会按 fail-closed 返回 `BLOCKLIST_UNAVAILABLE` 并拒绝新连接或 `AuthReq`
- Redis 动态黑名单使用短缓存，`PROXY_REDIS_BLOCKLIST_CACHE_TTL_MS` 默认 2 秒；只在连接建立和 `AuthReq` 本地 ticket 校验成功后查询，不对每个 packet 查询 Redis
- Redis 动态黑名单 key 为 `${REDIS_KEY_PREFIX}security:blocklist:ip:<ip>` 和 `${REDIS_KEY_PREFIX}security:blocklist:player:<player_id>`；值可以是任意非空字符串，也可以是 JSON `{"reason":"...","until":<unix_ms>}`，其中 `until` 小于当前时间时视为未封禁；部署侧仍推荐通过 Redis TTL 管理过期
- 当前 `game-proxy` 的静态 denylist 和连接数限制是单 proxy 进程内本地状态，不是 Redis 分布式全局限额；Redis 动态黑名单可跨 proxy 共享封禁状态，但不是连接数限额
- 当前 `game-proxy` admin HTTP 口已经有 token 鉴权和生产默认 token 拒绝；开发默认 token 只适合本地联调，生产必须改为高强度随机值并限制 admin 端口在内网
- 当前 proxy admin 修改接口会记录 action、关键目标和 ok/error 结果到结构化日志；尚未接入 MySQL 等持久审计库，也没有细粒度 RBAC
- 当前 `game-proxy` 已强制鉴权前消息白名单；AuthReq 失败后仍保持未认证，后续业务包只会返回 `PREAUTH_MESSAGE_NOT_ALLOWED`，不会被转发到 `game-server`
- `PROXY_MAX_CONNECTIONS=0` 表示不限制总前端连接数；配置为正整数时才启用拒绝新连接
- `PROXY_MAX_PREAUTH_FAILURES=0` 表示不按预鉴权失败次数断开；默认 `3` 会在同一连接累计三次非法预鉴权消息或鉴权失败后关闭连接
- `PROXY_MSG_RATE_WINDOW_MS=1000` 表示 proxy 单连接入站消息频率统计窗口；`PROXY_MSG_RATE_MAX=0` 默认关闭，配置为正整数时同一前端连接在窗口内超过阈值会收到 `ErrorRes(MSG_RATE_EXCEEDED)`，当前不断开连接且不累计 `PROXY_MAX_PREAUTH_FAILURES`
- proxy 单连接入站限流发生在读到完整 packet 后、进入 `AuthReq` 本地处理 / 预鉴权白名单 / 上游选择或转发前；已绑定上游后的客户端方向也按 packet 检查，超限包不会转发到 `game-server`
- `PROXY_IP_DENYLIST` 为空表示不启用；示例值如 `203.0.113.10,198.51.100.0/24`
- `PROXY_MAX_CONNECTIONS_PER_IP=0` 和 `PROXY_MAX_CONNECTIONS_PER_PLAYER=0` 默认不限制，避免破坏本地开发联调
- `PROXY_MAINTENANCE_CACHE_TTL_MS` 控制 proxy 读取 Redis 共享维护状态的短缓存，默认 2 秒，避免每个包都访问 Redis；维护状态只在新 `AuthReq` 阶段检查，不主动断开已认证连接
- `game-proxy` 校验成功后不会删除 Redis ticket，避免破坏后续 `game-server` 与 `chat-server` 的接入校验

---

## 4. game-server（游戏服）

### 4.1 当前已实现能力

- ticket 校验：校验 HMAC 签名，并检查 Redis 中是否存在对应 ticket
- 鉴权前消息白名单：未认证连接只允许 `AuthReq` 与 `PingReq`，其它业务消息在 dispatch 层返回 `ErrorRes(PREAUTH_MESSAGE_NOT_ALLOWED)`，不会进入房间、移动、背包等业务 handler
- 心跳超时：读取包头时使用 `heartbeat_timeout_secs`
- 最大包体限制：包体超过 `max_body_len` 时拒绝处理
- 单连接消息频率限制：读到完整 packet 后、业务 dispatch 前检查窗口计数；超过阈值返回 `ErrorRes(MSG_RATE_EXCEEDED)` 并记录连接审计事件，当前不断开连接
- 单玩家消息频率限制：连接级限流通过后，对已鉴权连接按 `player_id` 在当前 `game-server` 实例内统计窗口消息数；超过阈值返回 `ErrorRes(MSG_RATE_EXCEEDED)` 并记录连接审计事件，当前不断开连接
- 管理接口支持动态调整 `heartbeat_timeout_secs`、`max_body_len`、`msg_rate_window_ms`、`msg_rate_max`、`player_msg_rate_window_ms` 与 `player_msg_rate_max`
- 生产配置保护：`NODE_ENV=production` 或 `APP_ENV=production` 时，配置加载阶段拒绝默认或空的 `TICKET_SECRET`、`GAME_ADMIN_TOKEN`、`GAME_INTERNAL_TOKEN`

### 4.2 当前实际配置项

以下配置名来自 `apps/game-server/src/config.rs` 与 `apps/game-server/.env.example`：

```env
TICKET_SECRET=replace-with-a-long-random-string
GAME_ADMIN_TOKEN=dev-only-change-this-game-admin-token
GAME_INTERNAL_TOKEN=dev-only-change-this-game-internal-token
REDIS_KEY_PREFIX=
HEARTBEAT_TIMEOUT_SECS=30
MAX_BODY_LEN=4096
MSG_RATE_WINDOW_MS=1000
MSG_RATE_MAX=0
PLAYER_MSG_RATE_WINDOW_MS=1000
PLAYER_MSG_RATE_MAX=0
```

说明：

- `MSG_RATE_WINDOW_MS` 默认 `1000`，表示单连接频率统计窗口。
- `MSG_RATE_MAX` 默认 `0`，表示不限制；配置为正整数时，同一连接在窗口内超过该消息数会收到 `MSG_RATE_EXCEEDED`。
- `PLAYER_MSG_RATE_WINDOW_MS` 默认 `1000`，表示单玩家频率统计窗口。
- `PLAYER_MSG_RATE_MAX` 默认 `0`，表示不限制；配置为正整数时，同一玩家在当前 `game-server` 实例内的多连接合计消息数超过阈值会收到 `MSG_RATE_EXCEEDED`。
- admin TCP 的 `AdminUpdateConfigReq` 可通过 key/value 动态更新 `msg_rate_window_ms`、`msg_rate_max`、`player_msg_rate_window_ms` 与 `player_msg_rate_max`。
- 当前 `ServerStatusRes` 协议仍只回显 `max_body_len`、`heartbeat_timeout_secs` 等既有字段，尚未暴露消息频率限制配置。
- `TICKET_SECRET`、`GAME_ADMIN_TOKEN`、`GAME_INTERNAL_TOKEN` 的示例值只用于开发；生产环境必须替换为非默认值。该 fail-fast 保护的是 `game-server` 内网服务凭证边界，不表示 `game-server` 要作为生产公网入口暴露。

### 4.3 与旧文档的关键差异

- 旧文档中的 `HEARTBEAT_TIMEOUT` 并不存在，实际配置名是 `HEARTBEAT_TIMEOUT_SECS`
- 旧文档中的 `MSG_RATE_WINDOW` 并不存在，实际配置名是 `MSG_RATE_WINDOW_MS`；`MSG_RATE_MAX` 当前已读取，默认 `0` 关闭
- 当前“操作冷却”不是通过独立的通用风控配置实现的；代码里没有这一组统一环境变量

### 4.4 当前实现备注

- `game-server` 在认证阶段会验证 ticket 签名和 Redis 中的 ticket 所有权；这是 ticket 校验的核心落点之一，`auth-http` 只负责签发、存储与撤销
- `game-server` 校验 ticket 时依赖 `TICKET_SECRET` 和 `REDIS_KEY_PREFIX`；这两个值需要与 `auth-http` 的签发侧保持一致
- `game-server` 的 `GAME_ADMIN_TOKEN` 与 `GAME_INTERNAL_TOKEN` 用于内部管理/服务通道；生产拒绝默认值或空值，但仍应依赖私网、allowlist、TLS/mTLS 或同机 socket 等部署侧隔离
- ticket 校验成功后当前不会自动删除 Redis 中的 ticket，因此并非严格的一次性消费模型
- 这种设计和当前“同一 ticket 供 `game-proxy` / `game-server` / `chat-server` 复用”的接入方式是一致的；如果后续要降低重放风险，更适合考虑缩短 TTL、增加用途隔离或引入换票流程
- 心跳、包体长度、鉴权前白名单、单连接消息频率限制和本实例单玩家消息频率限制属于当前已经生效的安全边界；单 IP 频率限制、跨实例全局玩家频率限制、异常解析失败率阈值、时间戳窗口和反重放仍属于设计目标
- 由于生产链路中 `game-server` 通常通过 `game-proxy` 本地 socket 接入，`game-server` 侧未必能拿到真实客户端 IP；单 IP 频率限制仍应优先在 proxy、网关或后续透传协议层处理

---

## 5. chat-server（聊天服）

### 5.1 当前已实现能力

- 首包强制鉴权：连接建立后第一包必须是 `ChatAuthReq`
- ticket 校验：校验 HMAC 签名与过期时间，读取 `${REDIS_KEY_PREFIX}ticket:<sha256(ticket)>` 并要求 value 等于 ticket payload 中的 `playerId`
- ticket version 校验：读取 `${REDIS_KEY_PREFIX}player-ticket-version:<playerId>`，用于感知 logout / 改密等玩家级失效
- 心跳超时：首包读取和会话循环使用 `HEARTBEAT_TIMEOUT_SECS`
- 最大包体限制：包体超过 `MAX_BODY_LEN` 时拒绝处理
- 单连接消息频率限制：认证后读到完整 packet 并解析出 `MessageType` 后、业务 dispatch 前按连接本地窗口计数；`CHAT_MSG_RATE_MAX=0` 默认关闭，超限返回 `ErrorRes(MSG_RATE_EXCEEDED)`，当前不断开连接
- 单 IP / 单账号本地连接上限：`ChatAuthReq` ticket 校验通过后、注册 session 和 online route 前检查；超过上限返回 `ChatAuthRes(ok=false)` 并关闭新连接
- 有界出站写队列：每连接出站消息队列使用 `CHAT_OUTBOUND_QUEUE_CAPACITY` 限制容量，队列满时当前连接响应返回错误，其它玩家推送和邮件通知记录日志后跳过
- 在线推送与邮件通知订阅：依赖 Core NATS 与内存会话表

### 5.2 当前实际配置项

以下配置名来自 `apps/chat-server/src/main.rs` 与 `apps/chat-server/.env.example`：

```env
CHAT_BIND_ADDR=0.0.0.0:9001
HEARTBEAT_TIMEOUT_SECS=30
MAX_BODY_LEN=4096
CHAT_MSG_RATE_WINDOW_MS=1000
CHAT_MSG_RATE_MAX=0
CHAT_MAX_CONNECTIONS_PER_PLAYER=0
CHAT_MAX_CONNECTIONS_PER_IP=0
CHAT_OUTBOUND_QUEUE_CAPACITY=1024
TICKET_SECRET=replace-with-a-long-random-string
REDIS_URL=redis://127.0.0.1:6379
REDIS_KEY_PREFIX=
MYSQL_URL=mysql://root:password@localhost:3306/chat
MYSQL_POOL_SIZE=5
NATS_URL=nats://127.0.0.1:4222

REGISTRY_ENABLED=false
REGISTRY_URL=redis://127.0.0.1:6379
REGISTRY_HEARTBEAT_INTERVAL=10
SERVICE_NAME=chat-server
SERVICE_INSTANCE_ID=chat-server-001
CHAT_PUBLIC_HOST=127.0.0.1
CHAT_ONLINE_ROUTE_TTL_SECS=60

LOG_LEVEL=info
LOG_ENABLE_CONSOLE=true
LOG_ENABLE_FILE=false
LOG_DIR=logs
```

### 5.3 当前实现备注

- `chat-server` 已读取 `REDIS_KEY_PREFIX`，并同时用于 `ticket:<sha256(ticket)>` 和 `player-ticket-version:<playerId>` 两类 key
- `CHAT_OUTBOUND_QUEUE_CAPACITY` 默认 `1024`；未配置、解析失败或配置为 `0` 时使用默认值
- `CHAT_MSG_RATE_WINDOW_MS` 默认 `1000`，`CHAT_MSG_RATE_MAX` 默认 `0` 表示关闭限制；生产部署可显式配置为非零值
- `CHAT_MAX_CONNECTIONS_PER_PLAYER` 和 `CHAT_MAX_CONNECTIONS_PER_IP` 默认 `0` 表示关闭；配置为正整数时，仅限制当前 `chat-server` 实例内已通过 ticket 校验的连接。单账号超限返回 `PLAYER_CONNECTION_LIMIT_EXCEEDED`，单 IP 超限返回 `IP_CONNECTION_LIMIT_EXCEEDED`
- `NODE_ENV=production` 或 `APP_ENV=production` 时，`chat-server` 会在配置加载阶段拒绝空值、开发默认值或 `.env.example` 占位的 `TICKET_SECRET`；生产值必须与 `auth-http` / `game-server` 的 ticket 签发和校验侧保持一致
- 单张 ticket revoke 删除 Redis `ticket:<sha256(ticket)>` 后，新的 `ChatAuthReq` 会返回 `TICKET_REVOKED`
- 当前没有 Redis 黑名单或封禁列表逻辑；消息频率限制是单连接本地状态，连接数限制是单实例本地状态，二者都不是跨实例全局限额
- 当前内存会话表仍是 `player_id -> sender`，同一账号多连接时在线聊天推送和邮件通知仍以现有 session map 覆盖行为为准；连接数限制计数单独维护，不改变推送路由模型
- 当前没有公网 TLS 策略，生产部署时应放在 TLS 终止层或补充直接 TLS 支持
- `chat-server` 默认不作为生产公网入口；上述 fail-fast 只是内网服务凭证保护。如果内部或测试环境直连，也应继续保持与 `game-proxy` / `game-server` 一致的 ticket 校验边界

---

## 6. 配置项对照结论

为了避免继续误用旧配置名，本主题下应以代码实际读取的环境变量为准：

- `auth-http`：
  - 使用 `RATELIMIT_WINDOW_MS`、`RATELIMIT_MAX`
  - 使用 `ACCOUNT_LOCK_MAX_ATTEMPTS`、`ACCOUNT_LOCK_WINDOW_SECONDS`、`ACCOUNT_LOCK_TTL_SECONDS`
  - 使用 `TICKET_SECRET`、`TICKET_TTL_SECONDS`、`INTERNAL_API_TOKEN`、`GAME_ADMIN_TOKEN`
  - 使用 `AUTH_REDIS_BLOCKLIST_ENABLED`、`AUTH_REDIS_BLOCKLIST_CACHE_TTL_MS` 控制 Redis 动态黑名单；Redis 地址和 key 前缀复用 `REDIS_URL`、`REDIS_KEY_PREFIX`
  - `NODE_ENV=production` 或 `APP_ENV=production` 时拒绝默认或空的 `TICKET_SECRET`、默认或空的 `GAME_ADMIN_TOKEN`、空的 `INTERNAL_API_TOKEN`
- `game-proxy`：
  - 使用 `TICKET_SECRET`、`REDIS_URL`、`REDIS_KEY_PREFIX`
  - 使用 `PROXY_ADMIN_TOKEN` 保护 admin HTTP 口，生产环境拒绝空值或开发默认值
  - 使用 `PROXY_MAX_CONNECTIONS`、`PROXY_MAX_PREAUTH_FAILURES`、`PROXY_MSG_RATE_WINDOW_MS`、`PROXY_MSG_RATE_MAX`
  - 使用 `PROXY_MAINTENANCE_CACHE_TTL_MS` 控制 Redis 共享维护状态读取缓存；维护状态 key 为 `${REDIS_KEY_PREFIX}maintenance:global`
  - 使用 `PROXY_IP_DENYLIST`、`PROXY_MAX_CONNECTIONS_PER_IP`、`PROXY_MAX_CONNECTIONS_PER_PLAYER` 做本地连接治理
  - 使用 `PROXY_REDIS_BLOCKLIST_ENABLED`、`PROXY_REDIS_BLOCKLIST_CACHE_TTL_MS` 控制 Redis 动态黑名单；消息频率限制是单连接本地状态，不是跨 proxy 全局限额
- `game-server`：
  - 使用 `TICKET_SECRET`、`GAME_ADMIN_TOKEN`、`GAME_INTERNAL_TOKEN`、`REDIS_KEY_PREFIX`、`HEARTBEAT_TIMEOUT_SECS`、`MAX_BODY_LEN`、`MSG_RATE_WINDOW_MS`、`MSG_RATE_MAX`、`PLAYER_MSG_RATE_WINDOW_MS`、`PLAYER_MSG_RATE_MAX`
  - `NODE_ENV=production` 或 `APP_ENV=production` 时拒绝默认或空的 `TICKET_SECRET`、`GAME_ADMIN_TOKEN`、`GAME_INTERNAL_TOKEN`
  - 当前没有单 IP、Redis 黑名单、时间戳窗口或反重放环境变量；单玩家消息频率限制是单 `game-server` 实例内本地状态，不是跨实例全局限额
- `chat-server`：
  - 使用 `TICKET_SECRET`、`REDIS_URL`、`REDIS_KEY_PREFIX`、`HEARTBEAT_TIMEOUT_SECS`、`MAX_BODY_LEN`、`CHAT_MSG_RATE_WINDOW_MS`、`CHAT_MSG_RATE_MAX`、`CHAT_MAX_CONNECTIONS_PER_PLAYER`、`CHAT_MAX_CONNECTIONS_PER_IP`、`CHAT_OUTBOUND_QUEUE_CAPACITY`
  - `NODE_ENV=production` 或 `APP_ENV=production` 时拒绝默认、空值或明显占位的 `TICKET_SECRET`
  - 当前消息频率限制是单连接本地状态；连接数限制是单账号 / 单 IP 的本实例本地状态，不是跨实例全局限额
- `announce-service`：
  - 使用 `ANNOUNCE_ADMIN_TOKEN` 保护公告写接口 `POST/PUT/DELETE /api/v1/announcements...`
  - 支持 `Authorization: Bearer <token>` 和 `X-Admin-Token: <token>`，不支持 query token
  - `NODE_ENV=production` 或 `APP_ENV=production` 时拒绝空值或开发默认值
  - `GET /api/v1/announcements` 和 `GET /api/v1/announcements/:announceId` 当前仍是无 token 只读查询；如果临时公网暴露，必须通过网关/TLS/更高层鉴权和限流兜底

如果后续要真正落地 `game-proxy` / `game-server` / `chat-server` 的风控策略，建议先补代码，再在文档中新增配置项；不要先在文档里约定一组尚未读取的环境变量。
