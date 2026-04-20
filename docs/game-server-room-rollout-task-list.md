# game-server 空房接管式灰度任务清单

这份文档把 [空房接管式灰度发布技术规范](./game-server-room-rollout-spec.md) 拆成可执行任务清单。

默认约束:

- 当前只支持 `old_server + new_server`
- 第一阶段采用客户端显式重连
- `proxy` 只保存 room 路由元数据
- 旧 room 必须冻结后才能导出
- 未实现完整 transfer payload 的玩法暂不纳入灰度接管范围

当前核对说明（截至 `2026-04-20`）:

- `[x]` 表示仓库内已有明确代码或协议实现支撑。
- `[ ]` 表示当前未见实现。
- 保持 `[ ]` 但附注“部分完成/仅协议预留”表示只完成了协议、数据结构或局部链路，尚未达到任务原意。

当前总体判断:

- `M0`：已完成。规范源、接管判定、第一阶段切服方式、协议名和消息编号已经冻结。
- `M1`：核心能力已完成。`game-proxy` 已有 rollout session、room/player route 元数据、按 room/player 选 upstream 的路由逻辑和基础管理接口。
- `M2` ~ `M6`：大多未完成。`game-server` 侧目前主要还是协议/消息类型预留，尚未看到 freeze/export/import/retire/redirect 的处理链路。

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
- [ ] 为 `proxy` 增加灰度结束检测:
  - `owned_room_count == 0`
  - `migrating_room_count == 0`
  - `connection_count == 0`
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

当前状态（截至 `2026-04-20`）:

- `game-server` 的 `internal_server`、`admin_server` 和主消息分发里都还没有 rollout 请求处理。
- 当前最多只完成了 proto / `MessageType` 级别的消息预留，尚未形成“排空 -> 冻结 -> 导出 -> 退役”的服务端闭环。

### 4.1 旧服排空与 drain 模式

当前状态（截至 `2026-04-20`）:

- `game-server` 已有 server 级 `drain mode` 状态，`ServerStatusRes.status` 会返回 `ok` 或 `draining`。
- `admin_server` 已支持通过 `UpdateConfigReq(key=drain_mode|drain_mode_enabled)` 开启 / 关闭 drain，并可经 `auth-http` 内部接口转发。
- `RoomJoinReq`、`RoomJoinAsObserverReq`、`RoomReconnectReq` 与 `CreateMatchedRoomReq` 已接入“仅阻止新 room 创建，不影响已有 room”的最小 drain 判定。

- [x] 增加 server 级 `drain mode` 状态存储。
- [ ] 明确 `drain mode` 的最小状态字段:
  - `enabled`
  - `entered_at_ms`
  - `reason`
  - `source`
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
- [ ] 在 internal / admin / 本地 socket 三条建房路径上统一使用同一套 drain 判定，避免只挡住一条入口。
- [ ] 明确“新 room”判定规则:
  - 指定 `room_id` 但本地不存在时是否允许创建
  - 空 `room_id` 默认房是否一律禁止新建
  - `match_id` 房间是否一律禁止新建
- [x] 为 `RoomManager` 增加“是否已存在 room / 是否允许新建”的查询接口，避免在业务层复制房间存在性判断。
- [ ] 在 `drain mode` 下不影响已有 room 的正常运行:
  - ready / start / input / tick 继续工作
  - 离房 / 断线重连 / observer 进入继续工作
  - offline TTL / 空房清理继续工作
- [ ] 为 room 增加排空观测指标，至少能区分:
  - `owned_room_count`
  - `draining_candidate_room_count`
  - `empty_room_count`
  - `connection_count`
- [ ] 为 `GetRolloutDrainStatusReq/Res` 预留或落地以下统计:
  - 当前 `drain_mode`
  - 当前 `owner_server_id`
  - 旧 room 数量
  - 可接管空房数量
- [ ] 为运营 / 玩法层预留“诱导玩家离房”的触发点:
  - 广播提示
  - 禁止新开局
  - 对局结束后不再自动回默认房
- [ ] 为 `room` 进入“成员为空，可接管”状态增加关键日志。
- [x] 为“新房创建被 drain 拒绝”增加关键日志。
- [ ] 为“旧服排空完成”增加关键日志。
- [ ] 增加单元测试覆盖:
  - drain 开启 / 关闭状态切换
  - drain 下默认 room 创建被拒
  - drain 下已有 room join / reconnect 仍可成功
  - drain 下 matched room 创建被拒
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

- [ ] 为 room 增加 `DrainingOnOld`、`FrozenForTransfer` 等内部状态。
- [ ] 实现 room 冻结入口。
- [ ] 冻结时必须同时做到:
  - 拒绝新加入
  - 拒绝新输入
  - 停止 tick
  - 停止 NPC/怪物/行为树推进
  - 停止新的定时器推进
