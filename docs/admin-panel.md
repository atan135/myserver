# 管理后台

## 概述

当前仓库里的管理后台已经不止“登录 + 审计日志”。

现状由两部分组成：

- `apps/admin-api`：Node.js + NestJS 管理 API，默认监听 `3001`
- `apps/admin-web`：Vue 3 + Element Plus 管理前端，默认监听 `3002`

已经落地的后台能力包括：

- 管理员登录、登出、当前身份查询
- 管理员 token 批量撤销、管理员密码重置与 token version 联动失效
- 管理操作审计日志查询
- 安全日志查询
- 玩家列表查询、玩家详情查询、玩家状态修改
- GM 指令：广播、发道具、踢人、封禁
- 服务监控总览、服务监控详情、metrics 归档接口
- 维护模式状态查询与切换接口

维护模式当前已经是共享入口拦截能力：`admin-api` 写入 Redis 共享状态并保留审计，`auth-http` 会拦截普通玩家登录和新 game ticket 签发，`game-proxy` 会在新 `AuthReq` 接入时拒绝认证；已有在线连接不会被主动踢下线。

## 架构

```text
apps/
├── admin-api/   # Node.js + NestJS API (端口 3001)
└── admin-web/   # Vue 3 + Element Plus 前端 (端口 3002)
```

管理后台依赖的外部组件：

- MySQL：读取 `myserver_auth` 中的管理员、玩家、审计和安全日志数据
- Redis：读取各服务上报的 heartbeat 与 metrics，并写入维护模式共享状态
- Core NATS：发布 `myserver.session.kick.<player_id_token>`，用于 GM 踢人/封禁跨 `game-server` 实例断开在线连接
- `game-server` admin TCP 通道：执行 GM 指令；其中踢人/封禁的单实例调用保留为 legacy 兼容和辅助审计

## 快速启动

```powershell
# 安装依赖
npm install

# 初始化数据库
mysql -uroot -p < db/init.sql

# 启动 API 服务
npm run dev:admin-api

# 启动 Web 前端（新开终端）
npm run dev:admin-web
```

访问 `http://127.0.0.1:3002`

## 默认账号

| 用户名 | 密码 | 角色 |
|--------|------|------|
| admin | AdminPass123! | admin |

首次启动 `admin-api` 时，如果 `admin_accounts` 中不存在该用户，会自动创建。

## 当前页面与能力

### Web 前端已有页面

| 路由 | 页面 | 前端可见角色 | 说明 |
|------|------|--------------|------|
| `/login` | 登录页 | 全部 | 管理员登录 |
| `/` | 概览页 | 已登录用户 | 当前账号信息展示 |
| `/audit-logs` | 审计日志 | `admin` / `operator` / `viewer` | 查看管理操作日志 |
| `/security-logs` | 安全日志 | `admin` / `operator` / `viewer` | 查看安全事件日志 |
| `/players` | 玩家管理 | `admin` / `operator` / `viewer` | 查询玩家，`operator` 以上可改状态 |
| `/gm` | GM 命令 | `admin` / `operator` | 广播、发道具、踢人，`admin` 额外可封禁 |
| `/monitoring` | 服务监控总览 | `admin` / `operator` / `viewer` | 查看服务在线状态和实时指标 |
| `/monitoring/:service` | 服务监控详情 | `admin` / `operator` / `viewer` | 查看单服务 QPS / 延迟曲线 |

### 当前只有 API、尚未接入前端页面的能力

- 维护模式查询：`GET /api/v1/maintenance`
- 维护模式切换：`POST /api/v1/maintenance`
- 撤销指定管理员全部 token：`POST /api/v1/admins/:adminId/revoke-tokens`
- 重置指定管理员密码并撤销旧 token：`POST /api/v1/admins/:adminId/reset-password`
- 手动触发 metrics 归档：`POST /api/admin/monitoring/archive`

## 权限说明

### 设计上的角色分级

