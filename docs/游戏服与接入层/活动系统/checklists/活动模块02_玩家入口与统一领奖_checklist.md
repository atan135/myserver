# 活动模块02：玩家入口与统一领奖 Checklist

## 目标

实现玩家侧活动列表、详情、进度、阶段领取和通用动作入口，统一完成鉴权、状态校验、限流、请求幂等、语义防重、并发控制、奖励交付和提交后通知。

具体登录日期算法和抽奖奖池算法必须留在对应类型独立文件中。

## 基础原则

- [x] 玩家身份来自 character-bound ticket 和连接上下文。（验证：`activity/mod.rs` 所有玩家入口先调用 `ensure_authenticated_identity`，ActivityEngine 仅接收服务端 character_id。）
- [x] 客户端不能提交奖励、进度、概率、玩家 ID 或服务器时间。（验证：活动 proto、mock-client encoders 和 `activity.test.js` 字段边界测试均无这些客户端字段。）
- [x] unknown 结果必须查询原请求，不能直接新建奖励。（验证：ActivityClaimCoordinator 将 unknown 写入 `reconciliation_pending`，`reconcile` 复用原 RewardOrder；统一 reward_delivery query-first 测试通过。）
- [x] RewardDeliveryService 是唯一资产交付入口。（验证：ActivityRewardDelivery 仅作为 RewardDeliveryService 可替换 gateway，ActivityEngine 不直接修改资产或创建邮件。）

## 引用文档

- [活动功能总纲](../活动功能总纲.md)：统一活动入口、幂等、并发、类型处理器和奖励交付设计。
- [协议设计](../../../协议与客户端/协议设计.md)：玩家协议包、消息分层和兼容性规则。
- [外部客户端接入说明](../../../协议与客户端/外部客户端接入说明.md)：客户端鉴权、活动协议接入和联调边界。
- [统一资产事务与奖励交付运行边界](../../背包与物品/统一资产事务与奖励交付运行边界.md)：RewardOrder、资产事务、邮件 fallback 和未知结果恢复。
- [安全设计](../../../安全与监控/安全设计.md)：ticket 鉴权、重放防护、限流和服务端权威要求。
- [限流与安全现状](../../../安全与监控/限流与安全现状.md)：现有限流实现和安全能力接入边界。

## 阶段 1：协议与错误契约

- 开始时间：2026-08-21 12:55:55 +08:00
- 结束时间：2026-08-21 13:06:36 +08:00
- 开发总结：登记活动玩家协议 1435-1444，定义列表/详情/进度/领取/通用动作消息、服务端权威字段边界和稳定错误码；当前 dispatch 明确 deferred，不启用 ActivityEngine。
- 验证记录：server-only proto generation drift、baseline/breaking/fixtures 通过；game-protocol tests 3/3、game-server cargo check --locked 通过；完整生成因外部 mybevy vendored proto 未同步未执行，未修改外部仓库。

- [x] 定义活动列表、详情、进度、领取和通用动作请求/响应。（验证：`packages/proto/game.proto` 新增 `ActivityList/Detail/Progress/Claim/Action` req/res，消息号 1435-1444。）
- [x] 定义 activity_id、version、stage_id、action_type 和 client_request_id 边界。（验证：proto 注释明确 character-bound ticket、server-issued version/stage、opaque retry key 和禁止客户端身份/奖励字段。）
- [x] 定义鉴权失败、未开始、已结束、已下线、资格不足、重复和处理中错误码。（验证：协议文档登记 `ACTIVITY_AUTH_REQUIRED`、`ACTIVITY_NOT_STARTED`、`ACTIVITY_ENDED`、`ACTIVITY_OFFLINE`、`ACTIVITY_QUALIFICATION_NOT_MET`、`ACTIVITY_DUPLICATE`、`ACTIVITY_PROCESSING` 等稳定码；当前 disabled 入口返回 `ACTIVITY_ENGINE_UNAVAILABLE`，不再返回 `MESSAGE_NOT_SUPPORTED`。）
- [x] 更新 packages/proto 生成代码、兼容性基线和协议文档。（验证：game-server/proxy/match 生成代码、game-protocol、mock-client constants、baseline/routing metadata 与协议文档同步；server-only drift check 通过。）

## 阶段 2：ActivityEngine 公共流程

