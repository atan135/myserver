# 活动模块07：安全可靠性与首期验收 Checklist

## 目标

对阶段一公共能力和两个活动类型进行跨模块安全、可靠性、可观测性和端到端联调验收。发现问题时回到对应活动模块清单修复，并在本清单记录复验。

## 基础原则

- [x] 不因测试方便关闭鉴权、唯一约束、幂等、审计或限流。（验证：真实联调保持正式鉴权、RBAC、幂等、审计和限流；一次临时关闭限流的准备进程在任何业务请求前即被主代理停止并撤销配置，正式请求命中 `IP_RATE_LIMITED` 后按正常窗口继续。）
- [x] 真实联调前说明 PostgreSQL、Redis、NATS、auth-http、game-server、admin-api、admin-web 和 mock-client 依赖，并等待用户确认后启动。（验证：用户明确“确认联调”后才启动隔离 Redis/NATS、双 game-server、game-proxy、auth-http、admin-api、admin-web、mail-service、match-service 和 mock-client；默认端口进程未触碰。）
- [x] 测试/预发/线上服务间访问遵循 service registry。（验证：隔离 registry 前缀发现双 game-server，proxy `/readyz=200` 且日志持续 `endpoint_count=2`；缺少 match-service 时严格发现门禁先返回 503，补齐注册后才执行玩家主链。）

## 引用文档

- [活动功能总纲](../活动功能总纲.md)：阶段一最终完成定义、安全、恢复、可观测性和多实例要求。
- [安全设计](../../../安全与监控/安全设计.md)：鉴权、重放、反刷、敏感操作和审计要求。
- [限流与安全现状](../../../安全与监控/限流与安全现状.md)：限流实现、风险现状和验证边界。
- [监控设计](../../../安全与监控/监控设计.md)：指标、采集、低基数标签和故障观测要求。
- [游戏服务安全分层与敏感操作处理指南](../../../安全与监控/游戏服务安全分层与敏感操作处理指南.md)：游戏服玩家入口和后台高风险动作分层。
- [统一资产事务与奖励交付运行边界](../../背包与物品/统一资产事务与奖励交付运行边界.md)：资产流水、unknown 恢复、邮件 fallback 和对账。
- [服务注册中心设计](../../../周边服务/服务注册中心设计.md)：多实例发现、严格环境和联调入口。
- [高风险操作联调前置清单](../../../后台与运维/高风险操作联调前置清单.md)：真实联调前置条件和授权边界。

## 阶段 1：协议、数据库与配置验收

- 开始时间：2026-08-22 09:08:01 +08:00
- 结束时间：2026-08-22 15:44:36 +08:00
- 开发总结：已完成服务端协议、版本/摘要、缓存快照、类型目录和 PostgreSQL 五库空库验收；外部 `mybevy` 客户端同步属于独立兼容性检查，不作为本轮服务端联调门禁。
- 验证记录：`check:proto:server` 通过；活动 Node 测试 32/32、mock-client 19/19、game-server activity 53/53、admin-api TypeScript 检查通过。`npm run db:ci:rebuild` 在本机 PostgreSQL 创建五个随机 `myserver_stage6_*` 空库，全部 migration、history/checksum、关键表和 catalog drift 通过后自动删除；game target/actual 均为 625 objects、0 unapproved drift。

- [x] 运行协议生成和兼容性检查。（验证：`npm run check:proto:server` 覆盖兼容基线、server-only 生成漂移、breaking、路由和 fixture 检查；外部 mybevy 同步不属于本轮服务端联调范围。）
- [x] 在空库执行初始化/迁移，检查活动表、约束、索引和审计字段。（验证：受保护 `npm run db:ci:rebuild` 五库 migration/postflight 全部通过并删除 5 个随机临时库；game `20260822150000` target/actual catalog 均为 625 objects、manifest 一致且 0 unapproved drift，活动契约 8/8 通过。）
- [x] 验证草稿、发布版本、缓存快照和 schema 版本一致。（验证：admin-api 拒绝外层/类型 schema 版本不一致并生成稳定 config digest；game-server cache 校验摘要、schema 和 Redis 快照 identity；Node 32/32 与 Rust activity 53/53 通过。）
- [x] 验证未知类型、未知版本、非法阶段和非法奖池无法发布。（验证：`activity-control.service.test.js` 绕过草稿入口注入四类非法快照，publish preflight 均返回 `ACTIVITY_PRECHECK_FAILED`。）
- [x] 检查类型独有代码确实是同一 types/ 目录下的同级文件。（验证：`tests/activity/activity-contract.test.mjs` 枚举 game-server/admin-api/admin-web types 目录并断言仅含 login_reward/lottery 同级类型文件，公共 Activities 视图无类型专属分支。）