| 角色 | 说明 |
|------|------|
| viewer | 查看概览、日志、玩家信息、监控 |
| operator | viewer + 玩家状态调整 + GM 广播/发道具/踢人 |
| admin | operator + 封禁玩家 + 维护模式切换 + 管理员 token 生命周期操作 |

### 当前实现现状

- `admin-web` 的菜单和前端路由按角色做了显示与跳转限制
- `GM.vue` 页面中“封禁玩家”表单只对 `admin` 显示
- `admin-api` 当前所有 `/api/v1/*` 接口都通过 NestJS Guard 做 JWT 校验
- `admin-api` 已通过 `RolesGuard` 和 `@Roles()` 对审计、玩家、GM、维护模式和监控接口做后端角色校验

因此当前权限控制现状应理解为：

- 前端页面级权限限制已生效
- 后端接口级角色校验已生效
- 管理员 JWT 已包含 `jti` 和 `tokenVersion`，后端通过 Redis 管理员 session 校验实现登出撤销；Guard 每次仍会查库确认管理员存在且 `status=active`
- 管理员 token 撤销和密码重置接口已通过 bump Redis token version 让目标管理员全部旧 token 失效
- 管理员登录失败已按 username + client IP 维度计数和锁定，并写入 `security_audit_logs`
- 审计 IP 与控制面保护已按 `TRUST_PROXY` / `TRUSTED_PROXIES` 解析，不再无条件信任 `X-Forwarded-For` 或 `X-Forwarded-Proto`
- `admin-api` 支持请求级 HTTPS/TLS 强制和来源 IP allowlist；生产仍必须通过运营网段、堡垒机、VPN 或独立管理入口做网络隔离

如果后续补上更细粒度权限矩阵，本文应同步更新。

## 数据库表

### `admin_accounts`

管理员账号表。

| 字段 | 类型 | 说明 |
|------|------|------|
| id | BIGINT | 主键 |
| username | VARCHAR(64) | 用户名（唯一） |
| display_name | VARCHAR(64) | 显示名称 |
| password_algo | VARCHAR(32) | 密码算法标记 |
| password_salt | VARCHAR(128) | 盐值 |
| password_hash | VARCHAR(256) | 密码哈希 |
| role | VARCHAR(32) | 角色 |
| status | VARCHAR(32) | 状态 |
| created_at | DATETIME(3) | 创建时间 |
| last_login_at | DATETIME(3) | 最后登录时间 |

说明：

- 当前 `createAdmin()` 实际写入的是 `bcrypt` 哈希
- 默认初始化管理员由 `admin-api` 启动时自动补齐

### `admin_audit_logs`

管理操作审计表。

| 字段 | 类型 | 说明 |
|------|------|------|
| id | BIGINT | 主键 |
| admin_id | BIGINT | 管理员 ID |
| admin_username | VARCHAR(64) | 管理员用户名 |
| action | VARCHAR(64) | 操作类型 |
| target_type | VARCHAR(32) | 目标类型 |
| target_value | VARCHAR(256) | 目标值 |
| details_json | JSON | 详情 |
| ip | VARCHAR(64) | 来源 IP |
| created_at | DATETIME(3) | 时间 |

当前会写入的典型 `action` 包括：

- `admin_login`
- `admin_logout`
- `admin_tokens_revoked`
- `admin_tokens_revoke_failed`
- `admin_password_reset`
- `admin_password_reset_failed`
- `player_status_change`
- `maintenance_enabled`
- `maintenance_disabled`
- `gm_broadcast`
- `gm_send_item`
- `gm_kick_player`
- `gm_ban_player`

### `security_audit_logs`

安全事件查询表。

| 字段 | 类型 | 说明 |
|------|------|------|
| id | BIGINT | 主键 |
| event_type | VARCHAR(64) | 事件类型 |
| target_type | VARCHAR(32) | 目标类型 |
| target_value | VARCHAR(256) | 目标值 |
| client_ip | VARCHAR(64) | 客户端 IP |
| severity | VARCHAR(16) | 严重级别：`info` / `warning` / `critical` |
| details_json | JSON | 详情 |
| created_at | DATETIME(3) | 时间 |

