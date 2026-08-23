# 活动模块01：领域基础 Checklist

## 目标

建立不包含具体活动玩法的公共活动领域：草稿与发布版本、生命周期、阶段和奖励配置、玩家活动状态、领奖记录、审计记录、类型注册和配置校验接口。

任务/成就进度配置不属于活动配置来源。活动只能通过统一资产交付能力发奖，不直接复用任务模块的 progress_id 作为活动事实。

## 基础原则

- [x] 类型独有代码放在同一个 `activity/types/` 目录下的独立文件中。（验证：`apps/game-server/src/activity/types/login_reward.rs` 与 `lottery.rs` 同级注册，均为 contract-only handler。）
- [x] 已发布版本不可变，领奖记录引用活动版本。（验证：`activity_version`/claim FK 与 immutable trigger、`uk_activity_claim_semantic` 在 migration 中定义；Rust 版本隔离测试通过。）
- [x] PostgreSQL 是活动事实源，Redis 只做缓存、限流和刷新通知。（验证：活动 migration/init 为 PostgreSQL 表，`cache.rs` 仅提供 Redis snapshot/list/PUBLISH 适配器；正式文档已同步边界。）
- [x] 公共模块不得写入登录奖励或抽奖算法分支。（验证：公共 domain/repository/cache 无玩法分支，两个类型 handler 只返回 contract-only 结果；测试与 README 明确具体玩法未落地。）

## 引用文档

- [活动功能总纲](../活动功能总纲.md)：活动领域模型、版本、生命周期、类型注册和数据边界。
- [游戏业务模块开发规范](../../游戏业务模块开发规范.md)：业务模块目录、领域分层和跨模块职责。
- [数据库初始化说明](../../../数据库/数据库初始化说明.md)：数据库初始化和 schema 维护边界。
- [数据库迁移体系设计](../../../数据库/数据库迁移体系设计.md)：数据库迁移、兼容和回滚要求。
- [统一资产事务与奖励交付运行边界](../../背包与物品/统一资产事务与奖励交付运行边界.md)：奖励来源、资产流水、幂等和恢复边界。
- [服务注册中心设计](../../../周边服务/服务注册中心设计.md)：多实例服务发现和跨服务访问约束。

## 阶段 1：业务边界

- 开始时间：2026-08-21 11:26:02 +08:00
- 结束时间：2026-08-21 11:30:00 +08:00
- 开发总结：冻结首期活动边界：character scope、UTC/IANA 时间语义、生命周期迁移、ended 领取宽限和任务/进度隔离。
- 验证记录：活动总纲新增 1.3 契约，并同步状态表、版本字段和待决事项；`git diff --check` 通过，边界关键词检查通过。

- [x] 明确首期活动 owner 使用 character_id，账号级活动后置。（验证：活动总纲 1.3 明确首期 `scope=character`，使用可信 `character_id`，账号级活动后置。）
- [x] 明确 UTC 存储、IANA 时区计算和 `[start_at, end_at)` 时间窗。（验证：活动总纲 1.3 明确 `TIMESTAMPTZ`、IANA 时区及左闭右开边界。）
- [x] 明确草稿、published、running、ended、offline、archived 状态迁移。（验证：活动总纲 1.3/4.1 明确合法迁移路径及 offline/ended 行为。）
- [x] 明确活动结束后已产生资格的领取策略。（验证：活动总纲 1.3 明确 `claim_deadline`、ended 仅可领取已产生资格、offline 全拒绝。）
- [x] 明确配置表、活动版本和任务/进度模块的隔离边界。（验证：活动总纲 1.3 明确 PostgreSQL 活动事实源，禁止复用 `CharacterProgressTable.csv`、`progress_id` 及任务领奖日志。）

## 阶段 2：数据库模型与迁移

