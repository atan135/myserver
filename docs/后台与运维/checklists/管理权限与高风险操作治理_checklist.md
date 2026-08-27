# 管理权限与高风险操作治理 Checklist

## 目标

将现有静态管理员 role、分散 token 和写入口一次性替换为资源/动作级授权、高风险确认、审批预留、幂等执行和可恢复操作。覆盖 admin-api、admin-web、game-server 管理口、game-proxy 管理口及其管理 NATS 事件。

## 已确认实施策略

- 本功能在正式上线前一次性切换，不保留旧 role、旧 admin token、旧内部管理认证或旧写接口的运行时兼容分支。
- 现有管理员、审计和测试数据不要求迁移或修复；开发和测试库可按新 schema 重建并写入新的 seed。
- development/test 使用独立 bootstrap 管理员、权限和签名密钥，但必须走与 production 相同的授权、预检、断言和幂等代码，不提供关闭授权的 bypass。
- 不建设通用组织审批工作流；仅保留高风险操作所需的审批状态和最小证据字段。
- 权威设计见 `docs/后台与运维/管理权限与高风险操作治理设计.md`。

## 基础原则

- [x] 服务端强制授权，前端隐藏按钮不能作为安全边界。
- [x] 默认拒绝，权限按最小职责授予。
- [x] 高风险操作必须有 request_id、理由、目标范围和审计。
- [x] 审计记录追加写，不保存密码、token 或完整敏感 payload。
- [x] 不保留旧 role、通用 admin token 或开发/测试关闭授权的 fallback。

## 阶段 1：操作与风险目录

- 开始时间：2026-07-19 09:45:07 +08:00
- 结束时间：2026-07-19 09:45:07 +08:00
- 开发总结：已完成控制面操作盘点和目标风险目录，覆盖 admin-api、game-server、game-proxy 与管理 NATS 事件；明确采用上线前一次性切换，不保留旧认证或测试 bypass。
- 验证记录：已对照 `apps/admin-api` controller/Guard、`apps/game-server/src/admin_server`、`apps/game-proxy/src/admin_server` 与现有安全文档，形成 `docs/后台与运维/管理权限与高风险操作治理设计.md`；未运行服务或测试。

- [x] 盘点账号、角色、资产、广播、封禁、踢人、配置和服务控制操作。
- [x] 为操作标记资源、动作、风险级别、影响范围和当前鉴权点。
- [x] 找出仅在前端校验、共享 token 或缺少审计的入口。
- [x] 定义低、中、高、紧急操作的治理要求。
- [x] 明确首期不建设复杂组织审批工作流引擎。

## 阶段 2：权限模型与策略

- 开始时间：2026-07-19 09:55:12 +08:00
- 结束时间：2026-07-19 14:08:23 +08:00
- 开发总结：auth 数据库新增权限目录、内置角色、范围化角色/直接授权和追加授权审计；admin-api 新增 fail-closed policy service 与原子 bootstrap 授权。现有静态 route guard 保留给阶段 3 一次性切换，新 policy 不读取 legacy role 兜底。
- 验证记录：策略定向测试 9/9、`node --check src/admin-store.js` 与 `npx.cmd tsc --noEmit --project tsconfig.json` 通过；真实 loopback 临时 PostgreSQL capture 生成 auth target，`npm.cmd run db:deploy:validate`、`npm.cmd run db:ci`（51 passed、3 个既有 PostgreSQL guard skip）和 `npm.cmd run db:ci:rebuild` 通过，五个临时库均清理。完整 `npm.cmd run test --workspace admin-api` 在 124 秒工具上限超时且未输出失败断言，未作为通过证据。

