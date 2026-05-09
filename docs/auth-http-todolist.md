# auth-http 待完善功能清单

本文档基于对 `apps/auth-http` 全部源码的审查，列出当前需要完善和建议新增的功能项，供后续开发跟进。

审查基准：当前仓库代码实现 + `docs/security-design.md` 中的设计目标。

---

## 一、需要完善的现有功能

### P0（安全风险 / 基础功能闭环）

- [x] **#1 Internal 接口增加 service token 鉴权** ✅ 已完成
  - 文件：`src/routes.js`、`src/config.js`、`.env.example`
  - 实现：新增 `INTERNAL_API_TOKEN` 配置项和 `verifyInternalToken()` 校验函数；两个 `/api/v1/internal/*` 路由在处理前检查 `X-Service-Token` header；未配置 token 时默认放行（兼容开发环境），配置后强制校验
  - 联动：mock-client 新增 `--service-token` 参数，`updateGameServerConfig` / `fetchGameServerStatus` 自动携带 header
  - 提交：`a6d6d90 feat: 为 auth-http internal 接口增加 service token 鉴权`

- [x] **#2 登出接口缺失（Session 销毁）** ✅ 已完成
  - 文件：`src/auth-store.js`、`src/routes.js`
  - 实现：在 AuthStore 中新增 `destroySession()` 方法，删除 `session:{accessToken}` 和 `session-activity:{accessToken}` Redis key 并写 `logout` 审计日志；新增 `POST /api/v1/auth/logout` 路由，支持 Bearer token 鉴权 + 可选 body 传入 ticket 同时撤销
  - 联动：mock-client 新增 `logout` 场景（`--scenario logout`），覆盖 登录→验证→登出→确认销毁→确认 ticket 失效 完整流程
  - 提交：`33fc727 feat: 为 auth-http 新增登出接口`

### P1（稳定性 / 安全增强）

- [x] **#3 优雅关闭不完整** ✅ 已完成
  - 文件：`src/server.js`
  - 实现：shutdown 函数改为依次关闭 httpServer.close → metrics.stop → redis.quit → mysqlPool.end，每步独立 try/catch 并记录日志；增加 shuttingDown 防重入锁；退出前调用 log4js.shutdown() 确保文件日志 flush
  - 提交：`50df602 fix: 完善 auth-http 优雅关闭流程`

- [x] **#4 请求体字段长度缺少严格校验** ✅ 已完成
  - 文件：`src/password-utils.js`、`src/routes.js`
  - 实现：loginName 正则收紧为 `[a-z0-9_]{3,32}` 白名单模式（去掉 `.` 和 `-`，天然阻断注入字符）；登录路由中启用 `assertValidLoginName` 校验；password 新增长度限制 6-128 字符，防 scrypt DoS
  - 提交：`052c629 feat: 为 auth-http 登录接口增加请求体字段严格校验`

- [x] **#5 并发登录控制 / 踢出旧会话** ✅ 已完成
  - 文件：`src/auth-store.js`（auth-http）、`src/kick_subscriber.rs`、`src/server.rs`、`src/core/context.rs`、`src/core/service/core_service.rs`（game-server）、`packages/proto/game.proto`
  - 实现：
    - auth-http：新增 `player-session:{playerId}` Redis 映射；登录时检测旧 session 并销毁，通过 Redis PUBLISH `session:kick:{playerId}` 通知游戏服；destroySession 同步清理映射
    - game-server：新增 kick_subscriber 模块 psubscribe `session:kick:*`；新增 PlayerRegistry 在 AUTH 成功后注册、断连时注销；read loop 改用 tokio::select! 监听 kick_notify，收到通知后发送 SessionKickPush(1209) 并断开；同服务器重复登录直接通过 PlayerRegistry 踢旧连接
  - 联动：mock-client 新增 `kick-session` 场景，覆盖 Phase 1（HTTP 会话失效验证）和 Phase 2（TCP SessionKickPush 推送验证）
  - 提交：`291f746 feat: 实现并发登录控制与踢旧会话主动推送`

- [ ] **#6 IP denylist / allowlist 功能** ⏭️ 暂不实施
  - 现状：只有基于滑动窗口的限流，没有即时封禁 IP 的能力；`security-design.md` 明确要求接入层支持黑白名单
  - 建议：新增 Redis Set `security:denylist:ip`；在限流中间件前检查，命中直接 403；为 admin-api 提供动态添加/移除接口

