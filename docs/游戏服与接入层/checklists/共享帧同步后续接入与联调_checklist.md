# 共享帧同步后续接入与联调 Checklist

## 目标

在 P0/P1/P2 已完成 `sim-core`、offline replay、移动和基础战斗核心后，继续完成 MyServer 仓库内的后续接入闭环：`game-server` 新增共享核心验证入口，`tools/lockstep-client` 支持 online 对账，并通过服务端和仓库内联调工具在同一初始状态、配置 hash、随机 seed 和输入帧下得到一致 hash。

本清单是 MyServer 侧总控 checklist，负责仓库内服务端、协议、联调工具、文档和验收门禁。具体 game-server 侧开发细节以 `summary/游戏服共享帧同步接入_checklist.md` 为准；`mybevy` 客户端功能开发只在 `summary/mybevy共享帧同步接入_checklist.md` 维护，本清单仅保留与外部客户端交接相关的协议和联调边界。

## 基础原则

- [x] 不重复实现 P0/P1/P2 已完成的 `sim-core` 定点移动、基础战斗和 offline scenario 能力。（验证：`apps/game-server/Cargo.toml:23` 和 `tools/lockstep-client/Cargo.toml:10` 均引用 `packages/sim-core`；最终验收复跑 `move_straight` / `lockstep_demo_melee` offline scenario 通过）
- [x] `game-server` 和 `mybevy` 必须引用同一份 `sim-core`，不能在两端各自重写移动、命中、伤害、Buff 或 hash 规则。（验证：MyServer 侧 `game-server` 与 `tools/lockstep-client` 使用同仓 `packages/sim-core`；外部客户端引用方式和任务边界由 `summary/mybevy共享帧同步接入_checklist.md` 与 `docs/协议与客户端/外部客户端接入说明.md:120` 维护）
- [x] 服务端仍是权威边界，客户端本地模拟只用于 replay、预测、表现和对账。（验证：`tools/lockstep-client/src/online.rs:772` 从服务端 `initialSnapshot` 恢复本地 replay，`:1723` 对服务端 frame/hash/events 做对账；真实 online 移动/近战联调通过）
- [x] 第一阶段优先完成权威帧 replay 和 hash 对账，不默认实现复杂预测、NavMesh、生产 AOI 或跨服 room transfer。（验证：本阶段代码改动集中在 `lockstep_sim_demo` snapshot/frame replay 与 `tools/lockstep-client` online 对账；未引入预测、NavMesh、生产 AOI 或跨服 room transfer）
- [x] 本清单不拆解 `mybevy` 客户端场景、渲染、HUD、replay 缓存或客户端构建任务；相关开发只在 `summary/mybevy共享帧同步接入_checklist.md` 维护。（验证：`docs/协议与客户端/外部客户端接入说明.md:126` 明确客户端场景、渲染、HUD、事件表现、replay/rollback、构建和客户端验收由 mybevy checklist 承接）
- [x] 真实多服务联调或外部客户端联调前，必须先列出需要启动的服务、依赖和外部仓库路径，并等待用户确认。（验证：2026-07-05 本轮执行前已向用户列出 Redis、PostgreSQL、Core NATS、`auth-http`、`game-server`、可能的 `game-proxy` 和联调命令范围，用户回复“确认”后才启动最小 dev-stack）
- [x] 每个阶段完成后补充对应验证记录；涉及代码提交时，按功能模块拆分并排除 `summary/` checklist 进度记录。（验证：阶段 11 已补充开发总结和验证记录；后续提交按 `mygit-skill` 仅暂存代码路径，排除 `summary/`）

## 阶段 1：现状和边界收口

- 开始时间：2026-07-04 17:10:39 +08:00
- 结束时间：2026-07-04 17:21:37 +08:00
- 开发总结：只读核对完成。P0/P1/P2 checklist 均已 100% 完成，`packages/sim-core` 已具备定点移动、基础战斗、事件、snapshot 和稳定 hash；`tools/lockstep-client` 已支持 offline 双 world replay、事件断言、预期错误和移动/战斗 scenario。后续总控清单不重复实现 P0/P1/P2，重点转入 `game-server` 服务端权威接入、`lockstep-client` online 对账、MyServer 文档和外部客户端交接门禁；`mybevy` 客户端 replay、HUD 和 replay 缓存开发转由 `summary/mybevy共享帧同步接入_checklist.md` 维护。
- 验证记录：worker subagent 只读核对通过，未改文件、未运行测试、未提交；证据覆盖 P0/P1/P2 checklist、`packages/sim-core/src/lib.rs`、`tools/lockstep-client/src/offline.rs`、`tools/lockstep-client/README.md`、共享帧同步设计文档和两个细分 checklist。

- [x] 确认 P0/P1/P2 checklist 均已完成，并记录 `sim-core`、`lockstep-client` offline、移动和战斗 scenario 的当前能力。（验证：`summary/共享帧同步P0定点模拟核心_checklist.md`、`summary/共享帧同步P1移动与离线验证_checklist.md`、`summary/共享帧同步P2基础战斗核心_checklist.md` 均无未勾选项；`packages/sim-core/src/lib.rs` 导出核心 API，`tools/lockstep-client/src/offline.rs` 实现 offline 双 world replay）
- [x] 阅读 `docs/游戏服与接入层/共享帧同步移动战斗核心设计.md`，标记已经落地、需要接入和明确不在第一阶段解决的内容。（验证：已落地 `sim-core`、offline replay 和移动/战斗 scenario；MyServer 待接入项集中在 game-server policy、online client、协议文档和联调验收；mybevy 场景与 replay/rollback 由 mybevy 细分清单维护）
- [x] 确认本阶段不处理复杂物理、NavMesh、高级动画状态机、完整技能编辑器、大世界 AOI 和跨服完整迁移。（验证：设计文档 `## 24. 不在第一阶段解决的问题` 明确排除完整生产部署、大世界 AOI、复杂物理、NavMesh、高级动画状态机、完整技能编辑器和跨服 room transfer 完整迁移）
- [x] 确认 `game-server` 侧后续细分清单已存在并覆盖服务端接入。（验证：`summary/游戏服共享帧同步接入_checklist.md` 覆盖 room 链路盘点、引入 `sim-core`、`sim_input` 适配、`lockstep_sim_demo`、初始 world/config/hash、权威 tick、事件/snapshot、重连/观战、online 对账和测试文档）
- [x] 确认 `mybevy` 侧后续细分清单已存在并覆盖客户端接入。（验证：`summary/mybevy共享帧同步接入_checklist.md` 覆盖外部仓库确认、引入 `sim-core`、`arena.lockstep_sim`、快照解析、输入量化、`sim_input` 上行、权威帧 replay、渲染只读、事件/HUD/mismatch、replay 缓存和验收文档）
- [x] 验证项：输出一份总控范围说明，能解释本清单与两个细分 checklist 的关系。（验证：本阶段开发总结说明总控 checklist 负责 MyServer 侧整体路线和验收门禁，`游戏服共享帧同步接入_checklist.md` 与 `mybevy共享帧同步接入_checklist.md` 分别承接服务端和外部客户端细分开发）

