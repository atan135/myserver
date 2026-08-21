# 活动系统

活动系统负责游戏内限时活动的配置、展示、参与、资格判断、奖励领取和运营审计。

活动是奖励的业务来源，不直接修改角色背包或其他资产；奖励交付必须遵循[统一资产事务与奖励交付运行边界](../背包与物品/统一资产事务与奖励交付运行边界.md)。

本模块与任务/成就进度模块隔离：`CharacterProgressTable.csv` 和 `ApplyCharacterProgressReq/Res` 属于系统内置进度奖励，不是运营活动配置、活动状态或领奖记录的替代品。

- [活动功能总纲](./活动功能总纲.md)
- [checklists](./checklists/)

本模块首期设计覆盖登录奖励和随机抽奖活动。文档中的“目标设计”“建议”“拟采用”等表述不代表对应代码已经全部落地；实现状态以代码、协议、数据库初始化脚本和专项验收记录为准。

## 当前实现状态

已落地的是公共领域基础，不是具体玩法闭环：

- `apps/game-server/src/activity/domain.rs` 提供活动、版本、阶段、奖励、玩家状态和领取记录对象，以及 UTC 时间窗、生命周期、领取宽限和配置摘要校验。
- `apps/game-server/src/activity/repository.rs` 提供草稿写入与发布快照读取的仓储契约和内存测试实现；生产 PostgreSQL 仓储、后台发布接口和审计写入仍待后续阶段接入。
- `apps/game-server/src/activity/cache.rs` 提供 Redis 版本/活动列表缓存和刷新通知适配器；Redis 不是活动事实源。
- `apps/game-server/src/activity/types/` 注册 `login_reward` 与 `lottery` 的契约验证 handler。当前 handler 只验证 schema/action 并返回 contract-only 结果，不包含登录累计、抽奖随机、保底或奖励算法。
- `packages/activity-contract/` 是 `admin-api` 与 `admin-web` 共用的类型 schema 注册契约；未知类型、动作或 schema 版本会被拒绝。

目录约定：公共领域代码放在 `activity/` 根下，类型独有代码只放在同级 `activity/types/` 的独立文件中；活动奖励只能经统一资产交付能力处理，不复用任务 `progress_id` 作为活动事实。当前尚未新增玩家协议消息号，外部客户端仍使用既有最小进度协议边界。
