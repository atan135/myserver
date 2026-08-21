# 活动模块04：登录奖励 Checklist

## 目标

实现 login_reward 的全部独有逻辑，并将类型实现作为同一 `types/` 目录下的独立文件：

    apps/game-server/src/business/activity/types/login_reward.rs
    apps/admin-api/src/modules/activity/types/login_reward.ts
    apps/admin-web/src/modules/activity/types/login_reward.ts

公共 ActivityEngine 只调用该类型注册的 handler、validator 和 view builder。

## 基础原则

- [ ] 登录奖励属于运营活动，不复用任务/成就 progress_id。
- [ ] 登录事件由服务端产生，客户端不能提交登录天数。
- [ ] 日期按活动时区计算，period_key 和领取语义键由服务端生成。
- [ ] 阶段奖励通过统一领奖和资产交付入口结算。

## 引用文档

- [活动功能总纲](../../docs/游戏服与接入层/活动系统/活动功能总纲.md)：登录奖励的运营活动定位、时间、周期、阶段和领取规则。
- [任务系统设计](../../docs/游戏服与接入层/任务与世界/任务系统设计.md)：任务/进度模块边界；登录奖励不得复用任务 progress_id。
- [游戏业务模块开发规范](../../docs/游戏服与接入层/游戏业务模块开发规范.md)：活动来源与角色业务模块的职责边界。
- [统一资产事务与奖励交付运行边界](../../docs/游戏服与接入层/背包与物品/统一资产事务与奖励交付运行边界.md)：活动奖励交付、背包满 fallback、幂等和流水。
- [协议设计](../../docs/协议与客户端/协议设计.md)：活动详情、进度和领奖协议接入约束。
- [外部客户端接入说明](../../docs/协议与客户端/外部客户端接入说明.md)：客户端展示进度和领取结果的接入边界。

## 阶段 1：类型配置与注册

- 开始时间：2026-08-21 15:49:17 +08:00
- 结束时间：2026-08-21 15:58:41 +08:00
- 开发总结：完成 login_reward schema/version 1、三端同级 types 文件、配置 validator、状态解析和 contract-only view/handler；未接入玩家事件、领奖或资产。
- 验证记录：admin-api 定向测试 12/12；admin-web 类型测试 3/3；`cargo test activity::types` 6/6；`git diff --check` 通过。`cargo test -p game-server` 不适用于当前非 workspace/library 布局，已使用 game-server bin package 命令验证。

- [x] 定义 login_reward activity_type 和 schema_version。（验证：`packages/activity-contract/index.js` 与 Rust registry 均声明 `login_reward`/schema 1）
- [x] 定义事件来源、周期单位、连续/累计策略、断签策略、领取方式和阶段配置。（验证：共享 JS/Rust validator 校验 game_entry、natural_day、progression、miss_policy、claim_mode 及唯一阶段）
- [x] 在 game-server、admin-api 和 admin-web 的同级 types/ 目录注册 login_reward 文件。（验证：三端 types 文件均存在并有对应测试）
- [x] 类型文件独立封装 schema、handler、状态解析和展示构建，不向公共入口泄漏分支。（验证：三端文件提供 handler/state/view；Rust handler 仍返回 `contract_only`）

## 阶段 2：登录事件与周期进度

- 开始时间：2026-08-21 15:59:03 +08:00
- 结束时间：2026-08-21 16:27:00 +08:00
- 开发总结：接入服务端可信 game_entry 事件，按活动 IANA 时区生成自然日周期并通过 CAS 更新独立活动状态；状态适配到 PlayerActivityState，按角色、活动和版本隔离，保持同日幂等及生命周期校验。
- 验证记录：`cargo test --manifest-path apps/game-server/Cargo.toml activity:: --quiet` 32/32 通过；`git diff --check` 通过；覆盖时区/DST、同日重复、连续/累计、断签 reset/carry、活动结束/下线、版本隔离和 PlayerActivityState 字段同步。

- [x] 接入 game_entry 服务端事件。（验证：`ActivityEngine::on_game_entry` 仅接收服务端角色、活动版本和事件时间，并拒绝客户端身份边界外输入；引擎集成测试通过）
- [x] 按活动时区自然日生成 period_key。（验证：`chrono-tz` IANA 解析及上海、America/New_York DST/跨午夜测试通过）
- [x] 同角色同活动日期只增加一次进度。（验证：仓储以角色/活动/版本键保存，重复周期返回 duplicate 且 revision/计数不变）
- [x] 实现连续登录、累计登录和断签重置策略。（验证：类型测试覆盖 consecutive/cumulative、reset/carry 与跨日断签）
- [x] 保存最近登录周期、连续天数和当前阶段到 player_activity_state。（验证：适配器同步 `progress`、`type_state`、`current_stage_id`、`state_revision`，字段断言测试通过）
- [x] 明确跨日、服务重启、活动结束和版本切换行为。（验证：服务端生成周期键；状态按版本隔离；结束/下线事件拒绝；CAS revision 防并发覆盖）

## 阶段 3：阶段资格与领取

- 开始时间：2026-08-21 16:29:00 +08:00
- 结束时间：2026-08-21 16:45:00 +08:00
- 开发总结：实现登录奖励阶段资格计算、版本化语义领取键、手动/自动领取和统一奖励交付协调；领取状态通过 CAS 写回 PlayerActivityState，交付失败保留可重试状态，活动结束仅在领取窗口内允许已有资格领取。
- 验证记录：`cargo test --manifest-path apps/game-server/Cargo.toml activity:: --quiet` 33/33 通过；`git diff --check` 通过；覆盖语义键幂等、阶段资格、自动多阶段遍历、版本隔离、结束/下线边界和 RewardDelivery coordinator 状态映射。