## 阶段 2：安全与反刷验收

- 开始时间：2026-08-22 09:28:43 +08:00
- 结束时间：2026-08-22 11:09:58 +08:00
- 开发总结：已收紧玩家请求指纹、并发幂等、多维限流和后台越权边界；设备维度为 auth-http 服务端生成并签入 ticket 的 session-stable opaque subject，不宣称为物理设备指纹。
- 验证记录：`game-server` 618/618、`game-proxy` 171/171、`auth-http` 106/106、`admin-api` 180/180、`mock-client` 14/14；活动契约 6/6；auth/admin TypeScript 检查通过；`check:proto-routing` 0 diagnostics；`git diff --check` 通过。

- [x] 验证伪造角色、活动、阶段、奖励、概率和进度无效。（验证：活动 handler 仅从已鉴权 session 构造角色/账号上下文；mock-client 契约断言请求不包含角色、账号、奖励、权重、概率和进度字段，14/14 通过。）
- [x] 验证跨角色/跨活动复用 request_id、重放和快速重复请求。（验证：`ActivityEngine` 以角色级 request key 绑定活动/版本/动作/阶段指纹，跨活动复用返回 `REQUEST_FINGERPRINT_CONFLICT`，不同角色隔离，同指纹重放返回原结果并绕过 action limiter；`game-server` 618/618 通过。）
- [x] 验证并发登录、阶段领取和抽奖不会重复发奖或突破次数。（验证：并发 game-entry 只推进一次；领奖 coordinator 并发只调用一次 delivery 且 revision=1；不同抽奖请求并发只有一次成功交付，相同请求在执行中返回 processing。）
- [x] 验证角色、账号、IP、设备、活动和动作限流。（验证：`ActivityRateLimitPolicy` 定向测试覆盖角色、账号、ticket credential、session-stable device subject、活动和动作；`game-proxy` 真实前端 IP 共享消息桶跨 TCP/KCP 连接、按 IP 自身窗口重置并定期清理，171/171 通过。）
- [x] 验证管理员越权编辑、发布、下线和记录查询被拒绝并审计。（验证：活动路由使用服务端 `activity` policy scope，忽略 body/query 伪造目标；write/publish/offline/records.read 越权均 fail-closed 并写入不含 request_id、奖池或伪造目标的脱敏审计，admin-api 180/180 通过。）

## 阶段 3：故障恢复与多实例

- 开始时间：2026-08-22 11:12:10 +08:00
- 结束时间：2026-08-22 18:11:06 +08:00
- 开发总结：完成真实双 game-server 配置/领取一致性、活动缓存有界超时与 PostgreSQL 真值回退、Redis 长故障 lease fail-stop 和恢复演练；Windows Redis 冷重启约 6 秒超过 worker lease 容忍窗口，服务按设计安全退出，不宣称重启期间持续存活。
- 验证记录：game A 首次领取后 game B 读取同一 login v2/lottery v1，并对同 request_id 返回 `duplicate=true/state_revision=2`。隔离 Redis 停机期间已鉴权连接的 list/detail/progress 和 claim replay 均成功，日志记录 3 次 `cache_operation=read failure_reason=timeout timeout_ms=250`；长期中断后 registry readiness 503、lease 丢失并停止 accept loop。活动 Rust 85/85、`cargo check` 和隔离构建通过。

