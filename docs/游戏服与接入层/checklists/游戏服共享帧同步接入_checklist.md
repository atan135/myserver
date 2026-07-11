# 游戏服共享帧同步接入 Checklist

## 目标

在 `apps/game-server` 内新增 `lockstep_sim_demo` 或等价房间策略，复用现有房间帧同步框架，接入 `packages/sim-core` 做服务端权威推进、事件输出、hash 对账和恢复快照。本清单只覆盖 MyServer / game-server 侧接入，不修改外部 `mybevy` 仓库，不替换现有 `robot_sync_room`、`movement_demo` 或 `combat_demo`。

## 基础原则

- [x] 不替换现有 `robot_sync_room`、`movement_demo`、`combat_demo`，先新增共享 `sim-core` 验证入口。（验证：阶段 1、4、14 记录旧 demo 均保留；真实调试验证统一由 `summary/共享帧同步调试验证_checklist.md` 承接）
- [x] `sim-core` 只做确定性计算，网络、房间、鉴权、输入窗口和广播仍由 `game-server` 负责。（验证：阶段 2、7 记录 `sim-core` 仅作为确定性核心，room runtime 仍负责网络和房间链路）
- [x] `PlayerInputReq` 只表达输入意图，不允许客户端提交命中、伤害、Buff 结果或最终状态。（验证：阶段 3 的 `sim_input` 严格 JSON 和拒绝权威结果字段测试已覆盖）
- [x] 服务端是权威状态源，hash、快照、事件和纠正信息以服务端推进结果为准。（验证：阶段 7、8、9、15 记录服务端权威推进、hash、事件和 snapshot 下发）
- [x] 运行中 room 绑定创建时的 sim schema、配置版本、配置 hash 和随机 seed，不自动吃 CSV 热更。（验证：阶段 6 记录 `BoundSimConfig`、`configHash`、`simSchemaVersion` 和运行中不自动吃 CSV 热更）
- [x] 每个阶段完成后运行对应 Rust 测试、构建检查或手动验收记录；真实多服务联调前先列出依赖并等待用户确认。（验证：阶段 1-15 均有验证记录；后续真实调试验证统一由 `summary/共享帧同步调试验证_checklist.md` 承接）

## 阶段 1：现有房间链路盘点

- 开始时间：2026-07-05 10:25:23 +08:00
- 结束时间：2026-07-05 10:41:55 +08:00
- 开发总结：完成现有房间链路只读盘点。确认 `RoomManager` 生命周期、`RoomRuntimePolicy`、`RoomLogic` trait、`PlayerInputReq` 输入链路、缺帧补齐、`FrameBundlePush` 广播、旧 demo 职责边界以及下行承载位置均已有清晰代码证据；`lockstep_sim_demo` 的最小接入点是独立 policy、factory 分支和 `RoomLogic` 实现，复用现有输入等待、snapshot 和 room broadcast 链路，不替换旧 demo。
- 验证记录：本阶段未启动 Redis/NATS/PostgreSQL、game-server 或联调脚本；worker 只读盘点后由主 agent 复核 `lifecycle.rs`、`tick.rs`、`transfer_codec.rs`、`room_logic.rs`、`room_service.rs`、`gameroom/*`、`room_policy.rs`、proto 和相关文档位置；`git status --short` 在派发前为空。

- [x] 梳理 `RoomManager`、`RoomRuntimePolicy`、`RoomLogic` 的创建、ready、start、tick 和清理入口。（验证：`apps/game-server/src/core/runtime/room_manager/lifecycle.rs:166` 创建/加入 room 并调用 logic 生命周期，`:669` ready，`:693` start，`apps/game-server/src/core/runtime/room_manager/tick.rs:186` 调用 `RoomLogic::on_tick`，`apps/game-server/src/core/runtime/room_manager/storage.rs:169` 清理离线角色时调用 `on_character_leave`，`apps/game-server/src/core/logic/room_logic.rs:661` 定义生命周期 trait）
- [x] 梳理 `PlayerInputReq` 输入校验、等待、缓存、缺帧补齐和 `FrameBundlePush` 广播链路。（验证：`apps/game-server/src/proto/myserver.game.rs:82` 定义 `PlayerInputReq`，`apps/game-server/src/core/service/room_service.rs:487` 解包并转入 `accept_player_input`，`apps/game-server/src/core/runtime/room_manager/lifecycle.rs:726` 校验成员/帧窗口并写 pending input，`apps/game-server/src/core/runtime/room_manager/transfer_codec.rs:313` 解析缺帧策略，`apps/game-server/src/core/runtime/room_manager/tick.rs:256` 构造 `FrameBundlePush` 并广播）
- [x] 梳理 `robot_sync_room`、`movement_demo`、`combat_demo` 当前职责和可复用边界。（验证：`apps/game-server/src/gameroom/robot_sync_room/mod.rs:54` 只接受 `robot_move`，`apps/game-server/src/gameroom/movement_demo/mod.rs:114` 实现 movement demo 的移动/校正 RoomLogic，`apps/game-server/src/gameroom/combat_demo/mod.rs:362` 实现旧 ECS combat demo，`docs/游戏服与接入层/帧同步与房间生命周期设计.md:321` 记录旧 demo 边界）
- [x] 梳理 `GameMessagePush`、`RoomSnapshot` 或等价下行结构能承载 hash、事件和 debug state 的位置。（验证：`apps/game-server/src/core/room/mod.rs:419` 的 `Room::snapshot()` 从 logic 序列化 `game_state`，`apps/game-server/src/proto/myserver.game.rs:160` 的 `FrameBundlePush.snapshot` 可携带 snapshot，`:184` 的 `GameMessagePush.payload_json` 可承载玩法事件，`apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:39`/`:136`/`:257` 下发 `initialSnapshot`、`lastFrame`、`observerFrame`、hash、events 和 debug state）
- [x] 输出本阶段接入边界说明，明确新增入口不影响现有 demo room。（验证：`apps/game-server/src/core/runtime/room_policy.rs:221` 独立定义 `lockstep_sim_demo` policy，`:335` 单独注册，`apps/game-server/src/gameroom/factory.rs:24`-`:27` 对 `robot_sync_room`、`combat_demo`、`lockstep_sim_demo`、`movement_demo` 分支独立创建，`apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:685` 测试旧 demo 未被替换）
- [x] 验证项：通过代码引用或文档片段说明 `lockstep_sim_demo` 的最小接入点和不修改范围。（验证：最小接入点为 `apps/game-server/src/core/runtime/room_policy.rs:221` policy、`apps/game-server/src/gameroom/factory.rs:26` factory 分支、`apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:182` `RoomLogic` 实现和 `apps/game-server/src/core/runtime/room_manager/tick.rs:193` replay snapshot 特例；`docs/协议与客户端/外部客户端接入说明.md:141` 与 `docs/协议与客户端/协议设计.md:860` 说明沿用 `PlayerInputReq` / `RoomSnapshot.game_state`，不新增专用 wire protobuf、不替换旧 demo）

## 阶段 2：game-server 引入 sim-core

- 开始时间：2026-07-05 10:43:07 +08:00
- 结束时间：2026-07-05 10:52:40 +08:00
- 开发总结：完成 `game-server` 引入 `sim-core` 的依赖和边界核对，并补充一个最小编译级单元测试，直接引用 `SimWorld`、`SimInput`、`SimConfig` 和 `step` 推进一帧。当前适配层集中在 `core/system/lockstep_sim`，`lockstep_sim_demo` room logic 通过适配层调用共享核心，未引入 Bevy 或新的核心模拟依赖，也未让 `sim-core` 依赖 Protobuf。
- 验证记录：主 agent 复跑 `cargo test --manifest-path apps/game-server/Cargo.toml game_server_can_reference_sim_core_minimal_step_api`，1 passed；复跑 `cargo check --manifest-path apps/game-server/Cargo.toml` 通过，存在既有 deprecated/unused/dead_code warning；未启动 Redis/NATS/PostgreSQL、game-server 或联调脚本。

