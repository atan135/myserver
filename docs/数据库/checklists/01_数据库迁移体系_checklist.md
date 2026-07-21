# 数据库迁移体系 Checklist

## 目标

建立 PostgreSQL Schema 的版本化迁移、校验、回滚和部署准入体系，逐步替代仅靠 `db/init.sql` 描述目标态的方式。覆盖账号库、游戏库及各独立服务数据库；本清单不修改具体游戏业务模型。

当前状态：已完成。

启动条件：首次出现不可通过删库重建处理的持久数据、多人共享数据库、独立测试/预发环境或需要滚动升级的正式部署。在启动条件出现前，继续直接维护 `db/init.sql`，数据库结构变化后通过删库重建验证。

## 基础原则

- [x] 迁移按数据库和服务归属管理，禁止多个服务无边界修改同一组表。（验收：`db/migrations/{auth,game,chat,announce,mail}`、各 database `logicalOwner` 与五库部署顺序已由 `db:deploy:validate` 静态校验。）
- [x] 已部署迁移不可原地改写；修正必须新增迁移。（验收：`tools/db.js` 在 SQLx 执行前逐项比对 `_sqlx_migrations` 的 version/description/checksum/success；Stage 7 checksum 篡改实测返回 EXIT 4。）
- [x] 生产迁移默认向前兼容，破坏性变更采用 expand/migrate/contract。（验收：Stage 4 模板、安全元数据和 Stage 7 实库 expand/contract 恢复演练均已通过。）
- [x] 每个阶段独立验证、独立提交；执行真实数据库测试前先确认依赖。（验收：阶段 2-7 分别提交 `402c5ff`、`eec50fa`、`35f4bbd`、`72f8db6`、`c0e888a`、`d449662`；所有实库演练均由显式环境开关和用户授权保护。）

## 阶段 1：现状盘点与工具选型

- 开始时间：2026-07-18 14:31:12 +08:00
- 结束时间：2026-07-18 14:49:16 +08:00
- 开发总结：完成五个 PostgreSQL 数据库的对象、所有权、消费者、启动期 DDL、初始化与测试入口盘点；初始比较 Flyway、Liquibase、Atlas 和自研 Node runner，后续结合既有 Rust + Node.js 技术栈复核，确定以 SQLx CLI 作为执行内核、仓库提供薄包装层，并固定后续目录、版本、凭据、退出码、脱敏与回填边界。
- 验证记录：按 `db/init.sql` 分库重算得到 auth 11 表/29 索引、game 15 表/49 索引/2 函数/5 触发器、chat 3 表/6 索引、announce 1 表/4 索引/1 函数/1 触发器、mail 4 表/13 索引/1 函数/2 触发器，与设计文档一致；文档本地链接存在，`git diff --check` 通过，未运行 PostgreSQL 或启动服务。

- [x] 列出 `db/init.sql`、各服务 `db/init.sql` 的数据库、Schema、表、索引、触发器和所有者。（审核：`docs/数据库/数据库迁移体系设计.md` 按 auth/game/chat/announce/mail 列出对象数量、表级 owner/消费者、函数、触发器和重复/冲突来源；根 SQL 对象重算一致。）
- [x] 标注跨库连接、启动脚本、初始化脚本和测试数据入口。（审核：设计文档登记 auth/admin 双连接池、六个启动期 DDL、reset/init/seed 入口及 characters/mail/真实可靠性测试入口。）
- [x] 比较 Node.js 与 Rust 均可调用的迁移方案，记录事务、锁、checksum 和 Windows 支持。（审核：设计文档比较 SQLx CLI、Flyway、Liquibase、Atlas 和自研 Node runner，明确事务、锁、history/checksum、baseline、drift、Windows 与许可边界，并选定 SQLx CLI。）
- [x] 确定迁移目录、版本命名、数据库连接和凭据注入规范。（审核：设计文档定义 `db/bootstrap|config|migrations|schema|backfills|seeds`、UTC SQLx 文件名、五库 migration 环境变量、角色权限和稳定退出码。）
- [x] 明确迁移工具不负责业务数据修复和大表异步回填。（审核：设计文档单列 DDL 与业务数据边界，大表回填进入 `db/backfills` 独立状态与审计，不写入 SQLx migration history。）

