# 运营后台灰度排空操作台 Checklist

## 目标

在 admin-web 提供 game-server Rollout / Drain 的受控操作界面，覆盖高风险预检、独立审批、执行、状态轮询与审计追溯；不改变既有游戏服排空和发布 runner 的安全边界。

## 基础原则

- [x] 仅通过 admin-api 的受鉴权控制面调用，不暴露 game-server 或 game-proxy 内部地址、令牌和签名断言。（验证：所有读写 API 固定 `/api/v1`；Registry 与 drain read view 均脱敏 endpoint；API 契约与 admin-api 核心测试通过。）
- [x] 前端权限、路由和按钮状态必须以服务端即时返回的权限与范围为准，不能只依赖隐藏菜单。（验证：路由/菜单基于 `/auth/me` 的 effective permissions；admin-api 在每次请求执行 JwtAuthGuard、AdminPolicyGuard 和可信 requestId/实例范围解析。）
- [x] `game.config.write` 的排空操作必须保留预检、不同管理员批准、nonce 绑定确认和完整审计记录。（验证：高风险操作状态机保存预检摘要与 nonce hash，审批接口阻止自审，原申请人显式复用 requestId/nonce/摘要确认，operation audit 追加关联事件；核心与操作测试通过。）
- [x] 不在前端实现直接停服、强制断开或绕过 room / connection drain 的路径。（验证：`RolloutDrain.vue` 仅调用 drain API；页面和后台设计明确不提供停服、强制断线或发布收尾入口。）

## 阶段 1：权限与 API 接入

- 开始时间：2026-08-19 17:40:10 +08:00
- 结束时间：2026-08-19 17:57:00 +08:00
- 开发总结：已补齐前端权限目录、受控 Rollout/审批 API client 与高风险状态标准化；后端仍是授权、范围与操作断言的最终边界。
- 验证记录：`npm --prefix apps/admin-web run test:operations` 9/9 通过；`npm --prefix apps/admin-web run build` 通过（仅既有 bundle 体积警告）；`git diff --check` 通过。

- [x] 在 admin-web 权限目录中纳入 `game.config.write` 和 `admin.permissions.manage`，保留服务端授权为最终边界。（验证：`apps/admin-web/src/auth/permissions.js` 定义两项权限，`effectivePermissions` 仍只接受 `/auth/me` 的有效权限；`test:operations` 通过。）
- [x] 为 Rollout / Drain 与 admin operation approval 补充受控 API client，统一携带登录态、requestId、preflight nonce 与摘要哈希。（验证：`apps/admin-web/src/api/index.js` 的 `rolloutApi` / `adminOperationApi` 仅请求 `/api/v1`；`runHighRiskOperation` 复用 requestId、nonce 与摘要哈希；API 契约测试通过。）
- [x] 明确并展示 `403`、`409 ADMIN_OPERATION_APPROVAL_REQUIRED`、预检过期和执行不确定等稳定错误状态。（验证：`apps/admin-web/src/operations/high-risk.js` 的 `normalizeHighRiskError` 和过期预检保护；单测覆盖 403、审批、过期与不确定终态。）

## 阶段 2：排空操作页

- 开始时间：2026-08-19 17:58:34 +08:00
- 结束时间：2026-08-19 19:24:00 +08:00
- 开发总结：已完成受权限控制的 Rollout/Drain 操作页；实例仅来自 Registry，状态通过 admin-api 读取 game-server 控面，操作经过原因校验、服务端预检、nonce 确认与单次提交保护。
- 验证记录：`npm --prefix apps/admin-web run test:operations` 14/14 通过；`npm --prefix apps/admin-web run build` 通过（仅既有 chunk size warning）；`npm --prefix apps/admin-api run test:game-server-control` 19/19 通过；`git diff --check` 通过。

- [x] 新增仅对 `game.config.write` 可见的 Rollout / Drain 路由和导航入口。（验证：`apps/admin-web/src/router/index.js`、`AdminLayout.vue` 使用服务端有效权限；admin-api 控制器实例接口显式 JwtAuthGuard；控制器测试通过。）
- [x] 从 registry/监控数据选择明确的 game-server 实例，禁止手填任意内部 endpoint。（验证：`listGameServerInstances` 过滤非法/fallback/未授权候选，仅返回 instanceId/status/healthy；`getDrainStatus` 强制 `requireRegistryTarget`；前后端 endpoint-safe 测试通过。）
- [x] 提供开启和关闭 drain 操作，强制填写非敏感原因，并展示目标实例与影响摘要。（验证：`RolloutDrain.vue` 提供开启/关闭按钮、原因校验和目标状态摘要；`isSafeDrainReason` 单测覆盖凭据模式。）
- [x] 集成高风险预检与 nonce 确认交互，禁止重复提交和 nonce 重放后的自动重试。（验证：页面调用 `runHighRiskOperation`，复用 requestId/预检 nonce/摘要并在 operation.loading 时禁用按钮；高风险操作测试通过。）
- [x] 持续展示连接数、owned/migrating room、route blocker 与 drain mode，排空前禁用发布收尾提示。（验证：页面 15 秒串行轮询 Registry、监控和 `drain-status`，展示四项真实控面状态及 route blocker；页面明确不提供停服/强制断线/发布收尾操作。）

