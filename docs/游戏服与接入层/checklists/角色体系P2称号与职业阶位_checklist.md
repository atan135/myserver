# 角色体系 P2 称号与职业阶位 Checklist

来源文档：`docs/游戏服与接入层/角色体系与四属性设计.md`

## 目标

完成 P2“称号与职业阶位基础”阶段，在 P0 账号/角色身份拆分和 P1 四属性基础闭环之上，落地角色职业阶位事实、称号定义、称号拥有状态、称号解锁、称号装备和基础查询能力。

P2 完成后应达到：服务端可以按 `character_id` 查询角色已拥有称号和职业阶位；职业阶位可以作为称号解锁来源；称号可以由系统、GM/debug 或职业阶位检查器授予；同一角色同一时间只能装备一个主展示称号；称号授予、撤销、装备、卸下和过期处理都有可追踪日志；mock-client 可以完成“登录、选角、查询称号、触发测试授予/阶位变化、装备称号、再次查询验证”的调试流程。

本阶段重点扩展称号相关功能，并只落地职业/流派的“阶位事实基础”。完整职业学习、激活职业、技能池、战斗属性、任务/场景/道具联动和后台编辑闭环不放入本阶段。

## 基础原则

- [ ] 数据库按新开发处理，不编写历史 migrate；结构以 `db/init.sql` 空库初始化直接落新模型为准。
- [ ] P2 所有游戏内状态以 `character_id` 为主键，不用角色名或账号 `player_id` 识别角色。
- [ ] 职业阶位是规则事实，称号是展示和身份表达；业务判断优先依赖职业阶位和四属性，不只依赖称号文本。
- [ ] 一个角色可以拥有多个称号，但 P2 同一时间只能装备 `1` 个主展示称号。
- [ ] 称号第一阶段不提供战斗属性；`effects` 只允许为空或非战斗展示/交互标记。
- [ ] 称号定义优先使用 CSV 配置，后续如需运营动态管理再迁入数据库。
- [ ] 称号授予、撤销、装备、卸下、过期和自动解锁必须幂等，并写入来源、操作者、原因和前后状态日志。
- [ ] 称号过期后不得继续作为已装备称号展示；查询接口应能区分有效、已过期和隐藏称号。
- [ ] P2 可以提供受控 debug/GM 调试入口验证闭环，但不向普通玩家开放任意授予、撤销或改阶位能力。
- [ ] 完成功能后同步角色体系、协议、外部客户端接入、mock-client 和管理后台相关文档。
- [ ] 模块开发完成后，先提示需要启动的服务和依赖，待确认后再执行集成测试或联调脚本。

## 阶段 1：数据模型和配置边界

- 开始时间：2026-06-26 14:21:07 +08:00
- 结束时间：2026-06-26 14:43:36 +08:00
- 开发总结：新增 P2 职业阶位、称号持有和称号日志空库初始化结构；新增 TitleTable CSV、生成结构、加载/热加载接入和称号配置校验。
- 验证记录：主 agent 复跑 `cargo test title_table_ -- --nocapture`，5 passed；审核 `db/init.sql`、`apps/game-server/csv/TitleTable.csv`、`apps/game-server/src/gameconfig/registry.rs`、`apps/game-server/src/gameconfig/title_config.rs`。

