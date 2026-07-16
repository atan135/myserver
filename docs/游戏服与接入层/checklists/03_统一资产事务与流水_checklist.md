# 统一资产事务与奖励交付 Checklist

## 目标

在现有背包、仓库、装备、邮件附件和 GM 发物品基础上建立统一资产变更与奖励交付内核。来源模块只生成服务端权威奖励单并声明交付策略；`direct` 和 `mail` 最终共用 inventory 资产事务，提供幂等、原子性、容量保底、冻结、流水、审计和故障恢复。

首期只覆盖现有物品资产以及成就、任务、战斗、场景、活动、排行榜、GM 等模块接入奖励时所需的公共契约；不提前实现商城、拍卖、玩家交易、完整掉落算法或具体货币经济。

## 基础原则

- [ ] 服务端决定奖励内容和资产结果，客户端只提交成就 ID、结算 ID、掉落实体 ID、邮件 ID 等可验证意图。
- [ ] 区分业务来源 `origin`、交付方式 `delivery_method`、最终结算目标 `inventory` 和提交后客户端通知。
- [ ] 奖励在任意时刻必须存在于来源模块待结算、已提交 inventory 或已持久化奖励邮件之一，不能从三处同时消失。
- [ ] 每次资产变更必须有 `request_id`、来源、原因和操作者上下文；状态与流水同事务提交。
- [ ] 失败不产生部分结果；相同请求重试返回首次确定结果，结果未知时先查询而不是切换渠道。
- [ ] `ItemObtainPush`、`InventoryUpdatePush` 和新邮件提示都只通知已经提交的权威状态，push 失败不回滚资产。
- [ ] 各阶段只在获得用户确认后运行需要启动 PostgreSQL、Redis、NATS 或真实服务的联调命令。

## 阶段 1：现状盘点与统一资产模型

- 开始时间：2026-07-15 16:01:15 +08:00
- 结束时间：2026-07-15 16:17:22 +08:00
- 开发总结：建立可复用资产领域模型并接入现有物品堆叠判定；以代码路径记录现有资产写入口、JSONB 快照与并发边界，明确首期只覆盖物品资产。
- 验证记录：静态审核 `asset.rs`、`item.rs` 和写入口基线文档；`cargo test asset --manifest-path apps/game-server/Cargo.toml` 与 `cargo test inventory --manifest-path apps/game-server/Cargo.toml` 均通过，后者执行 30 项测试且 0 failed。

- [x] 盘点 inventory、warehouse、equipment、邮件附件、GM grant、职业学习消耗、角色进度和测试 `ItemAddReq` 的全部写入口。（验证：`docs/游戏服与接入层/统一资产模型与写入口基线.md`“已发现写入口基线”逐项记录代码路径与风险）
- [x] 记录 `character_inventory` JSONB 模型、在线内存副本、邮件 grant 事务和普通 `save_player` 整体覆盖的并发方式。（验证：基线文档“当前持久化与并发事实”关联 `db/init.sql`、`db_player_store.rs` 与 `player_manager.rs`）
- [x] 定义资产类型、容器、堆叠身份、绑定状态、锁定状态和配置版本。（验证：`apps/game-server/src/core/inventory/asset.rs` 定义 `AssetType`、`AssetContainer`、`ItemStackIdentity`、`AssetBinding`、`AssetLockState`、`AssetConfigVersion`，并由 `Item::can_stack_with` 接线）
- [x] 定义业务来源枚举与稳定来源 ID，至少覆盖 achievement、quest、battle、scene_pickup、activity、ranking、world_event 和 gm。（验证：`asset.rs` 的 `AssetOriginType` / `AssetOrigin` 覆盖全部目标来源并拒绝空 `origin_id`）
- [x] 明确 `direct` / `mail` 是交付方式、inventory 是最终道具结算目标、push 是提交后通知。（验证：`asset.rs` 的 `AssetDeliverySemantics` 以及对应单元测试通过）
- [x] 通过代码搜索形成现有资产旁路写入基线，并记录首期范围与暂缓的货币、交易、拍卖能力。（验证：基线文档“首期边界”“已发现写入口基线”“后续接线要求”明确上述范围和收敛约束）

## 阶段 2：奖励单、资产命令与结果契约