## 阶段 2：game-server 接入 API 收口

- 开始时间：2026-07-04 17:23:20 +08:00
- 结束时间：2026-07-04 17:38:16 +08:00
- 开发总结：只读核对完成。建议 `game-server` 新增 `core/system/lockstep_sim` 作为 `sim-core` 协议、配置、快照和下行 envelope 适配层，`gameroom/lockstep_sim_demo` 仅负责 `RoomLogic` 生命周期和广播。`SimWorld`、固定 `SimConfig`、控制绑定、config version/hash、sim schema version 与 rng seed 应在 `on_game_started` 创建并保存在 room logic 中，通过 `RoomSnapshot.game_state` 下发初始/恢复快照；每帧 `SimStepResult` 的事件、state hash 和轻量 debug state 可先用 `GameMessagePush` 承载。
- 验证记录：worker subagent 只读核对通过，未改文件、未运行测试、未提交；证据覆盖 `RoomLogic`、`RoomManager::process_room_tick`、`RoomSnapshot.game_state`、`PlayerInputRecord`、`packages/sim-core` 导出和 `game-server` 细分 checklist。当前 `game-server` 尚未引入 `sim-core` 依赖，也未注册 `lockstep_sim_demo`，但现有 room runtime 与细分 checklist 阶段 1-2 已具备后续实现边界。

- [x] 明确 `game-server` 调用 `sim-core` 的适配层模块边界。（验证：建议 `apps/game-server/src/core/system/lockstep_sim/` 负责 `PlayerInputRecord -> SimInput`、`RuntimeGameConfig -> SimConfig`、`SimStepResult -> 下行 JSON envelope`、初始 `SimWorld` 和控制绑定；`apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs` 负责 `RoomLogic` 生命周期）
- [x] 明确 `SimWorld`、`SimConfig`、`SimInput`、`SimStepResult` 在 room runtime 中的生命周期。（验证：`RoomManager::start_game` 调用 `logic.on_game_started` 后返回 `room.snapshot()`，`process_room_tick` 在 `on_tick` 前完成 `resolve_tick_inputs`；`SimWorld` 在 `on_game_started` 创建并由 `step` 原地推进，`SimStepResult` 每 tick 临时转为下行事件/hash/debug）
- [x] 明确 `character_id -> entity_id` 控制绑定由服务端初始快照下发。（验证：现有 `Room::snapshot()` 和输入参与者按 `character_id` 稳定排序，`PlayerInputReq` 不带 `character_id`，服务端用连接上的 `character_id` 调 `accept_player_input`；建议在 `RoomSnapshot.game_state.simInitialSnapshot.controlBindings` 下发绑定）
- [x] 明确配置 version、config hash、schema version 和 rng seed 的创建和下发位置。（验证：`RuntimeGameConfig.version` 可作为 config version，`SIM_CORE_SCHEMA_VERSION` 已在 `packages/sim-core/src/lib.rs` 暴露，`SimWorld::with_rng` 支持固定 seed/counter；建议 `on_game_started` 创建 `SimConfigBinding` 并通过 `RoomSnapshot.game_state` 下发）
- [x] 明确 `SimEvent`、state hash 和轻量 debug state 的下行承载结构。（验证：短期建议初始/恢复快照用 `RoomSnapshot.game_state`，每帧 `SimStepResult` 用 `GameMessagePush event=\"sim_step\"` 携带 frame、stateHashHex、events、debugState 和 configHash；长期可再评估扩展 `FrameBundlePush` 或专用 proto）
- [x] 验证项：game-server 细分 checklist 的阶段 1-2 具备可执行边界。（验证：`summary/游戏服共享帧同步接入_checklist.md` 阶段 1 覆盖 room 链路盘点，阶段 2 覆盖引入 `sim-core`；现有 `RoomRuntimePolicy`、`RoomLogic`、`process_room_tick` 和旧 demo factory 已提供清晰接入点）

## 阶段 3：sim_input 协议语义确定

- 开始时间：2026-07-04 17:39:45 +08:00
- 结束时间：2026-07-04 18:25:02 +08:00
- 开发总结：只读协议语义收口完成。第一阶段确认复用 `PlayerInputReq(action="sim_input", payload_json=...)`，payload JSON 使用 `version=1`、全局 `seq` 和 `commands` 数组承载 `move`、`stop`、`face`、`castSkill` 意图；字段命名沿用 camelCase，方向使用 `QuantizedDir` 的 `-1000..=1000` 且长度平方不超过 `1000 * 1000`，技能目标支持 none/entity/position/direction。客户端只提交输入意图，不提交命中、伤害、治疗、Buff、最终状态、hash 或事件结果；后续可在 JSON 契约稳定且带宽/解析成本明确后迁移专用 Protobuf，但第一阶段不提前固化。
- 验证记录：worker subagent 只读核对通过，未改文件、未运行测试、未提交；主 agent 复核 `packages/proto/game.proto`、`packages/sim-core/src/input.rs`、`packages/sim-core/src/math.rs`、`tools/lockstep-client/src/scenario.rs`、共享帧同步设计文档和 game-server / mybevy 两个细分 checklist，确认协议语义一致。未运行测试的原因是本阶段仅做协议语义和 checklist 证据收口，未改业务代码。