## 阶段 2：迁移骨架与版本表

- 开始时间：2026-07-18 15:57:37 +08:00
- 结束时间：2026-07-18 16:12:43 +08:00
- 开发总结：建立五库 SQLx migration 目录、数据库和预编译 CLI 制品 manifest、Node/PowerShell 统一入口及根 npm 命令；实现 `status`、`up`、`validate` 的稳定退出码、存量未基线熔断、版本/checksum 校验、凭据脱敏和持久审计。未登记受批准制品时所有需要 SQLx 的路径返回 `8`，不回退 `PATH` 或现场安装，也不提供强制 baseline。
- 验证记录：`node tools/run-node-tests.js tests/db/db-cli.test.mjs` 12/12 通过，覆盖 Node/PowerShell 退出码与单行 JSON、命令参数、五库顺序、脱敏、制品 hash、文件命名、存量库熔断、未批准制品、未初始化 status 和审计失败；`node --check tools/db.js`、`git diff --check` 通过。未运行 PostgreSQL、SQLx 二进制、服务、并发锁或 checksum 实库演练。

- [x] 建立按数据库分组的迁移目录和统一 CLI/PowerShell 入口。（审核：`db/migrations/{auth,game,chat,announce,mail}`、`tools/db.js`、`scripts/db.ps1` 与根 `package.json` 的 `db*` 命令已建立；12 项 Node 测试通过。）
- [x] 建立版本表，记录版本、名称、checksum、执行耗时、执行者和时间。（审核：SQLx `_sqlx_migrations` 记录版本、描述、checksum 和耗时；`tools/db.js` 在成功 `up` 后持久写入 `_myserver_migration_audit` 的 actor、操作、起止时间、结果和 history 摘要，审计失败返回 6。）
- [x] 实现 `status`、`up`、`validate` 命令及稳定退出码。（审核：`tools/db.js` 定义 0/2/3/4/5/6/7/8 契约；`tests/db/db-cli.test.mjs` 覆盖 Node/PowerShell 入口的 JSON 输出与退出码 2。）
- [x] 对重复执行、并发执行和 checksum 漂移返回明确错误。（审核：`up` 先运行 SQLx `migrate info`，由 `classifyFailure` 将 checksum/history 映射为 4、advisory lock 映射为 5；存量用户表无 history 在调用 SQLx 前返回 7。实库并发与 checksum 演练待阶段 7。）
- [x] 日志隐藏密码、连接串凭据和敏感 SQL 参数。（审核：`tools/db.js` 的 `redact` 脱敏 PostgreSQL userinfo 与 password/token 类字段，仅输出稳定错误类别；对应单元测试通过。）

## 阶段 3：基线迁移

- 开始时间：2026-07-18 16:13:50 +08:00
- 结束时间：2026-07-18 19:53:07 +08:00
- 开发总结：完成五库初始 Schema baseline、受审阅 catalog fingerprint 的存量库 history 标记、development bootstrap/seed 拆分和迁移式 reset 入口；同时将 psql 预检与审计的连接凭据改为仅通过子进程 PG* 环境注入，不再将完整 DSN 放入命令行。运行时 allowlist 仍故意为空，实际存量、预发和生产库在人工审阅并登记 fingerprint 前继续拒绝 baseline。
- 验证记录：主审核复跑 `node --check tools/db.js`、`node tools/run-node-tests.js tests/db/db-cli.test.mjs`（20/20）、`scripts/reset-dev-data.ps1` PowerShell Parser 和 `git diff --check` 均通过；实库演练在两批共 20 个 `myserver_stage3_*` 临时库中完成五库空库 `up`/`validate`、catalog 对照、未审阅 baseline 拒绝和测试 allowlist baseline 成功，最终残留数为 0；`bin/sqlx.exe --version` 为 `sqlx-cli 0.8.6`，制品 hash 与受控配置一致。