- 开始时间：2026-07-15 16:19:45 +08:00
- 结束时间：2026-07-15 16:51:49 +08:00
- 开发总结：定义可校验的服务端奖励单、全成全败资产命令、权限与前置条件、稳定结果/错误语义，并新增 Rust/Node 可共享的 JSON 兼容 fixture。
- 验证记录：主 agent 审阅后修复容器版本顺序导致的指纹不稳定及三处编译问题；`cargo test contract --manifest-path apps/game-server/Cargo.toml` 通过，`cargo test inventory --manifest-path apps/game-server/Cargo.toml --quiet` 执行 35 项测试且 0 failed。

- [x] 定义 `RewardOrder`，包含 `request_id`、`character_id`、`origin_type`、`origin_id`、`delivery_policy`、标准化 items、reason 和 operator。（验证：`apps/game-server/src/core/inventory/contract.rs` 的 `RewardOrder::new/validate` 复用 `AssetOrigin`、规范化 items 并校验操作者权限）
- [x] 定义 `MAIL_ONLY`、`PREFER_INVENTORY` 和 `INVENTORY_REQUIRED`，明确奖励类与资产交换类业务的默认策略。（验证：`RewardDeliveryPolicy` 与 `RewardBusinessClass::default_delivery_policy` 声明三种稳定序列化策略）
- [x] 定义 grant、consume、move、equip、unequip、freeze 和 unfreeze 命令的输入、权限、前置条件和稳定错误码。（验证：`AssetOperation`、`AssetPermission`、`AssetOperationPrecondition`、`AssetCommandErrorCode` 覆盖所有目标操作）
- [x] 定义 `applied`、`not_applied`、`unknown` 结果状态以及 `INVENTORY_CAPACITY_FULL/CAPACITY_BLOCKED` 的稳定语义。（验证：`AssetResultState`、`AssetCommandErrorCode` 和 fallback 单元测试区分 query-first、可 fallback 与玩家可重试）
- [x] 结果返回实际增减、容器版本、资产流水 ID、delivery method / ID 和 fallback reason。（验证：`AssetCommandResult`、`AssetQuantityDelta`、`AssetDeliveryReceipt` 和 `AssetFallbackReason` 定义并由 fixture 断言序列化形状）
- [x] 定义批量命令全成全败语义，禁止调用方直接修改 JSONB 资产快照。（验证：`AssetCommand` 固定 `AssetBatchAtomicity::AllOrNothing`，操作仅表达 UID/数量/容器 intent，注释明确拒绝 JSONB snapshot）
- [x] 为奖励单、规范化物品、请求指纹和结果兼容增加跨语言 fixture 或协议测试。（验证：`tests/fixtures/asset-contract-v1.json` 被 `contract.rs` 单元测试读取，固定规范化结果、SHA-256 指纹与结果 JSON）

## 阶段 3：统一资产事务与并发控制

- 开始时间：2026-07-15 16:54:07 +08:00
- 结束时间：2026-07-15 17:24:40 +08:00
- 开发总结：引入角色资产 revision、角色粒度锁和事务内请求/流水记录；普通保存和 grant 均改为持久化提交后才发布在线快照，并拒绝无法原子完成的跨 store 消耗。
- 验证记录：静态审阅 revision 条件更新、grant transaction 和协议处理失败分支；`cargo test --manifest-path apps/game-server/Cargo.toml --no-run` 通过，`player_manager` 13 项、`character_discipline` 12 项、`inventory` 35 项测试均通过。