- [x] 将粗粒度 role 映射为稳定 permission key。（验证：`db/migrations/auth/20260719100000_add_admin_authorization_policy.sql` 的 `admin_permissions` 目录和 `admin_role_permissions` 内置映射；真实 capture target 绑定该 migration。）
- [x] 支持数据库角色到权限集合，初始内置 viewer/operator/admin/super_admin，不保留静态旧 role 兜底。（验证：migration seed 四个内置角色；`apps/admin-api/src/auth/admin-policy.service.ts` 只查询 role/direct grant 权威表，策略测试覆盖有效授权和失效拒绝。）
- [x] 对字段脱敏、目标世界、目标服务和操作范围表达约束。（验证：migration `admin_policy_scope_is_valid` 与 scope JSON constraints；`admin-policy.service.test.js` 覆盖必填维度、通配符、字段和目标范围拒绝。）
- [x] 权限变更需要授权人、理由、生效/失效时间和审计。（验证：`apps/admin-api/src/admin-store.js` 的 grant/revoke 事务同时写 lifecycle 字段和 `admin_authorization_audit_events`；`admin-store.policy.test.js` 覆盖授权、撤销和回滚。）
- [x] 建立默认拒绝和未知 permission 失败策略。（验证：`AdminPolicyService.authorize` 返回 `UNKNOWN_PERMISSION`、`PERMISSION_INACTIVE`、`SCOPE_REQUIRED` 或 `SCOPE_DENIED`；策略定向测试 9/9 通过。）

## 阶段 3：统一服务端授权

- 开始时间：2026-07-19 14:10:17 +08:00
- 结束时间：2026-07-19 16:26:01 +08:00
- 开发总结：admin-api 管理路由改由数据库 policy guard 统一授权；admin-api 到 game-server/game-proxy 的写操作使用短时 Ed25519 assertion，邮件附件投递改用独立、最小权限的 mail assertion。auth-http 的旧游戏服写入口收口为 `CONTROL_PLANE_ONLY`，运行时不再存在 token-only 管理写或以 `GAME_ADMIN_TOKEN` 作为玩家 debug fallback。
- 验证记录：admin-api 受影响定向集 71/71、auth-http 41/41、mail config/client/recovery 77/77、game-server assertion 10/10、game-proxy 全量 159/159、Node/Rust shared assertion fixture、`npm.cmd run check:generated-proto` 与 `git diff --check` 均通过。未启动业务服务或访问业务库；完整 mock-client protocol 集仍有 1 个基线 AUTH_RES expectation 过期失败，新增 assertion 控制包测试通过。rollout/mock 演练工具的 direct token 参数留待阶段 7 统一迁移。

- [x] admin-api 建立统一 guard/policy 层。（验证：`AdminPolicyGuard` 已替换 9 个受保护 controller 的 `RolesGuard`，无声明权限默认拒绝；策略 guard/JWT/受影响 controller 定向测试 71/71 与 TypeScript 编译通过。）
- [x] game-server 与 game-proxy 管理口验证短时签名操作断言中的调用方身份、权限、目标范围和请求时效。（验证：共享 `admin-operation-assertion-v1` fixture 由 Node 签名验证并被 game-server 10/10、game-proxy assertion 4/4 接受；proxy 全量 159/159，覆盖 payload/path、permission、target、expiry 与 replay 拒绝。）
- [x] 防止 admin-api 通过内部接口扩大当前管理员权限。（验证：`AdminOperationAssertionService.issue` 对当前 actor 与服务端 scope 再次调用 policy；auth-http shutdown/config 改为不连接下游的 `410 CONTROL_PLANE_ONLY`；mail-service 使用独立 mail grant assertion，不能发送通用 GM 包。）
- [x] 批量操作逐项校验范围并限制最大目标数。（验证：`AdminPolicyGuard` 忽略客户端 permission/scope/targetCount，按所有目标生成 scope；策略 guard 测试覆盖混合批量越权，game-server/proxy verifier 对 target/service/instance 与 scope 重新校验。）
- [x] 返回稳定的未认证、无权限、范围越界和状态冲突错误。（验证：admin-api 映射 `UNAUTHORIZED`、`ADMIN_PERMISSION_*`、`ADMIN_SCOPE_*`；Rust assertion 返回稳定 `ADMIN_ASSERTION_*`、`ADMIN_REQUEST_REPLAY` 与 `ADMIN_REQUEST_CONFLICT`，定向测试通过。）

