# 限流与安全现状

本文对齐当前代码中的限流、安全校验与相关配置项，重点说明“已经实现的能力”和“仍属于设计目标的部分”，避免把旧配置名当成现网配置。

---

## 1. 总览

当前安全与限流相关服务的状态如下：

- `auth-http`：已经实现 IP 限流、账号锁定、ticket 签发/撤销、维护模式入口拦截，以及安全审计写库；配置项以 `apps/auth-http/src/config.js` 为准
- `game-proxy`：当前已实现 `AuthReq` 本地 ticket 校验、鉴权前消息白名单、单连接预鉴权失败阈值、总前端连接上限、静态 IP denylist、单 IP / 单玩家本地连接上限、接入转发、活跃前端连接数观测、本地开关 + Redis 共享状态的维护模式拦截与上游发现；文档里旧的 KCP 令牌桶限流配置项目前并不存在
- `game-server`：当前已实现 ticket 校验、鉴权前消息白名单、心跳超时、包体长度限制、单连接消息频率限制和本实例内单玩家消息频率限制；频率限制默认关闭，分别按 `MSG_RATE_WINDOW_MS` / `MSG_RATE_MAX` 与 `PLAYER_MSG_RATE_WINDOW_MS` / `PLAYER_MSG_RATE_MAX` 启用
- `chat-server`：当前已实现首包鉴权、ticket 签名与过期校验、Redis ticket 归属校验、ticket version 校验、心跳超时和包体长度限制；暂未做消息频率限制

---

## 2. auth-http（登录服）

### 2.1 已实现能力

- IP 限流：Redis 滑动窗口，命中后返回 `429`
- 账号锁定：连续密码登录失败后锁定账号
- ticket 签发：使用 `TICKET_SECRET` 生成 HMAC 签名 ticket，并写入 Redis
- ticket 撤销：`/api/v1/game-ticket/revoke` 会删除 Redis 中对应 ticket
- 维护模式：读取 `${REDIS_KEY_PREFIX}maintenance:global`，开启时普通玩家登录和 `/api/v1/game-ticket/issue` 返回 `MAINTENANCE_MODE`
- 安全审计：登录失败、账号锁定、IP 限流、ticket 撤销等事件会写入 `security_audit_logs`（前提是启用了 MySQL 存储）
- 内部接口 token：配置 `INTERNAL_API_TOKEN` 后，`/api/v1/internal/game-server/status` 与 `/api/v1/internal/game-server/config` 要求 `X-Service-Token`

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
TICKET_TTL_SECONDS=86400
TICKET_VALIDATE_ENABLED=true

# Security Audit
SECURITY_AUDIT_ENABLED=true

# Internal API
INTERNAL_API_TOKEN=
```

### 2.3 与旧文档的关键差异

- 旧文档中的 `RATELIMIT_IP_WINDOW` / `RATELIMIT_IP_MAX` 并不存在，实际配置名是 `RATELIMIT_WINDOW_MS` / `RATELIMIT_MAX`
- 旧文档中的 `ACCOUNT_LOCK_MAX` / `ACCOUNT_LOCK_TTL` 并不存在，实际配置名是 `ACCOUNT_LOCK_MAX_ATTEMPTS` / `ACCOUNT_LOCK_TTL_SECONDS`
- `ACCOUNT_LOCK_WINDOW_SECONDS` 与 `ACCOUNT_LOCK_TTL_SECONDS` 是两个不同概念：
  - 前者用于统计失败次数窗口
  - 后者用于真正的锁定时长

### 2.4 当前实现备注

- 文档旧版写“ticket 默认 5 分钟”已经不准确；当前默认值是 `TICKET_TTL_SECONDS=86400`，即 24 小时
- `TICKET_VALIDATE_ENABLED` 和 `SECURITY_AUDIT_ENABLED` 已进入配置结构，但当前代码没有完整用它们做开关控制：
  - ticket 校验实际发生在 `game-proxy`、`game-server` 与 `chat-server`
  - 安全审计当前由 `mysqlStore?.appendSecurityAudit?.(...)` 直接写库，未额外判断 `SECURITY_AUDIT_ENABLED`
- ticket 不是“使用后立即删除”的一次性票据；当前 `game-proxy`、`game-server` 与 `chat-server` 校验时都会检查签名和 Redis 中是否存在对应 ticket，成功认证后不会自动删除
- 当前同一张 ticket 会被 `game-proxy`、`game-server` 与 `chat-server` 复用；因此不能简单在首次校验成功后就删除 Redis 记录，否则会破坏多服务接入链路
- 维护模式不拦截 logout、game ticket revoke 等清理操作，也不主动踢已有在线连接

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
- 当前 `game-proxy` 的 denylist 和连接数限制是单 proxy 进程内本地状态，不是 Redis 分布式全局限额；多 proxy 部署仍需要 Redis/网关层策略
- 当前代码里没有 Redis 动态黑名单或封禁列表逻辑
- 当前 `game-proxy` admin HTTP 口已经有 token 鉴权和生产默认 token 拒绝；开发默认 token 只适合本地联调，生产必须改为高强度随机值并限制 admin 端口在内网
- 当前 proxy admin 修改接口会记录 action、关键目标和 ok/error 结果到结构化日志；尚未接入 MySQL 等持久审计库，也没有细粒度 RBAC
- 当前 `game-proxy` 已强制鉴权前消息白名单；AuthReq 失败后仍保持未认证，后续业务包只会返回 `PREAUTH_MESSAGE_NOT_ALLOWED`，不会被转发到 `game-server`
- `PROXY_MAX_CONNECTIONS=0` 表示不限制总前端连接数；配置为正整数时才启用拒绝新连接
- `PROXY_MAX_PREAUTH_FAILURES=0` 表示不按预鉴权失败次数断开；默认 `3` 会在同一连接累计三次非法预鉴权消息或鉴权失败后关闭连接
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

### 4.2 当前实际配置项

以下配置名来自 `apps/game-server/src/config.rs` 与 `apps/game-server/.env.example`：

```env
TICKET_SECRET=replace-with-a-long-random-string
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