- [x] 设计角色资产锁、乐观 revision 或 PostgreSQL 行锁策略，所有玩家操作、邮件、GM 和来源模块共享同一并发边界。（验证：`PlayerManager` 的 `asset_character_locks` 与 `character_inventory.asset_revision` 条件更新被普通保存和 `grant_items_with_request` 共用）
- [x] 在同一事务内读取、校验、计算、写入 inventory 快照和资产流水。（验证：`PgPlayerStore::save_with_grant_record` 在单一 transaction 内写兼容 grant、`character_asset_requests`、revision 快照和 `character_asset_ledger`）
- [x] 实现 `request_id` 唯一约束及请求指纹冲突检测；相同参数返回首次结果，不同参数明确拒绝。（验证：`character_asset_requests.request_id` 唯一约束与 `replay_or_conflict` 保留相同请求重放、角色或指纹冲突拒绝）
- [x] 让普通保存返回持久化结果，禁止数据库失败后向客户端或来源模块报告资产成功。（验证：`save_player` 返回 `Result<PlayerData, PlayerSaveError>`，inventory handler 仅在成功保存后回成功并推送）
- [x] 收敛四属性道具和职业学习等“先写外部状态再扣道具”的跨存储部分提交风险。（验证：item use 与职业学习对跨 store 消耗返回 `ASSET_CROSS_STORE_ATOMICITY_UNAVAILABLE`，character_discipline 测试通过）
- [x] 避免持有全局玩家写锁等待数据库 IO，保证不同角色可以并行结算。（验证：持久化期间仅持有 keyed character lock，`players` RwLock 仅用于短暂读取/发布；player_manager 锁粒度测试通过）
- [x] 增加并发消耗、玩家操作与邮件 grant 交错、版本冲突、事务回滚和提交结果未知测试。（验证：player_manager 覆盖普通保存失败不发布、unknown、旧快照与 grant 交错、revision conflict、角色锁粒度；13 项过滤测试通过）

## 阶段 4：堆叠、容量、绑定与冻结规则

- 开始时间：2026-07-15 17:26:33 +08:00
- 结束时间：2026-07-15 17:50:24 +08:00
- 开发总结：物品快照持久化冻结、配置版本和有效期；容器按 MaxStack 全量预演拆格/合并，拒绝无效资产并提供只读兼容扫描。
- 验证记录：container 7 项、compatibility scan 1 项、config reload 1 项离线测试通过；主审阅确认预演失败不会修改源容器。

- [x] 按 `ItemTable.MaxStack` 规划拆格、合并、部分消耗和批量发放，先完整预演容量再修改状态。（验证：`ItemContainer` 预演计划与 container 测试通过）
- [x] 拒绝零数量、溢出数量、重复 UID 和不合法绑定状态，保证失败不改变源容器。（验证：规则化容器 API 校验并由 container 测试覆盖）
- [x] 把绑定角色、成长属性、运行时属性、有效期、规则快照和锁定状态纳入堆叠身份。（验证：`Item` JSONB 字段与 `AssetStackIdentity` 完整映射）
- [x] 执行 `BindType` 及装备绑定规则，并为 ItemTable 增加类型、槽位、效果、冷却和数值范围校验。（验证：ItemTable registry 加载/热更校验测试通过）
- [x] 冻结资产不可消费、移动、装备、分解或重复冻结。（验证：`Item::freeze/unfreeze` 与 PlayerData/容器检查覆盖冻结拒绝）
- [x] 定义容量预检的确定结果，只有明确 `INVENTORY_CAPACITY_FULL/not_applied` 才允许奖励交付 fallback。（验证：预演容量错误保持未提交语义，阶段 2 fallback 契约继续复用）
- [x] 对现有 JSONB 中的零数量、超堆叠、重复 UID、非法绑定和容量异常执行只读兼容扫描。（验证：`compatibility.rs` 的 `scan_reports_legacy_anomalies` 测试通过且不修改 PlayerData）

## 阶段 5：奖励交付编排与可靠 fallback

- 开始时间：2026-07-15 17:52:35 +08:00
- 结束时间：2026-07-16 11:12:39 +08:00
- 开发总结：实现 RewardDeliveryService、交付记录与确定性奖励邮件 outbox，连接真实 PlayerManager 事务端口、容量预演、query-first 和提交后通知。
- 验证记录：主审阅确认 unknown 不 fallback、容量 fallback 仅发生于确定未提交；`cargo test reward_delivery --manifest-path apps/game-server/Cargo.toml --quiet` 10 passed。