- 开始时间：2026-08-21 13:08:01 +08:00
- 结束时间：2026-08-21 13:33:00 +08:00
- 开发总结：实现 ActivityEngine 列表、详情、进度和统一动作路由；生产装配保持 disabled，测试使用显式 in-memory repository，避免把内存状态当作生产权威数据。入口从连接上下文取得 character-bound identity，统一执行版本/时间窗/下线/资格/限流/请求幂等校验，并把类型注册表结果映射为稳定响应错误码。
- 验证记录：`$env:RUSTFLAGS='-Awarnings'; cargo test --manifest-path apps/game-server/Cargo.toml --bin game-server activity::engine:: --quiet` 4/4 通过；`cargo check --locked --manifest-path apps/game-server/Cargo.toml` 通过；`git diff --check` 通过（仅 Windows CRLF 转换提示）。

- [x] 实现活动列表/详情读取和服务器时间状态判断。（验证：`apps/game-server/src/activity/engine.rs:84-153` 使用请求 `now` 查询发布快照并校验生命周期；`activity/mod.rs:66-167` 返回 server_time/status；engine 定向测试通过。）
- [x] 实现统一动作入口和类型注册表分发。（验证：`apps/game-server/src/activity/engine.rs:155-239` 统一处理 claim/action，调用 `ActivityTypeRegistry::dispatch_action`；`activity/mod.rs:169-246` 接入协议响应。）
- [x] 实现角色归属、权限、版本和时间窗校验。（验证：`activity/mod.rs:50` 仅从 `ensure_authenticated_identity` 获取角色；`engine.rs:173-181` 鉴权/请求校验，`207-212` 版本和生命周期校验；not-started/ended/offline/version 测试通过。）
- [x] 实现角色/活动/动作维度限流和重放辅助。（验证：`engine.rs:95-97` 列表、`119-123` 详情、`189-200` 动作分别使用 read/action 命名空间限流；`182-187` 角色绑定 request_id 幂等；重复请求与跨角色隔离测试通过。）
- [x] 将类型处理器决策转换为公共事务上下文。（验证：`engine.rs:216-232` 以服务端 character_id 构造 `PlayerContext`、以 client_request_id 构造 `TransactionContext`，仅映射 handler outcome/error，不接受客户端奖励或进度；定向测试通过。）

## 阶段 3：幂等、并发与资产交付

- 开始时间：2026-08-21 13:36:00 +08:00
- 结束时间：2026-08-21 13:53:16 +08:00
- 开发总结：增加 ActivityClaimCoordinator 统一承接语义领取、请求幂等、状态推进和 RewardDeliveryService 适配；以稳定 activity_claim request_id、角色/活动/版本/语义键和同锁 player state revision 防止重复发奖。生产装配仍保持 disabled，真实持久化和服务 wiring 留待后续环境接入。
- 验证记录：`$env:RUSTFLAGS='-Awarnings'; cargo test --manifest-path apps/game-server/Cargo.toml --bin game-server activity:: --quiet` 23/23 通过；`$env:RUSTFLAGS='-Awarnings'; cargo test --manifest-path apps/game-server/Cargo.toml --bin game-server core::inventory::reward_delivery:: --quiet` 10/10 通过；`git diff --check` 通过（仅 Windows CRLF 转换提示）。