- [x] 在 `apps/game-server` 的 Cargo 配置中引入 `packages/sim-core` path dependency。（验证：`apps/game-server/Cargo.toml:23` 声明 `sim-core = { path = "../../packages/sim-core" }`）
- [x] 确认 `game-server` 编译不引入 Bevy、Tokio 以外的新核心模拟依赖。（验证：`packages/sim-core/Cargo.toml` 只声明 `serde` / `serde_json` 运行依赖；`cargo check --manifest-path apps/game-server/Cargo.toml` 通过，未新增 Bevy 依赖）
- [x] 新增或调整模块导出位置，隔离 `sim-core` 适配层和 room logic。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:5` 起集中封装 sim-core 适配、snapshot/envelope 和输入转换；`apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:9` 通过该适配层使用 `create_minimal_world`、`restore_initial_snapshot`、`step_world` 等函数）
- [x] 确认 `sim-core` API 使用不直接依赖 Protobuf 类型。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs` 使用 `serde` JSON envelope 和 `PlayerInputRecord` 作为 game-server 适配边界，未引用 `prost` 或 `proto` 类型；`sim-core` crate 依赖中无 Protobuf）
- [x] 增加最小编译验证，确认 `game-server` 能引用 `SimWorld`、`SimInput`、`SimConfig` 和 `step`。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:828` 的 `game_server_can_reference_sim_core_minimal_step_api` 直接 `use sim_core::{FrameId, SimConfig, SimInput, SimWorld, step}` 并推进到 frame 1）
- [x] 验证项：运行 `cargo check` 或对应 game-server 测试命令，确认依赖接入通过。（验证：`cargo test --manifest-path apps/game-server/Cargo.toml game_server_can_reference_sim_core_minimal_step_api` 1 passed；`cargo check --manifest-path apps/game-server/Cargo.toml` 通过）

## 阶段 3：sim_input JSON 协议适配层

- 开始时间：2026-07-05 10:54:48 +08:00
- 结束时间：2026-07-05 11:23:47 +08:00
- 开发总结：补齐 `sim_input` JSON 协议适配层的单元测试覆盖，确认 payload version、seq、commands、四类输入命令、当前房间身份到 `SimInput.character_id`、服务端控制绑定到 `entity_id`、非法输入错误返回和拒绝推进策略均有代码或测试证据。`PlayerInputReq` 当前不携带 `character_id`，身份由连接/房间链路写入 `PlayerInputRecord.character_id` 后交给适配层。
- 验证记录：主 agent 复跑 `cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim`，20 passed；复跑 `cargo check --manifest-path apps/game-server/Cargo.toml` 通过，存在既有 deprecated/unused/dead_code warning；`git diff --check -- apps/game-server/src/core/system/lockstep_sim/mod.rs` 通过，仅提示 LF/CRLF 替换；未启动 Redis/NATS/PostgreSQL、game-server 或联调脚本。worker 曾运行 `cargo fmt --manifest-path apps/game-server/Cargo.toml --check`，失败点为既有无关格式化差异 `tools/csv_codegen.rs`、`src/core/logic/room_logic.rs`、`src/core/runtime/room_manager/tick.rs`，本阶段未处理。

- [x] 定义 `sim_input` payload version、seq 和 commands 结构。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:17` 定义 `SIM_INPUT_VERSION`，`:485` `parse_sim_input_payload` 解析并校验 version，`:571` `SimInputPayload` 保存 seq/commands，`:578` `RawSimInputPayload` 定义 JSON 入口结构）
- [x] 支持 `move`、`stop`、`face`、`castSkill` 四类输入命令解析。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:586` `RawSimCommand` 定义四类命令，`:828` `sim_input_payload_accepts_supported_commands_and_preserves_seq` 断言 move/stop/face/castSkill 均解析为 `ParsedSimCommand`）
- [x] 将 `PlayerInputReq.character_id` 或当前房间身份映射到 `SimInput.character_id`。（验证：`apps/game-server/src/proto/myserver.game.rs:82` 的 `PlayerInputReq` 不含 `character_id`，`apps/game-server/src/core/runtime/room_manager/lifecycle.rs:726` `accept_player_input` 接收当前房间身份并在 `:755` 写入 `PlayerInputRecord.character_id`，`apps/game-server/src/core/system/lockstep_sim/mod.rs:477` 将其复制到 `SimInput.character_id`，`:873` 单测覆盖）
- [x] 根据服务端控制绑定填充或校验 `entity_id`。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:420` 从 `bindings` 按 `character_id` 取服务端 `EntityId`，`:478` 写入 `SimInput.entity_id`，`:873` 单测断言 `PLAYER_ENTITY_ID_BASE` 绑定生效）
- [x] 校验方向范围、技能 ID、target 类型、JSON 字段类型和 version 不兼容错误。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:490` 反序列化失败返回 `INVALID_SIM_INPUT_JSON`，`:492` 返回 `UNSUPPORTED_SIM_INPUT_VERSION`，`:898` `sim_input_payload_rejects_invalid_protocol_and_field_types` 覆盖非法方向、未知命令、target 类型、字段类型、技能 ID 和 target ID 越界）
- [x] 明确非法输入返回错误、丢弃、拒绝推进或记入事件的策略。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:378` `validate_player_input` 返回静态错误码，`apps/game-server/src/core/runtime/room_manager/lifecycle.rs:743` 在 pending input 写入前调用 `validate_character_input`，`apps/game-server/src/core/system/lockstep_sim/mod.rs:445` step 前解析失败会返回 `LockstepSimStepError::Input`，`:966` 单测证明非法输入不会推进 frame 或改变 hash）
- [x] 增加单元测试覆盖合法输入、未知 version、非法方向、未知命令和 target 类型错误。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:828` 覆盖合法输入，`:898` 覆盖未知 version、非法方向、未知命令和 target 类型错误；`cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim` 20 passed）
- [x] 验证项：输入适配测试能证明客户端不能提交命中、伤害或最终状态。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:577`/`:585` 对 payload 和 command 使用 `deny_unknown_fields`，`:947` `sim_input_payload_rejects_client_authoritative_state_fields` 覆盖 `entityId`、`hit`、`damage`、`buffs`、`finalState`、`stateHash` 均拒绝）

## 阶段 4：lockstep_sim_demo room policy 骨架

- 开始时间：2026-07-05 11:27:42 +08:00
- 结束时间：2026-07-05 11:43:35 +08:00
- 开发总结：完成 `lockstep_sim_demo` room policy 骨架核验，并补充 policy 注册/默认参数单元测试。确认该 demo 使用独立 policy 和 factory 分支，`RoomLogic` 生命周期接入创建、加入、开局和 tick，输入等待与缺帧补齐仍由 `RoomManager` runtime 层负责，`sim-core` 只消费已解析的 tick inputs。
- 验证记录：主 agent 复跑 `cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim_demo`，9 passed；复跑 `cargo test --manifest-path apps/game-server/Cargo.toml room_policy`，8 passed；`git diff --check -- apps/game-server/src/core/runtime/room_policy.rs` 通过，仅提示 LF/CRLF 替换；未启动 Redis/NATS/PostgreSQL、game-server 或联调脚本。