- [x] 在 `db/init.sql` 新增 `character_disciplines`，字段至少包含 `character_id`、`discipline_id`、`points`、`tier`、`active`、`learned_at`、`updated_at`，并设置 `(character_id, discipline_id)` 唯一约束。（验证：`db/init.sql:354` 创建表，`db/init.sql:363` 定义唯一约束）
- [x] 在 `db/init.sql` 新增 `character_titles`，字段至少包含 `character_id`、`title_id`、`source_type`、`source_id`、`is_equipped`、`unlocked_at`、`expires_at`、`created_at`、`updated_at`，并设置 `(character_id, title_id)` 唯一约束。（验证：`db/init.sql:381` 创建表，`db/init.sql:392` 定义唯一约束）
- [x] 在 `db/init.sql` 新增 `character_title_logs`，记录 `character_id`、`title_id`、`action`、`source_type`、`source_id`、`operator_type`、`operator_id`、`before_json`、`after_json`、`reason` 和 `created_at`。（验证：`db/init.sql:415` 创建表并包含日志字段）
- [x] 增加必要索引，至少覆盖按 `character_id` 查询职业阶位、按 `character_id` 查询称号、按装备状态查询、按过期时间扫描、按日志时间倒序查看。（验证：`db/init.sql:368`、`db/init.sql:400`、`db/init.sql:402`、`db/init.sql:406`、`db/init.sql:434` 定义对应索引）
- [x] 明确单角色仅一个主展示称号的实现方式：在服务事务中先卸下旧称号再装备新称号，避免依赖跨数据库兼容性差的复杂部分唯一约束。（验证：`db/init.sql:397` 表注释明确服务事务先卸旧再装备新称号）
- [x] 新增称号 CSV 配置，例如 `csv/TitleTable.csv`，字段至少包含 `TitleId`、`Name`、`Description`、`TitleType`、`SourceDomainId`、`TierRequired`、`UnlockRules`、`Effects`、`Rarity`、`Icon`、`Color`、`Tags`、`Hidden`、`Limited`、`SortOrder`。（验证：`apps/game-server/csv/TitleTable.csv:1` 定义完整字段，`apps/game-server/src/csv_code/titletable.rs:8` 生成 schema）
- [x] 为称号 CSV 增加配置加载和热加载校验，拒绝重复 `TitleId`、非法 JSON、非法 `TitleType`、战斗属性类 `Effects` 和缺失展示名。（验证：`apps/game-server/src/gameconfig/registry.rs:74` 初始加载校验，`apps/game-server/src/gameconfig/registry.rs:165` 热加载校验，`apps/game-server/src/gameconfig/title_config.rs:45`/`50`/`54`/`90`/`113` 覆盖校验，`cargo test title_table_ -- --nocapture` 通过）
- [x] 定义 P2 支持的最小称号类型：`identity`、`discipline`、`event`、`honor`、`gm`、`system`，并在配置校验中限制枚举。（验证：`apps/game-server/src/gameconfig/title_config.rs:8` 定义允许枚举，`apps/game-server/src/gameconfig/title_config.rs:54` 执行限制，`title_table_accepts_supported_minimal_types` 测试通过）

## 阶段 2：称号与职业阶位核心服务

- 开始时间：2026-06-26 14:46:25 +08:00
- 结束时间：2026-06-26 15:14:36 +08:00
- 开发总结：新增 `DisciplineService`/`PgDisciplineStore` 与 `TitleService`/`PgTitleStore`，完成职业阶位 upsert 查询、称号授予/撤销/装备/卸下/过期处理、日志写入和 `ServiceContext` 接入。
- 验证记录：主 agent 复跑 `cargo test character_ --manifest-path apps/game-server/Cargo.toml`，23 passed；复跑 `cargo check --manifest-path apps/game-server/Cargo.toml` 通过，存在仓库既有 warning。

