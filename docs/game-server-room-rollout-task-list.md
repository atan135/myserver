# game-server 空房接管式灰度任务清单

这份文档把 [空房接管式灰度发布技术规范](./game-server-room-rollout-spec.md) 拆成可执行任务清单。

默认约束:

- 当前只支持 `old_server + new_server`
- 第一阶段采用客户端显式重连
- `proxy` 只保存 room 路由元数据
- 旧 room 必须冻结后才能导出
- 未实现完整 transfer payload 的玩法暂不纳入灰度接管范围

当前核对说明（截至 `2026-06-15`）:

- `[x]` 表示仓库内已有明确代码或协议实现支撑。
- `[ ]` 表示当前未见实现。
- 保持 `[ ]` 但附注“部分完成/仅协议预留”表示只完成了协议、数据结构或局部链路，尚未达到任务原意。
- 若本清单与当前代码冲突，以当前代码为准，并同步更新本清单。

当前总体判断:

- `M0`：已完成。规范源、接管判定、第一阶段切服方式、协议名和消息编号已经冻结。
- `M1`：核心能力已完成。`game-proxy` 已有 rollout session、room/player route 元数据、按 room/player 选 upstream 的路由逻辑和基础管理接口。
- `M2` ~ `M3`：最小 room transfer 控制流基础已推进到可调用阶段。`game-server` 已有 freeze/export/import/confirm/retire，`tools/mock-client` 已提供显式编排入口，能按顺序调用 old freeze/export、new import、new confirm ownership、proxy route upsert 和 old retire。
- `M4`：已补齐 `ServerRedirectPush` 的可控下发入口、mock-client 监听验证入口和 mock-client 主动断线重连场景；mock-client 已在真实 old/new/proxy/auth 环境中验证 redirect -> transfer -> proxy reconnect，外部 mybevy 适配仍未完成。
- `M5` ~ `M6`：movement_demo / combat_demo 已具备 transfer schema v1 导出导入与一致性测试；combat ECS 已在 `combat_state_json` 中迁移玩家与怪物基础 ECS 数据，`npc_state_json` 已增加 `room-transfer.npc-state.v1` 结构化运行态契约骨架，并在 combat_demo 中导出 training dummy / Monster 的 demo 级 NPC 状态且导入时与 combat ECS 交叉校验；room runtime timer/scheduler 已有结构化迁移契约骨架，combat_demo 已用 demo 级周期快照 scheduler 跑通导出、导入和继续运行；game-server 已有旧服排空后的受控 graceful shutdown 安全闸；第一阶段 old/new/proxy/auth 空房迁移控制面已人工执行通过，脚本 dry-run/execute 报告、transfer CLI envelope、故障演练模拟入口已具备；尚未完成自动测试准入、真实三进程 route metadata 丢失恢复、完整行为树恢复、真实 AI timer/path/RNG 恢复、真实独立 timer wheel / scheduler、生产部署平台 stop hook 接入和外部 mybevy 验收。`game-proxy` 已具备基于 route store 的自动收尾入口，并可在显式启用时结合旧服真实 drain status 作为结束 rollout 的阻断条件。
- NPC / AI / 行为树 / path / RNG 的完整迁移暂不继续开发。当前只保留结构化契约骨架和 demo 级 roundtrip 事实，后续会在真实 AI 框架设计稳定后重新拆分任务。

## 1. 里程碑划分

建议按以下顺序推进:

1. `M0` 规格冻结与协议编号预留
2. `M1` `game-proxy` 的 room 路由元数据与灰度状态机
3. `M2` `game-server` 的旧服排空、冻结与导出接口
4. `M3` `game-server` 的新服导入与 owner 切换
5. `M4` 客户端显式重连切服链路
6. `M5` NPC / 怪物 / 行为树等完整运行态迁移
7. `M6` 自动化测试、演练脚本与上线验收

## 2. M0 规格冻结与协议准备

- [x] 确认 [game-server-room-rollout-spec.md](./game-server-room-rollout-spec.md) 作为唯一规范源。
- [x] 确认 `rollout_epoch`、`owner_server_id`、`migration_state` 的最终命名。
- [x] 确认 room 接管判定使用“成员为空”而不是“在线人数为 0”。
- [x] 确认第一阶段必须经过客户端显式重连，不做同连接换上游。
- [x] 为以下消息预留协议编号:
  - `ServerRedirectPush`
  - `FreezeRoomForTransferReq/Res`
  - `ExportRoomTransferReq/Res`
  - `ImportRoomTransferReq/Res`
  - `ConfirmRoomOwnershipReq/Res`
  - `RetireTransferredRoomReq/Res`
  - `GetRolloutDrainStatusReq/Res`
- [x] 确认 `RoomTransferPayload` 的最小字段集合。

完成标准:

- 协议名称、字段名、错误码前缀和状态枚举全部固定。
- 后续开发不再一边编码一边改术语。
- 当前状态：`game-server-room-rollout-spec.md` 已明确为唯一规范源；room 接管判定和第一阶段显式重连约束已冻结；协议名称、字段名、状态枚举和消息编号已经固定到 `packages/proto/game.proto`。

## 3. M1 game-proxy 任务

### 3.1 灰度会话状态

- [x] 为 `proxy` 增加 `RolloutSession` 数据结构。
- [x] 支持设置当前 `old_server_id`、`new_server_id`、`rollout_epoch`。
- [x] 支持灰度开始、灰度结束、灰度中断三种生命周期。
- [x] 解决注册中心发现覆盖手工 `Draining` / 运维状态的问题。
- [x] 将“注册中心健康状态”和“运维路由状态”拆开存储后再合并决策。

### 3.2 room 路由元数据

- [x] 为 `proxy` 增加 `RoomRouteRecord` 存储。
- [x] 为 `proxy` 增加 `PlayerRouteRecord` 存储。
- [x] 支持查询:
  - `room_id -> owner_server_id`
  - `player_id -> preferred_server_id`
- [x] 支持路由更新时校验 `rollout_epoch`，避免旧数据覆盖新数据。
- [x] 支持 route checksum / version，避免重复导入或乱序更新。

### 3.3 接入路由决策

- [x] 设计 `proxy` 的最小协议感知范围。
- [x] 明确 `proxy` 至少需要识别的消息:
  - `AuthReq`
  - `RoomJoinReq`
  - `RoomReconnectReq`
  - `RoomJoinAsObserverReq`
- [x] 实现“未绑定 room 前的临时会话态”。
- [x] 实现“收到 room 相关请求后，根据 room route 绑定 upstream”的逻辑。
- [x] 在绑定完成后继续走透明转发，不继续解析玩法消息。

### 3.4 管理接口与观测

- [x] 为 `proxy` 增加灰度状态查询接口。
- [x] 为 `proxy` 增加 room route 列表接口。
- [x] 为 `proxy` 增加玩家路由查询接口。
- [x] 为 `proxy` 增加基于 route store 的灰度结束检测:
  - `owned_room_count == 0`
  - `migrating_room_count == 0`
- [x] 将旧服真实 `connection_count == 0` 纳入自动收尾/停服前校验。
  - 当前实现说明：`game-proxy` 的 `complete-if-drained` 可在 `PROXY_ROLLOUT_DRAIN_STATUS_CHECK_ENABLED=true` 时通过 `auth-http` 内部接口查询旧服真实 drain status，只有 `connectionCount == 0`、`ownedRoomCount == 0`、`migratingRoomCount == 0` 且接口返回成功才结束 rollout；该能力默认关闭，失败时返回 `409 ROLLOUT_NOT_DRAINED` 并保留 rollout session，仍不等于控制面自动停止旧服进程。