- [x] 从当前初始化 SQL 生成可审阅的基线迁移，不改变现有 Schema 语义。（审核：`db/migrations/{auth,game,chat,announce,mail}/20260718161350_initial_schema.sql` 分离出五库 Schema，均标明逻辑 owner、expand 阶段和不可逆风险；五库临时库 catalog 与 `db/init.sql` 对应分段实测一致。）
- [x] 支持空库执行基线和存量库标记基线两种路径。（审核：`tools/db.js` 支持 SQLx 空库 `up` 与受 allowlist/fingerprint/advisory lock 保护的 `baseline`；五库空库 `up`/`validate` 返回 0，未审阅 fingerprint 返回 7，测试注入审阅 allowlist 的 baseline/validate 返回 0。）
- [x] 比较空库迁移结果与当前 `db/init.sql` 的表、列、约束和索引。（审核：`db/schema/catalog-snapshot.sql` 覆盖 table、column、constraint、index、trigger、function；五库 migration/init 临时库对照行数 auth/game/chat/announce/mail 为 245/316/38/24/150，实测 fingerprint 一致。）
- [x] 保留本地一键初始化入口，并改为调用迁移体系。（审核：`scripts/reset-dev-data.ps1` 保留 `-Confirm` 和 localhost 防护，顺序执行 development bootstrap、`tools/db.js up --database all --actor local-reset` 与开发 seed；PowerShell Parser 和 `tests/db/db-cli.test.mjs` 断言通过。）
- [x] 记录初始化 SQL 的兼容保留或退役策略。（审核：`docs/数据库/数据库初始化说明.md` 与 `docs/数据库/数据库迁移体系设计.md` 明确 `db/init.sql` 仅兼容/对照保留、常规入口使用 bootstrap + migration + seed，以及实际环境 fingerprint 经人工审阅前继续拒绝 baseline。）

## 阶段 4：前向兼容与回滚策略

- 开始时间：2026-07-18 19:55:25 +08:00
- 结束时间：2026-07-18 20:36:23 +08:00
- 开发总结：建立版本化 migration 安全元数据、受限事务外操作、全局 timeout 预算和 expand/migrate/contract 模板；修复存量 baseline 的版本绑定，使受审阅 fingerprint 只会写入目标版本及之前的 history，避免未来 migration 被错误标记为已执行。历史初始 baseline 以 SHA-384 绑定的 legacy 例外保留，不改写已发布 SQLx checksum。
- 验证记录：主审核复跑 `node --check tools/db.js`、`node --test tests/db/db-cli.test.mjs tests/db/stage4-rollout.test.mjs`（27 通过、1 个受保护实库测试默认跳过）和 `git diff --check`；显式启用实库 rollout 后 1/1 通过，`myserver_stage4_%` 临时库残留为 0。审核两轮修复分别补齐 baseline `sha256 + version + description` 截断 history 与混合 migration timeout 的有界最大值聚合。

