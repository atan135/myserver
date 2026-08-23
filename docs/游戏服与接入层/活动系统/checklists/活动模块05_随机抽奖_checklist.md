# 活动模块05：随机抽奖 Checklist

## 目标

实现 lottery 的全部独有逻辑，并将类型实现作为同一 `types/` 目录下的独立文件：

    apps/game-server/src/business/activity/types/lottery.rs
    apps/admin-api/src/modules/activity/types/lottery.ts
    apps/admin-web/src/modules/activity/types/lottery.ts

首期支持免费次数或道具券；正式货币、复杂保底、库存奖池和跨服共享奖池后置。

## 基础原则

- [x] 客户端不能提交奖品 ID、随机数、概率、权重或结果。（验证：Rust handler、shared activity-contract、admin-api/admin-web strict validator/serializer 拒绝 result/random/winner 等字段）
- [x] 使用服务端安全随机源和整数权重，禁止浮点累计误差。（验证：`draw_lottery_item` 使用 OS RNG rejection sampling 与 u64 累计权重，边界/3:7 分布测试通过）
- [x] 消耗、抽奖记录和奖励交付必须有明确原子性边界。（验证：voucher 使用 InventoryRequiredExchange AllOrNothing，免费/voucher 保存 RewardOrder，Applied 后才写 granted 状态）
- [x] 同一请求重试必须返回同一结果。（验证：draw_request_id + 角色/活动/版本语义键复用原 selection/order，retryable/unknown/通知失败测试通过）

## 引用文档

- [活动功能总纲](../活动功能总纲.md)：抽奖活动、奖池、权重、版本冻结、消耗和结果审计设计。
- [协议设计](../../../协议与客户端/协议设计.md)：抽奖动作、结果返回和协议兼容性约束。
- [统一资产事务与奖励交付运行边界](../../背包与物品/统一资产事务与奖励交付运行边界.md)：道具券消耗、奖励交付、原子事务和恢复边界。
- [安全设计](../../../安全与监控/安全设计.md)：服务端随机、请求防重放、反刷和敏感操作安全要求。
- [限流与安全现状](../../../安全与监控/限流与安全现状.md)：抽奖接口限流和异常频率检测的现状约束。
- [监控设计](../../../安全与监控/监控设计.md)：抽奖结果、失败分类、延迟和低基数指标要求。

## 阶段 1：类型配置与注册

- 开始时间：2026-08-21 17:30:00 +08:00
- 结束时间：2026-08-21 17:52:00 +08:00
- 开发总结：完成 lottery schema/version 1、奖池与整数权重配置、免费次数/道具券/次数限制字段、保底与限量扩展占位，以及 game-server/admin-api/admin-web 同级类型注册；保持 handler contract-only。
- 验证记录：admin-api/admin-web lottery 定向测试 6/6；`cargo test --manifest-path apps/game-server/Cargo.toml activity::types::lottery` 3/3；`git diff --check` 通过。

- [x] 定义 lottery activity_type 和 schema_version。（验证：共享 `LOTTERY_SCHEMA`、Rust handler 和三端类型均声明 lottery/schema 1，并直接拒绝缺失或错误版本）
- [x] 定义奖池、奖池项、整数权重、免费次数、道具券、每日次数和活动总次数。（验证：三端 config/validator 定义 pool_items、quantity、weight、free_draw_count、voucher_item_id、daily_draw_limit、total_draw_limit）
- [x] 定义后续保底/限量扩展字段，但首期不实现复杂策略。（验证：pity/limited_stock 作为受校验扩展对象，draw handler 仍返回 contract-only）
- [x] 在 game-server、admin-api 和 admin-web 的同级 types/ 目录注册 lottery 文件。（验证：三端 `types/lottery` 文件及注册导出存在）
- [x] 类型文件独立封装 validator、view builder、draw handler 和结果状态。（验证：LotteryHandler、LotteryConfig、LotteryState、LotteryDrawResult 与两端 view/parser 测试通过）

## 阶段 2：奖池校验与随机选择

- 开始时间：2026-08-21 17:54:00 +08:00
- 结束时间：2026-08-21 18:20:00 +08:00
- 开发总结：完成奖池奖励目录预检、整数权重溢出校验、OS 安全随机拒绝采样、累计权重选择、SHA-256 权重摘要和随机算法版本元数据；发布后通过 pool_version 与快照摘要禁止静默修改运行中奖池。
- 验证记录：Rust lottery 定向测试 8/8；admin-api 活动控制测试 9/9；`git diff --check` 通过；覆盖边界样本、3:7 分布、权重溢出、奖励目录缺失、同版本冻结和版本回退。