- [x] 注册 `lockstep_sim_demo` 或等价 policy ID。（验证：`apps/game-server/src/core/runtime/room_policy.rs:221` 定义 `RoomRuntimePolicy::lockstep_sim_demo()`，`:335` 在 `RoomPolicyRegistry::default()` 注册，`:475` 新增单测解析该 policy）
- [x] 新增 `RoomLogic` 实现，接入房间创建、ready、start 和 on_tick。（验证：`apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:182` 实现 `RoomLogic`，`:183` `on_room_created`，`:188` `on_character_join`，`:197` `on_game_started`，`:219` `on_tick`；ready 仍由 `RoomManager::set_ready` 管理成员状态并复用该 logic）
- [x] 复用现有输入等待和缺帧策略，不在 `sim-core` 内重新实现等待策略。（验证：`apps/game-server/src/core/runtime/room_manager/tick.rs:165` 按 `policy.wait_strategy` 决定推进，`:177` 调用 runtime `resolve_tick_inputs`，`apps/game-server/src/core/runtime/room_manager/transfer_codec.rs:313`/`:331` 按 `missing_input_strategy` 补齐缺帧；`lockstep_sim_demo/mod.rs:219` 只消费传入的 `tick_inputs`）
- [x] 为本 demo 设定默认 tick rate、最大人数、缺帧策略和 snapshot 策略。（验证：`apps/game-server/src/core/runtime/room_policy.rs:223` policy id，`:224` max 32，`:229` active 20 fps，`:237` snapshot 10 frame，`:238` input delay 2，`:240` Optimistic，`:241` Empty；`:475` 新增单测逐项断言）
- [x] 确保该 policy 不改变 `robot_sync_room`、`movement_demo`、`combat_demo` 默认行为。（验证：`apps/game-server/src/gameroom/factory.rs:24`-`:27` 四个 demo 独立分支；`apps/game-server/src/core/runtime/room_policy.rs:504`/`:514`/`:519` 新增单测断言旧 demo 关键策略值；`apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:685` 测试旧 demo factory 行为仍可用）
- [x] 增加 policy 注册和创建房间测试。（验证：`apps/game-server/src/core/runtime/room_policy.rs:475` 新增 policy 注册/参数测试；`apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:685` 通过 factory 创建 `lockstep_sim_demo` logic 并推进一帧）
- [x] 验证项：通过测试或本地构造确认能创建 `lockstep_sim_demo` room logic 实例。（验证：`cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim_demo` 9 passed；`cargo test --manifest-path apps/game-server/Cargo.toml room_policy` 8 passed）

## 阶段 5：初始 SimWorld 与控制绑定

- 开始时间：2026-07-05 11:46:47 +08:00
- 结束时间：2026-07-05 11:55:31 +08:00
- 开发总结：完成初始 `SimWorld` 与控制绑定核验并补充单元测试。确认 `create_minimal_world` 为玩家按输入顺序构建稳定实体和 `character_id -> entity_id` 绑定，追加训练目标实体，初始 world/snapshot 带 schema、frame、rng seed 与 hash 信息，并由输入适配层使用服务端绑定。
- 验证记录：主 agent 复跑 `cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim`，24 passed；`git diff --check -- apps/game-server/src/core/system/lockstep_sim/mod.rs` 通过，仅提示 LF/CRLF 替换；未启动 Redis/NATS/PostgreSQL、game-server 或联调脚本。