- [x] 定义增列、改名、删列、改类型、索引和约束变更模板。（审核：`db/migrations/templates/` 和 `docs/数据库/数据库迁移体系设计.md` 定义 expand/migrate/contract，覆盖增列、改名/删列/改类型、并发索引和约束收紧；`tests/db/db-cli.test.mjs` 校验模板元数据与阶段 fixture。）
- [x] 为不可逆迁移要求备份点、恢复命令和风险说明。（审核：`tools/db.js` 对 data-loss/data-rewrite/external-state 强制命名 Backup point、Recovery command 和 Risk summary；实库 contract fixture 删除旧列后从备份表恢复数据并重建旧列写入能力。）
- [x] 支持事务内迁移；不支持事务的操作必须显式标记。（审核：事务内 migration 由 SQLx 默认执行且拒绝 SQL 中的 BEGIN/COMMIT/ROLLBACK；事务外文件必须首行 `-- no-transaction`、匹配 `Transaction: no-transaction`、获批 reason 与实际并发索引语句，`tests/db/stage4-rollout.test.mjs` 实测 `CREATE INDEX CONCURRENTLY` 成功。）
- [x] 定义长事务、锁等待、statement timeout 和失败恢复规则。（审核：`db/config/migration-safety.json` 固定 5s/60s 默认与 15s/5min 上限；CLI 向 SQLx/psql 注入受控 `PGOPTIONS` 并拒绝 DSN `options` 覆盖，混合 60s/5min 批次实测/单测使用有界 5min；设计文档定义锁超时、语句失败和事务外恢复路径。）
- [x] 用样例迁移验证旧版本服务与新 Schema 的滚动兼容。（审核：`tests/db/stage4-rollout.test.mjs` 在受控临时 PostgreSQL 中验证旧形态读写、expand 后旧/双写、新索引、contract 删除旧列与备份恢复；主审核显式实库运行 1/1 通过且临时库残留为 0。）

## 阶段 5：数据回填与漂移检测

- 开始时间：2026-07-18 20:38:06 +08:00
- 结束时间：2026-07-18 21:28:31 +08:00
- 开发总结：实现受版本控制的五库 target catalog manifest、精确环境 allowance 和 `db drift` JSON 报告；实现独立 backfill task/state/audit 流程，支持批次、整数 cursor、限速、暂停/恢复、断点续跑与失败审计，且明确不写 SQLx migration history。审查修复使 failed state 的 `backfill-run` 返回稳定非零，只有显式 resume 后可再次执行，task id/目录/manifest id 也强制一致。
- 验证记录：主审核复跑 `node --check tools/db.js`、PowerShell Parser、`node --test tests/db/db-cli.test.mjs tests/db/stage5-drift-backfill.test.mjs`（32 通过、1 个受保护测试默认跳过）和 `git diff --check`；显式 stage 5 PostgreSQL 演练 6/6 通过，覆盖 clean/allowlisted/unapproved drift、回填暂停/恢复、失败阻断与恢复，`myserver_stage5_%` 残留为 0。worker 另在五库独立临时库完成 clean drift，对象数 auth/game/chat/announce/mail 为 245/316/38/24/150。

- [x] 为大表回填定义分批、游标、限速、暂停和断点续跑规范。（审核：`db/backfills/README.md`、`task.json` 与 `batch.sql` 定义整数 cursor、batch/max batch、最小批间延迟、statement timeout 和参数化单条 WITH 批处理；`tools/db.js` 以 durable cursor、pause/resume 和 per-batch advisory lock 执行。）
- [x] 数据回填与 DDL 迁移使用独立状态和审计记录。（审核：`_myserver_backfill_state` 与 `_myserver_backfill_audit` 独立于 `_sqlx_migrations`，先验证目标 migration；实库演练确认 history 仍仅有迁移记录、暂停/恢复/失败都有独立审计，failed run 返回 EXIT 6。）
- [x] 实现目标 Schema 漂移检测，覆盖表、列、索引、约束和触发器。（审核：`db/schema/catalog-snapshot.sql` 为 table/column/index/constraint/trigger/function 增加 drift-only identity，`db/schema/targets/*.json` 固定五库目标，`db drift` 对 missing/extra/definition-change 做对象级比较；五库临时 clean drift 实测通过。）
- [x] 区分允许的环境差异与未经迁移的人工变更。（审核：`db/schema/drift-policy.json` allowance 必须精确匹配 database、environment、direction、kind、identity、definition digest 和理由，拒绝通配；实库测试验证 allowlisted 索引通过、手工新增列返回 EXIT 7。）
- [x] 输出机器可读报告供 CI 和部署脚本消费。（审核：`tools/db.js` 的 `drift` 输出单行 JSON report 与稳定退出码，根 `npm run db:drift`、`scripts/db.ps1` 均暴露入口；Node/PowerShell CLI 测试通过。）