- [x] 确定第一阶段复用 `PlayerInputReq.payload_json` 和 `action = "sim_input"`。（验证：`packages/proto/game.proto:67` 定义 `PlayerInputReq` 当前字段为 `frame_id/action/payload_json/client_timestamp_ms`；`docs/游戏服与接入层/共享帧同步移动战斗核心设计.md:841`-`848` 明确推荐第一阶段使用单一 `action = "sim_input"` 和 versioned `payload_json`）
- [x] 定义 payload version、seq、commands、move、stop、face、castSkill 字段。（验证：`docs/游戏服与接入层/共享帧同步移动战斗核心设计.md:850`-`861` 给出 `version/seq/commands/type=move` 示例；`packages/sim-core/src/input.rs:22`-`52` 定义 `MoveCommand`、`FaceCommand`、`CastSkillCommand`、`SkillTarget` 和 `SimCommand::Move/Stop/Face/CastSkill`；`tools/lockstep-client/src/scenario.rs:691`-`717` 已用 JSON `type` 字段承载 `move`、`stop`、`face`、`castSkill`）
- [x] 明确客户端不得提交命中、伤害、治疗、Buff 结果和最终状态。（验证：`summary/游戏服共享帧同步接入_checklist.md:11` 明确 `PlayerInputReq` 只表达输入意图，不允许客户端提交命中、伤害、Buff 结果或最终状态；`summary/mybevy共享帧同步接入_checklist.md:80`-`84` 要求客户端只生成输入意图并禁止生成命中、伤害、治疗或 Buff 结果字段）
- [x] 明确 JSON 字段大小写、数值范围、非法字段和 version 不兼容错误。（验证：`tools/lockstep-client/src/scenario.rs:20`-`22` 使用 `camelCase` 和 `deny_unknown_fields`；`tools/lockstep-client/src/scenario.rs:696`-`716` 固定 `dirX`、`dirY`、`speedPerSecondMilli`、`skillId`、`target` 字段名；`packages/sim-core/src/math.rs:195`-`200` 定义方向分量范围和长度平方约束；`docs/游戏服与接入层/共享帧同步移动战斗核心设计.md:1480`-`1488` 明确 input payload 带 version 且版本不兼容必须明确失败）
- [x] 明确后续是否再迁移到专用 Protobuf 输入，不在第一阶段提前固化。（验证：`docs/游戏服与接入层/共享帧同步移动战斗核心设计.md:1238`-`1252` 记录第一阶段复用 `PlayerInputReq.payload_json` 的成本和边界；`docs/游戏服与接入层/共享帧同步移动战斗核心设计.md:1254`-`1260` 将新增 `SimInputPayload` Protobuf 放在第二阶段演进）
- [x] 验证项：game-server 和 mybevy 两侧 checklist 使用同一 payload 语义。（验证：`summary/游戏服共享帧同步接入_checklist.md:44`-`58` 要求定义 `sim_input` payload version、seq、commands 并解析 `move/stop/face/castSkill`；`summary/mybevy共享帧同步接入_checklist.md:88`-`101` 要求序列化为 `PlayerInputReq(action="sim_input")`，payload 包含 version、seq、commands 且字段格式与 game-server 适配层一致）

## 阶段 4：game-server lockstep_sim_demo 骨架

- 开始时间：2026-07-04 18:39:37 +08:00
- 结束时间：2026-07-04 19:34:36 +08:00
- 开发总结：`game-server` 已新增 `lockstep_sim_demo` 骨架入口并接入同仓 `packages/sim-core`。本阶段实现了 `lockstep_sim` 适配层、最小 `SimWorld`、`sim_input` JSON 校验和转换、训练目标与玩家控制绑定；`LockstepSimDemoLogic` 复用现有 room lifecycle，在 `on_game_started` 构建 world，在 `on_tick` 调用 `sim_core::step()` 推进服务端权威移动和基础战斗，并输出轻量 debug state。旧 `robot_sync_room`、`movement_demo`、`combat_demo` 入口保持可创建。
- 验证记录：主 agent 复核 worker diff 后运行 `cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim`，5 passed；运行 `cargo test --manifest-path apps/game-server/Cargo.toml robot_sync_room`，5 passed；运行 `cargo check --manifest-path apps/game-server/Cargo.toml` 通过，仅有仓库既有 warning，未启动外部服务。

- [x] 新增或确认 `lockstep_sim_demo` policy ID 和 room logic 入口。（验证：`apps/game-server/src/core/runtime/room_policy.rs:221` 定义 `RoomRuntimePolicy::lockstep_sim_demo()`，`:335` 注册 registry；`apps/game-server/src/gameroom/factory.rs:26` 创建 `LockstepSimDemoLogic`，`apps/game-server/src/gameroom/mod.rs:4`/`:15` 导出模块和类型）
- [x] 复用现有房间创建、ready、start、on_tick、输入等待和缺帧策略。（验证：`apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:52`、`:57`、`:91` 实现 `RoomLogic` 生命周期钩子；`apps/game-server/src/core/runtime/room_policy.rs:240`-`:241` 沿用 `InputWaitStrategy::Optimistic` 和 `MissingInputStrategy::Empty`）
- [x] 初始阶段不删除 `robot_sync_room`、`movement_demo` 或 `combat_demo`。（验证：`apps/game-server/src/gameroom/factory.rs:24`-`:27` 同时保留旧 demo 与新 `lockstep_sim_demo` 分支；`cargo test --manifest-path apps/game-server/Cargo.toml robot_sync_room` 通过 5 个旧入口相关测试）
- [x] 创建最小 `SimWorld`，包含玩家实体、训练目标、控制绑定和初始战斗状态。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:23` 创建 world 与 bindings，`:33` 加入训练目标，`:52` 建立基础战斗配置，`:351`/`:385` 定义玩家和训练目标实体；`minimal_world_contains_players_target_and_bindings` 测试通过）
- [x] 调用 `sim_core::step()` 完成服务端权威移动和基础战斗推进。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:86` 调用 `sim_core::step()`；`apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:91` 在 `on_tick` 推进，`lockstep_sim_demo_accepts_sim_input_and_advances_movement_and_combat` 覆盖移动和攻击训练目标）
- [x] 验证项：game-server 单元测试或构建检查能证明新 policy 可创建并推进一帧。（验证：`cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim` 5 passed，覆盖 factory 创建、start 后推进一帧、输入校验、移动和基础战斗；`cargo check --manifest-path apps/game-server/Cargo.toml` 通过）

## 阶段 5：初始快照、配置和 hash 下发

- 开始时间：2026-07-04 19:39:37 +08:00
- 结束时间：2026-07-04 20:10:10 +08:00
- 开发总结：`game-server` 的 `lockstep_sim_demo` 已通过 `RoomSnapshot.game_state` 输出可恢复的初始快照和每帧 envelope。`core/system/lockstep_sim` 新增 `SimInitialSnapshot`、`SimFrameEnvelope`、`SimHashEnvelope`、control bindings、config hash、恢复校验和 debug summary；`lockstep_sim_demo` 在 `get_serialized_state()` 下发 `initialSnapshot` 和 `lastFrame`，并支持从 `initialSnapshot` 恢复 world/bindings 后继续推进。本阶段未做 online client、观战/重连完整业务闭环或专用 proto。
- 验证记录：主 agent 复核 worker diff 后运行 `cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim`，8 passed；运行 `cargo check --manifest-path apps/game-server/Cargo.toml` 通过，仅有仓库既有 warning，未启动外部服务。