- [x] 为 `proxy` 增加关键日志:
  - room route 更新
  - player route 更新
  - redirect 后重连接入
  - 灰度结束

完成标准:

- `proxy` 已能按 room / player 元数据把请求路由到旧服或新服。
- `proxy` 已不再只是“默认挑一个 active upstream”。
- 当前状态：`game-proxy` 的 room route upsert 已增加幂等重放、版本倒退拒绝、同版本冲突拒绝、版本跳号拒绝，以及基于 `expected_room_version` / `expected_last_transfer_checksum` 的 CAS 式保护；`bind_room_owner` 也不会在 rollout 期间静默覆盖权威 owner。

## 4. M2 old_server 任务

当前状态（截至 `2026-05-19`）:

- `game-server` 的 `internal_server`、`admin_server` 和主消息分发里都还没有 rollout 请求处理。
- 当前最多只完成了 proto / `MessageType` 级别的消息预留，尚未形成“排空 -> 冻结 -> 导出 -> 退役”的服务端闭环。

### 4.1 旧服排空与 drain 模式

当前状态（截至 `2026-06-11`）:

- `game-server` 已有 server 级 `drain mode` 状态，`ServerStatusRes.status` 会返回 `ok` 或 `draining`。
- `admin_server` 已支持通过 `UpdateConfigReq(key=drain_mode|drain_mode_enabled|drain_mode_reason|drain_mode_source)` 开启 / 关闭 drain 和设置观测元信息，并可经 `auth-http` 内部接口转发。
- `RoomJoinReq`、`RoomJoinAsObserverReq` 与 `CreateMatchedRoomReq` 已接入 shared drain 新房判定；`RoomReconnectReq` 只回到已存在的离线 room，不触发新建。client TCP / 本地 socket 与 internal socket 的 matched-room 创建都走同一套 `create_matched_room_impl` 策略。admin TCP 当前没有独立建房入口。

- [x] 增加 server 级 `drain mode` 状态存储。
- [x] 明确 `drain mode` 的最小状态字段:
  - `enabled`
  - `entered_at_ms`
  - `reason`
  - `source`
- 当前实现说明：`RuntimeConfig` 保存 `drain_mode_enabled`、`drain_mode_entered_at_ms`、`drain_mode_reason`、`drain_mode_source`。`reason/source` 可通过 admin `UpdateConfigReq` 设置，默认分别为 `rollout` / `admin`；开启 / 关闭 drain、新房创建被拒和 drain completed 日志都会带这些字段。`GetRolloutDrainStatusRes` 同步返回 `drain_mode_reason` 与 `drain_mode_source`，便于控制面和工具直接观测。
- [x] 在 admin 通道增加开启 / 关闭 / 查询 `drain mode` 的入口。
- [x] `AdminServerStatusRes` 或等价状态接口返回 `drain_mode` 状态。
- [x] 在日志中记录 `drain mode` 开启 / 关闭事件。
- [x] 在 `RoomJoinReq` 路径区分:
  - 加入已存在 room
  - 触发默认 room 创建
- [x] 在 `drain mode` 下拒绝创建新的默认 room，并返回固定错误码。
- [x] 在 `drain mode` 下允许加入已存在 room。
- [x] 在 `drain mode` 下允许 `RoomReconnectReq` 进入已有 room。
- [x] 在 `drain mode` 下允许 `RoomJoinAsObserverReq` 进入已有 room。
- [x] 在 `drain mode` 下拒绝新的 `CreateMatchedRoomReq`，避免 MatchService 继续把新房落到旧服。
- [x] 在 internal / admin / 本地 socket 三条建房路径上统一使用同一套 drain 判定，避免只挡住一条入口。
- 当前实现说明：本地 socket 属于主客户端消息分发，`CreateMatchedRoomReq` 与 TCP client 入口共用 `handle_create_matched_room`；internal socket 入口共用 `handle_create_matched_room_internal`，最终都调用 `create_matched_room_impl` 和 `evaluate_drain_new_room_creation`。admin TCP 目前没有独立建房入口，因此没有第三套可绕过的建房实现。
- [x] 明确“新 room”判定规则:
  - 指定 `room_id` 但本地不存在时是否允许创建
  - 空 `room_id` 默认房是否一律禁止新建
  - `match_id` 房间是否一律禁止新建
- 当前实现规则：drain 开启时，只要目标 `room_id` 在本地 `RoomManager` 不存在，即判定为新 room 并拒绝创建；`RoomJoinReq` / observer 的空 `room_id` 会归一到 `room-default`，若本地不存在则拒绝；`CreateMatchedRoomReq` 一律按目标 `room_id` 创建 matched room，若本地不存在则拒绝，避免继续把 match 新房落到旧服。已存在 room 不因 drain 被拒，仍交给正常 room policy / transfer 状态校验。
- [x] 为 `RoomManager` 增加“是否已存在 room / 是否允许新建”的查询接口，避免在业务层复制房间存在性判断。
- [x] 在 `drain mode` 下不影响已有 room 的正常运行:
  - ready / start / input / tick 继续工作
  - 离房 / 断线重连 / observer 进入继续工作
  - offline TTL / 空房清理继续工作
- 当前实现说明：drain 判定只放在可能触发新建 room 的 handler 前置路径，不进入 `RoomManager` 的 ready/start/input/tick/leave/reconnect/cleanup 逻辑；单元测试覆盖 waiting room ready/start、in-game input/tick/reconnect、waiting room observer、cleanup 仍按原 room policy 工作。注意当前 `default_match` policy 本身不允许 in-game 新增 observer，这不是 drain 行为。
- [x] 为 room 增加排空观测指标，至少能区分:
  - `owned_room_count`
  - `connection_count`
  - `migrating_room_count`
  - 有限 `RoomRouteStatus` 样本
- [x] 为 `GetRolloutDrainStatusReq/Res` 落地以下统计:
  - 当前 `owner_server_id`
  - 旧服仍持有 / 阻止停服的 room 数量
  - 迁移中或未完成 room 数量
  - 当前连接数
- [x] 在 `GetRolloutDrainStatusReq/Res` 中补充明确 `drain_mode_enabled` 与 `drain_mode_entered_at_ms` 表达。
- [x] 在 `GetRolloutDrainStatusReq/Res` 中补充可接管空房分类:
  - `transferable_empty_room_count`
  - `transferable_empty_room_samples`
  - 仅统计仍为 `Owned` / 对外视作 `OwnedByOld` 且在线成员数为 `0` 的 room；`Frozen` / `Exported` / `Importing` 属于迁移中，不计入可接管空房。
- [x] 为运营 / 玩法层预留“诱导玩家离房”的触发点。
  - [x] 广播提示
  - [x] 禁止新开局
  - [x] 对局结束后不再自动回默认房