## 阶段 4：高风险执行协议

- 开始时间：2026-07-19 16:28:19 +08:00
- 结束时间：2026-07-19 17:38:55 +08:00
- 开发总结：auth schema 新增操作请求、预检、审批、break-glass 和追加审计模型；admin-api 将当前可达 high/emergency 写路由切换至服务端预检/执行包装，新增受限审批及 break-glass activate/revoke 入口。临时重建工具保留 loopback、`postgres` bootstrap、确认标识和临时库命名/清理约束，同时允许显式受控的本机端口，避免占用既有 5432 实例。
- 验证记录：真实独立 PostgreSQL 17（`127.0.0.1:55432`）应用 auth migration 后 capture 530 objects，target=actual 且无未批准 drift；`npm.cmd run db:ci` 通过（51 passed、3 个既有 opt-in PostgreSQL skip）；受控 55432 实例执行 `npm.cmd run db:ci:rebuild` 通过，五个 `myserver_stage6_*` 临时库全部删除且实例/data dir 已清理；admin-api 定向测试 85/85、`npx.cmd tsc --noEmit --project apps/admin-api/tsconfig.json` 与 `git diff --check` 通过。未启动业务服务、未访问或修改既有业务库。

- [x] 高风险请求强制 request_id、理由和影响预览。（验证：`apps/admin-api/src/operations/admin-high-risk-operation.service.ts` 无 request_id/理由拒绝，缺 nonce 时只调用 `preflight`；GM、maintenance、players ban、admins 和 MyForge 高风险路由均接入；定向测试覆盖预检不执行。）
- [x] 实现预检/执行两步协议，执行绑定预检摘要和短时效 nonce。（验证：`AdminHighRiskOperationService.run` 只在 nonce 与 summary hash 同时存在时 `claimExecution`；`AdminOperationService` 绑定 actor、permission、scope、target、payload 和 reason hash；定向测试覆盖篡改、过期与 replay 拒绝。）
- [x] 对重复执行返回首次结果，防止刷新或超时重试造成二次副作用。（验证：`admin_operation_requests.request_id` 唯一约束和 `AdminStore.claimAdminOperationExecution` 行锁/nonce 消费；高风险协议测试验证 duplicate 仅返回 terminal 状态、handler 仅调用一次。）
- [x] 资产、配置、批量处罚和服务关闭预留审批状态。（验证：migration 的 `admin_operation_approvals` 及 `pending/approved/rejected` 状态；`/api/v1/admin-operations/:requestId/approval` 受 `admin.permissions.manage` 保护并拒绝申请人自审，定向 controller 测试通过。）
- [x] 紧急 break-glass 使用独立权限、短时授权和强审计。（验证：migration 的 `admin_breakglass_grants` 限制不超过 15 分钟且审计事件追加写；`/api/v1/admin-operations/breakglass/activate|:grantId/revoke` 要求 `breakglass.activate`；emergency GM 执行前 `requireActiveGrant` 重新校验 permission、scope 与 target，定向测试通过。）

## 阶段 5：审计与证据

- 开始时间：2026-07-19 17:43:32 +08:00
- 结束时间：2026-07-19 18:09:51 +08:00
- 开发总结：高风险操作审计增加受限查询、导出和 keyset 游标；响应端统一脱敏敏感 reason、凭据字段和非必要长文本。高风险执行结果落库失败后收敛为 execution_uncertain 并返回稳定 503；广播改为保留 payload digest、长度和投递摘要，资产、维护和封禁补齐最小前后状态与业务关联摘要。
- 验证记录：阶段 5 定向 Node 测试 40/40 通过，`npx.cmd tsc --noEmit --project apps/admin-api/tsconfig.json` 与 `git diff --check` 通过；未启动业务服务或访问业务库。