- [x] 模拟响应丢失、进程中断、数据库回滚和奖励交付 unknown。（验证：重启 engine 查询原抽奖请求、领奖 unknown 重启恢复、事务失败保持原子性和通知失败不回滚资产的定向测试均通过；game-server 全量 642/642。）
- [x] 验证恢复只使用原 request_id、版本和奖励快照，不重新随机/发奖。（验证：`ActivityClaimCoordinator`、`PgActivityClaimStore` 和抽奖持久运行时保存原始 request/version/order/result；重启与 retryable 测试复用原选择和原订单，双 coordinator/engine 只交付一次。）
- [x] 使用至少两个 game-server 实例验证配置刷新和领奖一致性。（验证：A 首次领取 `duplicate=false/state_revision=2`；B 读取相同已发布版本和已领取状态，对同一 request_id 重放返回 `duplicate=true/state_revision=2`，proxy registry 同时发现两个健康 endpoint。）
- [x] 验证 Redis 不可用时状态仍以数据库为准，缓存失败可观测。（验证：真实停止隔离 Redis 后，已鉴权连接的 list/detail/progress 和 claim replay 在 lease 窗口内返回 PostgreSQL 真值，重放 `duplicate=true/revision=2`；日志记录 3 次固定低基数 `cache_operation=read/failure_reason=timeout/timeout_ms=250`。长期中断仍触发 global-id lease fail-stop。）
- [x] 验证容量不足、邮件 fallback 和 manual_review 可查询/恢复。（验证：容量不足保留原订单供 reconcile；邮件 outbox 使用确定性 mail ID、租约、退避和确认后 delivered；终态失败写入可查询 manual_review 且重放不再交付，settlement 8/8、dispatcher 6/6。）

## 阶段 4：端到端联调

- 开始时间：2026-08-22 13:33:03 +08:00
- 结束时间：2026-08-22 18:11:06 +08:00
- 开发总结：完成后台登录、草稿、预检、ETag 发布、正式玩家登录奖励和免费/券抽奖全链；联调发现并修复活动 RBAC migration、可信 game-entry 接线、ItemTable 绑定预检、奖励账本生产写入和 Redis cache 无界等待五类真实缺口。
- 验证记录：隔离 admin-api 发布 login_reward v2 与 lottery v1；完整角色执行登录领取、免费抽、券抽及同 request 重放，DB 为 3 claims、3 reward ledgers、2 draw results、5 asset ledgers，券 5003 净变化 0。GM 发券经过预检、备份引用、独立审批和执行。admin-web 隔离实例返回 200；因仓库无浏览器驱动且 Vite proxy 固定 3001，活动写操作通过相同鉴权/policy 的隔离 admin-api 完成。

- [x] 通过后台创建并发布登录奖励活动。（验证：正式 admin 登录后完成 draft -> preflight -> ETag publish，修复不可交付 v1 后通过 fork 发布同活动 v2，Redis refresh sent。）
- [x] 通过 mock-client 完成登录事件、详情、阶段领取和重复领取。（验证：经 auth-http -> game-proxy -> game-server 完成 Auth/list/detail/progress/claim；首次 granted，同 request 重放 `duplicate=true`，game-entry 后 `logged_in/cumulative=1/consecutive=1`。）
- [x] 通过后台创建并发布随机抽奖活动。（验证：正式后台创建、预检并发布 lottery v1，双实例均加载相同版本。）
- [x] 通过 mock-client 完成免费/道具券抽奖、重复请求和结果查询。（验证：免费抽与券抽首次均 granted，同 request replay 均 `duplicate=true`；GM 券发放经过高风险预检、恢复引用、独立审批和执行，5003 净变化 0。）
- [x] 核对活动版本、玩家状态、领奖、抽奖、奖励流水和审计记录。（验证：只读 SQL 核对 login_reward v2/lottery v1、完整角色 3 claims/3 reward ledgers/2 draw results/5 asset ledgers；auth 5 项活动权限、GM 独立审批和活动发布/读取审计均存在。）
- [x] 核对 push 只在资产提交成功后发送。（验证：登录奖励仅在 delivery `Applied` 后完成 grant ledger/claim；抽奖 notifier 位于 `lottery_states.grant` 提交成功之后，资产或账本失败进入 retryable/reconciliation 且不发送成功通知；事务契约测试 12/12。）