- 当前实现说明：`game-server` 已通过已鉴权 admin/internal 控制通道增加 `TriggerRolloutDrainNoticeReq/Res`，控制面或内部玩法编排可指定 `room_id` 和 `rollout_epoch`，向目标 room 当前在线且非同步中的成员下发既有玩家通道 `GameMessagePush`：`event="rollout_drain_notice"`、`action="leave_room"`，`payload_json` 包含 `room_id`、`rollout_epoch`、`reason`、`message`、`retry_after_ms`、`deadline_ms`。该触发点只负责结构化提示和投递计数审计，不会强制踢人、不会调用 redirect、不会自动 leave room，也不是同连接迁移或 old/new/proxy 真实集成联调。代码核查确认 `RoomEndReq -> handle_room_end -> RoomManager::end_game` 只结束当前 room 并返回当前 room 快照，不会隐式发起 `RoomJoinReq`，也不会创建或切回 `room-default`；drain 下缺失默认房仍由新房 gate 拒绝。已补 `room_service::tests::drain_mode_room_end_does_not_create_or_return_to_default_room` 锁定该契约。
- [x] 为 `room` 进入“成员为空，可接管”状态增加关键日志:
  - 仅在 `leave_room` / `disconnect_room_member` 导致 room 从有在线成员变为 `online_member_count == 0` 时记录，避免重复离线或重复断连刷日志。
- [x] 为“新房创建被 drain 拒绝”增加关键日志。
- [x] 为“旧服排空完成”增加关键日志:
  - `GetRolloutDrainStatusReq/Res` 的 admin/internal 构造路径在 `connection_count == 0 && owned_room_count == 0 && migrating_room_count == 0` 时记录关键字段，保留 `drain_mode_enabled` / `drain_mode_reason` / `drain_mode_source` 作为观测字段。
- [x] 增加单元测试覆盖:
  - drain 开启 / 关闭状态切换
  - drain 下默认 room 创建被拒
  - drain 下已有 room join / reconnect 仍可成功
  - drain 下 matched room 创建被拒
- 当前测试覆盖：`admin_server::tests::apply_runtime_config_updates_drain_mode*` 覆盖开关和 reason/source；`room_service::tests::drain_new_room_policy_*` 覆盖 drain off/on、默认 room / matched room 新建拒绝和已有 room 放行；`room_service::tests::create_matched_room_impl_rejects_internal_create_during_drain` 覆盖 internal matched-room 创建拒绝；`room_service::tests::drain_mode_room_end_does_not_create_or_return_to_default_room` 覆盖 drain 下对局结束路径不会创建或切回 `room-default`；`room_manager::tests::existing_room_runtime_paths_continue_for_drain_mode_contract` 覆盖已有 room 的 ready/start/input/tick/reconnect/observer/cleanup 运行契约；`room_manager::tests::rollout_drain_notice_*` 覆盖 room 在线成员提示投递、离线 / syncing 成员过滤和队列失败计数；`admin_server::tests::admin_rollout_drain_notice_*` 与 `internal_server::tests::*rollout_drain_notice*` 覆盖控制入口审计 target、写操作识别和非法包体错误响应。
- [ ] 增加集成测试覆盖:
  - drain 开启后旧房仍能自然结束并排空
  - drain 开启后 `proxy` 不再把新房流量导入旧服
  - drain 开启后 MatchService 不再把新房创建到旧服
- [x] 为 `tools/mock-client` 增加第一批 drain 验证场景:
  - `drain-new-room-rejected`
  - `drain-existing-room-join`
  - `drain-existing-room-reconnect`
  - `drain-existing-room-observer`
  - `drain-create-matched-room-rejected`

建议代码落点:

- `apps/game-server/src/server.rs`
  - 扩展 `RuntimeConfig` 或独立 rollout runtime，保存 `drain mode` 状态。
- `apps/game-server/src/core/context.rs`
  - 将 `drain mode` 状态注入 `ServiceContext` / `ServerSharedState`。
- `apps/game-server/src/admin_server.rs`
  - 增加 drain 开关与状态查询入口。
- `apps/game-server/src/internal_server.rs`
  - 在 `CreateMatchedRoomReq` 路径接入 drain 判定。
- `apps/game-server/src/core/service/room_service.rs`
  - 在 `RoomJoinReq` / `RoomReconnectReq` / `RoomJoinAsObserverReq` / `CreateMatchedRoomReq` 路径执行 drain 策略。
- `apps/game-server/src/core/runtime/room_manager.rs`
  - 提供 room 存在性、可排空统计、可接管候选统计等能力。

当前状态：第一批最小实现已落地。`game-server` 已增加 server 级 drain 开关，admin 可通过 `UpdateConfigReq(key=drain_mode|drain_mode_enabled)` 开启或关闭；`ServerStatusRes.status` 会返回 `ok` 或 `draining`。`RoomJoinReq`、`RoomJoinAsObserverReq` 与 `CreateMatchedRoomReq` 在 drain 期间都会拒绝创建本地不存在的新 room，但已有 room 的 join / observer / reconnect 仍可继续进入。`tools/mock-client` 也已补上对应 drain 场景，可直接验证“新房被拒 / 旧房 join / reconnect / observer 放行 / matched room 新建被拒”这批最小行为。

### 4.2 room 冻结

- [x] 为 room 增加最小 transfer 状态：`Owned`、`Frozen`、`Exported`、`Importing`、`OwnedByNew`、`Retired`。
- [x] 实现 room 冻结入口。
- [x] 冻结时最小实现已做到:
  - 拒绝新加入
  - 拒绝新输入
  - 停止 tick
  - 拒绝开始/结束游戏等会改变 room 状态的操作
- [ ] 后续玩法系统补齐后，继续确认 NPC/怪物/行为树和独立 timer/scheduler 在冻结点停止推进。
- [x] 冻结后产出只读的导出快照上下文。

当前实现说明：`freeze` 只允许没有在线成员的 room。有人在线房会返回 `ROOM_TRANSFER_HAS_ONLINE_MEMBERS`。这代表当前只支持空房/全员离线的低风险基础 transfer，不支持有人房无感迁移。

### 4.3 旧服导出接口

- [x] 在 internal/admin 通道中增加 `FreezeRoomForTransferReq/Res`。
- [x] 在 internal/admin 通道中增加 `ExportRoomTransferReq/Res`。
- [x] 导出结果包含当前可取得的:
  - room 基础信息
  - frame 与输入窗口
  - `RoomLogicTransfer::export_transfer_state()` 返回的独立 transfer 契约状态
  - runtime timer 契约 JSON 和框架摘要
  - movement transfer 契约 JSON
  - checksum
- [x] 导出失败时返回明确错误码。

限制：当前 checksum 基于清空 `checksum` 字段后的 `RoomTransferPayload` protobuf 编码计算 SHA-256；成员快照按 `player_id` 排序以保持稳定。未实现独立 transfer 契约的玩法会返回 `UNSUPPORTED_ROOM_TRANSFER`，不会继续复用轻量 snapshot 状态假装可迁移。movement_demo 已支持 movement runtime transfer schema v1；combat_demo 已支持 combat runtime transfer schema v1，`pending_events` 不导出也不在导入后重放。combat_demo 还会把 training dummy / Monster 导出到 `room-transfer.npc-state.v1`，包含位置、血量、demo 级行为节点占位、空 threat/blackboard/path 和技能冷却，并在导入时与 `combat_state_json` 恢复出的 ECS 实体交叉校验。room runtime timer/scheduler 已有 `room-transfer.runtime-timer-state.v1` 内层结构化契约和 `room-transfer.runtime-timers.v1` wrapper 校验；combat_demo 当前只填充 demo 级周期快照 scheduler，不代表真实独立 timer wheel。完整行为树、AI timer、路径和 RNG 恢复，以及真实独立 timer wheel / scheduler 的完整数据填充仍是后续任务。

### 4.4 旧服退役接口