## 阶段 6：部署与 CI 准入

- 开始时间：2026-07-18 21:30:23 +08:00
- 结束时间：2026-07-18 22:12:13 +08:00
- 开发总结：新增五库 `preflight -> apply -> postflight` 部署 gate、受保护空库重建和 Windows CI 入口；补齐静态兼容范围、恢复报告、关键对象/readiness 校验，并将 drift manifest digest 固定为可比较的语义三元组。
- 验证记录：主线程审核 `tools/db-deploy.js`、部署配置、PowerShell/CI 入口和文档；`node --test tests/db/stage5-drift-backfill.test.mjs tests/db/stage6-deploy.test.mjs` 为 14 passed、1 个受保护实库测试 skipped，最终 `npm.cmd run db:ci` 为 42 passed、2 个受保护实库测试 skipped，`node --check tools/db.js tools/db-deploy.js` 与 `git diff --check` 通过。创建失败回归确认只清理本次成功创建的临时库；worker 在受控 `myserver_stage6_*` 临时五库重建中确认 target/actual `manifest_sha256` 全部相等且残留为 0。

- [x] 增加根脚本执行迁移校验和空库重建。（验证：`package.json` 的 `db:ci`/`db:ci:rebuild`、`tools/db-deploy.js` 的 `validate`/`rebuild-check` 和 `tests/db/stage6-deploy.test.mjs` 临时库 finally 清理测试；五库受控重建通过且无残留。）
- [x] 部署前检查待执行版本、锁、备份条件和服务兼容范围。（验证：`tools/db-deploy.js` 的 `historySummary`、`inspectDeploymentDatabase`、`backupEvidence`、`preflightDatabase` 和 `db/config/deploy-gate.json`；锁不可用的定向测试通过并停止在 auth。）
- [x] 部署后校验版本、关键对象和服务 readiness。（验证：`tools/db-deploy.js` 的 `postflightDatabase` 检查 SQLx history、drift、`keyTables` 与显式 readiness；测试覆盖 not-configured 和不健康 readiness 阻断。）
- [x] 失败时停止后续部署并输出恢复步骤。（验证：`runPreflight`、`runApply`、`runPostflight` 的串行失败返回和 `recoverySteps`；`apply` 失败测试确认不执行后续 game migration。）
- [x] 同步数据库初始化、开发环境和生产部署文档。（验证：`docs/数据库/数据库部署准入说明.md` 新增部署准入、审批和临时库边界；`数据库初始化说明.md`、`数据库迁移体系设计.md` 已同步入口与 digest 规范。）

## 阶段 7：验证与演练

- 开始时间：2026-07-18 22:13:28 +08:00
- 结束时间：2026-07-19 00:06:36 +08:00
- 开发总结：新增 Stage 7 受保护临时库 drill、SQLx CLI 0.8.6 外层 advisory lock runner、history checksum precheck、真实 DDL lock-timeout fixture 和 Core NATS migration metrics producer；修复 SQLx CLI 自身不持 migration lock 的并发缺口，并将所有验证路径纳入静态 CI。
- 验证记录：主线程复跑 `npm.cmd run db:ci` 为 54 tests、51 pass、3 个显式 PostgreSQL skip，`node --check tools/db.js tools/db-lock-runner.js tools/db-migration-metrics.js tools/db-stage7-drill.js tools/db-stage7-worker.js` 与 `git diff --check` 通过。随后使用用户授权的本机 PostgreSQL 凭据运行 `npm.cmd run db:stage7:drill`：五库 empty/current/repeat/drift/audit、checksum=4、SQL failure=6、受控 PID 断连=3、advisory lock=5/释放后=0、实际 500ms DDL lock timeout=5/释放后=0、expand/contract recovery 全通过；11 个 `myserver_stage7_*` 临时库全部 dropped，独立 `pg_database` 查询残留为 0。metrics 以 fake NATS + 真实 collector `writeMetrics` 单测验证 envelope 与 Redis 字段，未启动 NATS/Redis/业务服务。