- [x] 实现统一 `RewardDeliveryService`，来源模块只能提交奖励单和交付策略。（验证：`reward_delivery.rs` 服务入口仅接收 `RewardOrder`）
- [x] `MAIL_ONLY` 幂等写奖励邮件；`PREFER_INVENTORY` 先结算 inventory；`INVENTORY_REQUIRED` 容量不足时整体拒绝。（验证：策略分支与 10 项 reward_delivery 测试通过）
- [x] inventory 首次成功或幂等成功后记录交付结果，并在提交后发送在线 push。（验证：delivery record 持久化后调用 notifier，push 失败不回滚）
- [x] `PREFER_INVENTORY` 明确容量不足时，通过可靠 outbox 创建确定性的奖励邮件。（验证：`RewardMailOutboxEntry::for_order` 和容量 fallback 测试通过）
- [x] 资产结果为 `unknown` 时固定进入 query-first，不得为了保底直接创建邮件。（验证：query-first 分支与 unknown 测试通过）
- [x] 来源模块只有在 inventory 成功或奖励邮件持久化成功后，才能标记原奖励已领取或移除场景掉落实体。（验证：交付 record 仅在 direct transaction 结果或 outbox 持久化后写入）
- [x] 覆盖直接成功、满包 fallback、邮件创建失败重试、结果未知、重复奖励单和进程中断测试。（验证：`cargo test reward_delivery` 10 passed）

## 阶段 6：奖励邮件托管与 blocked_capacity

- 开始时间：2026-07-16 11:13:46 +08:00
- 结束时间：2026-07-16 12:10:42 +08:00
- 开发总结：实现奖励邮件幂等托管、可信来源元数据、`blocked_capacity` 玩家重试与奖励保留保护，并扩展 mail claim v1 的请求、结果和共享 fixture 契约。
- 验证记录：主 agent 静态审阅阶段 6 的 19 个相关文件，确认容量阻塞仅对确定 `not_applied` 结果直接重试、未知结果仍 query-first；`npm --workspace mail-service test` 128 passed，`cargo test --manifest-path apps/game-server/Cargo.toml shared_node_fixture` 2 passed，`git diff --check` 通过（仅 Git CRLF 提示）。

- [x] 为奖励邮件创建增加稳定 `delivery_request_id` 和唯一约束，相同奖励单不得生成多封邮件。（验证：mails schema、DbStore 和受信 reward-deliveries 入口幂等校验）
- [x] 邮件记录保存可信 `origin_type`、`origin_id`、交付请求 ID 和操作者；玩家不能覆盖来源、附件或交付策略。（验证：受服务 token 保护的创建入口和 delivery fingerprint）
- [x] 版本化扩展邮件 -> game-server 请求、指纹和结果摘要，兼容现有 `source=mail-claim` v1 契约。（验证：`packages/proto/admin.proto` 添加 `contract_version`、`player_retryable` 和 query 版本字段；`game-admin-client.js` 编解码并校验 v1，Rust/Node 共享 fixture 测试 2 passed）
- [x] 在根 DB、mail-service DB 和运行时 schema 中增加 workflow `blocked_capacity`，并提供兼容迁移与回滚顺序。（验证：`db/init.sql`、`apps/mail-service/db/init.sql` 与 `db-client.js` 均以 additive column/constraint migration 接受该状态，SQL 注释规定先回滚应用再撤销 schema）
- [x] 将 `INVENTORY_FULL` 从 `PERMANENT_FAILURE` 改为 `CAPACITY_BLOCKED/not_applied`，响应 `retryable=false, player_retryable=true`。（验证：`apps/game-server/src/admin_server/gm.rs` 仅对 `mail-claim` 转换满包结果，`mails.service.test.ts` 断言 HTTP 409、`retryable=false` 和 `player_retryable=true`）
- [x] 容量不足时保留原 mail、冻结附件、指纹和 `claim_request_id`；不后台空转、不进入人工处理、不生成另一封邮件。（验证：`db-store.js` 保留 frozen workflow，`isClaimRecoveryDue` 排除 `blocked_capacity`，mail-service 与 recovery worker 测试均断言不进入恢复队列）
- [x] 玩家清理背包后重新领取时复用原工作流，并遵守 query-first、lease fencing 和首次结果校验。（验证：`reserveExistingClaimWorkflowMemory/Postgres` 复用 request/附件指纹并以 lease token 围栏；unknown 路径由 recovery worker query-first，确定 `blocked_capacity/not_applied` 以同 request ID 重试，`mails.service.test.ts` 覆盖）
- [x] 规定系统奖励邮件未领取附件的过期、硬删除、邮箱容量和归档保护，确保清理策略不能使奖励失去领取入口。（验证：`normalizedRewardDelivery` 拒绝 expiry，`DbMailStore.deleteMail` 拒绝删除未领取奖励邮件，管理 retention policy 声明 `no_expiry_no_hard_delete_until_claimed`）
- [x] 更新 mail-service 状态映射、HTTP 状态、安全文案、恢复 worker、管理查询、指标和相关测试。（验证：`mails.service.ts` 返回 409/安全文案，`claim-recovery.worker.ts` 停止自动恢复，`metrics.js` 记录 blocked capacity/backlog，`npm --workspace mail-service test` 128 passed）