- [x] 使用 client_request_id 处理重试，使用 semantic_claim_key 防止换请求号重复领取。（验证：`apps/game-server/src/activity/settlement.rs:77-155` 建立角色/请求唯一索引和语义键防重；同请求、换请求号、跨语义冲突和并发测试通过。）
- [x] 使用唯一约束和行锁/版本号保护 player_activity_state。（验证：`settlement.rs:102-151` 在同一 Mutex 锁内检查版本并推进 `state_revision`，`settlement.rs:158-181` 保留原记录恢复；版本冲突与 revision 单调测试通过；数据库唯一约束见 `db/migrations/game/20260821120000_add_activity_schema.sql:155-200`。）
- [x] 写入 processing、granted、retryable_failure、reconciliation_pending 和 manual_review 状态。（验证：`settlement.rs:11-18` 定义状态，`:136-155` 写入 processing，`:184-219` 按交付结果完成状态，`:237-265` 记录 manual_review；unknown/reconcile 和错误恢复测试通过。）
- [x] 生成稳定 RewardOrder/request_id 并调用统一资产事务。（验证：`settlement.rs:301-340` 以角色/活动/版本/语义键 SHA-256 生成稳定 request_id 和服务端 `RewardOrder`；`RewardDeliveryServiceAdapter` 将交付委托给统一资产服务；未接入生产服务时由 disabled engine 隔离。）
- [x] 仅在确定未提交且符合策略时处理背包满转邮件。（验证：活动适配只接受 `ActivityRewardDelivery`，实际 `RewardDeliveryService` 的 query-first、仅 `InventoryCapacityFull` fallback 契约由 `core::inventory::reward_delivery` 10 项测试覆盖；unknown 不创建邮件。）
- [x] 资产提交后再发送 push，push 失败不回滚奖励。（验证：统一 `RewardDeliveryService` 的提交后 notifier 边界与 push failure 语义由 `core::inventory::reward_delivery` 测试覆盖；ActivityClaimRecord 保存 `notification_failed`，不以通知失败回滚 `Granted`。）

## 阶段 4：测试与 mock-client

- 开始时间：2026-08-21 13:55:00 +08:00
- 结束时间：2026-08-21 14:02:48 +08:00
- 开发总结：补齐 fake handler/交付测试、活动协议 mock-client 编解码和离线 CLI 场景；覆盖请求幂等、语义防重、并发、unknown 恢复、跨角色隔离、伪造奖励单、版本/活动越权及 disabled 错误契约。
- 验证记录：`$env:RUSTFLAGS='-Awarnings'; cargo test --manifest-path apps/game-server/Cargo.toml --bin game-server activity:: --quiet` 23/23 通过；`npm --workspace tools/mock-client test` 14/14 通过；`node --check tools/mock-client/src/index.js tools/mock-client/src/messages.js tools/mock-client/src/scenarios/activity.js tools/mock-client/src/activity.test.js` 通过；未启动真实服务或外部客户端。

- [x] 使用 fake handler 覆盖列表、详情、分发和错误映射。（验证：`activity/engine.rs` fixture 使用默认 fake type registry 覆盖 list/detail/dispatch；`activity::engine` 4 项测试及全 activity 23 项通过。）
- [x] 覆盖同请求重试、同语义键换请求号、并发领取、进程中断和恢复。（验证：`activity/settlement.rs` 覆盖同请求/换请求号、Mutex 并发单次交付、unknown/reconcile 和 retryable 状态；23 项活动测试通过。）
- [x] 覆盖跨角色复用 request_id、伪造奖励和越权活动 ID。（验证：engine 跨角色 request_id、未知/越权 activity_id 返回 `ACTIVITY_NOT_FOUND`；settlement 伪造稳定 RewardOrder/request_id 进入 `ManualReview`；相关测试通过。）
- [x] 增加 mock-client 活动列表、详情、领取和幂等重试场景。（验证：`tools/mock-client/src/scenarios/activity.js` 及 CLI `--scenario activity` 发送 list/detail/claim/同 request 重试；`activity.test.js` 与 mock-client 14 项测试通过。）

## 最终完成定义

- 开始时间：2026-08-21 12:55:55 +08:00
- 结束时间：2026-08-21 14:02:48 +08:00
- 验收总结：活动玩家协议、公共 ActivityEngine、语义幂等与统一奖励交付边界、离线 mock-client 场景均已完成并分阶段提交。生产默认保持 disabled，真实 PostgreSQL 仓储、RewardDeliveryService 运行时 wiring 和外部客户端/多服务联调仍需后续环境接入与用户确认。

- [x] 玩家可通过统一协议查询活动并调用动作。（验证：game-server 路由 1435-1444 已接入 ActivityEngine handler，mock-client activity CLI 序列覆盖 list/detail/claim。）
- [x] 公共入口没有登录或抽奖专属判断。（验证：公共 engine 仅按注册 ActivityTypeHandler 分发，登录日期/抽奖算法未进入公共入口。）
- [x] 并发、重放和超时重试不会重复发奖。（验证：语义键/稳定 RewardOrder/request_id、Mutex 并发测试、unknown reconciliation 和统一 reward_delivery query-first/fallback 测试通过。）