- [ ] 冻结后产出只读的导出快照上下文。

### 4.3 旧服导出接口

- [ ] 在 internal/admin 通道中增加 `FreezeRoomForTransferReq/Res`。（仅协议预留）
- [ ] 在 internal/admin 通道中增加 `ExportRoomTransferReq/Res`。（仅协议预留）
- [ ] 导出结果包含:
  - room 基础信息
  - frame 与输入窗口
  - logic state
  - runtime timer
  - movement/combat state
  - checksum
- [ ] 导出失败时返回明确错误码。

### 4.4 旧服退役接口

- [ ] 增加 `RetireTransferredRoomReq/Res`。（仅协议预留）
- [ ] 只有在新服导入成功并确认 route 切换后，旧服才能真正 retire room。
- [ ] retire 后从旧服内存中删除 room，或标记为不可再恢复的 retired 状态。

完成标准:

- 旧服已经具备“排空 -> 冻结 -> 导出 -> 退役”的完整闭环。

## 5. M3 new_server 任务

当前状态（截至 `2026-04-20`）:

- `ImportRoomTransferReq/Res` 只在 proto 和消息枚举中出现，未看到 `game-server` 侧导入处理。
- `ConfirmRoomOwnershipReq/Res` 还未进入 `packages/proto`。

### 5.1 room 导入接口

- [ ] 在 internal/admin 通道中增加 `ImportRoomTransferReq/Res`。（仅协议预留）
- [ ] 新服收到导入请求时，使用相同 `room_id` 创建 room。
- [ ] 导入后校验:
  - `room_id`
  - `rollout_epoch`
  - `frame_id`
  - `checksum`

### 5.2 owner 切换确认

- [ ] 设计并实现 `ConfirmRoomOwnershipReq/Res` 或等价确认机制。
- [ ] 只有在导入成功后，才允许 `proxy` 更新 room route。
- [ ] 防止新旧服同时声称自己是 owner。

### 5.3 新服接管后的行为

- [ ] route 切到新服后，新的 `RoomJoinReq` 进入新服。
- [ ] route 切到新服后，新的 `RoomReconnectReq` 进入新服。
- [ ] 新服需要能识别“这是已接管 room，不是全新 room”。

完成标准:

- 新服已能从旧服导入 room，并在 route 切换后继续托管同 `room_id`。

## 6. M4 客户端显式重连任务

当前状态（截至 `2026-04-20`）:

- 只完成了 `ServerRedirectPush` 协议定义。
- `game-server` 未下发 redirect，`simple-client` 也还没有 redirect / reconnect 处理。

### 6.1 协议定义

- [x] 在 `packages/proto` 中新增 `ServerRedirectPush`。
- [x] 定义字段:
  - `reason`
  - `room_id`
  - `rollout_epoch`
  - `reconnect_required`
  - `retry_after_ms`

### 6.2 旧服通知客户端

- [ ] 在旧服业务层增加触发 redirect 的入口。
- [ ] 旧服下发 `ServerRedirectPush` 后主动断开连接。
- [ ] 断开前记录 room_id、player_id、rollout_epoch 审计日志。

### 6.3 客户端 / mock-client 处理

- [ ] 客户端收到 `ServerRedirectPush` 后执行断线重连。
- [ ] 重连后重新发起 `AuthReq`。
- [ ] 重连后重新发起 `RoomJoinReq` 或 `RoomReconnectReq`。
- [ ] `mock-client` 增加 redirect 场景支持。

完成标准:

- 客户端已经具备“收到 redirect 后重新进入正确 server”的稳定链路。

## 7. M5 RoomTransferPayload 与玩法运行态迁移任务

当前状态（截至 `2026-04-20`）:

- 只完成了 `RoomTransferPayload` 的协议结构定义。
- 尚未看到 export/import 运行态的 trait、旧服导出、新服导入和玩法恢复逻辑。

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

- [ ] 新增独立 trait，避免直接复用轻量 `get_serialized_state()`:
  - `export_transfer_state()`
  - `import_transfer_state()`
  - `checksum_transfer_state()`
- [ ] 对未实现该 trait 的玩法统一返回 `UNSUPPORTED_ROOM_TRANSFER`。

### 7.3 movement / combat 迁移

- [ ] movement 相关 room 导出实体位置信息、朝向、最近输入参考状态。
- [ ] combat 相关 room 导出实体列表、血量、buff、冷却、技能状态。
- [ ] 导入后恢复相同的 frame 基准。

### 7.4 NPC / 怪物 / 行为树

- [ ] 为 NPC / 怪物定义可导出的运行态结构。
- [ ] 导出怪物当前位置、血量、目标、仇恨、技能状态。
- [ ] 导出行为树当前节点。
- [ ] 导出行为树黑板或上下文变量。
- [ ] 导出 AI 定时器、等待状态、路径状态、RNG 状态。
- [ ] 导入后从相同运行点继续，而不是重新初始化。