- [x] **#7 限流响应缺少 Retry-After 头** ✅ 已完成
  - 文件：`src/rate-limiter.js`、`src/routes.js`
  - 实现：`isIpRateLimited()` 返回值改为 `{ limited, retryAfterSeconds }`，通过 ZRANGE 获取滑动窗口最早请求时间戳精确计算剩余秒数；routes.js 在 IP 限流 (429) 和账号锁定 (403) 两处响应中设置 `Retry-After` header
  - 提交：`b970cc9 feat: 为 auth-http 限流响应添加 Retry-After 头`

### P2（用户体验 / 功能完整度）

- [ ] **#8 游客账号绑定/升级接口缺失**
  - 现状：游客登录后无法绑定 loginName + password，换设备后丢失数据
  - 建议：新增 `POST /api/v1/auth/bind-account`，将 `account_type` 从 `guest` 升级为 `password`

- [x] **#9 修改密码接口缺失** ✅ 已完成
  - 文件：`src/routes.js`、`src/mysql-store.js`、`src/password-utils.js`
  - 实现：新增 `POST /api/v1/auth/change-password` 接口；Bearer Token 鉴权后验证旧密码，生成新 salt/hash 写入 MySQL；修改成功后强制踢除所有已有 session（Redis 删除 + Pub/Sub `session:kick:{playerId}` 通知 game-server 断开 TCP）；记录 auth_audit 和 security_audit
  - mysql-store.js 新增 `updatePassword()` 和 `findPasswordAccountByPlayerId()` 方法
  - 测试：`tests/change-password.test.mjs`（9 个用例覆盖鉴权、输入校验、旧密码错误、正确修改、session 踢除、MySQL 禁用）
  - 提交：`868dfc7 feat: 实现修改密码接口 POST /api/v1/auth/change-password`

- [x] **#10 Session 没有续期机制** ✅ 已完成
  - 文件：`src/auth-store.js`
  - 实现：在 `getSessionByAccessToken()` 中新增 `redis.expire()` 调用，每次读取 session 时自动续期 `session:{accessToken}` 和 `player-session:{playerId}` 的 TTL（滑动窗口）
  - 客户端接入要求：客户端在游戏进行期间应每隔 3-5 分钟调用 `GET /api/v1/auth/me` 保持 session 活跃，防止 accessToken 因长时间无 auth-http 交互而过期（TCP 连接不会因此断开，但断线重连时需要有效 token）
  - 此机制对所有 TCP 服务（game-server、chat-server 等）统一有效，各服务无需额外改动

- [ ] **#11 Token 刷新接口 (Refresh Token)**
  - 现状：accessToken 过期后只能重新登录（#10 滑动续期已缓解此问题，客户端定期调 `/auth/me` 即可保活）
  - 建议：登录时额外签发 refreshToken（长 TTL）；新增 `POST /api/v1/auth/refresh` 用 refreshToken 换新 accessToken
  - 备注：#10 已解决主要场景，本项优先级降低，适用于需要离线后长时间恢复登录态的移动端场景

- [ ] **#12 维护模式支持**
  - 现状：无维护模式开关，停机维护时无法向新请求返回维护通知
  - 建议：新增 `MAINTENANCE_MODE` 配置（可通过 Redis 动态切换）；开启后登录接口返回 503 + 维护公告；白名单 IP / GM 账号可绕过

- [x] **#13 健康检查增强** ✅ 已完成
  - 文件：`src/routes.js`
  - 实现：`/healthz` 改为实际探测 Redis PING + MySQL `SELECT 1`；任一失败返回 503；MySQL 未启用时 `checks.mysql` 为 `"skipped"` 不影响判定；响应新增 `checks` 字段向后兼容
  - 提交：`fb8884c feat: 增强 auth-http /healthz 接口检测 Redis 和 MySQL 连通性`

### P3（代码清洁 / 可观测性 / 细节优化）

- [ ] **#14 guest-login 独立限流**
  - 现状：所有路由共享同一个 IP 限流窗口；guest-login 无门槛，容易被刷创建大量游客账号
  - 建议：对 guest-login 设置独立的更严格限流窗口（如 10次/分钟/IP）

