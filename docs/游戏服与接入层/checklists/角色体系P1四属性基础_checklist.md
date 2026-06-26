# 角色体系 P1 四属性基础 Checklist

来源文档：`docs/游戏服与接入层/角色体系与四属性设计.md`

## 目标

完成 P1“四属性基础与变更闭环”阶段，在 P0 已完成账号 `player_id` 与游戏内 `character_id` 拆分的基础上，落地角色自身四属性状态查询和统一永久变更入口。

P1 完成后应达到：服务端可以按 `character_id` 查询角色 `affinity` 与 `mastery`；所有永久四属性变化必须经过统一服务入口；变更过程校验合法性、写入前后快照和来源日志；mock-client 可以完成“登录、选角、查询四属性、触发测试变更、再次查询验证”的调试流程。

本阶段只做 1-2 个大的功能点：角色四属性状态模型、统一四属性变更入口。职业/流派、称号、战斗公式、完整背包归属迁移和道具真实使用链路不放入本阶段。

## 基础原则

- [ ] 数据库按新开发处理，不编写历史 migrate；结构以 `db/init.sql` 空库初始化直接落新模型为准。
- [ ] `player_id` 继续表示账号玩家 ID；本阶段所有角色成长状态以 `character_id` 为主键。
- [ ] `affinity` 表示四属性倾向比例，总和固定为 `10000`。
- [ ] `mastery` 表示四属性实际掌握值，总和不固定，但单项不得被永久变更为负数。
- [ ] 所有永久四属性变化必须走统一服务端入口，不允许任务、道具、GM、战斗或调试代码直接改 `characters` 字段。
- [ ] 每次永久变化必须记录来源、操作者、原因、变更前快照和变更后快照。
- [ ] 本阶段可以预留 `source_type = item / quest / discipline / gm / system`，但不实现完整道具、任务、职业和称号联动。
- [ ] 背包和道具真实接入四属性变更服务不放入本阶段，只保留后续接入边界。
- [ ] 完成功能后同步协议文档、角色体系文档、外部客户端接入说明和 mock-client 说明。
- [ ] 模块开发完成后，先提示需要启动的服务和依赖，待确认后再执行集成测试或联调脚本。

## 阶段 1：数据模型和边界确认

- 开始时间：2026-06-26 11:26:37 +08:00
- 结束时间：2026-06-26 11:32:23 +08:00
- 开发总结：完成 P1 四属性基础数据库模型确认；`characters` 保留 affinity/mastery 八字段并补充非负与总和约束，新增 `character_element_logs` 作为永久四属性变更日志表，并补齐查询索引。
- 验证记录：`node --test --experimental-test-isolation=none --test-concurrency=1 tests/db-init-characters.test.mjs` 通过，8/8 tests passed。

- [x] 确认 `characters` 表中的 `affinity_earth/fire/water/wind` 与 `mastery_earth/fire/water/wind` 字段满足 P1 需求，并在 `db/init.sql` 中直接维护目标结构。（验证：`db/init.sql` 的 `characters` 表包含八个字段及默认值；`tests/db-init-characters.test.mjs` 的 `characters table contains P0 identity split base fields and defaults` 通过）
- [x] 新增 `character_element_logs` 初始化结构，字段包含 `character_id`、来源、操作者、八个 delta、`before_json`、`after_json`、`reason` 和 `created_at`。（验证：`db/init.sql` 新增 `character_element_logs` 表；`tests/db-init-characters.test.mjs` 的 `character element logs capture P1 source, operator, deltas, snapshots, and reason` 通过）
- [x] 增加必要索引和约束，至少覆盖 `character_id` 查询、按时间倒序查看日志、`affinity` 总和固定为 `10000`。（验证：`db/init.sql` 包含 `ck_characters_affinity_total`、四属性非负约束和 `idx_character_element_logs_character_created_at_desc` 等索引；`tests/db-init-characters.test.mjs` 的索引与约束用例通过）

## 阶段 2：四属性读取和统一变更服务

- 开始时间：2026-06-26 11:33:55 +08:00
- 结束时间：2026-06-26 11:49:22 +08:00
- 开发总结：新增 game-server 角色四属性模型、PostgreSQL store 和统一变更 service；服务挂入 `ServiceContext`，可按鉴权 identity 的 `character_id` 查询，并在事务内锁定角色、校验、更新与写入变更日志。
- 验证记录：`cargo test character_element --manifest-path apps/game-server/Cargo.toml` 通过，6/6 targeted tests passed；`rustfmt --edition 2024 --check apps\game-server\src\core\character_element.rs apps\game-server\src\core\context.rs apps\game-server\src\core\mod.rs apps\game-server\src\core\service\room_service.rs apps\game-server\src\internal_server.rs apps\game-server\src\server.rs` 通过。未启动 PostgreSQL。