## 阶段 7：现有入口与权限边界迁移

- 开始时间：2026-07-16 12:34:23 +08:00
- 结束时间：2026-07-16 13:22:54 +08:00
- 开发总结：退役玩家侧 `ItemAddReq/Res`，将装备、使用、丢弃和仓库存取收敛到角色资产事务；GM/mail grant 使用容量预演事务并以独立开关区分本地构造、邮件领取与紧急纠正，同时建立协议和资产写入口防回归检查。
- 验证记录：主 agent 审阅 23 个阶段 7 文件及入口迁移文档；`npm run check:proto`、`npm run check:asset-write-boundaries`、`npm --workspace mail-service test`（128 passed）、`cargo test --bin game-server --quiet`（476 passed）及 `git diff --check` 均通过。期间修复管理员审计异步写入未 flush 导致的即时可见性回归，并连续完整测试验证。

- [x] 从正式玩家 dispatch 移除 `ItemAddReq`，删除常规 mock-client 入口和外部客户端依赖，保留废弃消息号 1407/1408 且不得复用。（验证：`server.rs` 对 `DeprecatedItemAddReq/Res` 固定返回 `MESSAGE_TYPE_DEPRECATED`，`game.proto` 删除 payload，mock-client 删除 inventory-add，`npm run check:proto` 通过）
- [x] 本地构造物品能力改走受环境开关和管理凭证保护的内部工具或 admin 通道。（验证：`gm.rs` 的 `GM_ASSET_CONSTRUCTION_ENABLED` 默认关闭，admin 认证路径审计 actor/角色目标，`.env.example` 明确本地/admin 启用边界）
- [x] 迁移 GM 发物品；普通奖励使用 RewardDeliveryService，紧急资产纠正保留独立高权限、强审计入口。（验证：`gm.rs` 使用 `grant_items_with_request_using_table` 容量预演和 request 指纹；`gm-emergency-correction` 要求具名 actor 与 `GM_EMERGENCY_ASSET_CORRECTION_ENABLED`，普通业务来源的 RewardDeliveryService 接线留由阶段 8 实现）
- [x] 迁移邮件附件领取并兼容现有 `character_inventory_grants`、query-first 和滚动升级契约。（验证：`mail-claim` 继续调用 `grant_items_with_request_using_table`，由 `MAIL_CLAIM_ASSET_TRANSACTIONS_ENABLED` 独立控制；mail-service 128 项测试通过）
- [x] 迁移背包使用、丢弃、装备、卸装和仓库移动，删除对 `PlayerData` / `ItemContainer` 的旁路写入。（验证：`PlayerManager` 的 `commit_asset_mutation` 和命名操作持有角色锁、revision 持久化后发布快照；`inventory_service.rs` 仅调用事务入口，466 项 game-server 测试通过）
- [x] 通过代码搜索建立防回归检查，禁止新增未经统一资产事务的生产写入口。（验证：新增 `tools/check-asset-write-boundaries.js`，检查退役协议、handler 直写和 GM grant 事务；`npm run check:asset-write-boundaries` 通过）
- [x] 为每个入口提供开关、旧新结果对账和回滚策略。（验证：`统一资产阶段7入口迁移.md` 记录四类开关、revision/grant/audit 对账及“关闭仅阻断新写入”的回滚边界）

## 阶段 8：奖励来源模块接入

- 开始时间：2026-07-16 13:24:55 +08:00
- 结束时间：2026-07-16 13:53:41 +08:00
- 开发总结：新增服务端奖励来源适配层，以稳定 origin ID、来源状态门和 RewardDeliveryService 统一普通奖励；补齐战斗结算、场景拾取、界面领取与交换类的权威契约，并明确当前不存在正式入口的功能边界。
- 验证记录：主 agent 审阅 `reward_source.rs`、CharacterProgress 映射与阶段 8 文档；`cargo test reward_source --no-fail-fast --quiet` 10 passed、`cargo test character_progress --no-fail-fast --quiet` 14 passed、`cargo test --bin game-server --quiet` 476 passed，`git diff --check` 通过。当前没有物品型战斗结算、场景掉落或商店/制作/兑换正式入口，因此未启动无意义的真实服务联调。