- [x] **#15 请求 ID / Trace ID** ✅ 已完成
  - 文件：`src/app.js`、`src/logger.js`
  - 实现：新增 request-id 中间件，从请求 header `X-Request-Id` 读取或自动生成 16 位 hex ID；使用 `AsyncLocalStorage` 注入请求上下文，`log()` 函数自动以 `[requestId]` 为前缀输出；响应 header 始终返回 `X-Request-Id` 供客户端上报追踪
  - 测试：`tests/request-id.test.mjs`（5 个用例）
  - 提交：`fd5e3f0 feat: 为 auth-http 增加 X-Request-Id 请求追踪`

- [ ] **#16 CORS 配置**
  - 现状：无 CORS 中间件；admin-web 或未来 Web 客户端直接调用会跨域失败
  - 建议：增加可配置的 CORS 中间件，通过 `ALLOWED_ORIGINS` 环境变量控制

- [x] **#17 TicketValidator 死代码** ✅ 已完成
  - 文件：`src/rate-limiter.js`
  - 处理：确认 `TicketValidator` 类在整个项目中无任何引用，且使用 CJS `require` 在 ESM 项目中会报错；ticket 验证实际由 game-server Rust 侧通过 Redis 查询完成，该类属于死代码，已直接删除

- [ ] **#18 密码强度校验**
  - 现状：注册/seed 阶段无密码强度要求
  - 建议：账号创建时增加密码强度检查（最小长度、大小写、数字、特殊字符）

---

## 二、优先级总览

| 优先级 | 编号 | 主题 | 核心原因 |
|--------|------|------|----------|
| P0 | #1 | Internal 接口鉴权 | ✅ 已完成 |
| P0 | #2 | 登出接口 | ✅ 已完成 |
| P1 | #3 | 优雅关闭 | ✅ 已完成 |
| P1 | #4 | 字段长度校验 | ✅ 已完成 |
| P1 | #5 | 并发登录控制 | ✅ 已完成 |
| P1 | #6 | IP denylist | 安全设计要求 |
| P1 | #7 | Retry-After | ✅ 已完成 |
| P2 | #8 | 游客绑定 | 用户留存关键 |
| P2 | #9 | 修改密码 | ✅ 已完成 |
| P2 | #10 | Session 续期 | ✅ 已完成 |
| P2 | #11 | Token 刷新 | 移动端场景 |
| P2 | #12 | 维护模式 | 运维便利 |
| P2 | #13 | 健康检查增强 | ✅ 已完成 |
| P3 | #14 | guest-login 限流 | 防刷策略 |
| P3 | #15 | Request ID | ✅ 已完成 |
| P3 | #16 | CORS | Web 端接入 |
| P3 | #17 | 死代码清理 | ✅ 已完成 |
| P3 | #18 | 密码强度 | 注册安全 |

---

## 三、相关文件索引

| 文件 | 职责 |
|------|------|
| `src/app.js` | Express 应用组装、中间件注册 |
| `src/server.js` | 启动入口、shutdown 处理 |
| `src/config.js` | 环境变量解析 |
| `src/routes.js` | 路由与接口定义 |
| `src/auth-store.js` | Session/Ticket Redis 操作 |
| `src/mysql-store.js` | 玩家账号与审计 MySQL 操作 |
| `src/mysql-client.js` | MySQL 连接池与 schema 初始化 |
| `src/redis-client.js` | Redis 连接工厂 |
| `src/rate-limiter.js` | IP 限流、账号锁定 |
| `src/password-utils.js` | 密码哈希、格式校验 |
| `src/game-admin-client.js` | game-server admin TCP 协议客户端 |
| `src/service-discovery.js` | 基于 Redis 的服务发现 |
| `src/metrics.js` | QPS/延迟/在线数指标上报 |
| `src/http-errors.js` | 错误响应工具函数 |
| `src/logger.js` | log4js 日志配置 |

---

## 四、参考文档

- `docs/security-design.md` - 安全设计与分阶段落地建议
- `docs/rate-limit-and-security.md` - 限流与风控现状
- `docs/admin-panel.md` - 管理后台设计
- `.env.example` - 当前已支持的环境变量

---

最后更新：2026-05-09