- 开始时间：2026-08-21 11:31:14 +08:00
- 结束时间：2026-08-21 11:45:27 +08:00
- 开发总结：新增活动事实模型、版本/配置快照、玩家状态、领奖/抽奖、奖励交付流水和审计表；加入版本不可变、append-only、时间窗与唯一语义键约束，并同步初始化、迁移说明和迁移测试。
- 验证记录：`node --test tests/db/db-cli.test.mjs` 31/31、`node --test tests/characters/db-init-characters.test.mjs` 18/18；migration safety、init/migration DDL 一致性和 `git diff --check` 通过；未执行 PostgreSQL 写入型演练。

- [x] 创建 activity、activity_version、activity_stage 和奖励组/奖励项模型。（验证：`db/migrations/game/20260821120000_add_activity_schema.sql` 定义活动主表、不可变版本、阶段、奖励组和奖励项及 FK/check 约束。）
- [x] 创建 player_activity_state，保存公共状态和类型专属 JSON 状态。（验证：`player_activity_state` 包含 `character_id`、版本、`progress_json`、`type_state_json`、revision 和 owner/version 唯一约束。）
- [x] 创建 activity_claim_record、抽奖结果、reward_grant_ledger 和 activity_audit_log。（验证：migration 定义四张事实/流水表及状态、快照、request_id、审计字段；流水和审计含 append-only 触发器。）
- [x] 为 owner、activity_id、version、stage_id、period_key 和 semantic_claim_key 建立索引/唯一约束。（验证：owner/version、claim semantic key、stage/period 索引与 activity/version 唯一键均在 migration 中定义。）
- [x] 完成 additive migration、空库初始化和回滚兼容说明。（验证：game migration safety 通过；`db/init.sql` 与 migration DDL 一致；数据库初始化/迁移说明已同步；迁移事务为 expand、未执行破坏性变更。）

## 阶段 3：领域对象与仓储

- 开始时间：2026-08-21 11:46:41 +08:00
- 结束时间：2026-08-21 12:03:50 +08:00
- 开发总结：建立 activity 领域对象、生命周期/时间窗/领取截止校验、不可变版本摘要校验、草稿与发布 CAS repository，以及 Redis/内存缓存和刷新通知端口；错误码覆盖状态、版本、时间窗、未知类型和缓存异常。
- 验证记录：`cargo test --manifest-path apps/game-server/Cargo.toml --bin game-server activity:: --quiet` 最终 16/16（含下线 CAS）；新增 activity 文件 rustfmt 检查通过；migration safety、`db-cli` 31/31、迁移/init DDL 一致性和 `git diff --check` 通过。

- [x] 定义 Activity、ActivityVersion、ActivityStage、RewardGroup、PlayerActivityState 和 ClaimRecord。（验证：`apps/game-server/src/activity/domain.rs` 定义六类公共领域对象及奖励项结构。）
- [x] 实现时间窗判断、状态迁移、版本快照和配置摘要校验。（验证：`domain.rs` 实现 `[start_at,end_at)`、生命周期迁移、claim deadline 和 JSON digest 校验；对应边界测试通过。）
- [x] 实现草稿写入与发布版本读取的独立 repository。（验证：`repository.rs` 提供 `save_draft`、publish CAS、published snapshot/list 和内存 fake；测试覆盖草稿隔离、版本切换和冲突。）
- [x] 实现 Redis 活动列表/版本缓存和刷新通知。（验证：`cache.rs` 提供版本/list key、JSON snapshot、Redis `PUBLISH` 和内存 fake 通知；缓存测试通过。）
- [x] 定义状态非法、版本冲突、时间窗外和未知类型错误码。（验证：`ActivityErrorCode` 输出稳定 `ACTIVITY_*` 错误码，测试覆盖 unknown type、scope/version/digest、领取过期和 offline。）

## 阶段 4：类型注册契约

- 开始时间：2026-08-21 12:04:36 +08:00
- 结束时间：2026-08-21 12:14:00 +08:00
- 开发总结：建立五职责类型契约、版本化 schema/action 校验、同级 login_reward/lottery fake handler 注册和 dispatch；admin-api/admin-web 复用共享 schema 注册接口，公共流程不包含具体玩法规则。
- 验证记录：Rust activity/types 4/4、admin-api/admin-web Node 契约测试 4/4；rustfmt 与 `git diff --check` 通过。