- [x] 服务端计算阶段资格和可领取状态。（验证：`claim_login_reward` 根据服务端状态计数与阶段门槛计算资格，拒绝未达成阶段）
- [x] 使用 stage_id + period_key + activity_version 生成语义领取键。（验证：`login_reward_claim_key` 生成带三字段的稳定键，coordinator/order/状态记录共用，版本测试通过）
- [x] 支持手动领取，自动领取作为同一 handler 的受控策略。（验证：manual/automatic claim_mode 分支均进入 `claim_login_reward`；自动模式遍历所有已达成阶段）
- [x] 处理未达成、已领取、结束、下线和版本不匹配。（验证：资格、claimed CAS 幂等、claim_deadline 窗口、生命周期和版本校验测试通过）
- [x] 通过 RewardDeliveryService 生成活动来源奖励。（验证：登录奖励构造 `RewardOrder` 并交由 `ActivityClaimCoordinator`/统一 RewardDelivery policy，未直接修改资产）

## 阶段 4：后台配置与展示

- 开始时间：2026-08-21 16:47:00 +08:00
- 结束时间：2026-08-21 17:02:00 +08:00
- 开发总结：补齐三端登录奖励后台 DTO/schema、奖励组预检、阶段排序与专属编辑器；详情 view 从服务端 PlayerActivityState 构建阶段资格、领取状态、连续/累计天数，并按活动版本及时区生成领取键和今日状态。
- 验证记录：admin-api 活动测试 10/10；admin-api/web login_reward 类型测试 6/6；`cargo test --manifest-path apps/game-server/Cargo.toml activity:: --quiet` 34/34；`git diff --check` 通过。

- [x] 实现类型 DTO/schema 和发布预检。（验证：三端 login_reward schema/DTO 与 admin-api `validateDraft` 奖励组引用、排序和 typeConfig 一致性校验）
- [x] 实现阶段排序、条件、奖励组和展示文案配置。（验证：StageDisplay DTO 与排序/editor helper；未知奖励组和不一致配置返回字段错误）
- [x] 实现登录奖励专属阶段编辑组件。（验证：`apps/admin-web/src/modules/activity/LoginRewardStageEditor.vue` 仅编辑门槛与奖励组并展示引用状态）
- [x] 详情返回连续天数、今日状态、阶段奖励和可领取状态。（验证：Rust player view 与 TS DetailView 使用 PlayerActivityState；活动版本、IANA 时区和跨日测试通过）
- [x] 拒绝前端提交阶段进度、登录日期或奖励覆盖。（验证：共享 contract 拒绝 progress/state/period/count/claimed/reward_items 等服务端字段）

## 阶段 5：类型测试

- 开始时间：2026-08-21 17:04:00 +08:00
- 结束时间：2026-08-21 17:12:00 +08:00
- 开发总结：补齐非法周期、空奖励组和非法阶段校验；复核登录奖励专属边界测试及统一 RewardDelivery/settlement 的容量转邮件、流水、并发和重试覆盖。
- 验证记录：`cargo test --manifest-path apps/game-server/Cargo.toml activity:: --quiet` 34/34；admin-api/web login_reward 类型测试 6/6；`git diff --check` 通过；RewardDelivery 既有 capacity-mail、ledger idempotency/retry 和 settlement 并发测试通过于既有测试集。

- [x] 覆盖同日重复登录、跨日、连续、断签和时区边界。（验证：Rust activity 类型测试覆盖同日幂等、跨日、reset/carry、IANA/DST）
- [x] 覆盖阶段重复领取、并发领取、版本切换和下线。（验证：claim CAS 幂等、settlement 并发语义键、版本键隔离、生命周期测试通过）
- [x] 覆盖自动/手动领取、背包满转邮件和奖励流水。（验证：自动多阶段与手动 helper 测试；RewardDelivery 既有 capacity-mail/ledger/retry 测试覆盖统一交付）
- [x] 覆盖非法阶段、空奖励、重复阶段和错误周期配置。（验证：Rust validator 测试覆盖 stage_no=0、空 reward_group_key、重复 stage_no、weekly 周期；奖励组预检拒绝空/未知引用）

## 最终完成定义

- 开始时间：2026-08-21 15:49:17 +08:00
- 结束时间：2026-08-21 17:15:00 +08:00
- 验收总结：login_reward 已完成类型契约、服务端 game_entry 周期进度、阶段资格与统一领奖、后台配置/展示和类型测试；状态按角色/活动/版本隔离，周期与领取键由服务端生成，奖励统一经 RewardDelivery/ActivityClaimCoordinator 交付。生产 ActivityEngine 仍遵守 disabled 边界，未启动外部依赖。

- [x] login_reward 的逻辑、配置、表单和测试都位于同级类型文件。（验证：game-server/admin-api/admin-web types 文件、阶段编辑器和 3 端测试均已提交）
- [x] 玩家可完成登录、查看阶段进度并领取奖励。（验证：`on_game_entry`、详情 view、手动/自动 claim helper 与统一交付测试通过）
- [x] 同角色同活动周期不会重复产生登录奖励。（验证：角色/活动/版本状态键、周期幂等和语义领取键测试通过）