- [x] 定义或复用 `SimInitialSnapshot` / `SimSnapshot` 的下发结构。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:46` 定义 `SimInitialSnapshot` 并内嵌 `sim_core::SimSnapshot`，`:74` 定义每帧 `SimFrameEnvelope`；`apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:124` 在 `get_serialized_state()` 中生成 `initialSnapshot`）
- [x] 快照包含 schema version、room id、start frame、tick rate、config hash、rng seed、entities 和 control bindings。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:46`-`:57` 声明字段，`:154`-`:175` 创建快照并写入 config hash、rng seed、entities、control bindings；`initial_snapshot_restores_and_continues_with_same_hash` 测试断言 start frame、tick rate、rng seed 和 control binding）
- [x] 服务端能在开局、late join、重连和观战加入时下发恢复所需数据。（验证：现有 `Room::snapshot()` 在 `apps/game-server/src/core/room/mod.rs:447` 将 `logic.get_serialized_state()` 写入 `RoomSnapshot.game_state`；`join_room`、`reconnect_room`、`join_room_as_observer`、`start_game` 路径分别在 `apps/game-server/src/core/runtime/room_manager/lifecycle.rs:350`、`:521`、`:686`、`:719` 返回 room snapshot，`lockstep_sim_demo` 的 `game_state` 已包含 `initialSnapshot`）
- [x] 下行 hash 包含 frame 和稳定 hex 值，供工具和客户端对账。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:31` 定义 `SimHashEnvelope { frame, value, hex }`，`:131`-`:139` 生成 16 位稳定 hex；`:172` 和 `:196` 分别在初始快照和帧 envelope 中写入 state hash）
- [x] 下行事件包含基础战斗事件和必要 debug 摘要。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:74`-`:84` 的 `SimFrameEnvelope` 包含 `events` 和 `debugSummary`，`:179`-`:199` 从 `SimStepResult.events` 和 world/input 统计生成；`frame_envelope_contains_hash_events_and_debug_summary` 测试断言事件数量和 debug summary）
- [x] 验证项：从服务端快照恢复后继续应用后续输入，hash 与连续推进一致。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:767` 的 `initial_snapshot_restores_and_continues_with_same_hash` 和 `apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:475` 的 `lockstep_sim_demo_restores_snapshot_and_continues_with_same_hash` 均通过；`cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim` 8 passed）

## 阶段 6：服务端事件、重连和观战闭环

- 开始时间：2026-07-04 20:13:05 +08:00
- 结束时间：2026-07-04 20:35:03 +08:00
- 开发总结：`lockstep_sim_demo` 的每帧下行 envelope 已携带 `SimEvent`、输入来源摘要、hash 和 debug 计数，并在 `RoomSnapshot.game_state` 中补充面向观战/恢复读取的 `observerFrame`。服务端现在能显式区分真实输入、合成空输入和重复上一帧输入；缺帧策略下 `Empty` / `DropAfterMisses` 使用空输入，`RepeatLast` 克隆上一帧权威输入。观战加入不再触发 room logic 的玩家加入钩子，观战者不能提交输入，但可通过恢复快照读取当前 frame、lastFrame、事件和 hash；快照恢复后继续推进与连续 world hash 保持一致。
- 验证记录：主 agent 复核 worker diff 后运行 `cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim`，13 passed；运行 `cargo test --manifest-path apps/game-server/Cargo.toml observer_cannot_submit_or_generate_tick_inputs`，1 passed；运行 `cargo check --manifest-path apps/game-server/Cargo.toml` 通过，仅有仓库既有 warning，未启动外部服务。

- [x] 服务端能把 `SimEvent` 转成下行事件或 `GameMessagePush`。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:95` 的 `SimFrameEnvelope` 包含 `events`，`:202` 的 `create_frame_envelope` 从 `SimStepResult.events` 填充；`apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:235` 将 envelope 写入 `lastFrame`，`cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim` 13 passed）
- [x] 服务端能区分真实输入、合成空输入和重复上一帧输入。（验证：`apps/game-server/src/core/system/lockstep_sim/mod.rs:78` 定义 `SimFrameInputSourceSummary`，`:87` 定义 `real/synthesizedEmpty/synthesizedRepeatLast`，`:358` 生成 `inputSources`；`frame_envelope_distinguishes_real_empty_and_repeat_last_inputs` 测试通过）
- [x] 断线玩家在缺帧策略下的移动和战斗状态有明确规则。（验证：`apps/game-server/src/core/runtime/room_manager/transfer_codec.rs:332`-`:354` 明确 `Empty`/`DropAfterMisses` 合成空输入、`RepeatLast` 克隆上一帧输入；`apps/game-server/src/core/system/lockstep_sim/mod.rs:987` 和 `apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:559` 覆盖合成空输入继续移动且不重复释放技能）
- [x] 观战者不产生控制输入，但能收到快照、事件和 hash。（验证：`apps/game-server/src/core/runtime/room_manager/lifecycle.rs:104` 仅玩家触发 `on_character_join`，`:561` 的 `join_room_as_observer` 返回恢复快照；`apps/game-server/src/core/room/mod.rs:492`/`:503` 拒绝观战者输入；`apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:53`/`:136` 下发 `observerFrame`，`observer_cannot_submit_or_generate_tick_inputs` 和 `lockstep_sim_demo_observer_read_only_state_does_not_change_hash` 测试通过）
- [x] 重连恢复后 frame 衔接明确，不重复应用或跳过权威输入。（验证：`apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:261` 支持从 `initialSnapshot` 恢复，`:520` 测试恢复后继续推进与连续 world 的 `lastFrame.stateHash` 一致；`apps/game-server/src/core/system/lockstep_sim/mod.rs:1069` 验证 restore 后下一帧为 `snapshot.start_frame + 1`）
- [x] 验证项：重连或观战恢复不会破坏后续服务端 hash。（验证：`restore_then_continue_matches_continuous_world_and_snapshot_is_read_only`、`lockstep_sim_demo_restores_snapshot_and_continues_with_same_hash`、`lockstep_sim_demo_observer_read_only_state_does_not_change_hash` 均通过；`cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim` 13 passed）

## 阶段 7：lockstep-client online 模式

- 开始时间：2026-07-04 20:39:22 +08:00
- 结束时间：2026-07-04 21:26:26 +08:00
- 开发总结：`tools/lockstep-client` 已新增 `online` 模式和 dry-run 路径，支持解析本地服务地址、ticket/test-ticket、room、policy、character id 和 timeout，并复用 game-server 当前 protobuf 结构与 TCP packet header 编解码。online 路径可构造 `PlayerInputReq(action="sim_input")`，解析 `RoomSnapshot.game_state` 中的 `initialSnapshot`、`lastFrame` 和 `observerFrame.lastFrame`，用同仓 `sim-core` 恢复 `SimSnapshot` 并按服务端下发 frame/hash/events 做本地 replay。mismatch 诊断已包含首个不一致 frame、server/client hash、实体差异、事件差异和输入摘要；新增 `lockstep_demo_melee` 场景对齐服务端 `lockstep_sim_demo` 默认 player entity `1000`、skill `1` 和训练目标 `9000`，可作为真实 online 近战联调前的 dry-run / offline replay 准备。真实 online 移动/近战联调未执行，原因是按项目约定需要先由用户确认启动 Redis/NATS/PostgreSQL、auth/game 服务和相关端口。
- 验证记录：主 agent 复核 worker diff 后运行 `cargo test --manifest-path tools/lockstep-client/Cargo.toml`，19 passed；运行 `cargo check --manifest-path tools/lockstep-client/Cargo.toml` 通过；运行 `cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode online --scenario move_straight --dry-run` 通过，输出 5 个 `sim_input` packets 且未连接网络；运行 `cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode online --scenario lockstep_demo_melee --dry-run` 通过，输出 1 个 `sim_input` packet 且未连接网络；运行 `cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode offline --scenario lockstep_demo_melee` 通过，final hash `959839fddfc8c0dc`。