- [x] 增加 `RetireTransferredRoomReq/Res`。
- [x] retire 要求 room 已 frozen/exported，且请求 checksum 与最近 export checksum 一致。
- [x] retire 后保留本地 tombstone 状态，清空成员和 pending input，并拒绝后续 join/reconnect/input/start/end。
- [x] 控制面编排保证“新服导入成功、ownership confirm 成功并确认 route 切换后才 retire”。

当前实现说明：`tools/mock-client/src/rollout-transfer-cli.js` 已提供显式控制流入口。编排顺序固定为 old `FreezeRoomForTransfer` 成功、old `ExportRoomTransfer` 得到 payload/checksum、new `ImportRoomTransfer` 成功且 checksum 与 export 一致、new `ConfirmRoomOwnership` 使用 import 返回的 checksum/roomVersion 确认成功、proxy `/room-route/upsert` 切到 `OwnedByNew` 并携带 checksum/version CAS、old `RetireTransferredRoom` 使用同一 checksum。任一步失败都会返回明确 `stage` 和 `errorCode`，不会继续执行后续步骤；例如 import checksum mismatch 或 confirm 失败不会 upsert route，proxy upsert 失败不会 retire old room。CLI 已支持 `--dry-run` 输出 JSON 计划，校验必填参数、old/new 端点分离、proxy URL、端口和 route CAS 默认策略，且明确 `callsControlPlane=false` / `requestsShutdown=false`。

完成标准:

- 旧服已经具备“排空 -> 冻结 -> 导出 -> 退役”的完整闭环。

## 5. M3 new_server 任务

当前状态（截至 `2026-06-11`）:

- `ImportRoomTransferReq/Res` 已接入 `game-server` internal/admin 通道，能校验 checksum、拒绝 room_id 冲突，并创建同 room_id 的最小可运行 room；导入过程中短暂处于 `Importing`，完成后进入 `OwnedByNew`。
- 当前导入恢复 policy、room snapshot、frame、recent/waiting inputs，并通过 `RoomLogicTransfer::import_transfer_state()` 导入玩法迁移契约状态；导入成员统一标记为 offline，不宣称支持有人房无感迁移。
- `ConfirmRoomOwnershipReq/Res` 已进入 `packages/proto`，消息号为 `1613/1614`，并接入 `game-server` 已鉴权 internal/admin 通道；它在新服上校验 room 存在、状态为 `OwnedByNew`、`rollout_epoch`、`checksum` 和 `room_version` 都匹配后才返回成功。

### 5.1 room 导入接口

- [x] 在 internal/admin 通道中增加 `ImportRoomTransferReq/Res`。
- [x] 新服收到导入请求时，使用相同 `room_id` 创建 room。
- [x] 导入时校验:
  - `room_id`
  - `rollout_epoch`
  - `checksum`
- [x] 导入时校验 `room_id` 不冲突、snapshot/policy/owner/phase 等必要字段可用。
- [x] 已接入 transfer 契约 schema/version 骨架和导入侧校验。
- [ ] 后续补齐 NPC / 行为树、独立 timer wheel / scheduler 的完整运行态字段和兼容策略；movement_demo / combat_demo 后续只按 schema 演进继续补兼容策略。

### 5.2 owner 切换确认

- [x] 设计并实现 `ConfirmRoomOwnershipReq/Res` 确认机制。
- [x] 只有在导入成功后，才允许 `proxy` 更新 room route。
- [x] `tools/mock-client` 显式编排在 confirm 成功前不会更新 proxy route 或 retire 旧 room。
- [x] 真实 old/new/proxy/auth 空房迁移控制面已人工验收唯一 owner 闭环。
  - 当前验收说明：`movement_demo` 空房经 old freeze/export、new import/confirm ownership、proxy room route upsert、old retire 和 `complete-if-drained` 跑通；事后 proxy rollout session 清空，old 侧目标 room 进入 `RetiredOnOld`。该结果尚未纳入自动测试准入。

### 5.3 新服接管后的行为

- [x] route 切到新服后，新的 `RoomJoinReq` 进入新服。
- [x] route 切到新服后，新的 `RoomReconnectReq` 进入新服。
- [x] 新服需要能识别“这是已接管 room，不是全新 room”。

当前实现说明：`tools/mock-client` 的 transfer 编排入口已把“导入成功并且新服 ownership confirm 成功后才 upsert proxy route”作为调用顺序约束。proxy route 创建/更新仍依赖 `game-proxy` admin HTTP 的校验；route 已存在时使用 `expected_room_version` 和 `expected_last_transfer_checksum` 做 CAS，route 不存在时按当前 proxy 创建规则使用 `expected_room_version=0`、`room_version=1`。

代码覆盖状态：`game-proxy` 的包体级路由单测已覆盖 `RoomJoinReq` 在 room route owner 切到新服后选中新服，以及 `RoomReconnectReq` 在 player route 仍指向旧服时优先按 transferred room owner 选中新服。`game-server` 的 room transfer 单测已覆盖新服 `import_room_transfer` 后 room 进入 `OwnedByNew`，保留同一 `room_id`、版本和 checksum，后续 `RoomReconnectReq` / `RoomJoinReq` 命中该已接管 room 而不是创建全新 room。

完成标准:

- 新服已能从旧服导入 room，并在 route 切换后继续托管同 `room_id`。
- 真实 old/new/proxy/auth 空房迁移控制面已人工验收；自动测试准入、真实故障恢复和外部客户端验收仍在 M4/M6 边界任务中跟进。

## 6. M4 客户端显式重连任务

当前状态（截至 `2026-06-11`）:

- `ServerRedirectPush` 已扩展目标 proxy 信息，包含 `target_host`、`target_port`、`target_server_id` 和 `transport`。
- `game-server` 已通过已鉴权 admin/internal 通道支持 `TriggerServerRedirectReq/Res`，可向目标 room 当前在线成员下发 `ServerRedirectPush`；push 成功进入出站队列后，旧服会以 `server_redirect_reconnect_required` 主动请求关闭旧连接。
- `tools/mock-client` 已有 `server-redirect-listen` 场景和 parser 单测，用于认证/进房后监听并结构化输出 push。
- `tools/mock-client` 已有 `server-redirect-reconnect` 场景，用于收到 push 后主动关闭旧连接，按 `target_host` / `target_port` 连接目标入口，重新 `AuthReq`，并优先发送 `RoomReconnectReq`；可按显式参数在找不到房间/离线成员时 fallback 到 `RoomJoinReq`。
- mock-client 已完成真实 old/new/proxy/auth 场景下的 redirect -> transfer -> proxy reconnect 验收；外部 `mybevy` 适配、自动测试准入和同连接迁移仍未完成。

### 6.1 协议定义

- [x] 在 `packages/proto` 中新增 `ServerRedirectPush`。
- [x] 定义字段:
  - `reason`
  - `room_id`
  - `rollout_epoch`
  - `reconnect_required`
  - `retry_after_ms`
  - `target_host`
  - `target_port`
  - `target_server_id`
  - `transport`

### 6.2 旧服通知客户端

- [x] 在旧服已鉴权 admin/internal 控制面增加触发 redirect 的入口。
- [x] 只向当前 game-server 上仍在线、且属于目标 room 的连接推送。
- [x] 旧服下发 `ServerRedirectPush` 后主动断开连接。
- [x] 记录 room_id、player_id、rollout_epoch、目标地址和推送结果日志。

### 6.3 mybevy 客户端 / mock-client 处理

- [ ] 外部 `mybevy` 客户端收到 `ServerRedirectPush` 后执行断线重连。
- [x] `mock-client` 收到 `ServerRedirectPush` 后执行断线重连。
- [x] `mock-client` 重连后重新发起 `AuthReq`。
- [x] `mock-client` 重连后优先发起 `RoomReconnectReq`，并支持显式 fallback 到 `RoomJoinReq`。
- [x] `mock-client` 增加 redirect push 监听场景支持。