- [x] 在 `game-server` 新增职业阶位 store/service，支持按当前 `character_id` 查询职业阶位列表、读取单个职业阶位、受控 upsert 职业阶位。（验证：`apps/game-server/src/core/character_discipline.rs:92` 定义服务，`apps/game-server/src/core/character_discipline.rs:103`/`110`/`118` 提供 identity 绑定查询与 upsert，`cargo test character_ --manifest-path apps/game-server/Cargo.toml` 通过）
- [x] 职业阶位 service 校验 `discipline_id`、`tier`、`points` 和 `active`，拒绝空 ID、负数 points 和未知 tier。（验证：`apps/game-server/src/core/character_discipline.rs:9` 定义 tier 白名单，`apps/game-server/src/core/character_discipline.rs:355` 校验 upsert，`apps/game-server/src/core/character_discipline.rs:440` 测试空 ID/负数 points/未知 tier）
- [x] 在 `game-server` 新增称号 store/service，支持按当前 `character_id` 查询称号列表、读取装备称号、授予称号、撤销称号、装备称号、卸下称号和处理过期称号。（验证：`apps/game-server/src/core/character_title.rs:177` 定义服务，`apps/game-server/src/core/character_title.rs:190`/`198`/`206`/`215`/`224`/`235`/`340` 提供 identity 绑定能力）
- [x] 称号授予操作必须幂等：重复授予同一有效称号不创建重复持有记录，但可以按需要写入去重后的日志或返回已拥有状态。（验证：`apps/game-server/src/core/character_title.rs:493` 锁定已有称号，`apps/game-server/src/core/character_title.rs:504` 对重复授予写去重日志并返回 `AlreadyOwned`，`apps/game-server/src/core/character_title.rs:1352` 测试通过）
- [x] 称号撤销操作必须清理装备状态；撤销当前装备称号后，角色不应保留已装备但不可用的称号。（验证：`apps/game-server/src/core/character_title.rs:560` 撤销持有记录，`apps/game-server/src/core/character_title.rs:1426` 测试装备后撤销并确认无装备称号）
- [x] 装备称号操作必须校验称号已拥有、未过期、未隐藏或允许展示，且只装备当前鉴权角色自己的称号。（验证：`apps/game-server/src/core/character_title.rs:313` 装备前处理过期，`apps/game-server/src/core/character_title.rs:316` 拒绝隐藏称号默认展示，`apps/game-server/src/core/character_title.rs:623`/`628` 拒绝未拥有和过期，`apps/game-server/src/core/character_title.rs:224` 通过 identity character_id 调用）
- [x] 称号过期处理必须在查询和装备时生效；已过期称号不能装备，已装备称号过期后查询结果应不再显示为已装备。（验证：`apps/game-server/src/core/character_title.rs:249`/`263` 查询前处理过期，`apps/game-server/src/core/character_title.rs:742` 处理过期装备，`apps/game-server/src/core/character_title.rs:1488` 测试过期后查询无装备且装备返回 `TITLE_EXPIRED`）
- [x] 所有授予、撤销、装备、卸下和过期状态变更必须写入 `character_title_logs`，包含前后快照和来源/操作者。（验证：`apps/game-server/src/core/character_title.rs:929` 定义日志 SQL，`apps/game-server/src/core/character_title.rs:970` 统一写入 source/operator/before/after/reason，`apps/game-server/src/core/character_title.rs:544`/`589`/`663`/`727`/`769` 覆盖 grant/revoke/unequip/expire）
- [x] 错误码保持稳定且可测试，例如 `TITLE_NOT_FOUND`、`TITLE_ALREADY_OWNED`、`TITLE_NOT_OWNED`、`TITLE_EXPIRED`、`TITLE_CONFIG_NOT_FOUND`、`INVALID_TITLE_ACTION`、`INVALID_DISCIPLINE_TIER`。（验证：`apps/game-server/src/core/character_title.rs:144` 定义 title 错误码，`apps/game-server/src/core/character_discipline.rs:69` 定义 `INVALID_DISCIPLINE_TIER`，`apps/game-server/src/core/character_title.rs:1383`/`1488`/`1538` 和 `apps/game-server/src/core/character_discipline.rs:440` 覆盖错误码测试）
- [x] 服务挂入现有 `ServiceContext`，遵守 P1 的鉴权 identity 模型，不接受普通客户端在请求体中指定任意 `character_id`。（验证：`apps/game-server/src/core/context.rs:131`/`132` 挂入服务，`apps/game-server/src/server.rs:500`/`516` 初始化服务，`apps/game-server/src/core/character_title.rs:190` 和 `apps/game-server/src/core/character_discipline.rs:103` 通过 `AuthenticatedSessionIdentity.character_id` 访问）

## 阶段 3：称号解锁检查器和派生规则

- 开始时间：2026-06-26 15:17:06 +08:00
- 结束时间：2026-06-26 15:37:44 +08:00
- 开发总结：新增 `TitleUnlockService`，基于 TitleTable `UnlockRules` 检查当前角色职业阶位和四属性条件，幂等授予称号并返回新增/续授/跳过结果；服务已挂入 `ServiceContext` 供后续协议/debug 调用。
- 验证记录：主 agent 复跑 `cargo test character_title_unlock --manifest-path apps/game-server/Cargo.toml`，7 passed；复跑 `cargo test character_ --manifest-path apps/game-server/Cargo.toml`，30 passed；`cargo check --manifest-path apps/game-server/Cargo.toml` 通过，存在仓库既有 warning 以及阶段 4 前预留入口未读 warning。