该表由 `auth-http` 等服务写入；`admin-api` 也会写入管理员登录失败和锁定事件，并提供查询能力。

### `player_accounts`

玩家管理页面读取该表。

| 字段 | 类型 | 说明 |
|------|------|------|
| player_id | VARCHAR(64) | 玩家 ID |
| guest_id | VARCHAR(128) | 游客账号标识 |
| login_name | VARCHAR(64) | 登录名 |
| display_name | VARCHAR(64) | 显示名 |
| account_type | VARCHAR(32) | 账号类型 |
| status | VARCHAR(32) | `active` / `disabled` / `banned` |
| ban_expires_at | DATETIME(3) | 限时封禁到期时间，`NULL` 表示非封禁或永久封禁 |
| created_at | DATETIME(3) | 创建时间 |
| last_login_at | DATETIME(3) | 最后登录时间 |

### `metrics_archive`

监控归档表，用于保存从 Redis 迁移出来的历史指标。

| 字段 | 类型 | 说明 |
|------|------|------|
| service_name | VARCHAR(64) | 服务名 |
| bucket_time | INT | 5 秒桶时间戳 |
| qps | INT | QPS |
| latency_ms | INT | 延迟 |
| online_value | INT | 在线人数 / 连接数 / 池大小 |
| extra | JSON | 服务特有字段 |

## API 接口

### 认证

#### `POST /api/v1/auth/login`

管理员登录。

```json
{
  "username": "admin",
  "password": "AdminPass123!"
}
```

```json
{
  "ok": true,
  "accessToken": "eyJhbGciOi...",
  "expiresIn": "8h",
  "admin": {
    "id": 1,
    "username": "admin",
    "displayName": "Administrator",
    "role": "admin"
  }
}
```

#### `GET /api/v1/auth/me`

获取当前管理员信息。

#### `POST /api/v1/auth/logout`

登出，并写入审计日志。

### 管理员账号安全

以下接口均要求 `admin` 角色；`operator` 和 `viewer` 会被后端角色校验拒绝。

#### `POST /api/v1/admins/:adminId/revoke-tokens`

撤销指定管理员全部现有 token。实现方式是 bump 目标管理员 Redis token version；旧 JWT 和旧 session 中的 `tokenVersion` 会在下一次请求时被 Guard 拒绝。

请求体：

```json
{
  "reason": "权限调整"
}
```

`reason` 必填，最长 512 个字符。

返回结构：

```json
{
  "ok": true,
  "message": "Admin tokens revoked.",
  "targetAdmin": {
    "id": 2,
    "username": "ops",
    "displayName": "Ops",
    "role": "operator",
    "status": "active"
  },
  "tokenVersion": 3,
  "currentTokenInvalidated": false
}
```

如果管理员撤销自己的全部 token，本次请求会正常返回；响应中的 `currentTokenInvalidated` 为 `true`，表示当前请求使用的 token 已不能用于后续请求。

#### `POST /api/v1/admins/:adminId/reset-password`

重置指定管理员密码，并通过 bump 目标管理员 token version 使旧 token 全部失效。

请求体：

```json
{
  "newPassword": "NewPass456!X",
  "reason": "管理员轮换"
}
```

`reason` 必填，最长 512 个字符。密码长度要求为 12 到 128 个字符，不允许空白字符，且必须同时包含大写字母、小写字母、数字和符号。

该接口会先 bump 目标管理员 Redis token version，再更新 `admin_accounts.password_algo/password_salt/password_hash`。如果 Redis token version 更新失败，密码不会写库，避免出现密码已重置但旧 token 仍可用的状态；如果后续密码写库失败，目标管理员旧 token 已失效，需要重新处理密码重置。接口会写入 `admin_audit_logs`，审计详情不会记录明文密码。

### 审计日志

#### `GET /api/v1/audit-logs`

查询管理操作审计日志。

支持查询参数：