- [x] 扩展 `tools/lockstep-client` 支持 online 模式连接本地 MyServer。（验证：`tools/lockstep-client/src/main.rs:7` 分发 `--mode online`，`tools/lockstep-client/src/lib.rs:4` 导出 `online`，`tools/lockstep-client/src/online.rs:48` 解析 server/ticket/room/policy/timeout/dry-run，`:1321` 实现 TCP transport）
- [x] online 模式能登录或使用测试 ticket，加入 `lockstep_sim_demo` room。（验证：`tools/lockstep-client/src/online.rs:1386` 的 `drive_online_session` 依次发送 `AuthReq`、`RoomJoinReq(policy_id)`、`RoomReadyReq`、`RoomStartReq`；`:48`/`:167` 支持 `--ticket`/`--test-ticket` 和默认 `lockstep_sim_demo`）
- [x] online 模式能发送 `sim_input` 并消费服务端 frame、snapshot、hash 和事件。（验证：`tools/lockstep-client/src/online.rs:299`/`:305` 构造 `PlayerInputPlan`，`:321` 构造 `sim_input` JSON，`:591` 定义 `SimFrameEnvelope` 含 hash/events，`:1521`/`:1561` 消费 `RoomStatePush`、`FrameBundlePush` 和 `RoomSnapshot.game_state`）
- [x] online 模式本地使用同一 `sim-core` replay。（验证：`tools/lockstep-client/src/online.rs:703` 的 `OnlineReplay` 持有 `SimWorld`/`SimConfig`，`:712` 从 `SimInitialSnapshot.snapshot` 经 `sim_core::restore` 恢复，`:742` 调用 `sim_core::step` 对服务端 frame replay；`online_replay_matches_server_frame_hash_and_events` 测试通过）
- [x] mismatch 时输出首个不一致 frame、server hash、client hash、实体差异和事件差异。（验证：`tools/lockstep-client/src/online.rs:1048` 定义 `OnlineMismatchDiff`，`:1090` 的 Display 输出 frame/hash/entity/event/input 摘要；`online_replay_mismatch_reports_hash_entities_events_and_inputs` 测试通过）
- [x] 验证项：在用户确认启动依赖后，online 模式能跑通至少移动和近战场景对账。（验证：2026-07-05 主 agent 复跑最小 dev-stack，`move_straight` online room `lockstep-online-main-review-move` frame 5 hash `92d32b2541f32399`，`lockstep_demo_melee` online room `lockstep-online-main-review-melee` frame 1 hash `8634a2c5c36f789e`）

## 阶段 8：旧 demo 迁移评估

- 开始时间：2026-07-04 22:32:30 +08:00
- 结束时间：2026-07-04 22:44:07 +08:00
- 开发总结：已在共享帧同步设计文档补充旧 demo 迁移评估记录。本阶段结论是不删除旧 demo、不做业务代码迁移；`movement_demo` 保留为旧服务端权威移动和校正回归基线，`combat_demo` 保留为旧战斗 ECS、事件、NPC/timer/transfer 回归基线，`robot_sync_room` 保留为轻量输入转发和客户端协议联调样例；删除、合并或替换前必须先满足 `sim-core`、offline/online、旧协议兼容、客户端依赖和文档同步等回归条件。
- 验证记录：worker subagent 仅修改 `docs/游戏服与接入层/共享帧同步移动战斗核心设计.md`，未修改 `summary/`、未改业务代码、未启动服务；主 agent 复核文档新增 `### 阶段 8：旧 demo 迁移评估记录`、三个 demo 小节和删除/合并前置条件，`Select-String` 能定位相关条目。未运行单元测试，本阶段为文档评估。

- [x] 评估 `movement_demo` 是否迁移到 `sim-core` movement 或保留为旧样例。（验证：`docs/游戏服与接入层/共享帧同步移动战斗核心设计.md` 的 `#### movement_demo` 小节记录保留为旧服务端权威移动样例和回归基线，列明 `MovementSnapshotPush`、`MovementRecoveryState`、校正、离线停止、重连恢复、room transfer 和 f32 数据模型风险）
- [x] 评估 `combat_demo` 中 f32 坐标、范围、速度和旧 ECS 逻辑的迁移风险。（验证：同文档 `#### combat_demo` 小节记录保留旧 `RoomCombatEcs` 基线，列明 f32 坐标/range/AOE/displacement、旧事件/snapshot、NPC/timer/transfer、CSV 转换和 payload 兼容风险）
- [x] 评估 `robot_sync_room` 是否继续作为轻量输入转发样例。（验证：同文档 `#### robot_sync_room` 小节记录其保留为 `robot_move` 输入转发和客户端 replay 联调样例，并说明与 `lockstep_sim_demo` 权威模拟职责不同）
- [x] 列出旧 demo 删除、合并或保留前必须满足的回归条件。（验证：同文档 `#### 删除、合并或保留前置条件` 覆盖 `sim-core` 单测、offline scenarios、online dry-run、真实 online 对账、`lockstep_sim_demo` 测试、旧 movement/combat/robot 兼容和文档同步条件）
- [x] 本阶段不默认删除旧 demo，除非已有等价能力和明确回归结果。（验证：同文档阶段 8 结论明确“本阶段只形成处理决策，不删除旧 demo，不做业务代码迁移”，且删除前置条件末条要求没有等价能力和明确结果时默认继续保留）
- [x] 验证项：形成旧 demo 后续处理决策记录。（验证：同文档新增 `### 阶段 8：旧 demo 迁移评估记录`，包含评估日期、当前结论、三个 demo 决策、迁移风险、后续建议和前置条件）

## 阶段 9：协议、文档和测试同步

- 开始时间：2026-07-04 22:52:50 +08:00
- 结束时间：2026-07-04 23:15:03 +08:00
- 开发总结：已同步 MyServer 侧共享帧同步协议、实现状态、外部客户端接入边界和 `lockstep-client` 使用说明。共享设计文档记录 `lockstep_sim_demo` 当前实现快照和未落地范围；协议设计补充 `sim_input` payload、`RoomSnapshot.game_state` 的 `initialSnapshot` / `lastFrame` / `observerFrame`、hash、事件和 debug 字段语义；外部客户端接入说明只保留 MyServer 契约、`MYSERVER_CLIENT_ROOT` 和 mybevy checklist 入口；`tools/lockstep-client/README.md` 补齐 offline、online dry-run、真实 online movement/melee 命令和 mismatch / server rejection 诊断。
- 验证记录：主 agent 复核 worker diff，确认只修改文档和 `tools/lockstep-client/README.md`，未修改 `summary/` 或业务代码；`git diff --check -- docs/游戏服与接入层/共享帧同步移动战斗核心设计.md docs/协议与客户端/协议设计.md docs/协议与客户端/外部客户端接入说明.md tools/lockstep-client/README.md` 通过；`cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode online --scenario move_straight --dry-run` 通过，生成 5 个 `sim_input` packets 且 network 未启动；`cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode online --scenario lockstep_demo_melee --dry-run` 通过，生成 1 个 `sim_input` packet 且 network 未启动。