- [x] 审计记录管理员、权限、request_id、理由、目标、结果和 trace_id。（验证：`admin_operation_audit_events` 经 `AdminStore.listAdminOperationAuditEvents` 返回 actor、permission、request、reason、target、result、trace；敏感 reason/summary 在输出时脱敏，存储查询定向测试通过。）
- [x] 对关键操作保存最小必要前后摘要及关联业务流水。（验证：`gm.controller.ts` 广播只记录 digest/长度/投递摘要，资产记录 item delta/ledger reference，`maintenance.controller.ts` 与 `players.controller.ts` 记录前后状态及关联记录；GM 定向测试通过。）
- [x] 审计写入失败时，高风险操作按策略拒绝或进入可靠补偿路径。（验证：`AdminHighRiskOperationService` 在 completion 持久化失败后尝试 `markExecutionUncertain`，并稳定返回 `ADMIN_OPERATION_PERSISTENCE_FAILED` 503；定向测试覆盖 handler 已执行后的不确定状态收敛。）
- [x] 防止管理员修改或删除自己的审计记录。（验证：高风险审计仅由 store 追加写，`admin-store.operation.test.js` 断言操作写路径不发出 audit UPDATE/DELETE；无 audit 写 controller，读取路由强制 `audit.read` policy。）
- [x] 支持按管理员、动作、目标、时间和结果查询导出。（验证：`AuditController` 的 `audit.read` 路由支持 actor/action/target/time/result 过滤、keyset cursor、31 天查询窗口与 7 天/5000 行导出限制；`audit.controller.test.js` 覆盖分页、窗口、cursor 与导出上限。）

## 阶段 6：恢复、限流与告警

- 开始时间：2026-07-19 18:11:35 +08:00
- 结束时间：2026-07-19 18:53:27 +08:00
- 开发总结：高风险预检提供可逆补偿命令或不可逆备份/恢复证据；执行侧新增按管理员、permission、scope 和 request_id 的 Redis 原子限流，依赖不可用一律 fail-closed。break-glass 授权与 critical 安全告警同一事务提交，MyForge 队列新增持久 pause/resume/cancel 与生命周期审计；auth schema、drift target 和 deploy gate 已同步至 pause migration。
- 验证记录：admin-api 阶段 6 定向测试 100/100、`npx.cmd tsc --noEmit --project apps/admin-api/tsconfig.json` 与 `git diff --check` 通过；受控 `127.0.0.1:55432` 临时 PostgreSQL 实际应用 auth migration、capture 530 个对象且 drift 为零，`npm.cmd run db:ci` 通过（51 passed、3 个既有 PostgreSQL opt-in skip），`npm.cmd run db:ci:rebuild` 通过并删除五个 `myserver_stage6_*` 库。未启动业务服务、未访问业务库，临时实例/库/数据目录均已删除。

- [x] 为可逆操作定义补偿命令，不直接回写历史状态。（验证：`admin-operation-recovery.ts` 为维护、封禁和 MyForge 任务返回新的、单独授权的补偿命令并标记 preservesOriginalAudit；MyForge pause/resume/cancel 仅追加生命周期审计。）
- [x] 不可逆操作在执行前显示备份、影响和恢复条件。（验证：`AdminHighRiskOperationService` 预检写入 recovery plan；不可逆 permission 缺 `backupReference` 返回 `ADMIN_OPERATION_BACKUP_REFERENCE_REQUIRED`，定向测试通过。）
- [x] 按管理员与操作类型限流，批量任务支持暂停和取消。（验证：`AdminOperationSafetyService` 的 Redis Lua key 绑定 actor/permission/scope/request_id，allow/deny 均幂等；`MyforgeStore` 持久 `paused` 状态，暂停任务不能派发且 pause/resume/cancel 追加 audit，定向测试通过。）
- [x] 异常频率、越权尝试和 break-glass 使用产生安全告警。（验证：rate-limit、nonce replay、下游 assertion、持久化失败使用 `security_audit_logs`；`AdminPolicyGuard` 已审计越权，break-glass grant 与 critical alert 同事务，成功与失败回滚测试通过。）
- [x] 管理依赖不可用时禁止静默降级为放行。（验证：Redis eval 或 security audit 不可用返回稳定 503 并在 claim/side effect 前拒绝；MyForge store/control-plane 错误上抛，admin-api 定向 100/100 通过。）