| 参数 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| limit | number | 50 | 每页数量，最大 100 |
| offset | number | 0 | 偏移量 |
| action | string | - | 按操作类型筛选 |
| target_type | string | - | 按目标类型筛选 |

### 安全日志

#### `GET /api/v1/security-logs`

查询安全事件日志。

支持查询参数：

| 参数 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| limit | number | 50 | 每页数量，最大 100 |
| offset | number | 0 | 偏移量 |
| event_type | string | - | 事件类型 |
| target_type | string | - | 目标类型 |
| severity | string | - | 严重级别 |
| client_ip | string | - | 客户端 IP |

### 玩家管理

#### `GET /api/v1/players`

分页查询玩家列表。

支持查询参数：

| 参数 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| login_name | string | - | 登录名模糊查询 |
| guest_id | string | - | Guest ID 模糊查询 |
| status | string | - | `active` / `disabled` / `banned` |
| limit | number | 50 | 每页数量，最大 100 |
| offset | number | 0 | 偏移量 |

#### `GET /api/v1/players/:playerId`

查询单个玩家详情。

#### `PUT /api/v1/players/:playerId/status`

修改玩家状态。

请求体：

```json
{
  "status": "disabled"
}
```

允许值：

- `active`
- `disabled`
- `banned`

设置为 `active` 或 `disabled` 时会清空 `ban_expires_at`；手动设置为 `banned` 且不提供时长时，`ban_expires_at=NULL` 表示永久封禁。

### 维护模式

#### `GET /api/v1/maintenance`

读取当前维护模式状态。

返回结构：

```json
{
  "ok": true,
  "enabled": false,
  "reason": null,
  "updatedAt": null,
  "updatedBy": null
}
```

#### `POST /api/v1/maintenance`

切换维护模式。

请求体：

```json
{
  "enabled": true,
  "reason": "版本发布维护"
}
```

当前实现说明：

- `enabled` 必须是 boolean；`reason` 可选，传入时会 trim，最长 512 字符
- 该接口会写入 `${REDIS_KEY_PREFIX}maintenance:global`，值为 JSON，包含 `enabled`、`reason`、`updatedAt`、`updatedBy`
- 该接口会继续写入 `admin_audit_logs`，动作为 `maintenance_enabled` / `maintenance_disabled`
- `GET /api/v1/maintenance` 优先读取 Redis 共享状态；Redis 没有状态时，兼容读取最近一次维护模式审计记录来还原状态
- 维护开启后，`auth-http` 拒绝普通玩家登录和新 game ticket 签发，返回 `MAINTENANCE_MODE`；`game-proxy` 在新 `AuthReq` 阶段返回 `AuthRes(ok=false, error_code=MAINTENANCE_MODE)`
- 维护模式只阻止新登录、签票和新游戏接入，不主动踢已有在线连接；登出、ticket revoke 等清理操作不被拦截

### GM 命令

这些接口通过 `admin-api -> game-server admin TCP` 调用游戏服；GM 踢人/封禁还会通过 Core NATS 发布 `myserver.session.kick.<player_id_token>`，由各 `game-server` 实例订阅后断开本实例上的目标玩家连接。legacy 单实例 admin TCP 调用仍保留为兼容和辅助结果。

#### `POST /api/v1/gm/broadcast`

发送全服广播。当前 `game-server` 侧会向本实例已鉴权在线连接推送 `GameMessagePush(event="gm_broadcast", action="broadcast")`。

```json
{
  "title": "系统公告",
  "content": "今晚 20:00 维护",
  "sender": "System"
}
```

#### `POST /api/v1/gm/send-item`

给指定玩家发道具。

```json
{
  "playerId": "player-001",
  "itemId": "1001",
  "itemCount": 5,
  "reason": "活动补偿"
}
```

#### `POST /api/v1/gm/kick-player`