- [x] 发布前校验奖池非空、权重为正、总权重可计算、奖励存在且数量合法。（验证：Rust validator 与 admin-api validateDraft 校验 pool_items、rewardGroups 目录引用、quantity/weight 和总权重溢出）
- [x] 使用安全随机源在 [0, total_weight) 取样，按累计权重选择结果。（验证：`getrandom` OS 随机字节与 rejection sampling；整数边界和累计选择测试通过）
- [x] 记录奖池版本、权重摘要和随机算法版本。（验证：`LotteryPoolMetadata` 返回 pool_version、total_weight、SHA-256 weight_digest 和 `os_rng_rejection_v1`）
- [x] 完成边界样本和统计分布测试。（验证：0/边界/越界、u64 权重溢出及 1000 次 3:7 分布测试通过）
- [x] 发布后冻结奖池版本，禁止静默修改运行中权重。（验证：admin-api source snapshot preflight 拒绝同 pool_version 摘要变化和版本回退）

## 阶段 3：资格、消耗与次数

- 开始时间：2026-08-21 17:36:52 +08:00
- 结束时间：2026-08-21 17:57:48 +08:00
- 开发总结：接入 ActivityEngine lottery draw 入口，新增角色/活动/版本维度的串行状态存储和服务端 LotteryAssetGateway；免费次数优先，券消耗使用 InventoryRequiredExchange 的 Consume + Grant AllOrNothing 批次，资产 Applied 后才写入中奖状态。真实 inventory 适配和持久化存储保留为阶段 4 的接入边界。
- 验证记录：`cargo test --manifest-path apps/game-server/Cargo.toml activity::engine::tests::lottery_` 3/3；`cargo test --manifest-path apps/game-server/Cargo.toml activity::types::lottery` 12/12；`git diff --check` 通过。

- [x] 实现免费次数和道具券消耗，货币作为后续扩展。（验证：`engine.rs::draw_lottery` 由服务端 gateway 查询券并调用 `evaluate_lottery_draw`，免费优先；engine lottery 测试通过）
- [x] 实现每日次数和活动总次数校验。（验证：`evaluate_lottery_draw` 按活动时区周期键校验 daily/total，`lottery_voucher_path_uses_atomic_exchange_and_limit` 通过）
- [x] 消耗使用统一资产事务，不能先扣后盲目重试。（验证：`build_lottery_voucher_exchange` 构造 `InventoryRequiredExchange` 的 AllOrNothing Consume + Grant，只有 gateway 返回 Applied 才写状态）
- [x] 消耗失败或原子交付失败时不产生抽奖结果。（验证：`draw_lottery` 对 gateway 错误、NotApplied、Unknown 直接返回失败/对账状态且不调用 `LotteryStateStore::set`）
- [x] 明确活动结束、下线、次数重置和时间边界。（验证：`evaluate_lottery_draw` 仅允许 Running、使用 `[start_at,end_at)` 生命周期和活动时区自然日重置；engine ended/offline 测试通过）

## 阶段 4：抽奖动作与幂等

- 开始时间：2026-08-21 17:59:17 +08:00
- 结束时间：2026-08-21 18:13:17 +08:00
- 开发总结：为 lottery draw 增加服务端语义键、LotteryDrawRecord 和可恢复的 RewardOrder/资产请求快照；同请求重试复用原 selection/order，初次 Processing 执行原事务，恢复先查询原请求。Applied 后置 granted 并通知，Unknown/NotApplied/通知失败均保持明确状态且不重新随机。
- 验证记录：`cargo test --manifest-path apps/game-server/Cargo.toml activity::engine::tests::lottery_` 5/5；`cargo test --manifest-path apps/game-server/Cargo.toml activity::types::lottery` 12/12；`git diff --check` 通过。

- [x] 使用 draw_request_id 和服务端语义键防止重复抽奖。（验证：`draw_lottery` 以角色/活动/版本/request key 查询 LotteryDrawRecord；不同活动/版本不复用，重试测试通过）
- [x] 同一事务保存消耗快照、奖池版本、命中奖励、领奖记录和 RewardOrder。（验证：LotteryDrawRecord 保存 selection metadata、exchange、RewardOrder 及 request/fingerprint；免费路径测试断言 activity_claim request_id 和 sha256 fingerprint）
- [x] 超时先查询原 draw_request_id，禁止再次随机。（验证：已有 Processing/ReconciliationPending 记录先调用 `query_draw`，复用原 selection/order；Unknown 恢复测试通过）
- [x] 支持 granted、retryable_failure、reconciliation_pending 和 manual_review。（验证：`LotteryDrawStatus` 与 `resolve_lottery_record` 覆盖四种状态，Applied/NotApplied/Unknown/结构失败分支有定向测试）
- [x] 发奖成功后再推送结果，push 失败不能重新抽奖。（验证：`finish_lottery_granted` 先保存 granted 再调用 notifier；通知失败仅标记 `notification_failed`，unknown/push failure 测试通过）