当前实现说明：`TriggerServerRedirectReq/Res` 当前使用 `1611/1612`，可走 game-server admin TCP 或 internal socket 已鉴权通道。请求只触发 push，不自动 freeze/export/import/retire，不修改 room transfer 状态，也不实现同连接迁移。旧服只会在 `ServerRedirectPush` 成功进入目标连接出站队列后请求关闭该旧连接；排队失败的连接计入失败数，不额外覆盖已有关闭原因。`tools/mock-client` 的 `server-redirect-reconnect` 场景可验证工具侧“收到 push 后主动断线、连接目标入口、重新 `AuthReq`、优先 `RoomReconnectReq`”链路；新的 `server-redirect-transfer-reconnect` 场景已覆盖真实 old/new/proxy/auth 环境下 redirect、room transfer、player route upsert 和 proxy reconnect。两者都不代表 mybevy 已适配。

完成标准:

- 客户端已经具备“收到 redirect 后重新进入正确 server”的稳定链路。

## 7. M5 RoomTransferPayload 与玩法运行态迁移任务

当前状态（截至 `2026-06-11`）:

- 已完成 `RoomTransferPayload` 的协议结构定义和独立 transfer 契约骨架。
- `game-server` 已完成 room freeze/export/import/confirm/retire 的最小闭环，并有 `RoomManager` 单元测试覆盖。
- 已新增独立 transfer 契约骨架，`RoomLogic` 通过 `RoomLogicTransfer` 导出/导入迁移状态。
- 默认未实现迁移契约的玩法会返回 `UNSUPPORTED_ROOM_TRANSFER`，不会再把轻量 `get_serialized_state()` 当完整迁移能力。
- movement_demo 已支持 movement runtime transfer schema v1 导出 / 导入并有一致性测试；combat_demo 已支持 combat runtime transfer schema v1 导出 / 导入并有一致性测试，`pending_events` 不导出也不在导入后重放。
- room runtime timer/scheduler 已新增 `room-transfer.runtime-timer-state.v1` 内层契约，能表达 `runtimeSummary`、`timerEntries`、`schedulerEntries` 和 metadata，并由 `room-transfer.runtime-timers.v1` wrapper 在导入侧校验 schema/version、关键字段类型和基础范围；combat_demo 已用 demo 级周期快照 scheduler 验证 roundtrip 后继续按同一调度帧运行。
- NPC / 行为树、真实独立 timer wheel / scheduler、同连接迁移仍未完成。

### 7.1 通用 payload 结构

- [x] 定义 `RoomTransferPayload` 的 Rust 结构和 proto 结构。
- [x] 覆盖以下通用字段:
  - room 基础信息
  - policy_id
  - room_phase
  - current_frame_id
  - recent_inputs
  - waiting_frame_id
  - waiting_inputs
  - runtime_timers
  - checksum

### 7.2 RoomLogic 迁移能力

- [x] 新增独立 trait，避免直接复用轻量 `get_serialized_state()`:
  - `export_transfer_state()`
  - `import_transfer_state()`
  - checksum 仍由 `RoomTransferPayload` protobuf canonical encoding 统一计算
- [x] 对未实现该 trait 的玩法统一返回 `UNSUPPORTED_ROOM_TRANSFER`。

### 7.3 movement / combat 迁移

- [x] movement 相关 room 导出实体位置信息、朝向、最近输入参考状态。
- [x] combat 相关 room 导出实体列表、血量、buff、冷却、技能状态。
- [x] 导入后恢复相同的 frame 基准。

### 7.4 NPC / 怪物 / 行为树

- [x] 为 NPC / 怪物定义可导出的运行态结构。
- [x] 导出怪物当前位置、血量和技能基础状态；目标、仇恨当前为可校验空列表 / 空字段占位，真实 AI 填充未完成。
- [ ] 导出行为树当前节点。部分完成：`combat_demo` 仅填充 demo 级 `training_dummy.idle` 占位，不代表真实行为树恢复点。
- [ ] 导出行为树黑板或上下文变量。部分完成：契约已有 `blackboard` / `context` map 且会校验基础合法性，`combat_demo` 目前为空。
- [ ] 导出 AI 定时器、等待状态、路径状态、RNG 状态。部分完成：契约已有 `waitTimer` / `path` / `rngState` 字段占位，`combat_demo` 目前未填充真实运行态。
- [ ] 导入后从相同运行点继续，而不是重新初始化。部分完成：`combat_demo` 会校验 `npc_state_json` 与 `combat_state_json` 中恢复出的 Monster entity id、类型、位置、血量一致，并可继续 tick；完整行为树 / AI timer / path / RNG 仍未恢复。

暂停说明：完整 NPC / AI 迁移依赖后续真实行为树、AI timer、寻路和 RNG 设计。当前不继续在 demo 占位结构上扩展功能，避免在目标架构调整前固化错误契约。

### 7.5 定时器与一致性

- [x] 为 room 内部 timer / scheduler 增加可导出结构契约骨架。
- [x] 明确冻结点之后不允许再推进时间。
- [x] 至少一个 demo logic 导入后恢复等价 scheduler 运行态。
- [ ] 真实独立 timer wheel / scheduler 抽象与通用重建。

当前实现说明：`apps/game-server/src/core/runtime/room_manager.rs` 已锁定 transfer freeze 后停止 `RoomRuntime` tick handle / `tick_running`，清空 `wait_started_at`，且冻结/导出状态下 `process_room_tick` 不再推进 room 时间。`runtime_timers_json` 已收敛为 `room-transfer.runtime-timers.v1` wrapper，并校验 `schemaVersion`、内层 `timerStateJson` 契约和 `runtimeSummary` 基础字段。`apps/game-server/src/core/logic/room_logic.rs` 已提供 `RoomRuntimeTimerTransferState`，内层 schema 为 `room-transfer.runtime-timer-state.v1`，可表达 `runtimeSummary`、可选 `timerEntries`、可选 `schedulerEntries` 和 metadata；同文件也提供 `RoomNpcTransferState`，schema 为 `room-transfer.npc-state.v1`，用于表达 NPC / Monster entity id、kind、position、hp/max hp、target、threat、demo/真实行为节点、blackboard/context、rng、path、wait timer 和技能冷却。combat_demo 已导出 demo 级周期快照 scheduler，并在导入后恢复 `next_snapshot_frame` 继续运行同一调度点；也会导出 training dummy / Monster 的 demo 级 NPC 状态，并在导入后与 combat ECS 状态交叉校验。当前仓库仍没有真实独立 room timer wheel / scheduler 抽象，也没有完整行为树引擎迁移，因此这里完成的是结构化契约骨架和至少一个 demo 的等价运行态 roundtrip，不代表行为树 / AI timer / path / RNG 已完整迁移。

完成标准:

- 至少一个带实体与定时器的 room logic 已经可以完整导出、导入并继续运行。

## 8. M6 旧服停服与灰度收尾任务

当前状态（截至 `2026-05-19`，后续补充至 `2026-06-15`）:

- `GetRolloutDrainStatusReq/Res` 已在 `game-server` 已鉴权 admin/internal 通道落地处理，返回本进程真实 `drain_mode_enabled`、`drain_mode_entered_at_ms`、`drain_mode_reason`、`drain_mode_source`、`connection_count`、`owned_room_count`、`migrating_room_count`、`retired_room_count`、`transferable_empty_room_count`、最多 50 条 `RoomRouteStatus` 样本与最多 50 条可接管空房样本。`transferable_empty_room_count` 仅统计仍为 `Owned` / 对外视作 `OwnedByOld` 且在线成员数为 `0` 的 room；在线 `Owned` room 仍计入 `owned_room_count`，但不计入可接管空房；`Frozen` / `Exported` / `Importing` 属于迁移中，不计入可接管空房；`Retired` tombstone room 单独计入 `retired_room_count`，不计入仍持有 / 迁移中 / 可接管空房统计。`drain_mode_entered_at_ms` 未进入 drain mode 时为 `0`；`owner_server_id` 使用当前 `SERVICE_INSTANCE_ID` / 派生实例 id；`rollout_epoch` 仅在本进程 room transfer 状态能归纳出单一 epoch 时返回。当前 `empty_since_ms` 表示本进程内已空置时长，不是 wall-clock 绝对时间戳。
- `game-server` 已为 room 进入可接管空房候选状态和旧服真实排空完成补齐关键日志：前者只在 `leave_room` / `disconnect_room_member` 让在线成员数从大于 `0` 变为 `0` 时记录；后者只在 admin/internal drain status 构造路径观测到 `connection_count == 0 && owned_room_count == 0 && migrating_room_count == 0` 时记录。
- `auth-http` 已把 `GetRolloutDrainStatusReq/Res` 暴露为已鉴权内部控制接口 `GET /api/v1/internal/game-server/rollout-drain-status`，可作为 proxy 自动收尾后、停旧服前的人工或控制面校验入口。
- `tools/mock-client` 已增加 `rollout-drain-status` 场景，通过 `auth-http` 内部控制接口打印旧服真实 drain mode、连接数、仍持有 room 数、迁移中 room 数、已 retired room 数、可接管空房数量、route 样本和可接管空房样本。
- `game-proxy` 已支持手动结束 rollout 并清理 route metadata，也已支持 `POST /rollout/complete-if-drained` 基于 proxy route store 自动收尾：当前 epoch 内仍有 old owner / 迁移中 room route 或指向 old 的 player route 时返回阻塞计数和示例 id；排空后结束 rollout 并返回清理摘要。
- `game-proxy` 自动收尾已支持可选结合旧服真实 drain status：启用 `PROXY_ROLLOUT_DRAIN_STATUS_CHECK_ENABLED=true` 后，proxy route store 排空后还会通过 `auth-http` 内部接口查询旧服真实状态，只有 HTTP 2xx、`ok=true` 且 `ownedRoomCount == 0`、`migratingRoomCount == 0`、`connectionCount == 0` 才结束 rollout；`retiredRoomCount` / `retired_room_count` 作为观测字段透传，不改变 pass/fail 判定；失败、超时、非 2xx、JSON 解析失败或字段不满足会返回 `409` 并保留 rollout session。该能力默认关闭，仍不能替代生产部署平台自身的 stop hook / 实例管理接入。
- `game-server` 已通过已鉴权 admin/internal 控制通道增加 `RequestServerShutdownReq/Res`，并由 `auth-http` 暴露为 `POST /api/v1/internal/game-server/shutdown-if-drained`。入口会在触发前校验 `drain_mode_enabled == true`、`connection_count == 0`、`owned_room_count == 0`、`migrating_room_count == 0`；未通过时返回 `ok=false` 和明确错误码，不触发 shutdown；通过后先写回成功响应，再触发现有 graceful shutdown 信号。`retired_room_count` 只作为观测字段，不阻塞停服。`tools/mock-client` 已增加 `request-server-shutdown` 场景用于人工演练。
- `admin-api` / `admin-web` 已补齐第一阶段控制面周期轮询、展示和告警闭环：`GET /api/admin/monitoring/rollout-drain` 读取 `game-proxy` admin HTTP `GET /rollout`，归一化 active / empty / blocked / drained / interrupted / error 状态、阻塞计数和样本，并由监控总览页每 5 秒轮询展示。该能力只做只读观测和人工收尾提示，不会调用自动停旧服。
- `scripts/rollout-three-process-drill.ps1` 已提供第一阶段 old/new/proxy 演练入口，默认 dry-run，只做工具检查、端口探测、步骤命令输出和 `rollout-transfer-cli --dry-run` JSON 计划校验；显式 `-ExecuteSteps` 才调用已运行服务的控制面接口，`request-server-shutdown` 还需要额外 `-AllowShutdownRequest`。详见 [old/new/proxy 三进程 rollout 演练入口](./rollout-three-process-drill-runbook.md)。2026-06-13 已在真实 old/new/proxy/auth 环境中人工执行空房迁移控制面并通过；该验收仍未沉淀为自动测试准入。

### 8.1 旧服状态查询

- [x] 扩展旧服状态接口，返回:
  - `connection_count`
  - `owned_room_count`
  - `migrating_room_count`
  - `retired_room_count`
  - `transferable_empty_room_count`
  - route 样本中的 `RoomRouteStatus`
  - 可接管空房样本中的 `RoomRouteStatus`
- [x] 通过 `auth-http` 已鉴权内部控制接口暴露旧服真实 drain 状态查询。
- [x] 为 `tools/mock-client` 增加旧服真实 drain 状态查询场景。
- [x] 如停服编排需要，继续补充:
  - `retired_room_count`
  - 更明确的 `drain_mode` 字段
- [x] `proxy` 在 `complete-if-drained` 可选校验旧服真实状态，具备停服前最小阻断闭环。
- [x] 控制面定期轮询、展示和告警这些状态。
  - 当前实现说明：`admin-api` 新增 `/api/admin/monitoring/rollout-drain`，继续要求 `monitoring.read`，通过 `GAME_PROXY_ADMIN_HOST` / `GAME_PROXY_ADMIN_PORT` 和 read/admin token 查询 `game-proxy` `GET /rollout`；`admin-web` 监控总览页进入后立即加载并每 5 秒轮询，展示“已排空可收尾”“仍有旧服房间/玩家/迁移中阻塞”“控制面不可达”等状态。接口失败会返回或前端兜底成可展示错误状态，不会让监控页整体崩溃。

### 8.2 灰度结束判定

- [x] proxy route store 维度满足以下条件时可自动结束灰度:
  - `owned_room_count == 0`
  - `migrating_room_count == 0`
- [x] `complete-if-drained` 启用旧服校验时，满足旧服真实状态条件才允许结束 rollout:
  - `connection_count == 0`
  - `owned_room_count == 0`
  - `migrating_room_count == 0`
- [x] game-server 受控 graceful shutdown 入口满足同一组旧服安全闸才触发:
  - `drain_mode_enabled == true`
  - `connection_count == 0`
  - `owned_room_count == 0`
  - `migrating_room_count == 0`
  - `retired_room_count` 不作为阻塞项
- [x] 灰度结束后清理:
  - `rollout_epoch`
  - `old_server` 的 room route metadata
  - `old_server` 的 player route metadata

### 8.3 停服流程

- [x] 在 game-server 自身提供旧服排空后的受控 graceful shutdown 请求入口。
- [x] 接入本地 / 进程管理器 PID 验证适配层，在 shutdown 安全闸通过后等待旧进程退出。
- [ ] 接入生产部署平台自身的实例 ID、PID 文件或 stop hook。
- [x] 停服前再次校验 route 中已无 `owner_server_id == old_server` 的 room。
- [x] 旧服停服后，`proxy` 自动退回普通单服路由模式。