- [x] 为成就、任务、活动、排行榜和世界事件定义稳定 origin ID 与重复领取约束。（验证：`reward_source.rs` 的 `RewardSourceKind/RewardSource` 固定 canonical ID、request ID 和 state key，`RewardSourceService` 仅在 `applied` 后完成来源状态）
- [x] 战斗结算只提交服务端结果生成的 RewardOrder，客户端不得提交物品列表或结算成功声明。（验证：crate 内 `BattleServerResult::from_authoritative_simulation` 生成 battle claim；现有玩家协议没有战利品或客户端结算成功字段，reward_source 测试覆盖）
- [x] 场景拾取校验掉落实体、所有权、距离和状态；只有入包或邮件托管成功后才永久移除掉落。（验证：`SceneDrop::prepare_pickup` 校验 entity/owner/radius/removed，`remove_after_delivery` 校验相同 applied request；容量 fallback mail 测试通过）
- [x] 界面领取只提交业务对象 ID；服务端校验可领取状态并调用统一交付服务。（验证：正式 `ApplyCharacterProgressReq` 仅携带 `progress_id`，`character_progress` 与 CSV 校验映射服务端 source ID；当前表无物品奖励，新增物品奖励被文档约束为 `RewardSourceService -> RewardDeliveryService`）
- [x] 商店、制作和兑换默认使用 `INVENTORY_REQUIRED`，确保扣款 / 材料消耗与产物入包全成全败。（验证：`InventoryRequiredExchange` 固定 `AssetBatchAtomicity::AllOrNothing` 和 `RewardDeliveryPolicy::InventoryRequired`，单一 AssetCommand 同时表达 Consume/Grant 且无 mail fallback）
- [x] 每个来源覆盖直接交付、mail only、容量 fallback、重复请求和来源状态不重复完成测试。（验证：reward_source 10 项测试使用 `RewardDeliveryService` 覆盖全部 source kind、direct、MAIL_ONLY、capacity fallback、重放、战斗、掉落和交换契约）

## 阶段 9：流水、查询、审计与可观测性

- 开始时间：2026-07-16 13:55:02 +08:00
- 结束时间：2026-07-16 14:35:00 +08:00
- 开发总结：扩展统一资产流水元数据并让玩家操作、grant、奖励交付和紧急纠正同事务写入；提供受控 ledger 查询、最小管理视图、append-only 修正规则、异常安全审计和跨服务指标。
- 验证记录：主 agent 复跑 `cargo test test_metrics_collector --quiet`（1 passed）与 `npm --workspace admin-web run build`（passed）；worker 验证 mail-service 128 项、admin-api 29 项、玩家流水 1 项、真实 PostgreSQL schema/参数化查询/append-only trigger。`git diff --check` 通过。

- [x] 流水记录变更前后数量、容器、来源、request ID、交付方式、邮件 ID、fallback reason 和规则版本。（验证：`character_asset_ledger` 扩展元数据和容器 delta，grant、奖励交付、玩家操作与紧急纠正均同事务写入）
- [x] 提供按角色、request ID、origin、delivery ID 和时间查询流水的内部接口。（验证：`GET /api/v1/assets/ledger` 与 AdminStore 参数化过滤查询，真实 PostgreSQL pool 验证空结果及 count）
- [x] 管理后台只展示必要字段并受细粒度权限控制，紧急资产纠正要求独立权限和操作者。（验证：`assets.ledger.read` 权限、AssetLedger 最小视图，`gm.asset_correction.emergency` 要求 actor 和独立开关）
- [x] 增加事务耗时、版本冲突、幂等命中、容量 fallback、奖励邮件创建、blocked capacity 和 push 失败指标。（验证：game-server `MetricsCollector`、mail-service metrics 和相关测试覆盖新增指标）
- [x] 异常大额或高频变更生成安全审计事件；日志不输出完整附件、邮件正文、token 或无界资产快照。（验证：GM 高额/高频与紧急纠正写安全审计，ledger 查询审计仅记录过滤条件/数量，mail-service 128 项安全日志测试通过）
- [x] 定义流水纠正方式，禁止直接删除或改写历史记录。（验证：root DB 与 runtime migration 的 UPDATE/DELETE/TRUNCATE guard 拒绝改写，文档规定仅追加反向或补偿事务；真实 PostgreSQL transaction 验证通过）