- [x] 更新共享帧同步设计文档，记录 MyServer 当前实现状态和仍未落地内容。（验证：`docs/游戏服与接入层/共享帧同步移动战斗核心设计.md` 新增 `4.0 2026-07-04 当前实现快照`，记录 `packages/sim-core`、`lockstep_sim_demo`、`sim_input`、snapshot/hash/event、`tools/lockstep-client` 当前能力，以及未落地的专用 Protobuf、复杂预测、NavMesh、生产 AOI、正式 UI/动画和完整 room transfer）
- [x] 更新协议设计或外部客户端接入说明，记录 `sim_input`、snapshot、hash 和事件语义。（验证：`docs/协议与客户端/协议设计.md` 的 `PlayerInputReq` / `RoomSnapshot.game_state` 章节补充 `PlayerInputReq(action="sim_input")`、version/seq/commands、move/stop/face/castSkill、`initialSnapshot`、`lastFrame`、`observerFrame`、`stateHash`、events、inputSources 和 debugSummary 语义）
- [x] 更新 `lockstep-client` 使用说明，记录 offline、online、dry-run、移动和近战对账命令。（验证：`tools/lockstep-client/README.md` 补充 offline movement/combat 场景、online dry-run 命令、真实 online `move_straight` 和 `lockstep_demo_melee` 命令、downlink semantics 和 common failures；两个 online dry-run 命令均通过）
- [x] 在外部客户端接入说明中只记录 MyServer 契约、`MYSERVER_CLIENT_ROOT` 要求和客户端清单入口，不在本清单拆解客户端功能开发。（验证：`docs/协议与客户端/外部客户端接入说明.md` 记录 `lockstep_sim_demo` 契约、`MYSERVER_CLIENT_ROOT` / `local_help.txt` 定位规则，并明确外部 `mybevy` 功能开发、UI 拆解、协议绑定改造和联调任务以 `summary/mybevy共享帧同步接入_checklist.md` 为入口维护）
- [x] 汇总本阶段不支持的长期专题，避免后续误认为已完成生产能力。（验证：共享设计文档当前实现快照和不支持范围列明复杂客户端预测、正式回滚、NavMesh、复杂物理、完整生产 AOI、正式玩法 UI / 动画、跨服 room transfer 完整共享模拟迁移和生产级多服务编排验收均未落地）
- [x] 验证项：文档中的 policy ID、payload 字段、命令、路径和不支持范围与代码一致。（验证：主 agent 用 `Select-String` 核对 `apps/game-server/src/core/system/lockstep_sim/mod.rs`、`apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs`、`tools/lockstep-client/src/online.rs` 与文档中的 `lockstep_sim_demo`、`sim_input`、`targetEntityId`、`initialSnapshot`、`lastFrame`、`observerFrame`、`stateHash`、`debugSummary`、错误码和 dry-run 命令；`git diff --check` 与两个 dry-run 命令通过）

## 阶段 10：MyServer online 联调准备

- 开始时间：2026-07-04 23:17:16 +08:00
- 结束时间：2026-07-04 23:28:03 +08:00
- 开发总结：已在 `tools/lockstep-client/README.md` 增加真实 online 对账 runbook，覆盖启动门禁、依赖服务、端口和配置来源、ticket 准备、preflight dry-run、直连 game-server 与 game-proxy TCP fallback 的真实 online 命令、失败采集清单、停止和清理方式。runbook 明确任何服务启动或真实 online replay 前，必须先向用户列出依赖、命令和影响范围并等待确认。
- 验证记录：主 agent 复核 README diff，确认只修改 `tools/lockstep-client/README.md`，未修改业务代码或 `summary/`；`git diff --check -- tools/lockstep-client/README.md` 通过；核对 `apps/port.txt`、`apps/game-proxy/.env.example`、`apps/game-proxy/src/config.rs` 和 `apps/game-proxy/src/main.rs`，确认端口、`PROXY_TCP_FALLBACK_PORT`、默认 `14000` 和 admin `7101` 来源；两个 online dry-run 命令通过且输出 `network: not started; dry-run only`。

- [x] 列出真实 online 对账需要启动的 MyServer 依赖和服务。（验证：`tools/lockstep-client/README.md` 的 `Prerequisites` 列出 Redis、PostgreSQL、Core NATS、`auth-http`、`game-server`，以及验证正式玩家入口时需要 `game-proxy`；并说明直连 `game-server:7000` + local/dev ticket 只限本地调试边界）
- [x] 确认本地端口、环境变量、测试 ticket 或登录账号准备方式。（验证：同 README `Ports and config sources` 表记录 `auth-http:3000`、`game-server:7000/7500`、`game-proxy:4000/7101`、`game-proxy` TCP fallback、Redis/PostgreSQL/NATS 端口及配置来源；`Ticket preparation` 说明真实 auth-http ticket 路径、`--test-ticket` / `--ticket` 关系和 `TICKET_SECRET` 对齐要求）
- [x] 确认 `tools/lockstep-client` 的 `move_straight` 和 `lockstep_demo_melee` online 命令参数。（验证：同 README `Preflight dry-runs`、`Real direct-to-game-server online commands` 和 `Real game-proxy path commands` 给出 `--scenario move_straight`、`--scenario lockstep_demo_melee`、`--server`、`--ticket`、`--room lockstep-online-demo`、`--policy lockstep_sim_demo`；两个 dry-run 命令本地执行通过）
- [x] 准备联调失败时需要采集的日志、hash、事件、frame 和连接信息。（验证：同 README `Failure collection` 列出 auth/game/proxy/Redis/PostgreSQL/NATS 日志、连接 endpoint、transport path、room id、policy id、ticket source、character id、service-side frame、`RoomSnapshot.game_state.initialSnapshot`、`lastFrame`、`observerFrame.lastFrame`、`stateHash.hex`、events、inputSources、debugSummary、`FrameBundlePush.inputs`、`PlayerInputRes.error_code` 和 mismatch 报告）
- [x] 明确联调后服务停止和临时数据清理方式。（验证：同 README `Stop and cleanup after a run` 要求只停止本次启动的本地进程，使用 `scripts/dev-stack.ps1` 文档化 stop path，按 room/account/character/log marker 清理可识别 Redis keys 和测试数据，保留日志与 mismatch artifacts，不执行 destructive reset 或 broad Redis flush）
- [x] 验证项：在启动任何服务前先向用户列出依赖、命令和影响范围，并等待确认。（验证：同 README `Startup gate` 明确列出将启动的服务和依赖、精确 dry-run/real online 命令、影响范围，并在启动 Redis、PostgreSQL、Core NATS、`auth-http`、`game-server`、`game-proxy` 或真实 online replay 前等待确认）

