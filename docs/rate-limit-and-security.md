# 限流与安全现状

本文对齐当前代码中的限流、安全校验与相关配置项，重点说明“已经实现的能力”和“仍属于设计目标的部分”，避免把旧配置名当成现网配置。

---

## 1. 总览

当前三层服务的状态如下：

- `auth-http`：已经实现 IP 限流、账号锁定、ticket 签发/撤销，以及安全审计写库；配置项以 `apps/auth-http/src/config.js` 为准
- `game-proxy`：当前主要提供接入转发、连接数观测、维护模式与上游发现；文档里旧的 KCP 限流/黑名单配置项目前并不存在
- `game-server`：当前已实现 ticket 校验、心跳超时和包体长度限制；文档里旧的消息频率限制专用配置项目前并不存在

---

## 2. auth-http（登录服）

### 2.1 已实现能力

- IP 限流：Redis 滑动窗口，命中后返回 `429`
- 账号锁定：连续密码登录失败后锁定账号
- ticket 签发：使用 `TICKET_SECRET` 生成 HMAC 签名 ticket，并写入 Redis
- ticket 撤销：`/api/v1/game-ticket/revoke` 会删除 Redis 中对应 ticket
- 安全审计：登录失败、账号锁定、IP 限流、ticket 撤销等事件会写入 `security_audit_logs`（前提是启用了 MySQL 存储）

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
  - ticket 校验实际发生在 `game-server`
  - 安全审计当前由 `mysqlStore?.appendSecurityAudit?.(...)` 直接写库，未额外判断 `SECURITY_AUDIT_ENABLED`
- ticket 不是“使用后立即删除”的一次性票据；当前 `game-server` 校验时只检查签名和 Redis 中是否存在对应 ticket，成功认证后不会自动删除

---

## 3. game-proxy（接入代理）

### 3.1 当前已实现能力

- KCP 前端监听
- TCP fallback 前端监听
- 动态上游发现或静态上游路由
- 连接数统计与监控暴露
- 维护模式开关

### 3.2 当前实际配置项

`game-proxy` 当前没有独立的“IP 限流 / 黑名单 / 单账号连接数限制”配置项。代码实际读取的是以下配置：

```env
PROXY_HOST=127.0.0.1
PROXY_PORT=4000
PROXY_ADMIN_HOST=127.0.0.1
PROXY_ADMIN_PORT=7101
PROXY_TCP_FALLBACK_HOST=127.0.0.1
PROXY_TCP_FALLBACK_PORT=14000
PROXY_LOCAL_SOCKET_NAME=myserver-game-proxy.sock

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
- 当前 `game-proxy` 只统计总连接数，没有按 IP 或账号做连接上限控制
- 当前代码里也没有 Redis 黑名单或封禁列表逻辑

---

## 4. game-server（游戏服）

### 4.1 当前已实现能力

- ticket 校验：校验 HMAC 签名，并检查 Redis 中是否存在对应 ticket
- 心跳超时：读取包头时使用 `heartbeat_timeout_secs`
- 最大包体限制：包体超过 `max_body_len` 时拒绝处理
- 管理接口支持动态调整 `heartbeat_timeout_secs` 与 `max_body_len`

### 4.2 当前实际配置项

以下配置名来自 `apps/game-server/src/config.rs` 与 `apps/game-server/.env.example`：

```env
TICKET_SECRET=replace-with-a-long-random-string
REDIS_KEY_PREFIX=
HEARTBEAT_TIMEOUT_SECS=30
MAX_BODY_LEN=4096
```

### 4.3 与旧文档的关键差异

- 旧文档中的 `HEARTBEAT_TIMEOUT` 并不存在，实际配置名是 `HEARTBEAT_TIMEOUT_SECS`
- 旧文档中的 `MSG_RATE_WINDOW` / `MSG_RATE_MAX` 当前并不存在
- 当前“操作冷却”不是通过独立的通用风控配置实现的；代码里没有这一组统一环境变量

### 4.4 当前实现备注

- `game-server` 在认证阶段会验证 ticket 签名和 Redis 中的 ticket 所有权，因此 ticket 校验的真正落点在这里，而不是 `auth-http`
- `game-server` 校验 ticket 时依赖 `TICKET_SECRET` 和 `REDIS_KEY_PREFIX`；这两个值需要与 `auth-http` 的签发侧保持一致
- ticket 校验成功后当前不会自动删除 Redis 中的 ticket，因此并非严格的一次性消费模型
- 心跳和包体长度限制属于当前已经生效的安全边界；消息频率限制、异常解析失败率阈值等仍属于设计目标，尚未看到对应配置入口

---

## 5. 配置项对照结论

为了避免继续误用旧配置名，本主题下应以代码实际读取的环境变量为准：

- `auth-http`：
  - 使用 `RATELIMIT_WINDOW_MS`、`RATELIMIT_MAX`
  - 使用 `ACCOUNT_LOCK_MAX_ATTEMPTS`、`ACCOUNT_LOCK_WINDOW_SECONDS`、`ACCOUNT_LOCK_TTL_SECONDS`
- `game-proxy`：
  - 当前没有独立的限流环境变量，只有监听、路由发现和运维相关配置
- `game-server`：
  - 使用 `TICKET_SECRET`、`REDIS_KEY_PREFIX`、`HEARTBEAT_TIMEOUT_SECS`、`MAX_BODY_LEN`
  - 当前没有 `MSG_RATE_WINDOW`、`MSG_RATE_MAX`

如果后续要真正落地 `game-proxy` / `game-server` 的风控策略，建议先补代码，再在文档中新增配置项；不要先在文档里约定一组尚未读取的环境变量。