## 阶段 7：前端、测试与文档

- 开始时间：2026-07-19 18:55:04 +08:00
- 结束时间：2026-08-27 10:26:40 +08:00
- 开发总结：完成高风险控制面前端、联调工具、注册表演练和严格发现配置的迁移复核；修正旧角色测试 fixture、注册表内存演练替身以及活动帮助文档中的本地 fallback 标注，并补齐不存在玩家先校验的控制器路径。
- 验证记录：用户已确认允许启动测试；admin-api core 208/208、business 166/166、admin/rollout/registry 97/97、mock-client 20/20、admin-web operations 18/18 全部通过；admin-api TypeScript 编译、admin-web build、共享协议生成检查、`npm run fmt:rust:check`、`git diff --check` 和 discovery config CLI 全部通过。2026-08-27 在隔离临时 PostgreSQL/Redis namespace/NATS 和 `stage7-game-001` registry 实例上完成本地真实受控联调：广播、封禁、drain 开关均走 preflight；封禁与 drain 由独立合成管理员审批；重复执行返回首次终态；玩家状态、drain 状态、操作审计和脱敏安全日志已核验；未执行 shutdown 或 MyForge 外部任务。

- [x] admin-web 根据权限展示操作并处理预检、确认和执行状态。（验证：登录/`auth/me` 返回服务端 effective permissions；`admin-web/src/operations/high-risk.js` 复用 request_id、nonce 和 summary hash，GM、维护、封禁与 MyForge 创建接入，前端 build 与操作测试 4/4 通过。）
- [x] 覆盖水平越权、垂直越权、过期 token、重放和参数篡改测试。（验证：`admin-policy.guard.test.js` 覆盖横向 target 与 legacy role 垂直越权，`jwt-auth.guard.test.js` 覆盖过期 token；high-risk 既有篡改/过期/replay 测试及本轮定向集 29/29 通过。）
- [x] 覆盖审计失败、下游超时、重复请求和部分批量失败。（验证：high-risk protocol 测试覆盖审计持久化失败、重复终态、部分失败和新增 downstream timeout=execution_uncertain；阶段 6 safety 测试覆盖 audit 依赖 fail-closed。）
- [x] 更新管理员账号、权限目录、高风险操作和应急流程文档。（验证：`docs/后台与运维/管理后台设计.md`、`管理权限与高风险操作治理设计.md` 同步数据库 policy/预检契约，新增 `高风险操作联调前置清单.md` 明确依赖、影响、中止和回滚门槛。）
- [x] 将现有管理类测试、GM 样例和 rollout 演练一次性迁移到新 bootstrap 身份、签名断言和预检执行 helper，不保留旧认证样例。（验证：`RolesGuard` 运行时文件与 provider 已删除；registry drill、rollout transfer、mock-client control 定向测试通过；`GameServerTransferClient`/`ProxyAdminClient` 无断言写请求 fail-closed；mock-client 不再解析旧 admin token 参数。）
- [x] 真实高风险联调前列出依赖与影响并等待用户确认。（验证：已阅读并执行 `docs/后台与运维/高风险操作联调前置清单.md` 中的依赖、影响、中止和回滚门槛；2026-08-27 用户确认允许启动测试，并完成本地隔离环境受控联调；临时服务、数据库和 Redis namespace 已清理。）

## 最终完成定义

- 开始时间：2026-07-19 09:45:07 +08:00
- 结束时间：2026-08-27 10:26:40 +08:00
- 验收总结：管理权限、高风险预检/执行、追加审计、恢复与限流告警、控制面迁移、测试文档及一次本地真实隔离多服务受控联调均已完成复核；Linux/生产拓扑、真实玩家影响范围和发布验收仍未完成，需在对应测试/预发环境按前置清单另行执行。

- [x] 所有管理写操作都有服务端权限与范围校验。
- [x] 高风险操作可预览、幂等、审计并具备恢复说明。
- [x] 权限或审计依赖失败不会扩大权限。
- [x] 越权、重放和异常管理行为可检测。