## 阶段 11：MyServer 端到端 online 对账

- 开始时间：2026-07-05 08:38:34 +08:00
- 结束时间：2026-07-05 09:41:46 +08:00
- 开发总结：已完成 MyServer 侧端到端 online 对账闭环。worker 在 `tools/lockstep-client` online 模式中加入输入发送前的可提交帧等待，避免客户端提前提交导致 `INPUT_FRAME_TOO_FAR`；新增 observer recovery probe，使用独立 observer ticket 校验 `RoomJoinAsObserverRes.snapshot.game_state`。`game-server` 的 `lockstep_sim_demo` 现在每帧在 `FrameBundlePush` 携带 `RoomSnapshot.game_state`，使在线 replay 能持续读取 `initialSnapshot`、`lastFrame`、`observerFrame.lastFrame` 和 hash。主 agent 复核 diff、运行单元测试和 cargo check 后，用临时签名 ticket 与 Redis ticket owner/version key 启动最小 dev-stack 复跑移动、近战和 observer 恢复联调；联调完成后已清理临时 Redis key 并停止 NATS / game-server。
- 验证记录：`cargo test --manifest-path tools/lockstep-client/Cargo.toml` 20 passed；`cargo check --manifest-path tools/lockstep-client/Cargo.toml` 通过；`cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim_demo_frame_bundle_carries_snapshot_every_frame` 通过；`cargo check --manifest-path apps/game-server/Cargo.toml` 通过但有既有 warning；`cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode offline --scenario move_straight` 通过，final frame 5 hash `ad9a151d0953d437`；`cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode offline --scenario lockstep_demo_melee` 通过，final frame 1 hash `959839fddfc8c0dc`；主 agent 启动 `powershell -ExecutionPolicy Bypass -File scripts\dev-stack.ps1 -NoRedis -NoAuth -NoProxy -NoAdminApi -NoAdminWeb -NoMetricsCollector -WaitTimeoutSeconds 180`，复用 Redis `127.0.0.1:6379`，启动 NATS `4222`、game-server `7000` / admin `7500`，日志目录 `logs/dev-stack`；online `move_straight` room `lockstep-online-main-review-move` policy `lockstep_sim_demo` 5 个 `sim_input`、frames checked 5、final hash `92d32b2541f32399`，observer recovery ok，`initialSnapshot` / `lastFrame` / `observerFrame.lastFrame` 均为 frame 5，observer hash `92d32b2541f32399`；online `lockstep_demo_melee` room `lockstep-online-main-review-melee` policy `lockstep_sim_demo` 1 个 `sim_input`、frames checked 1、final hash `8634a2c5c36f789e`；最后运行 `scripts\dev-stack.ps1 -Stop` 并确认 7000/7500/4222 未监听。

- [x] 在用户确认后启动必要的 MyServer 服务和依赖。（验证：用户确认后运行 `scripts\dev-stack.ps1 -NoRedis -NoAuth -NoProxy -NoAdminApi -NoAdminWeb -NoMetricsCollector -WaitTimeoutSeconds 180`，复用 Redis `127.0.0.1:6379`，启动 NATS `4222`、game-server `7000` / admin `7500`；联调后执行 `scripts\dev-stack.ps1 -Stop`）
- [x] `tools/lockstep-client online` 能进入 `lockstep_sim_demo` room，收到初始快照并恢复本地 `SimWorld`。（验证：online `move_straight` room `lockstep-online-main-review-move` policy `lockstep_sim_demo` 输出 frames checked 5 / final frame 5；`tools/lockstep-client/src/online.rs:772` 从 `initialSnapshot.snapshot` 调用 `sim_core::restore` 恢复）
- [x] `move_straight` online 场景能完成服务端 hash 与工具本地 hash 对账。（验证：`cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode online --scenario move_straight --server 127.0.0.1:7000 --room lockstep-online-main-review-move --policy lockstep_sim_demo --probe-observer-recovery --timeout-ms 10000` 通过，5 个 `sim_input`，frames checked 5，final hash `92d32b2541f32399`）
- [x] `lockstep_demo_melee` online 场景能完成基础近战事件和 hash 对账。（验证：`cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode online --scenario lockstep_demo_melee --server 127.0.0.1:7000 --room lockstep-online-main-review-melee --policy lockstep_sim_demo --timeout-ms 10000` 通过，1 个 `sim_input`，frames checked 1，final hash `8634a2c5c36f789e`；`OnlineReplay` 逐帧比较 server events 与本地 replay events）
- [x] 重连或观战恢复路径能通过 `RoomSnapshot.game_state` 读取恢复所需 frame、snapshot、lastFrame 和 hash。（验证：online observer probe 对 room `lockstep-online-main-review-move` 返回 `observer recovery: ok`，observer current/snapshot/initial/last/observerFrame.lastFrame 均为 frame 5，observer hash `92d32b2541f32399`；`tools/lockstep-client/src/online.rs:1730` 校验 `initialSnapshot`、`lastFrame`、`observerFrame.lastFrame` 和 hash）
- [x] 记录联调命令、依赖服务、端口、日志位置和结果。（验证：本阶段验证记录写明 Redis `6379`、NATS `4222`、game-server `7000` / admin `7500`、日志目录 `logs/dev-stack`、启动/停止命令、online room/policy/frame/hash 结果）
- [x] 验证项：MyServer 端到端 online 链路完成并有可复查记录。（验证：本阶段验证记录包含主 agent 复跑的 offline、unit/check、online move、online melee、observer recovery 命令和结果；`Get-NetTCPConnection` 确认 7000/7500/4222 已释放）

## 阶段 12：外部客户端交接门禁

- 开始时间：2026-07-04 23:30:03 +08:00
- 结束时间：2026-07-04 23:44:32 +08:00
- 开发总结：已在外部客户端接入说明中新增共享帧同步外部客户端交接门禁，并在共享帧同步设计文档中补充入口引用。文档明确 MyServer 只维护服务端契约、联调前置条件和验收记录引用；`arena.lockstep_sim` 场景、渲染、HUD、事件表现、mismatch 诊断、replay / rollback 缓存、客户端构建/回归、单/双客户端验收等任务均由 `summary/mybevy共享帧同步接入_checklist.md` 承接。交接门禁同时记录外部仓库授权要求、MyServer 契约清单、外部客户端联调启动确认和结果记录边界。
- 验证记录：主 agent 复核 worker diff，确认只修改 `docs/协议与客户端/外部客户端接入说明.md` 和 `docs/游戏服与接入层/共享帧同步移动战斗核心设计.md`，未修改业务代码或 `summary/`；`git diff --check -- docs/协议与客户端/外部客户端接入说明.md docs/游戏服与接入层/共享帧同步移动战斗核心设计.md` 通过；`Select-String` 核对外部接入说明包含 `mybevy共享帧同步接入_checklist.md`、`MYSERVER_CLIENT_ROOT`、`lockstep_sim_demo`、`sim_input`、`initialSnapshot`、`lastFrame`、`observerFrame`、`stateHash`、`debugSummary`、`targetEntityId`、`inputSources` 和错误码；只读核对 mybevy checklist 覆盖客户端场景、渲染、HUD、事件、mismatch、replay/rollback、构建和双客户端验收。