- [x] 新增 `TitleUnlockService`，基于称号 CSV 的 `UnlockRules` 对当前角色执行解锁检查，并通过称号 service 幂等授予称号。（验证：`apps/game-server/src/core/character_title_unlock.rs:29` 定义服务，`apps/game-server/src/core/character_title_unlock.rs:51` 遍历 TitleTable 并调用 `TitleService::grant_for_identity`，`cargo test character_title_unlock --manifest-path apps/game-server/Cargo.toml` 通过）
- [x] P2 最小支持 `manual`、`discipline_tier`、`element_mastery`、`element_affinity` 和 `all_of` 组合规则，暂不接入任务、场景、道具、成就和排行榜。（验证：`apps/game-server/src/core/character_title_unlock.rs:350`/`399`/`413`/`424` 解析对应规则，`apps/game-server/src/core/character_title_unlock.rs:975` 对 event 规则返回 unsupported）
- [x] 职业阶位变化后可以触发称号解锁检查，用于验证“职业阶位 -> 称号”闭环。（验证：`apps/game-server/src/core/character_title_unlock.rs:186` 定义 `TitleUnlockTrigger::Discipline`，`apps/game-server/src/core/character_title_unlock.rs:774` 测试 upsert 职业阶位后触发检查授予称号）
- [x] 四属性变化后可以手动或服务内触发称号解锁检查，用于验证“mastery/affinity -> 称号”闭环，但不要求完整任务/道具来源接入。（验证：`apps/game-server/src/core/character_title_unlock.rs:186` 定义 `TitleUnlockTrigger::Element`，`apps/game-server/src/core/character_title_unlock.rs:808` 测试 mastery/affinity 阈值触发称号授予）
- [x] 解锁检查器必须对隐藏称号、限时称号、已拥有称号、过期后是否可重新解锁等规则给出明确行为。（验证：`apps/game-server/src/core/character_title_unlock.rs:81` 限时称号跳过，`apps/game-server/src/core/character_title_unlock.rs:143` 已拥有跳过，`apps/game-server/src/core/character_title_unlock.rs:881` 覆盖隐藏/限时/已拥有，`apps/game-server/src/core/character_title_unlock.rs:928` 覆盖过期续授）
- [x] 解锁检查器返回本次新增称号列表和跳过原因，便于 mock-client、测试和后台排查。（验证：`apps/game-server/src/core/character_title_unlock.rs:196` 定义 `TitleUnlockCheckResult.unlocked/skipped`，`apps/game-server/src/core/character_title_unlock.rs:217` 定义跳过原因枚举及 code）
- [x] 称号来源写入必须能区分 `discipline`、`element`、`gm`、`system` 等来源，避免所有自动称号都被记成 debug 来源。（验证：`apps/game-server/src/core/character_title_unlock.rs:545` 根据 trigger/规则构造 source_type，`apps/game-server/src/core/character_title_unlock.rs:928` 测试过期续授来源为 `element` 并写入日志）

## 阶段 4：玩家协议、debug 入口和 mock-client

- 开始时间：2026-06-26 15:40:09 +08:00
- 结束时间：2026-06-26 16:10:48 +08:00
- 开发总结：新增称号/职业阶位 P2 协议 1417-1424、服务端 handler 和受控 debug 入口；mock-client 新增称号授予装备与职业阶位解锁调试场景，并补齐参数、解码和说明。
- 验证记录：主 agent 复跑 `cargo test character_title_service --manifest-path apps/game-server/Cargo.toml`，2 passed；复跑 `cargo check --manifest-path apps/game-server/Cargo.toml` 通过，存在仓库既有 warning；复跑 `node --check tools/mock-client/src/messages.js; node --check tools/mock-client/src/scenarios/character.js; node --check tools/mock-client/src/index.js` 通过；复跑 `git diff --check` 通过，仅 Git CRLF 提示；`npm run check:mock-client-protocol` 因外部客户端 `C:\project\mybevy` 下未找到 `protocol.rs` 失败，仓库内 mock-client 字段 schema 未见额外 drift 输出。