- [x] 为进入房间的玩家构建玩家实体，包含 `character_id`、`entity_id`、team、transform、movement 和 combat 状态。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:113` 按输入玩家构建实体，`:700` `player_entity` 填充 kind/owner/team/transform/movement/combat；`:1025` 新增单人玩家初始状态测试逐项断言）
- [x] 添加训练假人或敌方实体，支持移动和基础战斗 scenario 验证。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:119` 追加训练目标，`:732` `training_target_entity` 定义 Monster/team 90/位置/战斗状态；`:1059` 新增训练目标状态测试，`:1162` 移动后 castSkill 测试验证战斗目标扣血）
- [x] 建立 `character_id -> entity_id` 控制绑定，并在输入适配层使用。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:110`/`:115` 建立 bindings，`:420` 输入适配层按 `input.character_id` 查绑定，`:873` `sim_inputs_use_room_identity_and_server_control_binding` 断言输入映射到服务端实体）
- [x] 设置 `schema_version`、`start_frame`、`rng seed` 和初始 `frame`。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:121` 创建初始 frame 0 world，`:189`/`:191`/`:194` initial snapshot 写入 schema_version/start_frame/rng_seed；`:1025` 断言 world schema/frame/rng seed，`:1079` 断言 snapshot start_frame/rng_seed/hash）
- [x] 保证实体 ID 分配稳定，不能依赖 HashMap 遍历顺序。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:113` 使用输入 slice enumerate 分配 `PLAYER_ENTITY_ID_BASE + index`，`:270` snapshot control bindings 排序输出，`:1079` 新增相同玩家输入生成相同 world/bindings/hash 并断言实体 ID 顺序）
- [x] 增加初始世界构造测试，覆盖单人、双人和训练目标。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:1025` 覆盖单人玩家，`:1003` 覆盖双人绑定和训练目标存在，`:1059` 覆盖训练目标详细状态）
- [x] 验证项：相同房间输入得到相同初始 `SimWorld` 和初始 hash。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:1079` 两次以相同玩家输入创建 world，断言 `world_a == world_b`、`world_hash` 一致、snapshot 一致；`cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim` 24 passed）

## 阶段 6：SimConfig 与配置版本绑定

- 开始时间：2026-07-05 11:57:44 +08:00
- 结束时间：2026-07-05 12:24:28 +08:00
- 开发总结：完成 `SimConfig` 与 room 生命周期配置元信息绑定。`lockstep_sim` 适配层新增 `BoundSimConfig`，为初始快照、帧 envelope 和 demo debug state 统一下发 `configVersion`、`configHash`、`simSchemaVersion`；`lockstep_sim_demo` 在创建时绑定当前配置表版本元信息和固定 demo SimConfig，运行中不跟随 CSV runtime 自动变化。当前技能/Buff 尚未完整映射 CSV 到 sim-core，已用固定 demo config source 和迁移边界显式标记。
- 验证记录：主 agent 复跑 `cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim`，28 passed；复跑 `cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim_demo`，10 passed；复跑 `cargo test --manifest-path apps/game-server/Cargo.toml room_policy`，8 passed；复跑 `cargo check --manifest-path apps/game-server/Cargo.toml` 通过，存在既有 deprecated/unused/dead_code warning；`git diff --check -- apps/game-server/src/core/system/lockstep_sim/mod.rs apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs apps/game-server/src/gameroom/factory.rs` 通过，仅提示 LF/CRLF 替换；未启动 Redis/NATS/PostgreSQL、game-server 或联调脚本。

- [x] 构造 `SimConfig`，包含 tick rate、movement bounds、技能配置和 Buff 配置。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:154` `default_sim_config` 设置 tick rate、movement bounds 和默认移动速度，`:167` 构造默认技能，`:178` 构造默认 Buff；`:1101` `default_sim_config_contains_movement_skill_and_buff_definitions` 逐项断言）
- [x] 为 room 绑定 `config_version`、`config_hash` 和 `sim_schema_version`。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:51` 定义 `BoundSimConfig` 三项元信息，`:195` `room_sim_config` 绑定版本、hash 和 `SIM_CORE_SCHEMA_VERSION`；`apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:81` room logic 持有 `BoundSimConfig`，`:184` 创建时绑定）
- [x] 明确运行中 room 不自动应用 CSV 热更，新 room 使用新配置。（验证：`apps/game-server/src/gameroom/factory.rs:26` 创建 `lockstep_sim_demo` 时读取 `config_tables.current_snapshot().version`，`apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:81` 将配置保存在 room logic，`:239` on_tick 使用已绑定 `self.config` 推进；`:611` `lockstep_sim_demo_binds_config_metadata_for_room_lifetime` 断言 fps 变化后 config hash/version 保持房间生命周期绑定。当前新 room 使用的是创建时配置版本元信息，技能/Buff 内容仍处于固定 demo config 边界）
- [x] 如果当前 CSV 尚未完整承载技能和 Buff，先提供 demo 内固定配置并记录迁移边界。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:25` 定义 `LOCKSTEP_SIM_DEMO_CONFIG_SOURCE = "lockstep_sim_demo.fixed_v1"`，`:26` 定义 `LOCKSTEP_SIM_DEMO_CONFIG_MIGRATION_BOUNDARY`，`apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:156`-`:160` debug state 下发 config source 和迁移边界）
- [x] 将配置 hash 纳入下发快照或 debug 信息，供客户端对账。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:73`/`:126` 在 initial snapshot 和 frame envelope 中包含 `config_hash`，`:252`/`:288` 写入 hash；`apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:157` debug state 包含 `configHash`，`:141`/`:256` 下发快照和帧 envelope 使用同一个 bound config）
- [x] 增加配置构造和 hash 稳定性测试。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:1101` 覆盖配置构造，`:1138` 覆盖 hash 稳定与变化识别，`:1161` 覆盖 snapshot/frame envelope 元数据绑定；`apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:611` 覆盖 room 生命周期绑定）
- [x] 验证项：同一配置多次构造 hash 一致，配置变化能被识别。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:1138` 中 `room_sim_config(7, 20)` 两次构造 hash 相等，并断言 tick rate 和 skill cooldown 变化会改变 hash；`cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim` 28 passed）

## 阶段 7：服务端权威 tick 推进

- 开始时间：2026-07-05 12:28:23 +08:00
- 结束时间：2026-07-05 12:48:09 +08:00
- 开发总结：完成服务端权威 tick 推进补强。`lockstep_sim_demo` 的 `RoomLogic::on_tick` 使用当前帧输入、房间绑定配置和服务端控制绑定调用 `step_world_with_config`，适配层统一转换为 `SimInput` 后调用 `sim_core::step`。本阶段将推进逻辑改为先在 clone world 上执行，成功后提交，避免 step 错误导致 room 世界半更新，并补齐停止输入、输入来源、错误保留上一帧和重复回放 hash 一致测试。
- 验证记录：主 agent 复跑 `cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim`，34 passed；复跑 `cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim_demo`，13 passed；`git diff --check -- apps/game-server/src/core/system/lockstep_sim/mod.rs apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs` 通过，仅提示 LF/CRLF 替换；未启动 Redis/NATS/PostgreSQL、game-server 或联调脚本。

- [x] 在 `RoomLogic::on_tick` 中把当前帧权威输入集合转换为 `SimInput`。（验证：`apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:239` `on_tick` 接收当前帧 `inputs`，`:252` 传入 `step_world_with_config`；`apps/game-server/src/core/system/lockstep_sim/mod.rs:487` 调用 `sim_inputs_from_records`，`:505` 起逐条按服务端绑定转换为 `SimInput`）
- [x] 调用 `sim_core::step(world, frame, inputs, config)` 推进服务端权威世界。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:480` `step_world_with_config`，`:490` 调用 `sim_core::step` 并传入 `FrameId`、转换后的 sim inputs 和 bound config；`apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:252` room tick 使用该适配层推进）
- [x] 处理 `step` 错误，保证错误不会导致 room panic 或 frame 状态半更新。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:488` 先 clone world，`:496` 成功后才提交；`:1115` `sim_step_error_does_not_commit_partial_world_updates` 断言错误后 frame/hash/world 不变；`apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:265` 错误分支记录 `last_error` 而不 panic，`:826` 单测断言上一成功帧 world/hash/lastFrame 保持）
- [x] 保存每帧 `SimStepResult.frame`、events 和 state hash。（验证：`apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:254` 保存 `result.state_hash`，`:255` 保存 event count，`:256` 保存由 `create_frame_envelope_with_config` 构造的 `lastFrame`；`apps/game-server/src/core/system/lockstep_sim/mod.rs:287`-`:291` frame envelope 写入 frame、state_hash、events 和 debug summary）
- [x] 区分真实输入、合成空输入和重复上一帧输入的来源，用于审计和 debug。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:441` `frame_input_source_summary` 输出每条输入来源，`:552` `sim_input_source` 区分 real、synthesized empty 和 repeat last；`apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:779` 单测断言 `lastFrame.inputSources` 和 debug summary 三类计数）
- [x] 增加 tick 推进测试，覆盖移动、停止、技能释放和非法输入。（验证：`apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:560` 覆盖移动和技能释放，`:606` 覆盖 stop 输入和 frame/hash 保存，`:826` 覆盖 step 错误保留上一帧；`apps/game-server/src/core/system/lockstep_sim/mod.rs:1064` 覆盖非法输入拒绝推进，`:1585` 覆盖 stop 输入，`cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim_demo` 13 passed）
- [x] 验证项：服务端同一输入序列多次推进得到一致 hash。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:1633` `same_input_sequence_replays_to_identical_hashes` 使用 move、repeatLast、stop、castSkill、empty 序列双 world 回放并断言每帧 hash 与最终 world hash 一致；`cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim` 34 passed）

## 阶段 8：事件和 hash 下发

- 开始时间：2026-07-05 12:51:12 +08:00
- 结束时间：2026-07-05 13:12:30 +08:00
- 开发总结：完成事件和 hash 下发结构补齐。`SimFrameEnvelope` 保留原始 `events` 兼容字段，同时新增 `eventCount`、稳定 `eventSummaries` 和轻量 `debugState`；`lockstep_sim_demo` serialized state 顶层与 `observerFrame` 也暴露 `lastStateHash`、`lastEventCount` 和 `lastEventSummaries`，供客户端或工具从 `RoomSnapshot.game_state` / frame envelope 读取。真实客户端或 online 工具联调按用户要求跳过。
- 验证记录：主 agent 复跑 `cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim`，36 passed；复跑 `cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim_demo`，13 passed；`git diff --check -- apps/game-server/src/core/system/lockstep_sim/mod.rs apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs` 通过，仅提示 LF/CRLF 替换；未启动 Redis/NATS/PostgreSQL、game-server、客户端或 online 工具联调。

- [x] 将 `SimEvent` 转成 `GameMessagePush`、room broadcast 或等价下行结构。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:190` `SimFrameEnvelope` 作为 frame/snapshot 下行 JSON envelope，`:206` 新增 `event_summaries`；`apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:183` serialized state 暴露 `lastFrame`，该 state 经 `RoomSnapshot.game_state` 下发）
- [x] 下发 frame、state hash、事件数量和关键事件字段。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:194`/`:201`/`:203`/`:206` 定义 frame、state_hash、event_count、event_summaries；`:365`-`:373` 创建 envelope 时写入这些字段；`apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:187`-`:189` 顶层下发 `lastStateHash`、`lastEventCount` 和 `lastEventSummaries`）
- [x] 对 Damage、Heal、Buff、Death、SkillCast 等事件保留稳定字段名和版本。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:147` `SimFrameEventSummary` 定义 `schemaVersion/kind/frame/sourceEntityId/targetEntityId/skillId/buffId/amount/sequence`，`:536`-`:667` 覆盖 SkillCast、Damage、Heal、BuffApplied、BuffExpired、Death、BuffTick 映射；`:1778` 单测覆盖排序和字段 JSON 名）
- [x] 增加轻量 debug state，下发实体 id、位置 raw、hp 和 alive 状态。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:162` `SimFrameDebugState`，`:179` `SimFrameEntityDebugState` 只含 `entityId/xRaw/yRaw/hp/maxHp/alive`，`:680`-`:700` 从 world 生成；`:1697` 和 `apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:619` 测试断言下发字段）
- [x] 控制下行频率和 payload 大小，避免每帧发送过大的完整世界 JSON。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:32` 将 frame debug state 限制为 32 个实体，`:686` 使用 `.take(SIM_FRAME_DEBUG_STATE_ENTITY_LIMIT)`；`:1902` `frame_envelope_debug_state_is_lightweight_and_bounded` 断言 41 个实体时只下发 32 个摘要且 JSON 中无 `transform/movement/combat/buffs/snapshot` 完整状态）
- [x] 增加事件序列化测试，覆盖事件排序和字段完整性。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:1778` `event_summaries_preserve_stable_fields_and_sort_order` 覆盖 SkillCast、BuffApplied、BuffTick、Damage、Heal、BuffExpired、Death 的排序和 JSON 字段；`:1697` 覆盖 frame envelope 中 hash/events/debug summary/debug state）
- [x] 验证项：客户端或 online 工具能从下行消息拿到服务端 hash 和事件摘要。（验证：离线验证 `apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:652`-`:701` 证明 serialized state / observerFrame / lastFrame 可读到 `lastStateHash`、`eventSummaries` 和事件摘要；`cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim_demo` 13 passed。真实客户端或 online 工具联调按用户要求先跳过，留到 Stage 11/15）

## 阶段 9：初始快照与恢复快照下发

- 开始时间：2026-07-05 13:15:41 +08:00
- 结束时间：2026-07-05 13:44:17 +08:00
- 开发总结：`lockstep_sim_demo` 的 `RoomSnapshot.game_state.initialSnapshot` 和每帧 `FrameBundlePush.snapshot` 恢复契约已补齐离线验证。新增测试覆盖 `SimInitialSnapshot` JSON 字段、serde round-trip、`restore_initial_snapshot`、从 serialized state 恢复，以及从 `FrameBundlePush.snapshot` 恢复后继续应用下一帧输入并与服务端连续推进 hash 对齐。当前 policy 仍不允许新玩家游戏中加入，本阶段仅验证已有成员 rejoin、断线重连和观战加入的恢复快照路径。
- 验证记录：主 agent 复核 worker diff 后运行 `cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim`，39 passed；运行 `cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim_demo`，15 passed；运行 `cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim_demo_frame_bundle`，1 passed；运行 `git diff --check -- apps/game-server/src/core/system/lockstep_sim/mod.rs apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs apps/game-server/src/core/runtime/room_manager/tests/tick.rs` 通过。未执行真实多服务/客户端联调，按用户要求需要联调的先跳过。

- [x] 定义 `SimInitialSnapshot` 或复用 `SimSnapshot` 的下发结构。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:68` 定义 `SimInitialSnapshot`，`:315` 通过 `create_initial_snapshot_with_config` 写入内嵌 `SimSnapshot`，`:393` 提供 `restore_initial_snapshot`）
- [x] 快照包含 schema version、room id、start frame、tick rate、config hash、rng seed、entities 和 control bindings。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:1697` 的 `initial_snapshot_json_contains_full_recovery_contract_and_round_trips` 逐项断言 schema/schemaVersion/roomId/startFrame/tickRate/configHash/configVersion/simSchemaVersion/rngSeed/entities/controlBindings/stateHash/snapshot）
- [x] 在开局时向客户端下发初始快照。（验证：`apps/game-server/src/core/runtime/room_manager/tests/tick.rs:245` 断言 `start_game` 返回的 `RoomSnapshot.game_state.initialSnapshot` 可反序列化并恢复；`apps/game-server/src/core/room/mod.rs:419` 和 `:447` 通过 `logic.get_serialized_state()` 写入 `RoomSnapshot.game_state`）
- [x] 在 late join、重连和观战加入时下发可恢复快照。（验证：`apps/game-server/src/core/runtime/room_manager/tests/tick.rs:305` 覆盖已有成员 rejoin、`reconnect_room` 和 `join_room_as_observer` 返回 snapshot 且可恢复；`lockstep_sim_demo` 当前 `allow_join_in_game=false`，新玩家游戏中加入不在本阶段支持范围）
- [x] 明确 snapshot 与 subsequent `FrameBundlePush` 的衔接 frame。（验证：`apps/game-server/src/core/runtime/room_manager/tick.rs:193`-`:205` 对 `lockstep_sim_demo` 每帧强制生成 snapshot，`:256`-`:263` 写入 `FrameBundlePush.snapshot`；`apps/game-server/src/core/runtime/room_manager/tests/tick.rs:263`/`:289` 分别验证 frame 1、frame 2 snapshot 的 `startFrame` 与 `current_frame_id` 衔接）
- [x] 增加 snapshot 序列化和恢复测试。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:1697` 覆盖 JSON contract 和 serde round-trip，`apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:814` 覆盖 serialized state initialSnapshot 恢复，`apps/game-server/src/core/runtime/room_manager/tests/tick.rs:3` 增加 `assert_lockstep_snapshot_recoverable`）
- [x] 验证项：从快照恢复后继续应用后续输入，hash 与服务端持续推进一致。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:1697` 和 `apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:814` 均验证恢复后下一帧 hash 与连续推进一致；`apps/game-server/src/core/runtime/room_manager/tests/tick.rs:218` 从 `FrameBundlePush.snapshot` 恢复后应用 frame 2 输入并对齐 manager 连续 hash；相关三条 cargo test 均通过）

## 阶段 10：重连、观战和缺帧策略适配

- 开始时间：2026-07-05 13:47:46 +08:00
- 结束时间：2026-07-05 14:06:30 +08:00
- 开发总结：`lockstep_sim_demo` 的重连、观战和缺帧策略离线覆盖已补齐。当前 demo policy 明确为 `Optimistic + Empty`，缺帧时继续推进并生成 synthetic empty input；通用 runtime 仍保留 `Strict + RepeatLast` 和 `DropAfterMisses` 测试证据。下行 snapshot/debug 中的 `inputSources` 与 `debugSummary` 可区分 real、synthesizedEmpty、synthesizedRepeatLast；观战者不进入 control binding 且不能提交输入，但能拿到可恢复 snapshot/hash；重连 snapshot 恢复后继续推进与服务端权威 hash 对齐。真实多服务/客户端联调按用户要求先跳过。
- 验证记录：主 agent 复核 worker diff 后运行 `cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim`，40 passed；运行 `cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim_demo`，16 passed；运行 `cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim_demo_optimistic_empty_missing_input_keeps_source_metadata`，1 passed；运行 `cargo test --manifest-path apps/game-server/Cargo.toml drop_after_misses_marks_player_offline_after_threshold`，1 passed；运行 `git diff --check -- apps/game-server/src/core/runtime/room_manager/tests/tick.rs apps/game-server/src/core/runtime/room_manager/tests/storage.rs` 通过。未启动外部服务。

- [x] 确认 `Strict`、`Optimistic`、`Empty`、`RepeatLast`、`DropAfterMisses` 对 `lockstep_sim_demo` 的适用规则。（验证：`apps/game-server/src/core/runtime/room_policy.rs:221` 定义 `lockstep_sim_demo`，`:240`/`:241` 配置为 `Optimistic + Empty`，`:475` 的 policy 测试断言该组合；`apps/game-server/src/core/runtime/room_manager/tests/tick.rs:919` 覆盖通用 `Strict + RepeatLast`，`apps/game-server/src/core/runtime/room_manager/tests/storage.rs:482` 覆盖通用 `DropAfterMisses` 标记离线）
- [x] 对缺帧补空输入和重复上一帧输入保留 `SimInputSource` 或等价 metadata。（验证：`apps/game-server/src/core/runtime/room_manager/transfer_codec.rs:291`/`:298` 生成 `is_synthetic=true` 的空输入，`:302`/`:309` 生成 `is_synthetic=true` 的 repeat-last 输入；`apps/game-server/src/core/runtime/room_manager/tests/tick.rs:305` 断言 `inputSources` 和 `debugSummary` 包含 `real`、`synthesizedEmpty`，`:919` 断言 repeat-last 输入为 synthetic）
- [x] 确认断线玩家实体在短期缺帧下如何保持、停止或继续上一移动状态。（验证：`apps/game-server/src/core/runtime/room_manager/tests/tick.rs:305` 中 `Optimistic + Empty` 缺帧时 player-b 收到空 synthetic input 且位置保持 start x；`apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:942` 覆盖 synthetic empty 保持上一移动但不重复释放技能；`apps/game-server/src/core/runtime/room_manager/tests/storage.rs:482` 覆盖 `DropAfterMisses` 到阈值后标记离线）
- [x] 确认观战者不产生控制输入，但能收到快照和 hash。（验证：`apps/game-server/src/core/runtime/room_manager/tests/tick.rs:493` 断言 observer snapshot 可恢复且 `controlBindings` 不包含 observer，`:501` 断言成员角色为 Observer，`:508`-`:517` 断言提交输入返回 `OBSERVER_CANNOT_SEND_INPUT`；同测试和 `observer_cannot_submit_or_generate_tick_inputs` 覆盖观战只读）
- [x] 增加重连恢复和观战加入的单元测试或集成测试准备。（验证：`apps/game-server/src/core/runtime/room_manager/tests/tick.rs:423` 覆盖已有成员 rejoin、`reconnect_room`、`join_room_as_observer`、observer 拒绝输入和 in-game late join 仍返回 `ROOM_ALREADY_IN_GAME`；本阶段仅做离线单元/集成测试准备，未跑真实服务联调）
- [x] 验证项：重连/观战从快照恢复后不会破坏服务端权威 hash。（验证：`apps/game-server/src/core/runtime/room_manager/tests/tick.rs:531`-`:564` 从 `reconnect.snapshot.game_state` 恢复 `LockstepSimDemoLogic`，应用 frame 2 输入后与 manager 连续推进的 `lastFrame.stateHash` 和 `observerFrame.stateHash` 一致；`cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim_demo` 16 passed）

## 阶段 11：lockstep-client online 对账入口

- 开始时间：2026-07-05 14:09:40 +08:00
- 结束时间：2026-07-05 14:20:30 +08:00
- 开发总结：`tools/lockstep-client` online 入口的离线可验证能力已补齐。dry-run 现在输出 Auth、RoomJoin、RoomStatePush、RoomReady、RoomStart、FrameBundlePush 和 PlayerInputReq 的 packet plan，能展示 create-or-join `lockstep_sim_demo`、`sim_input` payload、下行 snapshot/hash/events/inputSources 消费预期。测试覆盖 RoomStatePush 初始快照恢复、FrameBundlePush snapshot + inputs replay、mismatch 诊断、参数校验和 payload 错误路径。真实 online 移动/近战对账按用户要求未执行。
- 验证记录：主 agent 复核 worker diff 后运行 `cargo test --manifest-path tools/lockstep-client/Cargo.toml`，25 passed；运行 `cargo check --manifest-path tools/lockstep-client/Cargo.toml` 通过；运行 `cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode online --scenario move_straight --dry-run` 通过，输出 5 个 `sim_input` packet 且 `network: not started; dry-run only`；运行 `cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode online --scenario lockstep_demo_melee --dry-run` 通过，输出 `castSkill` target `9000` packet 且未连接网络；运行 `git diff --check -- tools/lockstep-client/src/online.rs` 通过。

- [x] 扩展 `tools/lockstep-client` online 模式以创建或加入 `lockstep_sim_demo` room。（验证：`tools/lockstep-client/src/online.rs:411` 的 dry-run packet plan 输出 `RoomJoinReq` create-or-join room/policy，dry-run 输出包含 `RoomJoinReq(1101): create-or-join room=lockstep-online-demo policy=lockstep_sim_demo`；真实连接入口仍由 `drive_online_session` 负责，未启动服务）
- [x] 支持发送 `sim_input` JSON payload。（验证：`tools/lockstep-client/src/online.rs:395` 从 `SimInput` 构造 `PlayerInputPlan`，`:478` 构造 version/seq/commands JSON；`builds_server_sim_input_payload_from_supported_sim_core_commands` 覆盖 move/stop/face/castSkill，两个 dry-run 分别输出 move payload 和 `castSkill skillId=1 targetEntityId=9000`）
- [x] 支持消费服务端快照、frame bundle、hash 和事件。（验证：`tools/lockstep-client/src/online.rs:1758` 的 `maybe_consume_push_packet` 消费 `RoomStatePush`/`FrameBundlePush`，`:2454` 的 `room_state_and_frame_bundle_push_restore_and_replay_snapshot` 覆盖 `RoomSnapshot.game_state.initialSnapshot` 恢复和 `FrameBundlePush.snapshot` replay）
- [x] 本地使用同一 `sim-core` replay 并比对服务端 hash。（验证：`tools/lockstep-client/src/online.rs:899` 的 `OnlineReplay::apply_server_frame` 使用 `sim-core` 应用服务端帧输入并比较 `state_hash`；`online_replay_matches_server_frame_hash_and_events` 和 `room_state_and_frame_bundle_push_restore_and_replay_snapshot` 测试通过）
- [x] 输出首个 mismatch frame、server hash、client hash、实体差异和事件差异。（验证：`tools/lockstep-client/src/online.rs:2538` 的 `online_replay_mismatch_reports_hash_entities_events_and_inputs` 断言错误文本包含 `first mismatch frame`、`server_hash`、`client_hash`、`entity diffs`、`event diffs` 和 `inputs`）
- [x] 增加 online 模式的参数校验和错误输出。（验证：`tools/lockstep-client/src/online.rs:2216` 覆盖缺 `--scenario`、重复参数、非法 timeout、unsupported mode 和未知参数，`:2264` 覆盖非 dry-run 缺 ticket 在网络连接前报错；`sim_input_payload_reports_validation_errors` 覆盖版本、方向、速度、skillId 和 unsupported target 错误）
- [x] 验证项：在用户确认启动服务后，online 模式能跑通至少移动和近战场景对账。（完成记录：MyServer 侧真实 online 对账已在 `summary/共享帧同步后续接入与联调_checklist.md` 阶段 11 完成；后续复跑、脚本化和失败归档统一由 `summary/共享帧同步调试验证_checklist.md` 阶段 3 承接）

## 阶段 12：测试覆盖与构建检查

- 开始时间：2026-07-05 14:23:15 +08:00
- 结束时间：2026-07-05 14:33:49 +08:00
- 开发总结：本阶段未新增业务代码或测试文件，复核后确认前序阶段已经覆盖输入解析、初始世界、配置、tick、事件、snapshot、`sim-core` 与 `game-server` 适配层、`tools/lockstep-client` offline/online dry-run 入口。主 agent 复跑本地 crate 单测、构建检查和 offline scenario，结果可重复；真实 Redis、PostgreSQL、NATS、多服务启动、客户端或 online 网络联调按用户要求先跳过。`apps/game-server` 的 fmt check 已执行但仍命中既有无关 rustfmt 差异，本阶段只记录不修复。
- 验证记录：主 agent 复跑 `cargo test --manifest-path packages/sim-core/Cargo.toml`，98 passed；`cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim`，40 passed；`cargo test --manifest-path tools/lockstep-client/Cargo.toml`，25 passed；`cargo check --manifest-path packages/sim-core/Cargo.toml`、`cargo check --manifest-path tools/lockstep-client/Cargo.toml`、`cargo check --manifest-path apps/game-server/Cargo.toml` 均通过，其中 game-server 仅有既有 warnings；`cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode offline --scenario move_straight` 通过，final frame 5，hash `ad9a151d0953d437`；`cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode offline --scenario lockstep_demo_melee` 通过，final frame 1，hash `959839fddfc8c0dc`；`cargo fmt --check --manifest-path packages/sim-core/Cargo.toml` 和 `cargo fmt --check --manifest-path tools/lockstep-client/Cargo.toml` 通过；`cargo fmt --check --manifest-path apps/game-server/Cargo.toml` 失败，差异涉及 `apps/game-server/tools/csv_codegen.rs`、`apps/game-server/src/core/logic/room_logic.rs`、`apps/game-server/src/core/runtime/room_manager/tests/tick.rs`、`apps/game-server/src/core/runtime/room_manager/tick.rs`、`apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs`，按既有格式化差异记录未修复。

- [x] 增加输入解析、初始世界、配置、tick、事件、snapshot 的单元测试。（验证：现有 `packages/sim-core` 单测覆盖输入、数学、snapshot、tick、hash、combat，`cargo test --manifest-path packages/sim-core/Cargo.toml` 98 passed；`apps/game-server` 的 `lockstep_sim`/`lockstep_sim_demo` 覆盖输入解析、初始世界、配置元信息、tick、事件摘要和快照恢复，主 agent 复跑 `cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim` 40 passed）
- [x] 增加 game-server 相关 crate 的格式化和构建检查。（验证：`cargo check --manifest-path apps/game-server/Cargo.toml` 通过，仅有既有 warnings；`cargo fmt --check --manifest-path apps/game-server/Cargo.toml` 已执行但失败于既有无关 rustfmt 差异，涉及 `csv_codegen.rs`、`room_logic.rs`、`room_manager/tick.rs`、`room_manager/tests/tick.rs`、`lockstep_sim_demo/mod.rs`，本阶段按记录不修复）
- [x] 增加 `sim-core` 与 `game-server` 适配层的回归测试。（验证：`cargo test --manifest-path packages/sim-core/Cargo.toml` 98 passed；`cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim` 40 passed；适配层已覆盖 `sim_input`、绑定、配置、hash、事件、snapshot 和恢复后继续推进）
- [x] 保留 offline scenario 作为共享核心回归基线。（验证：`cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode offline --scenario move_straight` 通过，final frame 5，hash `ad9a151d0953d437`；`cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode offline --scenario lockstep_demo_melee` 通过，final frame 1，hash `959839fddfc8c0dc`）
- [x] 对需要真实 Redis、PostgreSQL、NATS 或多服务启动的测试，先列出依赖并等待用户确认。（验证：真实联调依赖 Redis、PostgreSQL、NATS、auth-http、game-proxy、game-server、真实客户端或 online 网络连接；用户要求“需要联调的先跳过”，本阶段未启动这些服务，仅记录待用户后续确认）
- [x] 验证项：本阶段能给出可重复的本地验证命令和结果记录。（验证：本阶段验证记录列出 `cargo test`、`cargo check`、`cargo fmt --check` 和 offline scenario 的完整命令与结果；失败的 game-server fmt check 也记录了具体差异范围和未修复原因）

## 阶段 13：文档和协议说明同步

- 开始时间：2026-07-05 14:35:40 +08:00
- 结束时间：2026-07-05 14:49:12 +08:00
- 开发总结：完成共享帧同步相关正式文档同步。文档已记录 `lockstep_sim_demo` 的独立 policy 边界、服务端权威推进和 `sim-core` 职责划分；协议与外部接入说明已补齐 `sim_input` JSON、禁止客户端提交权威结果字段、hash/event/snapshot 下发语义；`tools/lockstep-client` README 已补充 offline、online dry-run 和真实 online 前置依赖说明。真实 online / 外部客户端联调仍按用户要求跳过，文档中只记录待用户确认启动依赖服务后执行。
- 验证记录：主 agent 审核 worker diff 后运行 `git diff --check -- docs/游戏服与接入层/共享帧同步移动战斗核心设计.md docs/游戏服与接入层/帧同步与房间生命周期设计.md docs/协议与客户端/协议设计.md docs/协议与客户端/外部客户端接入说明.md tools/lockstep-client/README.md` 通过，仅有 LF/CRLF 提示；使用 `Select-String` 核对文档出现 `lockstep_sim_demo`、`sim_input`、`stateHash`、`initialSnapshot`、`FrameBundlePush.snapshot`、`eventSummaries`、`debugState`、`--mode online --scenario ... --dry-run` 等关键字段；对照 `room_policy.rs`、`lockstep_sim/mod.rs`、`lockstep_sim_demo/mod.rs` 和 `tools/lockstep-client/src/online.rs` 确认 policy ID、payload 字段、hash/event/snapshot 字段和命令一致。

- [x] 更新共享帧同步设计文档，记录 `lockstep_sim_demo` 当前实现边界。（验证：`docs/游戏服与接入层/共享帧同步移动战斗核心设计.md:82` 记录 policy ID/参数和不启用旧 movement correction/AOI，`:170` 说明 `game-server` 仍是房间权威和网络边界，`:209` 说明输入等待、缺输入补偿、广播、snapshot、重连和观战由 `RoomManager` / `RoomLogic` 承担）
- [x] 更新协议设计或外部客户端接入说明，记录 `sim_input` JSON 结构、hash 下发和 snapshot 语义。（验证：`docs/协议与客户端/协议设计.md:736` 起记录 `PlayerInputReq(action="sim_input")` 和 JSON payload，`:763` 记录禁止提交权威结果字段，`:872`/`:874` 记录 `FrameBundlePush.snapshot`、`stateHash`、`eventSummaries`、`debugState`；`docs/协议与客户端/外部客户端接入说明.md:145` 起同步 payload schema，`:173` 起同步 `RoomSnapshot.game_state` 恢复与对账结构）
- [x] 更新 mock/lockstep-client 使用说明，记录 offline 和 online 验证命令。（验证：`tools/lockstep-client/README.md:19`/`:40` 记录 offline 命令，`:76`/`:78` 记录 move 和 melee 的 online dry-run 命令，`:118`/`:129` 记录真实 online 命令模板，`:165` 起列出真实 online 前置服务依赖）
- [x] 明确现有 `robot_sync_room`、`movement_demo`、`combat_demo` 与新 demo 的关系。（验证：`docs/协议与客户端/外部客户端接入说明.md:185`-`:187` 明确 `lockstep_sim_demo` 不替换旧 demo 并说明三者职责；`docs/游戏服与接入层/共享帧同步移动战斗核心设计.md:848`-`:853` 记录旧 demo 保留关系；`tools/lockstep-client/README.md:105`-`:109` 同步 README 说明）
- [x] 记录暂不解决的生产部署、复杂物理、NavMesh、AOI 和跨服迁移能力。（验证：`docs/游戏服与接入层/共享帧同步移动战斗核心设计.md:96`-`:97` 记录真实客户端联调、跨服迁移、复杂物理、NavMesh、生产 AOI 和完整 CSV 技能/Buff 映射未完成；`docs/游戏服与接入层/帧同步与房间生命周期设计.md:42` 记录同类不支持范围；`tools/lockstep-client/README.md:298` 起记录工具和当前 demo 不覆盖范围）
- [x] 验证项：文档中的命令、policy ID、payload 字段与代码一致。（验证：`apps/game-server/src/core/runtime/room_policy.rs:223` policy ID 为 `lockstep_sim_demo`，`:237`-`:246` 参数与文档一致；`apps/game-server/src/core/system/lockstep_sim/mod.rs:17`/`:945`/`:953` 定义 `sim_input` version 和严格 JSON 结构，`:190`-`:211` 定义 frame envelope 字段；`tools/lockstep-client/src/online.rs:28`-`:36`、`:411`、`:478` 与 README 命令和 dry-run 行为一致；文档级 `git diff --check` 通过）

## 阶段 14：旧 demo 迁移评估

- 开始时间：2026-07-05 14:52:16 +08:00
- 结束时间：2026-07-05 15:06:57 +08:00
- 开发总结：完成旧 demo 迁移评估文档更新。当前决策是 `movement_demo`、`combat_demo`、`robot_sync_room` 均继续保留，暂不由 `lockstep_sim_demo` 直接替换；后续仅在 hash、snapshot、events、inputSources、debugSummary/debugState、重连/观战、旧协议兼容、客户端场景和真实联调记录满足前置条件后，再评估合并或删除。真实联调仍按用户要求跳过，文档中明确需要用户确认启动依赖服务后执行。
- 验证记录：主 agent 审核 worker diff 后运行 `git diff --check -- docs/游戏服与接入层/共享帧同步移动战斗核心设计.md` 通过，仅有 LF/CRLF 提示；使用 `Select-String` 核对文档包含 `movement_demo`、`combat_demo`、`robot_sync_room`、`lockstep_sim_demo`、`MovementSnapshotPush`、`MovementRejectPush`、`MovementRecoveryState`、`hash`、`snapshot`、`events`、`inputSources`、`debugSummary`、`debugState`、真实联调确认说明；对照 `movement_demo/mod.rs`、`combat_demo/mod.rs`、`robot_sync_room/mod.rs`、`room_policy.rs`、`factory.rs` 和 combat input/event 代码确认旧合同名与 policy 显式注册关系一致。

- [x] 评估 `movement_demo` 是否能复用 `sim-core` movement 或只保留为旧样例。（验证：`docs/游戏服与接入层/共享帧同步移动战斗核心设计.md:1567` 记录保留决策，`:1573`-`:1575` 说明可迁移的移动计算和不能直接替换的 `MovementSnapshotPush` / `MovementRejectPush` / `MovementRecoveryState` / AOI / transfer 旧合同，`:1592`-`:1595` 列出后续映射和兼容差异任务）
- [x] 评估 `combat_demo` 的 f32 坐标、范围、速度和旧 ECS 逻辑迁移风险。（验证：`docs/游戏服与接入层/共享帧同步移动战斗核心设计.md:1599` 记录保留决策，`:1605` 记录旧输入、`GameMessagePush`、`CombatSnapshot`、NPC/timer/transfer 合同，`:1611`-`:1613` 记录 f32 和旧事件名差异，`:1617`-`:1625` 列出迁移风险与 CSV/CombatConfig 后续任务）
- [x] 评估 `robot_sync_room` 是否继续作为轻量输入转发样例，或后续并入 `lockstep_sim_demo`。（验证：`docs/游戏服与接入层/共享帧同步移动战斗核心设计.md:1631` 记录继续保留且本阶段不合并/不删除，`:1635`-`:1637` 说明其与 `lockstep_sim_demo` 的职责差异，`:1653`-`:1654` 列出未来并入前的替代说明和下线条件）
- [x] 列出迁移前必须满足的 hash、快照、事件和客户端场景验收条件。（验证：`docs/游戏服与接入层/共享帧同步移动战斗核心设计.md:1656` 起定义迁移前验收条件，`:1659` 记录 hash 条件，`:1660` 记录 snapshot 条件，`:1662` 记录 events 条件，`:1663` 记录 inputSources/debugSummary/debugState，`:1668` 记录外部客户端场景条件）
- [x] 不在本阶段删除旧 demo，除非已有等价功能和明确回归结果。（验证：`docs/游戏服与接入层/共享帧同步移动战斗核心设计.md:1563` 明确本阶段不删除旧 demo、不做业务代码迁移、不替换 policy、不变更协议；`:1678` 记录没有等价能力和明确结果时默认继续保留旧 demo；`git status --short` 仅显示该设计文档被修改，无 Rust/Node 业务代码删除）
- [x] 验证项：输出旧 demo 保留、合并或替换的决策记录和后续任务。（验证：`docs/游戏服与接入层/共享帧同步移动战斗核心设计.md:1559` 起输出阶段 14 评估记录，包含三类旧 demo 决策、后续建议和迁移前验收条件；文档级 `git diff --check` 通过；未启动真实服务或联调）

## 阶段 15：最终联调验收

- 开始时间：2026-07-05 15:09:56 +08:00
- 结束时间：2026-07-05 15:19:57 +08:00
- 开发总结：完成最终离线验收和联调前 dry-run 核验。本阶段未修改业务代码或正式文档，未启动 Redis、PostgreSQL、NATS、auth-http、game-server、game-proxy、admin、mybevy 或真实客户端；真实 online hash 对账和端到端联调按用户要求先跳过，保留为后续待验收项。
- 验证记录：worker 与主 agent 核对 `git status --short` 均无业务代码改动；`cargo test --manifest-path packages/sim-core/Cargo.toml` 98 passed；`cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim` 40 passed；`cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim_demo` 16 passed，存在既有 warnings；`cargo test --manifest-path tools/lockstep-client/Cargo.toml` 25 passed；`cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode offline --scenario move_straight` 通过，final frame 5，hash `ad9a151d0953d437`；`cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode offline --scenario lockstep_demo_melee` 通过，final frame 1，hash `959839fddfc8c0dc`；`cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode online --scenario move_straight --dry-run` 通过，输出 5 个 `sim_input` packet 且 `network: not started; dry-run only`；`cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode online --scenario lockstep_demo_melee --dry-run` 通过，输出 1 个 `castSkill` packet 且 `network: not started; dry-run only`。

- [x] offline scenario 能验证移动和基础战斗 hash 一致。（验证：`cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode offline --scenario move_straight` 通过，final frame 5，hash `ad9a151d0953d437`；`cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode offline --scenario lockstep_demo_melee` 通过，final frame 1，hash `959839fddfc8c0dc`）
- [x] `lockstep_sim_demo` 能调用同一 `sim-core` 推进服务端权威状态。（验证：`apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:270` 调用 `step_world_with_config`，`:274` 生成权威 frame envelope；`apps/game-server/src/core/system/lockstep_sim/mod.rs:742` 为 sim-core 适配推进入口；`cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim` 40 passed，`cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim_demo` 16 passed）
- [x] `tools/lockstep-client` online 能与服务端 hash 对账。（完成记录：MyServer 侧真实 online 移动、近战和 observer recovery 已在 `summary/共享帧同步后续接入与联调_checklist.md` 阶段 11 完成；后续统一复验入口为 `summary/共享帧同步调试验证_checklist.md`）
- [x] 服务端能下发初始快照、恢复快照、事件和 hash。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:68`/`:190` 定义 `SimInitialSnapshot` 与 `SimFrameEnvelope`，包含 `stateHash`、`eventSummaries`、`debugState`；`apps/game-server/src/core/runtime/room_manager/tests/tick.rs:218` 覆盖 `FrameBundlePush.snapshot` 每帧携带并可恢复；`cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim` 40 passed）
- [x] 重连或观战能从快照恢复，不破坏后续 hash。（验证：`apps/game-server/src/core/runtime/room_manager/tests/tick.rs:423` 覆盖 rejoin、reconnect、observer snapshot 恢复和 observer 拒绝输入，`:558`-`:563` 比较恢复后的 `lastFrame.stateHash` 与 `observerFrame.stateHash` 一致；`cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim_demo` 16 passed）
- [x] 相关文档已说明当前实现、联调命令、依赖服务和不支持能力。（验证：阶段 13/14 已提交文档更新；`docs/游戏服与接入层/共享帧同步移动战斗核心设计.md` 记录实现边界和旧 demo 迁移评估，`docs/协议与客户端/外部客户端接入说明.md` 记录 `sim_input`、snapshot/hash/events 语义和真实联调依赖，`tools/lockstep-client/README.md` 记录 offline、online dry-run 与真实 online 命令模板）
- [x] 验证项：在用户确认启动依赖服务后完成一次端到端联调记录。（完成记录：MyServer 侧端到端 online 对账记录已在 `summary/共享帧同步后续接入与联调_checklist.md` 阶段 11 完成；外部 mybevy 和后续调试验收统一迁移到 `summary/共享帧同步调试验证_checklist.md`）

