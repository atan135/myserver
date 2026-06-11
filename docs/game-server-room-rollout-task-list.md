# game-server 空房接管式灰度任务清单

这份文档把 [空房接管式灰度发布技术规范](./game-server-room-rollout-spec.md) 拆成可执行任务清单。

默认约束:

- 当前只支持 `old_server + new_server`
- 第一阶段采用客户端显式重连
- `proxy` 只保存 room 路由元数据
- 旧 room 必须冻结后才能导出
- 未实现完整 transfer payload 的玩法暂不纳入灰度接管范围

当前核对说明（截至 `2026-05-19`）:

- `[x]` 表示仓库内已有明确代码或协议实现支撑。
- `[ ]` 表示当前未见实现。
- 保持 `[ ]` 但附注“部分完成/仅协议预留”表示只完成了协议、数据结构或局部链路，尚未达到任务原意。
- 若本清单与当前代码冲突，以当前代码为准，并同步更新本清单。

当前总体判断:

- `M0`：已完成。规范源、接管判定、第一阶段切服方式、协议名和消息编号已经冻结。
- `M1`：核心能力已完成。`game-proxy` 已有 rollout session、room/player route 元数据、按 room/player 选 upstream 的路由逻辑和基础管理接口。
- `M2` ~ `M3`：最小 room transfer 控制流基础已推进到可调用阶段。`game-server` 已有 freeze/export/import/confirm/retire，`tools/mock-client` 已提供显式编排入口，能按顺序调用 old freeze/export、new import、new confirm ownership、proxy route upsert 和 old retire。
- `M4`：已补齐 `ServerRedirectPush` 的可控下发入口、mock-client 监听验证入口和 mock-client 主动断线重连场景；mybevy 适配和三进程端到端自动化联调仍未完成。
- `M5` ~ `M6`：完整玩法 payload、真实旧服状态联动、演练和旧服自动停止仍未完成；`game-proxy` 已具备基于 route store 的自动收尾入口。

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
- [ ] 将旧服真实 `connection_count == 0` 纳入自动收尾/停服前校验。
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

当前状态（截至 `2026-05-19`）:

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

限制：当前 checksum 基于清空 `checksum` 字段后的 `RoomTransferPayload` protobuf 编码计算 SHA-256；成员快照按 `player_id` 排序以保持稳定。未实现独立 transfer 契约的玩法会返回 `UNSUPPORTED_ROOM_TRANSFER`，不会继续复用轻量 snapshot 状态假装可迁移。movement/combat/NPC/timer 的完整数据填充仍是后续任务。

### 4.4 旧服退役接口

- [x] 增加 `RetireTransferredRoomReq/Res`。
- [x] retire 要求 room 已 frozen/exported，且请求 checksum 与最近 export checksum 一致。
- [x] retire 后保留本地 tombstone 状态，清空成员和 pending input，并拒绝后续 join/reconnect/input/start/end。
- [x] 控制面编排保证“新服导入成功、ownership confirm 成功并确认 route 切换后才 retire”。

当前实现说明：`tools/mock-client/src/rollout-transfer-cli.js` 已提供显式控制流入口。编排顺序固定为 old `FreezeRoomForTransfer` 成功、old `ExportRoomTransfer` 得到 payload/checksum、new `ImportRoomTransfer` 成功且 checksum 与 export 一致、new `ConfirmRoomOwnership` 使用 import 返回的 checksum/roomVersion 确认成功、proxy `/room-route/upsert` 切到 `OwnedByNew` 并携带 checksum/version CAS、old `RetireTransferredRoom` 使用同一 checksum。任一步失败都会返回明确 `stage` 和 `errorCode`，不会继续执行后续步骤；例如 import checksum mismatch 或 confirm 失败不会 upsert route，proxy upsert 失败不会 retire old room。

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
- [ ] 后续补齐 movement/combat/NPC/timer 的完整玩法状态字段和兼容策略。

### 5.2 owner 切换确认

- [x] 设计并实现 `ConfirmRoomOwnershipReq/Res` 确认机制。
- [x] 只有在导入成功后，才允许 `proxy` 更新 room route。
- [x] `tools/mock-client` 显式编排在 confirm 成功前不会更新 proxy route 或 retire 旧 room。
- [ ] 真实 old/new/proxy 三进程自动化联调仍需验证唯一 owner 闭环。

### 5.3 新服接管后的行为

- [ ] route 切到新服后，新的 `RoomJoinReq` 进入新服。
- [ ] route 切到新服后，新的 `RoomReconnectReq` 进入新服。
- [ ] 新服需要能识别“这是已接管 room，不是全新 room”。