- [x] 在 `packages/proto` 新增角色称号查询协议，例如 `GetCharacterTitlesReq/Res`，返回称号定义摘要、拥有状态、装备状态、来源、过期时间和排序字段。（验证：`packages/proto/game.proto:685` 定义称号摘要，`packages/proto/game.proto:709` 定义查询协议，`apps/game-server/src/protocol/message_type.rs:74`/`75` 分配 1417/1418）
- [x] 新增装备称号协议，例如 `EquipCharacterTitleReq/Res`，客户端只能装备当前已鉴权角色拥有的有效称号。（验证：`packages/proto/game.proto:720` 定义装备协议，`apps/game-server/src/core/service/character_title_service.rs:82` 从连接 identity 鉴权，`apps/game-server/src/core/service/character_title_service.rs:128` 调用 `equip_for_identity` 且不接受请求体 `character_id`）
- [x] 新增职业阶位查询协议，例如 `GetCharacterDisciplinesReq/Res`，返回当前角色职业阶位、points、active 和更新时间。（验证：`packages/proto/game.proto:731`/`740` 定义职业阶位摘要和查询协议，`apps/game-server/src/core/service/character_title_service.rs:166` 通过当前 identity 查询，`apps/game-server/src/core/service/character_title_service.rs:180` 返回 points/tier/active/时间字段）
- [x] 新增受控 debug/GM 调试协议，用于授予/撤销称号、设置职业阶位、触发称号解锁检查，必须要求 `GAME_ADMIN_TOKEN` 或独立 P2 debug token。（验证：`packages/proto/game.proto:750` 定义 debug 请求，`apps/game-server/src/core/service/character_title_service.rs:230` 校验 token，`apps/game-server/src/core/service/character_title_service.rs:579`/`590` 支持配置 token、`GAME_ADMIN_TOKEN` 和 `MYSERVER_CHARACTER_TITLE_DEBUG_TOKEN`）
- [x] 所有 P2 玩家协议按 `messageType + seq` 匹配响应，并为错误路径返回稳定 `ok=false` 与 `error_code`。（验证：`apps/game-server/src/server.rs:1223`-`1239` 分发 4 类请求，`apps/game-server/src/core/service/character_title_service.rs:73`/`194`/`731`/`755` 使用原始 `packet.header.seq` 响应，`apps/game-server/src/core/service/character_title_service.rs:64`/`147`/`186`/`231` 覆盖错误响应）
- [x] 更新 `tools/mock-client`，新增称号调试场景：登录选角、查询称号、debug 授予称号、装备称号、再次查询并输出 JSON。（验证：`tools/mock-client/src/constants.js:241` 注册场景，`tools/mock-client/src/index.js:441` 接入入口，`tools/mock-client/src/scenarios/character.js:430` 实现查询、grant、equip、再次查询流程）
- [x] 更新 `tools/mock-client`，新增职业阶位调试场景：登录选角、设置职业阶位、触发解锁检查、确认职业阶位称号被授予。（验证：`tools/mock-client/src/constants.js:242` 注册场景，`tools/mock-client/src/index.js:452` 接入入口，`tools/mock-client/src/scenarios/character.js:501` 实现 set_discipline、triggerUnlockCheck、查询 titles/disciplines）
- [x] mock-client 参数支持 `--title-id`、`--discipline-id`、`--discipline-tier`、`--title-debug-token`、`--title-change-reason`、`--json-output`。（验证：`tools/mock-client/src/args.js:67`-`72` 定义默认值，`tools/mock-client/src/args.js:285`-`302` 解析参数，`tools/mock-client/README.md:428`-`432` 和 `tools/mock-client/help.txt:31`-`35` 写明用法）
- [x] mock-client 输出应包含 `before`、`action`、`after`、`unlockedTitles`、`equippedTitle` 和 `discipline` 摘要，便于测试断言。（验证：`tools/mock-client/src/scenarios/character.js:475`-`491` 输出称号场景摘要，`tools/mock-client/src/scenarios/character.js:544`-`557` 输出职业阶位场景摘要，`tools/mock-client/README.md:313` 记录输出字段）

## 阶段 5：管理后台只读查询边界

- 开始时间：2026-06-26 16:16:29 +08:00
- 结束时间：2026-06-26 16:47:10 +08:00
- 开发总结：在 `admin-api` 玩家域新增角色称号/职业阶位只读查询接口，按 `character_id` 返回称号、当前有效装备称号、职业阶位和最近称号日志；复用后台 JWT/权限模型并写入查询审计；正式文档明确 P2 不做后台写操作和 admin-web UI。
- 验证记录：主 agent 复跑 `node --test --experimental-test-isolation=none --test-concurrency=1 apps/admin-api/src/players/players.controller.test.js`，8 passed；复跑 `npm test --workspace admin-api`，107 passed；复跑 `git diff --check` 通过，仅 Git CRLF 提示；审核 `db/init.sql` 确认 `character_title_logs.id` 字段存在。