## 最终完成定义

以下项目作为整体完成标准，不要求每个开发阶段都执行，由所有相关阶段完成后统一验收。

- 开始时间：2026-07-05 15:19:57 +08:00
- 结束时间：2026-07-09 17:28:35 +08:00
- 验收总结：本接入清单的 game-server 功能实现、离线验证、online 对账和文档同步已收口；MyServer 侧真实 online 证据以 `summary/共享帧同步后续接入与联调_checklist.md` 阶段 11 为准，后续所有共享帧同步调试、自动化复验、外部 mybevy 联调和失败归档统一迁移到 `summary/共享帧同步调试验证_checklist.md`。

- [x] `game-server` 已提供独立的 `lockstep_sim_demo` 或等价策略，不影响旧 demo。（验证：阶段 4 证明 `room_policy.rs` 独立注册 `lockstep_sim_demo`，`factory.rs` 四个 demo 分支独立；阶段 14 文档记录旧 demo 均保留且不替换）
- [x] 服务端权威 tick 使用 `sim-core` 推进，并输出稳定 hash 和事件。（验证：阶段 7/8/15 证明 `lockstep_sim_demo` 通过 `lockstep_sim` 适配层调用 `sim-core` 推进，`SimFrameEnvelope` 下发 `stateHash` 与 `eventSummaries`；`cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim` 40 passed）
- [x] 服务端下发的初始快照、恢复快照和配置 hash 足以让客户端或 online 工具本地 replay。（验证：阶段 6/9/11/15 覆盖 `initialSnapshot`、`FrameBundlePush.snapshot`、`configHash`、recovery snapshot 和 `tools/lockstep-client` 本地 replay；`cargo test --manifest-path tools/lockstep-client/Cargo.toml` 25 passed）
- [x] `tools/lockstep-client` online 能与服务端完成 hash 对账。（完成记录：`summary/共享帧同步后续接入与联调_checklist.md` 阶段 11 已记录移动、近战和 observer recovery online 对账；后续复跑由 `summary/共享帧同步调试验证_checklist.md` 阶段 3 承接）
- [x] 相关测试、文档和联调记录覆盖移动、基础战斗、非法输入、重连或观战恢复。（完成记录：本清单已覆盖离线测试和文档；MyServer online 联调证据见 `summary/共享帧同步后续接入与联调_checklist.md` 阶段 11；外部 mybevy 调试验收迁移到 `summary/共享帧同步调试验证_checklist.md`）