- [x] 定义 ActivityTypeHandler、ConfigValidator、PlayerViewBuilder、ActionEvaluator 和 ActionApplier。（验证：`apps/game-server/src/activity/types/mod.rs` 定义五个职责 trait，ActivityTypeHandler 组合其余四项。）
- [x] 在 `game-server/.../activity/types/mod.rs` 注册同级 `login_reward.rs` 与 `lottery.rs`。（验证：`types/mod.rs` 默认注册两个同级 handler 文件，均实现五职责契约。）
- [x] 为 admin-api/admin-web 定义同样的类型 schema 注册接口。（验证：`packages/activity-contract/index.js` 提供共享 registry/schema/action API，两个后台入口 re-export 并各有契约测试。）
- [x] 未知类型、未知动作和 schema 版本不兼容时拒绝处理。（验证：Rust registry 与 Node shared contract 返回 `ACTIVITY_UNKNOWN_TYPE`、`ACTIVITY_UNKNOWN_ACTION`、`ACTIVITY_SCHEMA_VERSION_UNSUPPORTED`，测试通过。）
- [x] 使用 fake handler 验证公共流程，不引入任何具体玩法。（验证：`login_reward.rs`/`lottery.rs` 仅返回 `contract_only` fake view/outcome，未实现登录进度或抽奖算法；dispatch 测试通过。）

## 阶段 5：测试与文档

- 开始时间：2026-08-21 12:14:38 +08:00
- 结束时间：2026-08-21 12:18:43 +08:00
- 开发总结：新增无外部服务依赖的活动 schema/DDL 契约测试，覆盖迁移初始化一致性、唯一约束、版本不可变和 append-only；同步活动 README、架构/数据库/协议入口并明确当前实现状态。
- 验证记录：活动及 admin 契约测试 7/7、Rust activity 最终 16/16、数据库 CLI 49/49（含角色初始化 18 项）、`git diff --check` 通过；未启动 PostgreSQL、Redis、Docker 或服务。

- [x] 覆盖状态迁移、时间边界、版本不可变、缓存刷新和数据库唯一约束。（验证：Rust activity 14/14 覆盖状态/时间/版本/缓存；`tests/activity/activity-contract.test.mjs` 覆盖 schema 唯一键与不可变触发器。）
- [x] 覆盖草稿/发布隔离、非法配置拒绝和审计写入。（验证：repository/type tests 覆盖草稿隔离、scope/version/digest/unknown action 拒绝；静态 schema 测试覆盖审计 actor/event、append-only 与 revoke。）
- [x] 同步活动总纲、数据库说明、协议入口和代码目录约定。（验证：活动 README、游戏服索引、整体架构、数据库初始化说明和协议设计新增当前实现状态/边界/目录说明。）

## 最终完成定义

- 开始时间：2026-08-21 12:20:06 +08:00
- 结束时间：2026-08-21 12:35:49 +08:00
- 验收总结：活动领域基础已完成：公共模型与数据库迁移、版本/生命周期/下线、草稿与发布读取仓储、Redis 缓存通知、类型注册契约、后台 schema 契约、测试和文档边界均已落地。具体登录奖励/抽奖玩法、生产 PostgreSQL 仓储、后台发布 API 和玩家协议仍按后续 checklist 开发。

- [x] 公共领域可以保存、发布、读取和下线活动版本。（验证：`ActivityRepository` 提供 save_draft/publish/get/list/offline；InMemory repository 测试覆盖发布读取、CAS 下线、版本冲突和离线隐藏。）
- [x] 所有活动类型以同一 types/ 目录下的独立文件注册。（验证：`activity/types/mod.rs` 默认注册同级 `login_reward.rs` 与 `lottery.rs`，Rust 类型测试通过。）
- [x] 公共模块不包含登录或抽奖专属规则分支。（验证：公共模块仅处理生命周期、版本、缓存和契约；fake handler 无进度累计、随机、保底或发奖算法。）