- [x] 在 `game-server` 侧新增角色四属性 store/service，支持按已鉴权上下文中的 `character_id` 读取当前 `affinity` 与 `mastery`。（验证：`apps/game-server/src/core/character_element.rs` 定义 `CharacterElementService::get_elements_for_identity`，只读取 `AuthenticatedSessionIdentity.character_id`；`apps/game-server/src/core/context.rs` 将 service 挂入 `ServiceContext`）
- [x] 实现统一永久变更入口，例如 `CharacterElementService.apply_change(character_id, change, source, reason)`，集中处理校验、更新和日志写入。（验证：`apps/game-server/src/core/character_element.rs` 的 `PgCharacterElementStore::apply_change` 使用 `FOR UPDATE` 读取角色、更新 `characters` 并插入 `character_element_logs`；`server.rs` 初始化并关闭 `PgCharacterElementStore`）
- [x] 实现变更合法性校验：拒绝导致 `affinity` 总和不为 `10000` 的变更，拒绝导致任一 `mastery` 小于 `0` 的变更，并返回明确错误码。（验证：`apps/game-server/src/core/character_element.rs` 中 `validate_affinity` 返回 `INVALID_AFFINITY_TOTAL`，`validate_mastery` 返回 `NEGATIVE_MASTERY`；`cargo test character_element --manifest-path apps/game-server/Cargo.toml` 通过）

## 阶段 3：协议接口和 mock-client 调试流程

- 开始时间：2026-06-26 11:50:59 +08:00
- 结束时间：2026-06-26 12:28:23 +08:00
- 开发总结：新增角色四属性查询和受控 debug 变更玩家协议，协议号为 `1413-1416`；查询与变更均绑定当前鉴权 `character_id`，debug 变更还要求匹配 `GAME_ADMIN_TOKEN`，并通过统一四属性服务写入日志；mock-client 新增 `character-elements-debug` 场景完成查询、测试变更、再次查询 JSON 输出。
- 验证记录：`cargo test character_element --manifest-path apps/game-server/Cargo.toml` 通过，8/8 targeted tests passed；`cargo check --manifest-path apps/game-server/Cargo.toml` 通过；`cargo check --manifest-path apps/match-service/Cargo.toml` 通过；`node --check` 覆盖修改的 mock-client JS 文件通过；`node tools/check-mock-client-protocol.js` 因外部 `C:\project\mybevy` 不存在而失败，未启动服务或执行联调。

- [x] 新增角色四属性查询协议或接口，客户端只能查询当前已鉴权角色的四属性状态。（验证：`packages/proto/game.proto` 定义 `GetCharacterElementsReq/Res`，`apps/game-server/src/protocol/message_type.rs` 分配 `1413/1414`，`character_element_service::handle_get_character_elements` 调用 `get_elements_for_identity(identity)`）
- [x] 新增受控的测试/GM 调试变更入口，用于触发一次四属性变更并验证日志，不向普通玩家开放任意修改能力。（验证：`DebugApplyCharacterElementChangeReq/Res` 使用 `1415/1416`，handler 不接收客户端 `character_id`，只用鉴权 `identity.character_id`，并要求 `debug_token` 匹配 `GAME_ADMIN_TOKEN` 后调用 `CharacterElementService.apply_change`）
- [x] 更新 `tools/mock-client`，支持登录选角后查询四属性、触发测试变更、再次查询并输出 JSON 结果。（验证：`tools/mock-client/src/scenarios/character.js` 新增 `runCharacterElementsDebug`，`constants.js` 注册 `character-elements-debug`，`index.js` 完成场景路由；修改文件 `node --check` 通过）

## 阶段 4：测试、文档和验收

- 开始时间：2026-06-26 12:30:07 +08:00
- 结束时间：2026-06-26 12:50:19 +08:00
- 开发总结：补齐 mock-client 四属性协议编解码和 `character-elements-debug` 场景自动化测试；同步角色体系、协议、外部客户端接入和 mock-client 文档，明确 P1 范围、异步状态处理要求、debug token 边界和手动联调依赖。
- 验证记录：`node --test --experimental-test-isolation=none --test-concurrency=1 tests/db-init-characters.test.mjs tests/mock-client-protocol.test.mjs tests/mock-client-character.test.mjs` 通过，26/26 tests passed；`node --check tools/mock-client/src/messages.js` 与 `node --check tools/mock-client/src/scenarios/character.js` 通过；`cargo test character_element --manifest-path apps/game-server/Cargo.toml` 通过，8/8 targeted tests passed。未启动服务或执行联调。