当前实现说明：`tools/mock-client` 的 transfer 编排入口已把“导入成功并且新服 ownership confirm 成功后才 upsert proxy route”作为调用顺序约束。proxy route 创建/更新仍依赖 `game-proxy` admin HTTP 的校验；route 已存在时使用 `expected_room_version` 和 `expected_last_transfer_checksum` 做 CAS，route 不存在时按当前 proxy 创建规则使用 `expected_room_version=0`、`room_version=1`。

完成标准:

- 新服已能从旧服导入 room，并在 route 切换后继续托管同 `room_id`。

## 6. M4 客户端显式重连任务

当前状态（截至 `2026-06-11`）:

- `ServerRedirectPush` 已扩展目标 proxy 信息，包含 `target_host`、`target_port`、`target_server_id` 和 `transport`。
- `game-server` 已通过已鉴权 admin/internal 通道支持 `TriggerServerRedirectReq/Res`，可向目标 room 当前在线成员下发 `ServerRedirectPush`；push 成功进入出站队列后，旧服会以 `server_redirect_reconnect_required` 主动请求关闭旧连接。
- `tools/mock-client` 已有 `server-redirect-listen` 场景和 parser 单测，用于认证/进房后监听并结构化输出 push。
- `tools/mock-client` 已有 `server-redirect-reconnect` 场景，用于收到 push 后主动关闭旧连接，按 `target_host` / `target_port` 连接目标入口，重新 `AuthReq`，并优先发送 `RoomReconnectReq`；可按显式参数在找不到房间/离线成员时 fallback 到 `RoomJoinReq`。
- 真实 old/new/proxy 多进程联调自动化、外部 `mybevy` 适配和同连接迁移仍未完成。

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

当前实现说明：`TriggerServerRedirectReq/Res` 当前使用 `1611/1612`，可走 game-server admin TCP 或 internal socket 已鉴权通道。请求只触发 push，不自动 freeze/export/import/retire，不修改 room transfer 状态，也不实现同连接迁移。旧服只会在 `ServerRedirectPush` 成功进入目标连接出站队列后请求关闭该旧连接；排队失败的连接计入失败数，不额外覆盖关闭原因。`tools/mock-client` 的 `server-redirect-reconnect` 场景可验证工具侧“收到 push 后主动断线、连接目标入口、重新 `AuthReq`、优先 `RoomReconnectReq`”链路；它仍不是 old/new/proxy 多进程自动化联调，也不代表 mybevy 已适配。

完成标准:

- 客户端已经具备“收到 redirect 后重新进入正确 server”的稳定链路。

## 7. M5 RoomTransferPayload 与玩法运行态迁移任务

当前状态（截至 `2026-06-11`）:

- 已完成 `RoomTransferPayload` 的协议结构定义和独立 transfer 契约骨架。
- `game-server` 已完成 room freeze/export/import/confirm/retire 的最小闭环，并有 `RoomManager` 单元测试覆盖。
- 已新增独立 transfer 契约骨架，`RoomLogic` 通过 `RoomLogicTransfer` 导出/导入迁移状态。
- 默认未实现迁移契约的玩法会返回 `UNSUPPORTED_ROOM_TRANSFER`，不会再把轻量 `get_serialized_state()` 当完整迁移能力。
- 尚未完成 movement/combat/NPC/timer 完整状态填充，也没有 proxy route 仲裁或同连接迁移。

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

当前状态（截至 `2026-05-19`，后续补充至 `2026-06-11`）:

- `GetRolloutDrainStatusReq/Res` 已在 `game-server` 已鉴权 admin/internal 通道落地处理，返回本进程真实 `drain_mode_enabled`、`drain_mode_entered_at_ms`、`connection_count`、`owned_room_count`、`migrating_room_count`、`transferable_empty_room_count`、最多 50 条 `RoomRouteStatus` 样本与最多 50 条可接管空房样本。`transferable_empty_room_count` 仅统计仍为 `Owned` / 对外视作 `OwnedByOld` 且在线成员数为 `0` 的 room；在线 `Owned` room 仍计入 `owned_room_count`，但不计入可接管空房；`Frozen` / `Exported` / `Importing` 属于迁移中，不计入可接管空房。`drain_mode_entered_at_ms` 未进入 drain mode 时为 `0`；`owner_server_id` 使用当前 `SERVICE_INSTANCE_ID` / 派生实例 id；`rollout_epoch` 仅在本进程 room transfer 状态能归纳出单一 epoch 时返回。当前 `empty_since_ms` 表示本进程内已空置时长，不是 wall-clock 绝对时间戳。
- `auth-http` 已把 `GetRolloutDrainStatusReq/Res` 暴露为已鉴权内部控制接口 `GET /api/v1/internal/game-server/rollout-drain-status`，可作为 proxy 自动收尾后、停旧服前的人工或控制面校验入口。
- `tools/mock-client` 已增加 `rollout-drain-status` 场景，通过 `auth-http` 内部控制接口打印旧服真实 drain mode、连接数、仍持有 room 数、迁移中 room 数、可接管空房数量、route 样本和可接管空房样本。
- `game-proxy` 已支持手动结束 rollout 并清理 route metadata，也已支持 `POST /rollout/complete-if-drained` 基于 proxy route store 自动收尾：当前 epoch 内仍有 old owner / 迁移中 room route 或指向 old 的 player route 时返回阻塞计数和示例 id；排空后结束 rollout 并返回清理摘要。
- `game-proxy` 自动收尾已支持可选结合旧服真实 drain status：启用 `PROXY_ROLLOUT_DRAIN_STATUS_CHECK_ENABLED=true` 后，proxy route store 排空后还会通过 `auth-http` 内部接口查询旧服真实状态，只有 HTTP 2xx、`ok=true` 且 `ownedRoomCount == 0`、`migratingRoomCount == 0`、`connectionCount == 0` 才结束 rollout；失败、超时、非 2xx、JSON 解析失败或字段不满足会返回 `409` 并保留 rollout session。该能力默认关闭，仍不能替代完整旧服自动停进程控制面。