## 阶段 10：测试、灰度、文档与最终验收

- 开始时间：2026-07-16 14:36:50 +08:00
- 结束时间：2026-07-16 15:17:07 +08:00
- 开发总结：完成资产全量离线验证、真实服务发现邮件领取联调、存量对账与迁移演练；修复邮件 `expires_at` 参数类型和初始化脚本连接串泄漏，同步运行边界文档。
- 验证记录：inventory 49、reward 29、capacity 12、concurrent 2、transaction_failure 1；mail-service 129 passed；`check:proto` 通过。真实 PostgreSQL/Redis/NATS/auth-http/game-server/game-proxy/mail-service 联调验证邮件创建、领取、重复领取与冻结 request recovery，ledger/workflow 对账无异常。

- [x] 运行 game-server 资产相关单元测试。（验证：inventory 49、reward 29、capacity 12、concurrent 2、transaction_failure 1 通过）
- [x] 运行 `npm --workspace mail-service test`。（验证：129 passed，覆盖 blocked capacity、原请求重试和奖励邮件保护）
- [x] 更新并运行数据库初始化、共享协议和 mock-client 协议检查。（验证：root DB init 与 mail `db:init` 幂等通过，`npm run check:proto` 通过）
- [x] 覆盖容量、堆叠、幂等、并发、回滚、未知结果、中断与跨服务恢复。（验证：离线过滤测试和真实 mail claim/frozen request recovery 通过）
- [x] 对存量资产和邮件工作流执行兼容扫描、迁移演练、对账和回滚演练。（验证：实库 workflow/mail mismatch 0、orphan 0、重复 request ledger 0，append-only rollback guard 通过）
- [x] 经用户授权运行真实联调。（验证：PostgreSQL、Redis、NATS、auth-http、game-proxy、game-server、mail-service 已启动并完成 proxy 到 mail claim 链路；mock-client 协议由 check:proto 覆盖）
- [x] 同步背包、邮件、协议、数据库、监控、运维和外部客户端接入文档。（验证：新增运行边界文档并更新相关专题文档）
- [x] checklist 完成后移动到 `docs/<领域>/checklists/` 归档，再纳入 Git 提交。（验证：主 agent 归档到 `docs/游戏服与接入层/checklists/` 并纳入本轮提交）

## 最终完成定义

以下项目作为整体完成标准，不要求每个开发阶段都执行，由所有相关阶段完成后统一验收。

- 开始时间：2026-07-16 11:13:46 +08:00
- 结束时间：2026-07-16 15:17:07 +08:00
- 验收总结：阶段 1-10 已完成；统一资产事务、奖励 direct/mail、邮件容量阻塞重试、入口迁移、来源契约、流水查询与真实服务联调均已验证，保留既有编译 warning 与未实现业务入口边界。

- [x] 现有物品资产变更不存在未经统一资产事务内核的生产写入口。（验证：`check:asset-write-boundaries` 通过）
- [x] 成就、战斗、场景、活动、GM 等来源与 direct / mail 交付方式可独立追踪。（验证：RewardSource 与 ledger 元数据）
- [x] 奖励直接入包容量不足时可靠转为唯一奖励邮件，结果未知时不会重复发放。（验证：reward/mail 测试与实库链路）
- [x] 邮件领取容量不足时保留原附件并可由玩家重试，不进入永久失败、不生成新邮件。（验证：blocked_capacity 测试）
- [x] 重试、并发、超时和进程中断不会造成重复、丢失、负数、部分提交或成功记录被旧快照覆盖。（验证：并发、未知和 recovery 测试）
- [x] 每笔奖励和资产变化可按来源、交付方式、request ID 和流水完整追溯。（验证：资产 ledger 查询和 append-only guard）
- [x] 生产玩家协议不再暴露直接构造道具能力，废弃消息号不会被复用。（验证：1407/1408 reservation 和 protocol check）
- [x] 现有客户端背包、邮件领取、GM 纠正和在线 push 行为保持兼容或具备明确迁移方案。（验证：真实 proxy/mail claim 联调与阶段 7 文档）