- [x] 确认 `summary/mybevy共享帧同步接入_checklist.md` 承接全部 `mybevy` 客户端场景、渲染、HUD、replay 缓存、客户端构建和客户端验收任务。（验证：`docs/协议与客户端/外部客户端接入说明.md` 的 `共享帧同步外部客户端交接门禁` 明确这些客户端任务均由 `summary/mybevy共享帧同步接入_checklist.md` 承接；只读核对 mybevy checklist 覆盖 `arena.lockstep_sim`、实体渲染、战斗事件、HUD/mismatch、最小 replay 缓存、客户端构建和单/双客户端验收）
- [x] 确认外部客户端修改前必须先校验 `MYSERVER_CLIENT_ROOT` 并获得用户允许。（验证：同文档 `外部仓库修改门禁` 要求先读取 `MYSERVER_CLIENT_ROOT` 或 `local_help.txt`，校验外部 `mybevy` 仓库路径，向用户列出外部路径、文件范围、计划命令和影响，并获得明确允许）
- [x] 输出给外部客户端使用的 MyServer 契约清单：policy ID、payload schema、snapshot/envelope 结构、hash/event 语义和错误处理。（验证：同文档 `MyServer 契约清单` 记录 `policy_id=lockstep_sim_demo`、`PlayerInputReq(action="sim_input")`、version/seq/commands、move/stop/face/castSkill、`targetEntityId`、`initialSnapshot`、`lastFrame`、`observerFrame.lastFrame`、`stateHash.hex`、events、inputSources、debugSummary 和常见错误码）
- [x] 外部客户端联调前，先列出需要启动的 MyServer 服务、依赖、端口、外部仓库路径和预期影响，并等待用户确认。（验证：同文档 `外部客户端联调启动门禁` 要求列出 Redis、PostgreSQL、Core NATS、`auth-http`、`game-proxy`、`game-server`、端口、外部仓库路径、命令和预期影响，并在用户确认前不启动任何服务、真实 online 或真实 mybevy 客户端）
- [x] 外部客户端联调结果只作为交接验收记录引用，不在本清单拆解客户端实现任务。（验证：同文档 `联调结果记录边界` 要求记录 MyServer/mybevy commit 或 diff、服务、端口、命令、room、policy、frame、`stateHash.hex`、events、mismatch 和日志位置，并明确这些结果只用于证明 MyServer 契约可被外部客户端消费，不在 MyServer 总控 checklist 拆客户端实现任务）
- [x] 验证项：本清单不再包含 `mybevy` 客户端功能开发条目，相关功能条目均可在 `mybevy` 清单中找到。（验证：主 agent 用 `Select-String` 核对总控 checklist 中剩余 `mybevy` 文字均为边界说明、历史验证记录或本阶段交接门禁；客户端功能开发关键字 `arena.lockstep_sim`、渲染、HUD、事件、mismatch、replay/rollback、构建、双客户端验收均可在 `summary/mybevy共享帧同步接入_checklist.md` 找到）

## 最终完成定义

以下项目作为整体完成标准，不要求每个开发阶段都执行，由所有相关阶段完成后统一验收。

- 开始时间：2026-07-05 09:41:46 +08:00
- 结束时间：2026-07-05 09:41:46 +08:00
- 验收总结：MyServer 侧总控 checklist 已完成：P0/P1/P2 的 `sim-core`、offline replay、移动和基础战斗能力可被仓库内服务端和联调工具复用；`game-server lockstep_sim_demo` 作为权威模拟入口接入同一核心；`tools/lockstep-client` online 能进入本地 game-server、恢复服务端快照、逐帧 replay 并完成移动和近战 hash / event 对账；observer 恢复路径可读取 `RoomSnapshot.game_state` 中的恢复字段。外部 `mybevy` 客户端实现继续由独立 checklist 承接。

- [x] `packages/sim-core` 可被 `game-server` 和 `tools/lockstep-client` 引用；外部客户端引用方式在 `mybevy` 清单维护。（验证：`apps/game-server/Cargo.toml:23` 与 `tools/lockstep-client/Cargo.toml:10` 均声明 `sim-core = { path = "../../packages/sim-core" }`；外部客户端任务入口见 `summary/mybevy共享帧同步接入_checklist.md`）
- [x] offline scenario 能验证移动和基础战斗 hash 一致。（验证：`cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode offline --scenario move_straight` 通过，final frame 5 hash `ad9a151d0953d437`；`--scenario lockstep_demo_melee` 通过，final frame 1 hash `959839fddfc8c0dc`）
- [x] `game-server` 的 `lockstep_sim_demo` 或等价策略能调用同一 `sim-core` 推进服务端权威状态。（验证：`apps/game-server/src/gameroom/factory.rs:26` 注册 `lockstep_sim_demo`，`apps/game-server/src/core/runtime/room_manager/tick.rs:193` 对该 policy 每帧生成 snapshot；`cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim_demo_frame_bundle_carries_snapshot_every_frame` 通过）
- [x] `tools/lockstep-client` online 能与服务端 hash 对账。（验证：主 agent 真实 online `move_straight` 和 `lockstep_demo_melee` 均通过，分别得到 final hash `92d32b2541f32399` 与 `8634a2c5c36f789e`）
- [x] 同一初始状态、配置 hash、随机 seed 和输入帧下，服务端与 online 工具得到一致 hash。（验证：`tools/lockstep-client/src/online.rs:772` 从服务端 `initialSnapshot` 恢复，`:1712` 读取服务端 `RoomSnapshot.game_state`，`:1723` 用服务端 frame/envelope 与 `FrameBundlePush.inputs` 本地 replay；真实 online 移动 frame 5 与近战 frame 1 对账无 mismatch）
- [x] 外部客户端所需 MyServer 契约、联调依赖和验收入口已同步到文档或 `mybevy` 清单。（验证：`docs/协议与客户端/外部客户端接入说明.md:126` 起的共享帧同步交接门禁记录 `mybevy` checklist、`MYSERVER_CLIENT_ROOT`、policy、`sim_input`、snapshot/hash/event 契约和联调启动门禁）
- [x] 相关文档、协议说明、联调命令和已知不支持能力已同步记录。（验证：`tools/lockstep-client/README.md:68` 起记录 dry-run/online 命令、依赖、ticket 准备、结果记录和故障排查；`docs/游戏服与接入层/共享帧同步移动战斗核心设计.md:82` 起记录当前实现、wire payload、snapshot/envelope、online 模式和不支持的目标类型）