踢出玩家。`admin-api` 会校验并 trim `playerId` / `reason`，发布 NATS session kick 事件，实现跨 `game-server` 实例断开在线连接；如果 legacy 单实例 admin TCP 返回 `PLAYER_OFFLINE`，不会导致全局踢人失败。NATS 发布失败时接口返回结构化失败，因为跨实例断开无法保证；审计 details 会记录 global kick 与 legacy 调用结果。

```json
{
  "playerId": "player-001",
  "reason": "重新登录"
}
```

#### `POST /api/v1/gm/ban-player`

封禁玩家。`admin-api` 会先把 `player_accounts.status` 更新为 `banned`，并按 `durationSeconds` 写入 `ban_expires_at`，随后发布 NATS session kick 事件，确保目标玩家在任意 `game-server` 实例上的在线连接被断开；legacy 单实例 admin TCP ban 调用仍作为 best-effort 结果进入审计。如果 NATS 发布失败，不回滚已写入的 banned 状态，响应和审计会标明 global kick 失败。限时封禁不依赖常驻定时器，`auth-http` 会在玩家登录或申请 game ticket 时惰性检查 `ban_expires_at`，过期后自动恢复为 `active`；未到期或永久封禁仍按现有 `ACCOUNT_DISABLED` 错误拒绝。

```json
{
  "playerId": "player-001",
  "durationSeconds": 3600,
  "reason": "作弊"
}
```

### 服务监控

#### `GET /api/admin/monitoring/services`

查询所有服务状态与最新指标。

当前服务清单固定为：

- `auth-http`
- `game-server`
- `game-proxy`
- `chat-server`
- `match-service`
- `announce-service`
- `mail-service`
- `admin-api`

状态判定规则：

- 通过 Redis `metrics:heartbeat:<service>` 判断服务是否在线
- 30 秒内有心跳视为在线
- 在线时再读取 `metrics:<service>:*` 中最新桶数据

#### `GET /api/admin/monitoring/services/:name/metrics`

查询单服务历史指标曲线。

支持窗口：

- `1m`
- `5m`
- `15m`
- `1h`

#### `POST /api/admin/monitoring/archive`

手动触发 metrics 归档任务，把超过 7 天的 Redis 指标迁移到 MySQL `metrics_archive`。

当前实现备注：

- 这组监控接口已挂 `JwtAuthGuard` 和 `RolesGuard`
- `GET` 监控查询允许 `viewer` / `operator` / `admin`
- `POST /api/admin/monitoring/archive` 仅允许 `admin`

## 前端页面说明

### 概览页

当前只是轻量首页，显示：

- 当前登录用户名
- 显示名称
- 角色

还没有聚合统计卡片、待办告警或快捷操作面板。

### 玩家管理页

已支持：

- 按登录名筛选
- 按 Guest ID 筛选
- 按状态筛选
- 分页浏览
- 禁用玩家
- 解禁玩家

当前前端没有单独的玩家详情页弹窗或跳转页，只有列表操作。

### GM 命令页

已支持：

- 发送广播
- 发放道具
- 踢出玩家
- 封禁玩家

其中封禁表单只在前端对 `admin` 角色显示；后端也通过 `@Roles("admin")` 校验。GM 踢人/封禁已通过 NATS session kick 跨 `game-server` 实例断开在线连接，legacy 单实例 admin TCP 调用仍保留为兼容辅助；GM 封禁会持久化账号状态和 `ban_expires_at`，限时封禁由 `auth-http` 登录/签票路径惰性自动解封。

### 服务监控页

总览页已支持：

- 轮询刷新，当前周期 5 秒
- 查看在线 / 离线状态
- 查看 QPS、延迟、在线人数 / 连接数 / 匹配池大小

详情页已支持：

- 指标时间窗口切换
- QPS 折线图
- 延迟折线图
- 当前数值摘要卡片

## 配置项

### `admin-api` (`apps/admin-api/.env`)