## 阶段 5：可观测性与文档归档

- 开始时间：2026-08-22 14:26:07 +08:00
- 结束时间：2026-08-22 18:11:06 +08:00
- 开发总结：完成低基数指标、缓存超时结构化告警、实现文档和最终清单归档；联调进程、临时凭据/日志/Redis 数据及三套命名 E2E 数据库均已清理，默认端口进程未触碰。
- 验证记录：Rust activity 85/85、admin-api core 211/211 + business 161/161 + control 19/19、TypeScript、活动/数据库组合 45 pass + 1 expected skip、受保护五库空库重建和 `git diff --check` 通过；Redis 真实故障日志含固定 `read/timeout/250ms` 字段。

- [x] 增加低基数请求、成功、资格失败、重复、限流、发奖延迟、恢复 backlog 和刷新失败指标。（验证：`MetricsCollector` 为固定 list/detail/progress/claim/draw/game_entry/unknown 动作输出五类窗口计数；登录领奖 coordinator 与 lottery apply_draw 记录发奖延迟；邮件 outbox 聚合可恢复 backlog；缓存 read/write/refresh 失败分类；全量 645/645。）
- [x] 禁止角色 ID、request_id、claim_id 和 token 作为指标标签。（验证：动作和缓存原因均为编译期固定枚举，指标字段测试扫描 character/account/role/activity_id/version/request_id/claim_id/token 均不存在，定向 1/1 通过。）
- [x] 同步活动总纲、协议、后台、数据库和部署依赖说明。（验证：已更新活动总纲/README、玩家协议、admin-api 控制面、管理后台、整体架构、监控设计和数据库初始化说明，明确当前实现及未验收边界。）
- [x] 记录验证命令、环境前置条件、已知限制和后续阶段。（验证：活动 README 记录 Windows 离线命令、PostgreSQL/Redis/NATS/service registry 与应用依赖、临时数据清理要求；监控文档记录 5 秒窗口与实例级 backlog gauge 限制；本清单补录空库、双实例、Redis 中断和服务端 E2E 最终证据，外部 mybevy 兼容仍为独立检查。）
- [x] 完成的实现清单按仓库约定归档，保留最终验收证据。（验证：本清单归档至 `docs/游戏服与接入层/活动系统/checklists/活动模块07_安全可靠性与首期验收_checklist.md`，记录真实端口隔离、DB 数量、双实例、Redis fallback、fail-stop 边界和测试命令。）

## 最终完成定义

- 开始时间：2026-08-22 09:08:01 +08:00
- 结束时间：2026-08-22 18:11:06 +08:00
- 验收总结：活动首期两种类型完成后台配置、玩家操作、资产交付、append-only 奖励账本和审计闭环；主要安全、并发、重试、恢复、双实例和 Redis 故障场景均有自动化与真实联调证据。Redis 长中断时全局 ID/registry lease 会使服务 fail-stop，活动请求在 lease 窗口内可回退 PostgreSQL，但不承诺 Redis 冷重启期间进程持续存活。

- [x] 两种活动完成后台配置、玩家操作、奖励交付和审计闭环。（验证：login_reward v2 与 lottery v1 真实发布；完整角色 3 claims/3 reward ledgers/2 draw results/5 asset ledgers，重复请求无重复发奖。）
- [x] 主要攻击、并发、重试、故障恢复和多实例场景有验证记录。（验证：阶段 2 自动化覆盖伪造、重放、并发和多维限流；阶段 3/4 真实覆盖双服重放、Redis cache fallback、lease fail-stop、GM 独立审批与账本对账。）
- [x] 未发现公共入口包含类型独有逻辑或任务模块与活动混用。（验证：跨层契约枚举 types/ 同级文件且公共 Activities 视图无类型分支；活动配置与 CharacterProgress/任务表保持独立，活动契约 12/12。）