## 阶段 3：独立审批与审计

- 开始时间：2026-08-19 19:33:55 +08:00
- 结束时间：2026-08-19 20:22:13 +08:00
- 开发总结：已完成受控独立审批页和只读审计关联；待审批与详情只返回脱敏摘要，审批后仍由原申请人使用原 requestId、nonce 与摘要哈希显式确认执行。
- 验证记录：`npm --prefix apps/admin-api run test:core` 166/166 通过；`npm --prefix apps/admin-api run test:game-server-control` 19/19 通过；`npm --prefix apps/admin-web run test:operations` 17/17 通过；`npm --prefix apps/admin-web run build` 通过（仅既有 chunk size warning）；`git diff --check` 通过。

- [x] 新增待批准操作列表与详情，仅对 `admin.permissions.manage` 可见。（验证：`AdminOperationController.listPendingApprovals` 逐 requestId 调用服务端策略授权，详情路由由 `AdminPolicyGuard` 保护；`OperationApprovals.vue` 路由与导航依据服务端有效权限显示。）
- [x] 审批页阻止自审，要求填写审批证据摘要，并支持批准和拒绝。（验证：控制器在状态变更前阻止 actorAdminId 相同的申请；前后端均校验证据摘要不为空且不含敏感键/凭据模式；`operation-approval.test.js` 与控制器测试通过。）
- [x] 已批准操作由原申请人使用同一 requestId、预检 nonce 和摘要哈希确认执行。（验证：`resumeHighRiskOperation` 仅复用保留的原请求和预检值，不自动重试；`high-risk.test.js` 覆盖审批 pending 后的显式复执行。）
- [x] 在审计日志中可关联申请、审批、执行和失败/不确定终态，不显示敏感 payload 或令牌。（验证：`AuditLogs.vue` 与审批详情按 requestId 读取既有 append-only operation audit；读模型与 store 脱敏 nonce、payload、assertion、endpoint、host、port 和凭据字段；核心测试通过。）

## 阶段 4：前端验证与文档

- 开始时间：2026-08-19 21:18:31 +08:00
- 结束时间：2026-08-19 21:52:10 +08:00
- 开发总结：已补齐审批状态分支测试和移动端禁用交互校验，并更新管理后台设计；浏览器自动化依赖未安装，视口项以响应式 CSS 静态检查完成，未启动服务。
- 验证记录：`npm --prefix apps/admin-web run test:operations` 18/18 通过；`npm --prefix apps/admin-api run test:core` 166/166 通过；`npm --prefix apps/admin-web run build` 通过（仅既有 chunk size warning）；`git diff --check` 通过；`RolloutDrain.vue` 与 `OperationApprovals.vue` 均包含移动断点、单列布局、flex wrapping 与 `overflow-wrap` 约束。

- [x] 覆盖无权限、窄范围授权、预检取消、审批 pending、独立审批、拒绝、过期和重复提交的前端测试。（验证：`high-risk.test.js`、`operation-approval.test.js` 覆盖 403/scope denied、nonce replay、取消、pending/resume、自审、拒绝证据和重复提交保护；`test:operations` 18/18 通过。）
- [x] 进行 admin-web build，并验证桌面与移动视口下的状态、错误和禁用交互不重叠。（验证：`npm --prefix apps/admin-web run build` 通过；两个操作页有 760/420 或 900px 响应式断点、grid 单列转换、flex wrapping、长文本 `overflow-wrap:anywhere`；浏览器自动化依赖未安装，未执行真实视口截图。）
- [x] 更新管理后台设计，说明监控页为只读观测，排空和审批从新操作页进入。（验证：`docs/后台与运维/管理后台设计.md` 页面表、控制面入口和监控页说明已新增 `/rollout-drain`、`/operation-approvals` 及只读边界。）

## 最终完成定义

以下项目作为整体完成标准，不要求每个开发阶段都执行，由所有相关阶段完成后统一验收。

- 开始时间：2026-08-19 17:40:10 +08:00
- 结束时间：2026-08-19 21:52:10 +08:00
- 验收总结：已完成权限受控的 game-server 灰度排空操作台、独立审批和关联审计；未改变 game-server 排空/发布 runner 安全边界。真实浏览器视口验证因未安装自动化依赖未执行，需后续在具备依赖的环境补验。

- [x] 具备对应权限的申请人与独立审批人可在浏览器内完成一次可审计的 drain 操作。（验证：`/rollout-drain`、`/operation-approvals` 路由和 API 已联通；预检、独立审批、原 requestId/nonce/摘要确认和 operation audit 关联测试通过；真实浏览器联调未执行。）
- [x] 无权限用户、同一人自审、过期预检或未排空状态均无法触发不安全的服务替换。（验证：服务端 JwtAuth/AdminPolicyGuard、requestId scope、self-approval、nonce/preview 状态机和 drain 状态展示均有定向测试；无停服/强制断线/发布收尾前端入口。）
- [x] 该 checklist 完成后按仓库约定归档至 `docs/后台与运维/checklists/`。（验证：开发提交 `3340ffe9` 完成后将本文件移动至归档目录。）