```env
PORT=3001
HOST=127.0.0.1
NODE_ENV=development
LOG_LEVEL=info
LOG_ENABLE_CONSOLE=true
LOG_ENABLE_FILE=true
LOG_DIR=logs/admin-api

REDIS_URL=redis://127.0.0.1:6379
REDIS_KEY_PREFIX=

MYSQL_URL=mysql://root:password@127.0.0.1:3306/myserver_auth
MYSQL_POOL_SIZE=10

JWT_SECRET=replace-with-a-long-random-string-for-jwt
JWT_EXPIRES_IN=8h
ADMIN_SESSION_TTL_SECONDS=28800
ADMIN_LOGIN_MAX_FAILURES=5
ADMIN_LOGIN_FAILURE_WINDOW_SECONDS=900
ADMIN_LOGIN_LOCK_SECONDS=900
TRUST_PROXY=false
TRUSTED_PROXIES=
ADMIN_API_REQUIRE_TLS=false
ADMIN_API_REQUIRE_IP_ALLOWLIST=false
ADMIN_API_IP_ALLOWLIST=127.0.0.1,::1

GAME_SERVER_ADMIN_HOST=127.0.0.1
GAME_SERVER_ADMIN_PORT=7500
GAME_ADMIN_TOKEN=dev-only-change-this-game-admin-token

ADMIN_USERNAME=admin
ADMIN_PASSWORD=AdminPass123!
ADMIN_DISPLAY_NAME=Administrator
```

说明：

- `GAME_SERVER_ADMIN_HOST` / `GAME_SERVER_ADMIN_PORT` / `GAME_ADMIN_TOKEN` 用于 GM 指令转发
- 默认仍对接 `game-server` admin 端口 `7500`
- 生产环境必须修改 `JWT_SECRET`、`GAME_ADMIN_TOKEN` 和初始管理员密码；`NODE_ENV=production` 下明显默认的 `JWT_SECRET` / `GAME_ADMIN_TOKEN` / `ADMIN_PASSWORD` 会导致配置加载失败
- `ADMIN_API_REQUIRE_TLS` 开发默认 `false`，`NODE_ENV=production` 默认 `true`；直连 HTTPS 通过 socket TLS 判断，经代理部署时只有可信代理来源的 `X-Forwarded-Proto=https` 才生效
- `ADMIN_API_REQUIRE_IP_ALLOWLIST=false` 默认关闭；启用后 `ADMIN_API_IP_ALLOWLIST` 支持精确 IP 和 IPv4 CIDR，例如 `127.0.0.1,10.0.0.0/24`
- `TRUST_PROXY=false` 时审计 IP 和 allowlist 来源 IP 使用直连来源；只有开启 `TRUST_PROXY` 且直连来源显式列在 `TRUSTED_PROXIES` 中时才采用 `X-Forwarded-For` 首个地址

## 安全说明

1. `admin-api` 的 `/api/v1/*` 接口使用 `Authorization: Bearer <token>` 做 JWT 鉴权。
2. 管理员密码当前使用 `bcrypt` 哈希存储。
3. 登录成功会创建 Redis 管理员 session，JWT 中的 `jti` 必须仍存在；`POST /api/v1/auth/logout` 会删除当前 session，同一 token 后续会被拒绝。
4. 管理员账号被禁用后，Guard 会在下一次请求查库时拒绝访问；token version 已用于管理员 token 批量撤销和密码重置后的旧 token 失效。
5. 登录失败、账号锁定等安全事件会写入 `security_audit_logs`，关键后台操作会写入 `admin_audit_logs`。
6. 审计 IP 解析遵循 `TRUST_PROXY` / `TRUSTED_PROXIES`，生产如有反向代理需要显式配置可信代理地址。
7. 安全事件通过 `security_audit_logs` 提供检索。
8. 监控接口 `/api/admin/monitoring/*` 已挂 JWT 与角色校验，但生产仍应通过运营网段、堡垒机、VPN 或独立管理入口访问。
9. 当前后端已有角色校验、请求级 TLS 强制和来源 IP allowlist；这些代码侧保护不替代安全组、防火墙、VPN 或堡垒机隔离。
10. 更细粒度权限矩阵仍需继续推进。