## 阶段 5：后台配置与展示

- 开始时间：2026-08-21 18:14:05 +08:00
- 结束时间：2026-08-21 18:24:30 +08:00
- 开发总结：补齐 admin-api/admin-web lottery 严格 schema、奖池摘要与权重摘要、配置序列化清理，并新增 LotteryPoolEditor.vue 展示次数/券/奖池编辑、权重合计和奖励目录缺失提示；共享 activity-contract 同步 Rust u32/i32 边界与扩展字段校验。
- 验证记录：admin-api activity-control + lottery 14/14；admin-web lottery 5/5；`npm run build` 通过；`git diff --check` 通过。

- [x] 实现奖池/奖池项 DTO、schema 和发布预检。（验证：admin-api/admin-web lottery 类型严格校验 root/pool/extension 字段，activity-control 发布预检覆盖数量、奖励目录、总权重和 pool_version 冻结）
- [x] 实现抽奖专属奖池编辑组件、权重合计和非法配置提示。（验证：`apps/admin-web/src/modules/activity/LotteryPoolEditor.vue` 提供次数/券/quantity/weight 编辑、总权重、目录缺失标签和错误提示；admin-web lottery 5/5）
- [x] 详情返回免费次数、道具券数量、次数上限和奖池摘要。（验证：admin-api `buildLotteryView` 返回 state、free/voucher/daily/total、pool_summary、weight_digest；测试断言摘要和 digest）
- [x] 前端不得提交或覆盖中奖结果。（验证：`serializeLotteryConfig` 清理 type/state/result/random/winner/result_item_id/reward_exists 等服务端字段；admin-api/admin-web serializer 测试通过）

## 阶段 6：类型测试

- 开始时间：2026-08-21 18:25:52 +08:00
- 结束时间：2026-08-21 18:29:56 +08:00
- 开发总结：补齐 lottery 类型、ActivityEngine、共享 activity-contract、admin-api 和 admin-web 的边界及回归测试；进程中断语义以 Processing/ReconciliationPending 记录和 query_draw 的 Unknown/Applied 恢复路径覆盖。
- 验证记录：admin-api 定向 17/17；admin-web activity contract + lottery 8/8；`cargo test --manifest-path apps/game-server/Cargo.toml activity::engine::tests::lottery_` 6/6；`cargo test --manifest-path apps/game-server/Cargo.toml activity::types::lottery` 12/12；`git diff --check` 通过。

- [x] 覆盖空奖池、零/负权重、极端总权重和非法奖励。（验证：Rust lottery 空池/负权重/u64 溢出/奖励目录测试；admin contract unsafe total weight、i32/u32 边界测试通过）
- [x] 覆盖免费次数、道具券、次数上限、并发抽奖和消耗失败。（验证：engine 6 个 lottery 测试覆盖免费、券、daily/total、不同 request 并发、NotApplied；类型测试覆盖资格边界）
- [x] 覆盖同请求重试、不同请求并发、进程中断和 unknown 恢复。（验证：原 selection/order 重试、Processing/ReconciliationPending 查询原请求、Unknown -> Applied 恢复及通知失败测试通过）
- [x] 覆盖奖池版本冻结、时间边界、下线和发布审计。（验证：Rust pool freeze/time/offline 测试；admin activity-control 发布冻结、审计和快照测试通过）

## 最终完成定义

- 开始时间：2026-08-21 17:36:52 +08:00
- 结束时间：2026-08-21 18:32:05 +08:00
- 验收总结：完成 lottery 类型逻辑、服务端 draw 入口、免费/道具券资格、整数权重随机、幂等与恢复状态、后台 schema/奖池编辑、结果字段清理和跨端测试。运行时通过受控 LotteryAssetGateway/RewardOrder 接入统一资产事务；当前默认 gateway 和状态记录仍是明确的进程内适配边界，真实 inventory/持久化/多实例联调由活动模块 07 验收继续覆盖。

- [x] lottery 的逻辑、配置、表单和测试都位于同级类型文件。（验证：game-server/admin-api/admin-web 均有独立 `types/lottery`，后台编辑器与定向测试已提交）
- [x] 玩家可安全消耗免费次数/道具券并获得服务端随机结果。（验证：ActivityEngine draw 仅接受服务端 gateway，免费/voucher 均构造 RewardOrder，安全随机和 6 个 engine lottery 测试通过）
- [x] 概率、次数、消耗和奖励不会被客户端伪造或并发突破。（验证：共享 contract/前后台严格拒绝结果字段，角色活动版本锁串行化次数，6 个 engine + 17/8 个后台契约测试通过）