- [x] 在 `admin-api` 增加角色称号和职业阶位只读查询接口，支持按 `character_id` 查询称号列表、当前装备称号、职业阶位和最近称号日志。（验证：`apps/admin-api/src/players/players.controller.ts:108` 定义 `GET /api/v1/players/characters/:characterId/titles`，`apps/admin-api/src/admin-store.js:587` 查询 titles/disciplines/logs，`apps/admin-api/src/players/players.controller.test.js:111` 覆盖响应）
- [x] 管理后台查询接口必须进行管理员鉴权和审计，不能复用玩家 debug token。（验证：`apps/admin-api/src/players/players.controller.ts:25` 使用 `JwtAuthGuard`/`RolesGuard`，`apps/admin-api/src/players/players.controller.ts:109` 使用 `@Permissions("players.read")`，`apps/admin-api/src/players/players.controller.ts:131`/`150` 写成功/失败审计，`rg` 未发现该接口读取玩家 debug token）
- [x] `admin-api` 返回值需要展示称号来源、操作者、过期时间、隐藏/限时标记和装备状态，便于运营识别同名角色。（验证：`apps/admin-api/src/admin-store.js:125` 映射 source/operator/expires/equipped/expired，`apps/admin-api/src/players/title-table.js:60` 解析 TitleTable hidden/limited，`apps/admin-api/src/players/players.controller.ts:58` 合并定义字段）
- [x] P2 不实现后台授予、撤销、装备或改阶位写操作；这些写操作保留给后续 GM 闭环阶段。（验证：`apps/admin-api/src/players/players.controller.ts:108` 仅新增 GET 查询路由，`docs/后台与运维/管理后台设计.md:477` 明确后台写操作后续阶段处理）
- [x] 如 `admin-web` 本阶段只做最小展示，应包含加载、空状态、错误状态和过期称号展示；如不做 UI，则文档中明确 P2 只完成 API 查询能力。（验证：`docs/后台与运维/管理后台设计.md:477` 明确本阶段 `admin-web` 不新增页面，只完成 `admin-api` 查询能力）

## 阶段 6：测试、文档和手动验收准备

- 开始时间：2026-06-26 16:50:13 +08:00
- 结束时间：2026-06-26 17:17:37 +08:00
- 开发总结：补齐 P2 数据库结构静态测试、mock-client 协议编解码/场景形态测试、TitleTable 样例与重复 ID 校验；同步角色体系、协议、外部客户端接入和 mock-client 文档，明确 P2 范围、协议、错误码、手动验收依赖和 admin-web 本阶段无页面边界。
- 验证记录：主 agent 复跑 `node --test --experimental-test-isolation=none --test-concurrency=1 tests/db-init-characters.test.mjs tests/mock-client-protocol.test.mjs`，24 passed；复跑 `cargo test title_table_ --manifest-path apps/game-server/Cargo.toml -- --nocapture`，7 passed；复跑 `cargo test character_ --manifest-path apps/game-server/Cargo.toml`，32 passed；复跑 `cargo test character_title_unlock --manifest-path apps/game-server/Cargo.toml`，7 passed；复跑 `cargo test character_title_service --manifest-path apps/game-server/Cargo.toml`，2 passed；复跑 mock-client `node --check` 通过；复跑 `git diff --check` 通过，仅 Git CRLF 提示。未运行真实联调或启动服务；`npm run check:mock-client-protocol` 仍受外部 `C:\project\mybevy` 缺少 `protocol.rs` 影响，未作为阶段 6 必过项。