- [x] 验证空库、当前存量库、重复执行和并发执行。（验证：`tools/db-stage7-drill.js` 的 `runFiveDatabaseLifecycle`/`runConcurrentDrill`；实库五库 history、repeat up、clean drift 与 audit 通过，第二 runner 在 advisory lock 持有时返回 5、释放后返回 0。）
- [x] 验证 checksum 篡改、SQL 失败、连接中断和锁超时。（验证：受控实库 drill 分别得到 checksum 4、SQL failure 6、backend terminate 断连 3；`runLockTimeoutDrill` 持 `ACCESS EXCLUSIVE` 后使 500ms `ALTER TABLE` 返回 5、blocked 列和 sentinel 均未创建，释锁重试成功。）
- [x] 演练一次向前兼容变更和一次不可逆变更恢复。（验证：`runCompatibilityRecoveryDrill` 分别断言 first/repeated expand 为 0、旧调用方读写兼容、contract 删除旧列及从备份表恢复旧调用方写入；实库 report 通过。）
- [x] 确认日志、指标和审计能定位具体迁移版本。（验证：成功 `up` JSON audit 含 `migrationVersions`/`targetMigrationVersion`，`_myserver_migration_audit.history_summary` 含 `versions=`；`tools/db-migration-metrics.js` 发布 collector-compatible Core NATS envelope，`stage7-verification.test.mjs` 用 fake NATS 与真实 collector `writeMetrics` 验证 Redis 字段，metrics child 采用无凭据白名单环境。）
- [x] 未经用户确认不运行需要 PostgreSQL 的集成演练。（验证：本阶段 drill 要求 `MYSERVER_STAGE7_RUN_POSTGRES=1` 和 localhost guard；本轮在用户明确授权后执行，未启动服务且仅操作 `myserver_stage7_*` 临时库。）

## 最终完成定义

- 开始时间：2026-07-18 14:31:12 +08:00
- 结束时间：2026-07-19 00:18:02 +08:00
- 验收总结：七个阶段均完成并独立验证、提交；最终阶段提交为 `d449662`。五库迁移、存量 baseline、漂移/回填、部署 gate、受保护实库故障与恢复演练均具备机器可读结果和文档化操作边界。本清单已从 `summary/` 归档至 `docs/数据库/checklists/`。

- [x] 所有数据库结构都能从版本化迁移在空库重建。（验收：Stage 3 五库空库 `up`/`validate`、Stage 6 受控重建及 Stage 7 五库 empty/current/repeat/drift/audit 演练均通过，临时库残留为 0。）
- [x] 存量环境可安全建立基线且不会重复建表或丢数据。（验收：Stage 3 fingerprint + reviewed allowlist baseline 仅写入批准目标版本；未审阅存量库拒绝 baseline，Stage 4 修复未来版本误标记风险。）
- [x] 漂移、并发、失败和不可逆风险有明确检测与恢复路径。（验收：Stage 5 drift/backfill 状态审计、Stage 7 checksum=4、SQL failure=6、断连=3、advisory/DDL lock=5 及 expand/contract 恢复均通过实库演练。）
- [x] CI 与部署能够阻止非法迁移进入环境。（验收：`db:deploy:validate` 校验五库 target、兼容范围和 key tables；`npm.cmd run db:ci` 最终为 51 passed、3 个显式 PostgreSQL guard skip，CI 已监听 Stage 7 fixtures 与指标实现。）