### 7.5 定时器与一致性

- [ ] 为 room 内部 timer / scheduler 增加可导出结构。
- [ ] 明确冻结点之后不允许再推进时间。
- [ ] 导入后重建 timer wheel 或等价运行态。

完成标准:

- 至少一个带实体与定时器的 room logic 已经可以完整导出、导入并继续运行。

## 8. M6 旧服停服与灰度收尾任务

当前状态（截至 `2026-04-20`）:

- `GetRolloutDrainStatusRes` 已在 proto 中定义，但 `game-server` 还没有对应处理。
- `game-proxy` 已支持手动结束 rollout 并清理 route metadata，但尚未实现基于旧服状态的自动收尾。

### 8.1 旧服状态查询

- [ ] 扩展旧服状态接口，返回:
  - `connection_count`
  - `owned_room_count`
  - `migrating_room_count`
  - `retired_room_count`
- [ ] `proxy` 或控制面能定期轮询这些状态。

### 8.2 灰度结束判定

- [ ] 满足以下条件时结束灰度:
  - `owned_room_count == 0`
  - `migrating_room_count == 0`
  - `connection_count == 0`
- [x] 灰度结束后清理:
  - `rollout_epoch`
  - `old_server` 的 room route metadata
  - `old_server` 的 player route metadata

### 8.3 停服流程

- [ ] 在灰度结束后执行旧服停止。
- [ ] 停服前再次校验 route 中已无 `owner_server_id == old_server` 的 room。
- [ ] 旧服停服后，`proxy` 自动退回普通单服路由模式。

完成标准:

- 旧服只能在 room 全部接管并且连接排空后退出。

## 9. 测试任务

### 9.1 单元测试

- [ ] `proxy` 的 `RolloutSession` 状态机测试。
- [ ] `RoomRouteRecord` 更新顺序与 epoch 校验测试。
- [ ] 旧服 freeze/export 失败路径测试。
- [ ] 新服 import/checksum 校验测试。

### 9.2 集成测试

- [ ] `old_server + new_server + proxy` 三进程联调测试。
- [ ] redirect 后客户端重连进入新服测试。
- [ ] 空房接管后相同 `room_id` 在新服恢复测试。
- [ ] route 切换失败回滚测试。

### 9.3 玩法测试

- [ ] movement room 导出导入一致性测试。
- [ ] combat room 导出导入一致性测试。
- [ ] NPC / 怪物状态一致性测试。
- [ ] 行为树恢复点一致性测试。

### 9.4 故障演练

- [ ] 导出中断演练。
- [ ] 导入失败演练。
- [ ] redirect 后客户端不重连演练。
- [ ] route metadata 丢失演练。

完成标准:

- 每个关键状态转换都至少有单元测试或集成测试覆盖。

## 10. 日志、监控与审计任务

- [ ] 为 `proxy` 增加灰度会话日志字段:
  - `rollout_epoch`
  - `old_server_id`
  - `new_server_id`
  - `room_id`
  - `player_id`
- [ ] 为 `game-server` 增加 room freeze/export/import/retire 日志。
- [ ] 为 transfer payload 增加 checksum 和版本日志。
- [ ] 为 redirect 增加审计日志。
- [ ] 为灰度结束增加最终汇总日志。

完成标准:

- 任意一个 room 的接管过程，都可以通过日志串出完整链路。

## 11. 推荐开发顺序

建议按下面顺序逐步合并:

1. `proto` 新消息与字段预留
2. `proxy` 的灰度会话和 route metadata
3. `old_server` 的 drain/freeze/export
4. `new_server` 的 import/ownership confirm
5. 客户端 redirect + reconnect
6. movement/combat 的 transfer payload
7. NPC / 怪物 / 行为树迁移
8. 自动化测试和演练脚本

## 12. 当前阶段的最低可交付版本

如果要先做一版最小可运行版本，建议最低交付范围为:

- [x] `proxy` 已能按 room route 把 join / reconnect 请求送到旧服或新服。
- [ ] 旧服已支持 redirect + 断线。
- [ ] 旧服已支持空房 freeze/export。
- [ ] 新服已支持 import 并接管同 `room_id`。
- [ ] `proxy` 已能根据旧服状态决定灰度结束。
- [ ] 至少一个简单 room logic 已跑通完整接管链路。

## 13. 暂缓项

以下任务建议在首版之后再考虑:

- [ ] 多版本并行灰度
- [ ] 按比例放量
- [ ] 同一客户端连接内无重连切服
- [ ] 在线有人 room 的无感迁移
- [ ] 代理层更深的玩法协议理解