- [x] 增加自动化测试，覆盖空库初始化结构、四属性查询、合法变更、非法变更、日志写入和 mock-client 输出。（验证：`tests/db-init-characters.test.mjs` 覆盖表结构和日志字段；`apps/game-server/src/core/character_element.rs` 测试覆盖合法/非法变更；`tests/mock-client-protocol.test.mjs` 覆盖四属性协议编解码；`tests/mock-client-character.test.mjs` 覆盖 `character-elements-debug` JSON 输出）
- [x] 同步更新角色体系、协议、外部客户端接入和 mock-client 文档，明确真实 client 需要把四属性变化推送或查询结果作为异步状态处理。（验证：`docs/游戏服与接入层/角色体系与四属性设计.md`、`docs/协议与客户端/协议设计.md`、`docs/协议与客户端/外部客户端接入说明.md`、`tools/mock-client/README.md` 和 `help.txt` 已写明 `1413-1416`、P1 范围与异步状态处理）
- [x] 完成最终验收前整理手动联调步骤，列出需要启动的 PostgreSQL、Redis、NATS、auth-http、game-proxy 和 game-server 依赖，并等待用户确认后再执行。（验证：`docs/游戏服与接入层/角色体系与四属性设计.md` 的 “P1 手动联调步骤” 列出 PostgreSQL、Redis、Core NATS、`auth-http`、`game-proxy`、`game-server` 依赖和等待确认要求）

## 最终完成定义

以下项目作为整体完成标准，不要求每个开发阶段都执行，由所有相关阶段完成后统一验收。

- 开始时间：2026-06-26 12:50:19 +08:00
- 结束时间：2026-06-26 13:33:13 +08:00
- 验收总结：P1 四属性基础的数据库结构、game-server 统一服务、玩家协议、mock-client 调试流程、自动化测试和文档已完成并提交。经用户确认后已启动 PostgreSQL、Redis、Core NATS、auth-http、match-service、game-server 和 game-proxy 完成真实服务联调；合法四属性变更成功落库并写入完整日志，bad token 与非法 affinity 总和负例均被拒绝且未写入成功日志。

- [x] 空库执行 `db/init.sql` 后包含目标四属性字段和 `character_element_logs`。（验证：`db/init.sql` 包含 `characters` 八个四属性字段和 `character_element_logs`；`tests/db-init-characters.test.mjs` 静态结构测试通过。未启动真实 PostgreSQL 执行建库）
- [x] 已选角进入游戏的连接可以按 `character_id` 查询当前四属性。（验证：`GetCharacterElementsReq/Res` 使用当前鉴权 `identity.character_id`，`tests/mock-client-character.test.mjs` fake TCP 场景验证选角后按 `seq=2/4` 查询四属性输出）
- [x] 合法四属性变更可以成功落库，并写入完整变更日志。（验证：真实服务栈下 `node tools/mock-client/src/index.js --scenario character-elements-debug ... --character-id chr_1rq2yxb40020 --json-output` 返回 `ok=true`；PostgreSQL 查询确认 `characters` 更新为 `affinity_earth=2400, affinity_fire=2600, affinity_water=2500, affinity_wind=2500, mastery_fire=10`，`character_element_logs` 写入 `source_type=gm`、`source_id=debug-character-elements`、`operator_id=plr_1rq2yvpg0020`、八个 delta、`before_json`、`after_json` 和 `reason=p1-server-integration-20260626`）
- [x] 非法 `affinity` 总和和非法负数 `mastery` 变更会被拒绝。（验证：`apps/game-server/src/core/character_element.rs` 单元测试覆盖 `INVALID_AFFINITY_TOTAL` 与 `NEGATIVE_MASTERY`，`cargo test character_element --manifest-path apps/game-server/Cargo.toml` 通过）
- [x] mock-client 可以完成查询、测试变更和结果验证流程。（验证：`tests/mock-client-character.test.mjs` 的 `character-elements-debug queries, applies a controlled change, and emits JSON output` 通过；真实服务栈下 `character-elements-debug` 经 `auth-http -> game-proxy TCP fallback 14000 -> game-server` 返回 before/change/after，合法变更 `ok=true`）
- [x] 文档明确 P1 范围，以及背包、职业、称号、战斗联动属于后续阶段。（验证：`docs/游戏服与接入层/角色体系与四属性设计.md` 的 P1 当前范围 / P1 不包含章节已明确边界）