### 4.3 与旧文档的关键差异

- 旧文档中的 `HEARTBEAT_TIMEOUT` 并不存在，实际配置名是 `HEARTBEAT_TIMEOUT_SECS`
- 旧文档中的 `MSG_RATE_WINDOW` 并不存在，实际配置名是 `MSG_RATE_WINDOW_MS`；`MSG_RATE_MAX` 当前已读取，默认 `0` 关闭
- 当前“操作冷却”不是通过独立的通用风控配置实现的；代码里没有这一组统一环境变量

### 4.4 当前实现备注

- `game-server` 在认证阶段会验证 ticket 签名和 Redis 中的 ticket 所有权；这是 ticket 校验的核心落点之一，`auth-http` 只负责签发、存储与撤销
- `game-server` 校验 ticket 时依赖 `TICKET_SECRET` 和 `REDIS_KEY_PREFIX`；这两个值需要与 `auth-http` 的签发侧保持一致
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
- 在线推送与邮件通知订阅：依赖 Core NATS 与内存会话表

### 5.2 当前实际配置项

以下配置名来自 `apps/chat-server/src/main.rs` 与 `apps/chat-server/.env.example`：

```env
CHAT_BIND_ADDR=0.0.0.0:9001
HEARTBEAT_TIMEOUT_SECS=30
MAX_BODY_LEN=4096
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
- 单张 ticket revoke 删除 Redis `ticket:<sha256(ticket)>` 后，新的 `ChatAuthReq` 会返回 `TICKET_REVOKED`
- 当前没有消息频率限制、单 IP / 单账号连接数限制、Redis 黑名单或封禁列表逻辑
- 当前没有公网 TLS 策略，生产部署时应放在 TLS 终止层或补充直接 TLS 支持
- `chat-server` 默认不作为生产公网入口；如果内部或测试环境直连，也应继续保持与 `game-proxy` / `game-server` 一致的 ticket 校验边界

---

## 6. 配置项对照结论

为了避免继续误用旧配置名，本主题下应以代码实际读取的环境变量为准：

- `auth-http`：
  - 使用 `RATELIMIT_WINDOW_MS`、`RATELIMIT_MAX`
  - 使用 `ACCOUNT_LOCK_MAX_ATTEMPTS`、`ACCOUNT_LOCK_WINDOW_SECONDS`、`ACCOUNT_LOCK_TTL_SECONDS`
  - 使用 `TICKET_SECRET`、`TICKET_TTL_SECONDS`、`INTERNAL_API_TOKEN`
- `game-proxy`：
  - 使用 `TICKET_SECRET`、`REDIS_URL`、`REDIS_KEY_PREFIX`
  - 使用 `PROXY_ADMIN_TOKEN` 保护 admin HTTP 口，生产环境拒绝空值或开发默认值
  - 使用 `PROXY_MAX_CONNECTIONS`、`PROXY_MAX_PREAUTH_FAILURES`
  - 使用 `PROXY_MAINTENANCE_CACHE_TTL_MS` 控制 Redis 共享维护状态读取缓存；维护状态 key 为 `${REDIS_KEY_PREFIX}maintenance:global`
  - 使用 `PROXY_IP_DENYLIST`、`PROXY_MAX_CONNECTIONS_PER_IP`、`PROXY_MAX_CONNECTIONS_PER_PLAYER` 做本地连接治理
  - 当前没有 Redis 动态黑名单或消息频率限制环境变量
- `game-server`：
  - 使用 `TICKET_SECRET`、`REDIS_KEY_PREFIX`、`HEARTBEAT_TIMEOUT_SECS`、`MAX_BODY_LEN`、`MSG_RATE_WINDOW_MS`、`MSG_RATE_MAX`、`PLAYER_MSG_RATE_WINDOW_MS`、`PLAYER_MSG_RATE_MAX`
  - 当前没有单 IP、Redis 黑名单、时间戳窗口或反重放环境变量；单玩家消息频率限制是单 `game-server` 实例内本地状态，不是跨实例全局限额
- `chat-server`：
  - 使用 `TICKET_SECRET`、`REDIS_URL`、`REDIS_KEY_PREFIX`、`HEARTBEAT_TIMEOUT_SECS`、`MAX_BODY_LEN`
  - 当前没有消息频率限制配置

如果后续要真正落地 `game-proxy` / `game-server` / `chat-server` 的风控策略，建议先补代码，再在文档中新增配置项；不要先在文档里约定一组尚未读取的环境变量。