- [x] 增加数据库结构测试，覆盖 `character_disciplines`、`character_titles`、`character_title_logs` 字段、约束和索引。（验证：`tests/db-init-characters.test.mjs:172`/`200` 覆盖职业表字段/索引，`tests/db-init-characters.test.mjs:220`/`254` 覆盖称号表字段/索引，`tests/db-init-characters.test.mjs:273`/`304` 覆盖日志字段/索引，node 测试 24 passed）
- [x] 增加称号 CSV 配置校验测试，覆盖重复 ID、非法 JSON、非法类型、非法战斗效果和基础样例称号。（验证：`apps/game-server/src/gameconfig/title_config.rs:193` 覆盖基础样例称号，`apps/game-server/src/gameconfig/title_config.rs:207` 覆盖重复 ID，既有 `title_table_rejects_invalid_json`/`invalid_title_type`/`rejects_combat_effects` 继续通过，`cargo test title_table_ --manifest-path apps/game-server/Cargo.toml -- --nocapture` 7 passed）
- [x] 增加 `game-server` 单元测试，覆盖称号授予、重复授予、撤销、装备唯一性、过期处理、日志写入和错误码。（验证：阶段 2 已在 `apps/game-server/src/core/character_title.rs` 覆盖授予/重复/撤销/装备/过期/日志/错误码，阶段 6 复跑 `cargo test character_ --manifest-path apps/game-server/Cargo.toml` 32 passed）
- [x] 增加 `TitleUnlockService` 单元测试，覆盖 `manual`、`discipline_tier`、`element_mastery`、`element_affinity` 和 `all_of` 规则。（验证：阶段 3 已在 `apps/game-server/src/core/character_title_unlock.rs` 覆盖对应规则，阶段 6 复跑 `cargo test character_title_unlock --manifest-path apps/game-server/Cargo.toml` 7 passed）
- [x] 增加协议编解码和 mock-client 场景测试，覆盖称号查询、debug 授予、装备、职业阶位触发解锁和 JSON 输出。（验证：`tests/mock-client-protocol.test.mjs:244` 覆盖 1417-1424，`tests/mock-client-protocol.test.mjs:260` 覆盖请求编码，`tests/mock-client-protocol.test.mjs:294` 覆盖响应解码，`tests/mock-client-protocol.test.mjs:414` 覆盖场景异步匹配和 JSON 字段，node 测试 24 passed）
- [x] 同步更新 `docs/游戏服与接入层/角色体系与四属性设计.md` 的 P2 当前实现状态、称号规则、职业阶位边界和手动联调步骤。（验证：`docs/游戏服与接入层/角色体系与四属性设计.md:14` 记录 P2 当前实现，`docs/游戏服与接入层/角色体系与四属性设计.md:906` 说明 P2 范围，`docs/游戏服与接入层/角色体系与四属性设计.md:976` 增加 P2 手动联调步骤）
- [x] 同步更新 `docs/协议与客户端/协议设计.md` 和 `docs/协议与客户端/外部客户端接入说明.md`，明确新增协议、异步响应匹配、装备称号展示和错误码。（验证：`docs/协议与客户端/协议设计.md:176` 新增 1417-1424，`docs/协议与客户端/协议设计.md:323` 起说明新增消息结构和错误码，`docs/协议与客户端/外部客户端接入说明.md:53`/`60`/`77` 说明称号/职业协议和异步匹配）
- [x] 同步更新 `tools/mock-client/README.md` 和 `help.txt`，给出称号和职业阶位调试命令示例。（验证：`tools/mock-client/README.md:293`/`303` 给出场景示例，`tools/mock-client/README.md:432` 补充 `--discipline-points`，`tools/mock-client/help.txt:107`/`111` 给出命令）
- [x] 整理手动联调步骤，列出 PostgreSQL、Redis、NATS、auth-http、game-proxy、game-server 和可选 admin-api/admin-web 依赖，并等待用户确认后再执行。（验证：`docs/游戏服与接入层/角色体系与四属性设计.md:976` 明确未确认前不启动服务或联调，`docs/游戏服与接入层/角色体系与四属性设计.md:980`-`989` 列出 PostgreSQL/Redis/Core NATS/auth-http/game-proxy/game-server/可选 admin-api，并说明 admin-web 本阶段无页面，`tools/mock-client/README.md:314` 再次提示执行真实联调前需用户确认）

## 最终完成定义

以下项目作为整体完成标准，不要求每个开发阶段都执行，由所有相关阶段完成后统一验收。

- 开始时间：2026-06-26 14:21:07 +08:00
- 结束时间：2026-06-26 17:17:37 +08:00
- 验收总结：P2 称号与职业阶位基础已完成代码、测试和正式文档闭环；真实服务联调和 mock-client 场景执行仍需用户确认启动 PostgreSQL、Redis、Core NATS、auth-http、game-proxy、game-server 和可选 admin-api 后再进行。