### 8.1 旧服状态查询

- [x] 扩展旧服状态接口，返回:
  - `connection_count`
  - `owned_room_count`
  - `migrating_room_count`
  - `transferable_empty_room_count`
  - route 样本中的 `RoomRouteStatus`
  - 可接管空房样本中的 `RoomRouteStatus`
- [x] 通过 `auth-http` 已鉴权内部控制接口暴露旧服真实 drain 状态查询。
- [x] 为 `tools/mock-client` 增加旧服真实 drain 状态查询场景。
- [ ] 如停服编排需要，继续补充:
  - `retired_room_count`
  - 更明确的 `drain_mode` 字段
- [x] `proxy` 在 `complete-if-drained` 可选校验旧服真实状态，具备停服前最小阻断闭环。
- [ ] 控制面定期轮询、展示和告警这些状态。

### 8.2 灰度结束判定

- [x] proxy route store 维度满足以下条件时可自动结束灰度:
  - `owned_room_count == 0`
  - `migrating_room_count == 0`
- [x] `complete-if-drained` 启用旧服校验时，满足旧服真实状态条件才允许结束 rollout:
  - `connection_count == 0`
  - `owned_room_count == 0`
  - `migrating_room_count == 0`
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
- [x] transfer 编排顺序与失败停止测试。
- [x] route 切换失败停止测试。
- [ ] 旧服 freeze/export 失败路径测试。
- [x] 新服 import/checksum 校验测试。
- [x] 新服 ownership confirm 成功与 mismatch 拒绝测试。

### 9.2 集成测试

- [ ] `old_server + new_server + proxy` 三进程联调测试。
- [ ] redirect 后客户端重连进入新服测试。
- [ ] 空房接管后相同 `room_id` 在新服恢复测试。
- [ ] route 切换失败集成演练。

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

- [x] 为 `proxy` 增加灰度会话日志字段:
  - `rollout_epoch`
  - `old_server_id`
  - `new_server_id`
  - `room_id`
  - `player_id`
  - route update 日志已覆盖 `room_id` / `player_id` 与 `rollout_epoch`，rollout lifecycle 日志已覆盖 `rollout_epoch` / `old_server_id` / `new_server_id`。
- [x] 为 `game-server` 增加 room freeze/export/import/confirm/retire 日志；成功、拒绝原因、checksum/version mismatch 和幂等 replay 路径都带 `room_id`、`rollout_epoch`、`error_code`、状态与版本上下文。
- [x] 为 transfer payload 增加 checksum 和版本日志。
- [ ] 为 redirect 增加审计日志。
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
9. 自动化测试和演练脚本

## 12. 当前阶段的最低可交付版本

如果要先做一版最小可运行版本，建议最低交付范围为:

- [x] `proxy` 已能按 room route 把 join / reconnect 请求送到旧服或新服。
- [ ] 旧服已支持 redirect + 断线。
- [ ] 旧服已支持空房 freeze/export。
- [ ] 新服已支持 import 并接管同 `room_id`。
- [x] `proxy` 已能根据 route store 判定并自动结束灰度。
- [ ] `proxy` / 控制面已能结合旧服真实状态决定停旧服。
- [ ] 至少一个简单 room logic 已跑通完整接管链路。

## 13. 暂缓项

以下任务建议在首版之后再考虑:

- [ ] 多版本并行灰度
- [ ] 按比例放量
- [ ] 同一客户端连接内无重连切服
- [ ] 在线有人 room 的无感迁移
- [ ] 代理层更深的玩法协议理解