完成标准:

- 旧服只能在 room 全部接管并且连接排空后退出。

当前实现说明：`game-proxy` 的 `complete-if-drained` 和 `/rollout/end` 在结束 rollout 前通过 route store 显式检查当前 `rollout_epoch` 内是否仍有 `owner_server_id == old_server` 的 room route，并继续阻塞迁移中 / 失败 room route 与指向旧服的 player route；未排空时保留 rollout session，不允许自动收尾。route store 完成 rollout 后会将 `new_server` 的 upstream operation state 置为 `Active`、`old_server` 置为 `Draining`，使没有 `rollout_session` 的默认新房路由稳定落到新服。game-server 现在可在已 drain 且真实排空后通过 `RequestServerShutdownReq/Res` 触发自身 graceful shutdown 信号；`tools/mock-client` 与 `scripts/rollout-three-process-drill.ps1` 已能在安全闸 `ok=true` 后等待指定旧服 PID 退出，并把 `shutdown-safety-gate` / `old-process-stop` 写入报告。生产部署平台仍需要把自身实例 ID、PID 文件或 stop hook 接入该入口；当前监控页只展示状态和告警，不提供自动停进程按钮。

## 9. 测试任务

### 9.1 单元测试

- [x] `proxy` 的 `RolloutSession` 状态机测试。
- [x] `RoomRouteRecord` 更新顺序与 epoch 校验测试。
- [x] transfer 编排顺序与失败停止测试。
- [x] route 切换失败停止测试。
- [x] 旧服 freeze/export 失败路径测试。
- [x] 新服 import/checksum 校验测试。
- [x] 新服 ownership confirm 成功与 mismatch 拒绝测试。

当前实现说明：`apps/game-proxy/src/admin_server.rs` 已用 `rollout_start_rejects_unknown_or_same_upstream`、`rollout_start_and_state_accept_valid_query`、`rollout_state_rejects_invalid_or_missing_session`、`rollout_complete_if_drained_reports_blockers_without_ending`、`rollout_complete_if_drained_ends_when_routes_are_drained`、`rollout_complete_if_drained_rejects_without_active_rollout` 覆盖 rollout start、state change、no active state change、blocked、drained/end 和 no-active complete-if-drained；`apps/game-proxy/src/route_store.rs` 已用 `rollout_drain_evaluation_reports_no_active_rollout`、`rollout_drain_evaluation_blocks_old_room_routes`、`rollout_complete_if_drained_blocks_stop_gate_when_current_epoch_old_owner_room_exists`、`end_rollout_rejects_when_current_epoch_old_owner_room_exists`、`rollout_drain_evaluation_blocks_old_player_routes`、`rollout_complete_if_drained_ends_and_cleans_current_epoch_routes`、`rollout_completion_returns_default_routing_to_new_server`、`rollout_completion_reload_returns_default_routing_to_new_server` 覆盖 route store 维度的排空判断、停服前 old owner room route 阻塞、手动结束的同等阻塞、player route 阻塞、结束清理，以及本地完成 / 共享持久化 reload 后默认路由回到 new server。`RoomRouteRecord` 更新顺序和 epoch 校验由 `room_route_replay_is_idempotent`、`room_route_rejects_stale_version`、`room_route_rejects_same_version_conflict`、`room_route_rejects_version_gap`、`room_route_rejects_checksum_mismatch`、`room_route_rejects_rollout_epoch_mismatch`、`rollout_complete_if_drained_ends_and_cleans_current_epoch_routes` 覆盖，分别对应初始 create/幂等重放、版本倒退拒绝、同版本冲突、版本跳号拒绝、checksum mismatch、rollout_epoch mismatch，以及灰度结束时当前 epoch 清理、旧 epoch route 保留。

当前实现说明：`apps/game-server/src/core/runtime/room_manager.rs` 已用 `freeze_room_for_transfer_rejects_invalid_epoch_or_missing_room` 覆盖 `freeze_room_for_transfer` 的 `INVALID_ROLLOUT_EPOCH`、`ROOM_NOT_FOUND`，用 `freeze_online_room_for_transfer_is_rejected` 覆盖 `ROOM_TRANSFER_HAS_ONLINE_MEMBERS`，用 `freeze_room_for_transfer_rejects_mismatched_epoch_after_freeze` 覆盖已冻结后 epoch mismatch 的 `ROOM_TRANSFER_EPOCH_MISMATCH`。`export_room_transfer_rejects_invalid_epoch_or_missing_room` 覆盖 `export_room_transfer` 的 `INVALID_ROLLOUT_EPOCH`、`ROOM_NOT_FOUND`，`export_room_transfer_rejects_room_that_was_not_frozen` 覆盖 `ROOM_TRANSFER_NOT_FROZEN`，`export_room_transfer_rejects_mismatched_epoch` 覆盖 `ROOM_TRANSFER_EPOCH_MISMATCH`，`export_room_transfer_rejects_logic_without_transfer_contract` 覆盖 `UNSUPPORTED_ROOM_TRANSFER`。

### 9.2 集成测试

- [x] 第一阶段 old/new/proxy 演练脚本入口已具备。
  - 当前实现说明：`scripts/rollout-three-process-drill.ps1` 可按 preflight、rollout start、old drain、空房选择说明、room transfer 编排、drain status、complete-if-drained 和可选 shutdown safety gate 输出或执行步骤；默认 dry-run，不启动服务，不调用写接口，并会调用 `tools/mock-client/src/rollout-transfer-cli.js --dry-run` 输出 room transfer JSON 计划。`rollout-transfer-cli --dry-run` 已有 Node test 覆盖计划结构、缺失参数和 same-server 拒绝。
- [x] `old_server + new_server + proxy` 三进程空房迁移控制面人工联调测试。
  - 当前验收说明：2026-06-13 使用 Redis、NATS、auth-http、old/new game-server 和 game-proxy 跑通 `movement_demo` 空房迁移，覆盖 old freeze/export、new import/confirm ownership、proxy route upsert、old retire 和 `complete-if-drained`；未执行 `-AllowShutdownRequest`。
- [x] redirect 后客户端重连进入新服测试。
  - 当前验收说明：mock-client 场景 `server-redirect-transfer-reconnect` 已在真实服务中验证 `ServerRedirectPush`、transfer、proxy player route upsert、重新连接 proxy、重新 `AuthReq` 并优先 `RoomReconnectReq` 成功；外部 mybevy 未覆盖。
- [x] 空房接管后相同 `room_id` 在新服恢复测试。
- [ ] route 切换失败集成演练。

### 9.3 玩法测试

- [x] movement room 导出导入一致性测试。
- [x] combat room 导出导入一致性测试。
- [x] NPC / 怪物状态一致性测试。
- [ ] 行为树恢复点一致性测试。部分完成：已有 demo 级 `behaviorNode` 占位校验，未覆盖真实行为树恢复。

### 9.4 故障演练

当前状态（截至 `2026-06-15`）:

- `tools/mock-client/src/rollout-fault-drill-cli.js` 已提供脚本级故障演练入口。默认 `dry-run` 只输出 JSON 计划，不访问服务、不调用写接口、不请求旧服停服；`--simulate` 使用纯 mock client 验证编排停止点；只有显式 `--execute` 才调用已运行服务的控制面接口。详见 [rollout 故障演练入口](./rollout-fault-drill-runbook.md)。
- `orchestrateRoomTransfer` 已增加 opt-in failure injection，默认路径保持兼容。当前可模拟/执行 `import-failure` 和 `route-upsert-failure`，结果会输出 `ok=false`、`stage`、`expectedFailure=true`、`completedStages` 等字段，便于归档和后续 CI 消费。
- `redirect-no-reconnect` 入口只触发或计划 `ServerRedirectPush`，并明确不运行 mock-client reconnect；该演练只覆盖 push/操作步骤，不代表 mybevy 已适配。独立的 `server-redirect-transfer-reconnect` 场景已覆盖 mock-client 真实重连验收。
- 这些故障条目已覆盖脚本入口、纯模拟验证和一轮真实 route metadata 缺失安全失败验收。真实 old/new/proxy 三进程自动化故障联调、生产部署平台 stop hook 接入、同连接迁移和 route metadata 自动修复仍未完成。

- [ ] 导出中断演练。
- [x] 导入失败演练。
  - 当前覆盖：`import-failure` 在 old freeze/export 后篡改 payload，预期停在 `new_import`，不会继续 confirm/upsert/retire；已具备 dry-run、纯模拟和可选执行入口。
- [x] route upsert 失败演练。
  - 当前覆盖：`route-upsert-failure` 在 import + confirm 成功后使用错误 `expected_room_version` 触发 proxy CAS 失败，预期停在 `proxy_route_upsert`，不会 retire old room；已具备 dry-run、纯模拟和可选执行入口。
- [x] redirect 后客户端不重连演练。
  - 当前覆盖：`redirect-no-reconnect` 只触发或计划 redirect push，不运行 reconnect，不验证 mybevy。
- [x] route metadata 丢失模拟演练。
  - 当前覆盖：`route-metadata-missing` 在 proxy 查询不到既有 room route metadata 后停在 `proxy_route_upsert`，不调用 `/room-route/upsert` 创建新 route，也不会 retire old room；已覆盖 dry-run、simulate 和 Node 测试。
- [x] route metadata 真实丢失后的恢复演练。
  - 当前覆盖：`route-metadata-missing --execute` 会真实读取 proxy `/room-routes`，当目标 room route metadata 缺失时停在 `proxy_route_upsert`，报告 `ROOM_ROUTE_METADATA_MISSING`，不调用 `/room-route/upsert`，不 retire old room。2026-06-15 已用直连 old 创建的 `movement_demo` 空房 `route-missing-room-20260615101735` 验证真实缺失路径；fault drill 结束后、人工恢复前 old room 停在 `FrozenForTransfer`，proxy 仍无该 route。runbook 已补人工恢复、重新执行和保守中止策略；自动修复不在本项范围。

完成标准:

- 每个关键状态转换都至少有单元测试或集成测试覆盖。

## 10. 日志、监控与审计任务

- [x] 为 `proxy` 增加灰度会话日志字段:
  - `rollout_epoch`
  - `old_server_id`
  - `new_server_id`
  - `room_id`
  - `player_id`
  - route update 日志已覆盖 `room_id` / `player_id` 与 `rollout_epoch`，rollout lifecycle 日志已覆盖 `rollout_epoch` / `old_server_id` / `new_server_id`。
- [x] 为 `game-server` 增加 room freeze/export/import/confirm/retire 日志；成功、拒绝原因、checksum/version mismatch 和幂等 replay 路径都带 `room_id`、`rollout_epoch`、`error_code`、状态与版本上下文。
- [x] 为 transfer payload 增加 checksum 和版本日志。
- [x] 为 redirect 增加审计日志；admin TCP 审计记录 `actor/action/target/ok/error_code`，internal socket 使用结构化控制面审计日志，`RoomManager` 普通日志覆盖 `room_id`、`player_id`、`rollout_epoch`、目标地址和推送结果。
- [x] 为灰度结束增加最终汇总日志，包含 removed / remaining room route 与 player route 计数。

完成标准:

- 任意一个 room 的接管过程，都可以通过日志串出完整链路。

## 11. 推荐开发顺序

建议按下面顺序逐步合并:

1. `proto` 新消息与字段预留
2. `proxy` 的灰度会话和 route metadata
3. `old_server` 的 drain/freeze/export
4. `new_server` 的 import/ownership confirm
5. 显式控制面编排入口：old freeze/export -> new import -> new confirm ownership -> proxy route upsert -> old retire
6. 客户端 redirect + reconnect
7. movement/combat 的 transfer payload
8. NPC / 怪物 / 行为树迁移

  - 当前已完成结构化 `npc_state_json` 契约骨架和 combat_demo training dummy / Monster 示例，但完整行为树、AI timer、路径和 RNG 恢复仍未完成。
  - 当前暂停继续实现，等待真实 AI / 行为树 / path / RNG 方案重新设计后再拆分。
9. 自动化测试和演练脚本

## 12. 当前阶段的最低可交付版本

如果要先做一版最小可运行版本，建议最低交付范围为:

- [x] `proxy` 已能按 room route 把 join / reconnect 请求送到旧服或新服。
- [x] 旧服已支持 redirect + 断线。
  - 当前实现说明：`game-server` 已通过已鉴权 admin/internal 通道支持 `TriggerServerRedirectReq/Res`，下发 `ServerRedirectPush` 成功进入出站队列后会以 `server_redirect_reconnect_required` 请求关闭旧连接；这仍不是同连接迁移，也不代表外部 `mybevy` 已适配。
- [x] 旧服已支持空房 freeze/export。
  - 当前实现说明：`FreezeRoomForTransferReq/Res` 和 `ExportRoomTransferReq/Res` 已接入已鉴权 internal/admin 通道；freeze 只允许无在线成员 room，有人在线会拒绝，因此只覆盖空房 / 全员离线的低风险 transfer。
- [x] 新服已支持 import 并接管同 `room_id`。
  - 当前实现说明：`ImportRoomTransferReq/Res` 会校验 checksum、拒绝 `room_id` 冲突，并创建同 `room_id` 的 `OwnedByNew` room；后续 join / reconnect 会命中已接管 room，不是新建另一个 room。
- [x] `proxy` 已能根据 route store 判定并自动结束灰度。
- [x] `proxy` / 控制面已能结合旧服真实状态提供停服前阻断与人工收尾依据。
  - 当前实现说明：`PROXY_ROLLOUT_DRAIN_STATUS_CHECK_ENABLED=true` 时，`complete-if-drained` 会在 route store 排空后继续校验旧服真实 `connectionCount/ownedRoomCount/migratingRoomCount` 全为 `0` 才结束 rollout；`admin-api` / `admin-web` 已能轮询展示 proxy route store 的 drain 状态和阻塞项。game-server 已提供受控 graceful shutdown 安全闸入口，供后续控制面或部署编排调用。该能力默认关闭或只读展示时都只提供停服前最小阻断 / 告警闭环，不会自动停止旧服进程。
- [x] 至少一个简单 room logic 已跑通完整接管链路。
  - 当前实现说明：`tools/mock-client` 显式编排已覆盖 old freeze/export -> new import -> new confirm ownership -> proxy route upsert -> old retire；movement_demo / combat_demo 已支持 transfer schema v1 导出导入并有一致性测试。`movement_demo` 空房已在真实 old/new/proxy/auth 控制面执行验收中跑通；自动测试准入仍未完成。

## 13. 暂缓项

以下任务建议在首版之后再考虑:

- [ ] 多版本并行灰度
- [ ] 按比例放量
- [ ] 同一客户端连接内无重连切服
- [ ] 在线有人 room 的无感迁移
- [ ] 代理层更深的玩法协议理解
