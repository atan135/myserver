# 管理后台

## 概述

当前仓库里的管理后台已经不止“登录 + 审计日志”。

现状由两部分组成：

- `apps/admin-api`：Node.js + NestJS 管理 API，默认监听 `3001`
- `apps/admin-web`：Vue 3 + Element Plus 管理前端，默认监听 `3002`

已经落地的后台能力包括：

- 管理员登录、登出、当前身份查询
- 管理操作审计日志查询
- 安全日志查询
- 玩家列表查询、玩家详情查询、玩家状态修改
- GM 指令：广播、发道具、踢人、封禁
- 服务监控总览、服务监控详情、metrics 归档接口
- 维护模式状态查询与切换接口

其中“维护模式”当前只在后台侧记录状态和审计日志，还没有联动到登录服或游戏服做强制拦截。

## 架构

```text
apps/
├── admin-api/   # Node.js + NestJS API (端口 3001)
└── admin-web/   # Vue 3 + Element Plus 前端 (端口 3002)
```

管理后台依赖的外部组件：

- MySQL：读取 `myserver_auth` 中的管理员、玩家、审计和安全日志数据
- Redis：读取各服务上报的 heartbeat 与 metrics
- `game-server` admin TCP 通道：执行 GM 指令

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
- 手动触发 metrics 归档：`POST /api/admin/monitoring/archive`

## 权限说明

### 设计上的角色分级

| 角色 | 说明 |
|------|------|
| viewer | 查看概览、日志、玩家信息、监控 |
| operator | viewer + 玩家状态调整 + GM 广播/发道具/踢人 |
| admin | operator + 封禁玩家 + 维护模式切换 |

### 当前实现现状

- `admin-web` 的菜单和前端路由按角色做了显示与跳转限制
- `GM.vue` 页面中“封禁玩家”表单只对 `admin` 显示
- `admin-api` 当前所有 `/api/v1/*` 接口都通过 NestJS Guard 做 JWT 校验
- `admin-api` 已通过 `RolesGuard` 和 `@Roles()` 对审计、玩家、GM、维护模式和监控接口做后端角色校验

因此当前权限控制现状应理解为：

- 前端页面级权限限制已生效
- 后端接口级角色校验已生效
- 管理员 JWT 已包含 `jti` 和 `tokenVersion`，后端通过 Redis 管理员 session 校验实现登出撤销；Guard 每次仍会查库确认管理员存在且 `status=active`
- 管理员登录失败已按 username + client IP 维度计数和锁定，并写入 `security_audit_logs`
- 审计 IP 已按 `TRUST_PROXY` / `TRUSTED_PROXIES` 解析，不再无条件信任 `X-Forwarded-For`

如果后续补上批量撤销、重置密码联动 token version、管理面 IP allowlist 或更细粒度权限矩阵，本文应同步更新。

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

### 维护模式

#### `GET /api/v1/maintenance`

读取当前维护模式状态。

返回结构：

```json
{
  "ok": true,
  "enabled": false,
  "reason": null,
  "updatedAt": null
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

- 该接口会写入 `admin_audit_logs`
- `GET /api/v1/maintenance` 通过读取最近一次 `maintenance_enabled` / `maintenance_disabled` 审计记录来还原状态
- 目前没有把维护状态下发给其他服务，也没有真的阻止玩家登录或进入游戏

### GM 命令

这些接口通过 `admin-api -> game-server admin TCP` 调用游戏服。

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

踢出玩家。当前只处理目标玩家位于当前 `game-server` 实例上的在线连接；离线或不在本实例时会返回游戏服错误，不写成功审计。

```json
{
  "playerId": "player-001",
  "reason": "重新登录"
}
```

#### `POST /api/v1/gm/ban-player`

封禁玩家。`admin-api` 会先把 `player_accounts.status` 更新为 `banned`，随后尽力通知当前 `game-server` 实例对在线连接推送 `SessionKickPush` 并断开；如果玩家离线或不在该实例，账号封禁仍会成功。

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

其中封禁表单只在前端对 `admin` 角色显示；后端也通过 `@Roles("admin")` 校验。GM 踢人当前只对目标所在 `game-server` 实例的在线连接生效；GM 封禁已持久化账号状态并尽力踢在线连接，但 `durationSeconds` 当前只进入审计详情，不会自动定时解封。

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
- `TRUST_PROXY=false` 时审计 IP 使用直连来源；只有开启 `TRUST_PROXY` 且直连来源显式列在 `TRUSTED_PROXIES` 中时才采用 `X-Forwarded-For` 首个地址

## 安全说明

1. `admin-api` 的 `/api/v1/*` 接口使用 `Authorization: Bearer <token>` 做 JWT 鉴权。
2. 管理员密码当前使用 `bcrypt` 哈希存储。
3. 登录成功会创建 Redis 管理员 session，JWT 中的 `jti` 必须仍存在；`POST /api/v1/auth/logout` 会删除当前 session，同一 token 后续会被拒绝。
4. 管理员账号被禁用后，Guard 会在下一次请求查库时拒绝访问；token version 已进入校验链路，后续可用于重置密码或批量撤销。
5. 登录失败、账号锁定等安全事件会写入 `security_audit_logs`，关键后台操作会写入 `admin_audit_logs`。
6. 审计 IP 解析遵循 `TRUST_PROXY` / `TRUSTED_PROXIES`，生产如有反向代理需要显式配置可信代理地址。
7. 安全事件通过 `security_audit_logs` 提供检索。
8. 监控接口 `/api/admin/monitoring/*` 已挂 JWT 与角色校验，但生产仍应通过运营网段、堡垒机、VPN 或独立管理入口访问。
9. 当前后端已有角色校验；管理面 IP allowlist、HTTPS/TLS 强制和更细粒度权限矩阵仍需继续推进。
