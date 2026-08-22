# 活动系统

活动系统负责游戏内限时活动的配置、展示、参与、资格判断、奖励领取和运营审计。

活动是奖励的业务来源，不直接修改角色背包或其他资产；奖励交付必须遵循[统一资产事务与奖励交付运行边界](../背包与物品/统一资产事务与奖励交付运行边界.md)。

本模块与任务/成就进度模块隔离：`CharacterProgressTable.csv` 和 `ApplyCharacterProgressReq/Res` 属于系统内置进度奖励，不是运营活动配置、活动状态或领奖记录的替代品。

- [活动功能总纲](./活动功能总纲.md)
- [checklists](./checklists/)

本模块首期实现覆盖登录奖励和随机抽奖活动。文档中的“目标设计”“建议”“拟采用”等表述仍不代表已通过真实环境验收；实现状态以代码、协议、数据库迁移和专项验收记录为准。

## 当前实现状态

当前已形成服务端和运营控制面的最小闭环：

- `apps/game-server/src/activity/domain.rs` 提供活动、版本、阶段、奖励、玩家状态和领取记录对象，以及 UTC 时间窗、生命周期、领取宽限和配置摘要校验。
- `apps/game-server/src/activity/repository.rs` 提供 PostgreSQL 发布快照仓储和内存测试实现；玩家状态、领奖、抽奖原始快照及恢复状态均以 PostgreSQL 为事实源。
- `apps/game-server/src/activity/cache.rs` 提供 Redis 版本/活动列表缓存和刷新通知适配器；Redis 不是活动事实源。
- `apps/game-server/src/activity/types/` 注册 `login_reward` 与 `lottery` 的同级类型处理器；登录周期推进、阶段资格、服务端抽奖选择、次数/消耗和奖励结算已接入 `ActivityEngine`。
- `packages/activity-contract/` 是 `admin-api` 与 `admin-web` 共用的类型 schema 注册契约；未知类型、动作或 schema 版本会被拒绝。
- `apps/admin-api/src/activity/` 已提供 PostgreSQL 运营控制面：草稿保存、字段级预检、发布/下线 CAS、已发布版本 fork 新草稿、运行记录查询、Redis 刷新通知、RBAC 和审计；内存 repository 仅用于离线契约测试。
- 玩家 TCP 协议已登记并路由 `1435-1444`，覆盖列表、详情、进度、阶段领取和通用动作；玩家身份、资格、概率、奖励和进度均来自服务端可信上下文。
- `apps/game-server/src/metrics.rs` 按固定动作集合上报请求、成功、资格失败、重复、限流、发奖延迟、恢复 backlog 和缓存刷新失败。字段名由固定枚举生成，不接收角色、账号、活动、请求、领奖、token 或版本值作为维度。

目录约定：公共领域代码放在 `activity/` 根下，类型独有代码只放在同级 `activity/types/` 的独立文件中；活动奖励只能经统一资产交付能力处理，不复用任务 `progress_id` 作为活动事实。

## Windows 联调与验收边界

真实联调前需要 PostgreSQL、Redis、Core NATS、service registry、`auth-http`、`game-server`、`admin-api`、`admin-web` 和 `tools/mock-client`。本地默认入口为 `3000`、`7000`、`3001`、`3002`；测试、预发和线上内部访问必须使用 registry endpoint，不能依赖默认端口直连。数据库迁移使用 `npm run db:validate` / `npm run db:up`，协议检查使用 `npm run check:proto:server`，Rust 离线检查使用 `cargo test --manifest-path apps/game-server/Cargo.toml activity` 和 `cargo check --manifest-path apps/game-server/Cargo.toml`。

涉及创建活动、玩家进度、领奖、抽奖、奖励流水和审计记录的联调会写入临时测试数据。执行前必须明确测试库、测试账号和活动前缀；结束后按 migration 约束和外键顺序清理测试数据，不在共享或生产库执行未审阅的清理脚本。

截至 2026-08-22，代码和离线测试不能替代以下验收：空数据库完整迁移、真实 PostgreSQL/Redis/NATS 联调、Redis 中断演练、至少两个 `game-server` 实例的一致性验证，以及真实外部客户端端到端验收。上述项目完成前不得宣称首期最终验收通过。