- [x] 空库执行 `db/init.sql` 后包含 `character_disciplines`、`character_titles` 和 `character_title_logs`，并具备必要约束和索引。（验证：`db/init.sql:354`/`381`/`415` 建表，`tests/db-init-characters.test.mjs:172`-`304` 静态测试覆盖字段/约束/索引）
- [x] 称号 CSV 可以被加载和校验，非法称号定义会被明确拒绝。（验证：`apps/game-server/src/gameconfig/title_config.rs:193`/`207` 和既有非法 JSON/类型/战斗效果测试，`cargo test title_table_ --manifest-path apps/game-server/Cargo.toml -- --nocapture` 7 passed）
- [x] 已选角进入游戏的连接可以查询当前角色职业阶位和称号列表。（验证：`apps/game-server/src/core/service/character_title_service.rs:29`/`161` 定义查询 handler 并使用当前鉴权 identity，`packages/proto/game.proto:709`/`740` 定义查询协议，`cargo test character_title_service --manifest-path apps/game-server/Cargo.toml` 2 passed）
- [x] debug/GM 入口可以授予称号、撤销称号、设置职业阶位并触发称号解锁检查。（验证：`apps/game-server/src/core/service/character_title_service.rs:202` 处理 debug 请求，`apps/game-server/src/core/service/character_title_service.rs:260`/`265`/`270`/`279` 分发 grant/revoke/set_discipline/check_unlock，`tests/mock-client-protocol.test.mjs:260` 覆盖 debug 编码）
- [x] 同一角色同一时间最多只有一个主展示称号处于装备状态。（验证：`apps/game-server/src/core/character_title.rs` 的装备切换测试随 `cargo test character_ --manifest-path apps/game-server/Cargo.toml` 32 passed，`db/init.sql:397` 记录服务事务边界）
- [x] 已过期称号不能继续装备；已装备称号过期后查询结果不再显示为当前装备称号。（验证：`apps/game-server/src/core/character_title.rs` 过期装备测试随 `cargo test character_ --manifest-path apps/game-server/Cargo.toml` 32 passed，`docs/游戏服与接入层/角色体系与四属性设计.md:906` 记录 P2 范围）
- [x] 职业阶位达到称号配置要求后，可以通过解锁检查器授予对应称号。（验证：`apps/game-server/src/core/character_title_unlock.rs` 的 discipline_tier 测试随 `cargo test character_title_unlock --manifest-path apps/game-server/Cargo.toml` 7 passed，`tools/mock-client/src/scenarios/character.js:501` 实现职业阶位 debug 场景）
- [x] 称号授予、撤销、装备、卸下和过期处理均写入完整 `character_title_logs`。（验证：`db/init.sql:415` 定义日志表，`apps/game-server/src/core/character_title.rs` 日志写入测试随 `cargo test character_ --manifest-path apps/game-server/Cargo.toml` 32 passed）
- [x] mock-client 可以完成称号查询、测试授予、装备和结果验证流程。（验证：`tools/mock-client/src/scenarios/character.js:430` 实现 `character-titles-debug`，`tools/mock-client/README.md:293` 给出命令，`tests/mock-client-protocol.test.mjs:414` 覆盖异步匹配和 JSON 输出形态）
- [x] admin-api 至少可以只读查询角色称号、职业阶位和称号日志；如 admin-web 不做 UI，文档明确边界。（验证：`apps/admin-api/src/players/players.controller.ts:108` 定义只读 API，`apps/admin-api/src/admin-store.js:587` 查询 titles/disciplines/logs，`docs/后台与运维/管理后台设计.md:477` 和 `docs/游戏服与接入层/角色体系与四属性设计.md:989` 明确 admin-web 本阶段无页面）
- [x] 文档明确 P2 范围，以及完整职业学习、任务/场景/道具联动、战斗属性和后台写操作属于后续阶段。（验证：`docs/游戏服与接入层/角色体系与四属性设计.md:906`-`923` 说明 P2 当前范围和不包含项，`docs/协议与客户端/外部客户端接入说明.md:77` 说明外部客户端边界）
