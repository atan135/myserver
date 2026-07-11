# mybevy 共享帧同步接入 Checklist

## 目标

在外部 `mybevy` 客户端中新增或演进 `arena.lockstep_sim` 测试场景，引用 MyServer 中同一份 `sim-core`，消费服务端权威帧输入和快照，本地 replay 移动与战斗，并展示 hash、事件和 mismatch 状态。本清单只记录 `mybevy` 与 MyServer 共享帧同步接入相关的客户端功能开发、联调和验收事项；不承接独立客户端玩法、纯表现优化或与 MyServer 协议无关的工作。实际修改外部仓库前必须确认 `MYSERVER_CLIENT_ROOT` 指向正确仓库并获得允许。

## 基础原则

- [x] 通过 `MYSERVER_CLIENT_ROOT` 定位外部客户端，不在 MyServer 文档、脚本或代码中写死本机绝对路径。（验证：阶段 1/2 已记录外部仓库定位和相对 path dependency；后续调试验证统一由 `summary/共享帧同步调试验证_checklist.md` 承接）
- [x] Bevy 只做渲染表现，`Transform`、动画、插值、UI 和特效不得反写 `SimWorld`。（验证：阶段 8/9/10 已通过只读 replay、事件表现和 HUD 诊断测试覆盖；真实画面 smoke 迁移到 `summary/共享帧同步调试验证_checklist.md`）
- [x] 客户端输入必须先量化成 `QuantizedDir` / `SimCommand`，再发送 `sim_input`。（验证：阶段 5/6 已覆盖输入量化、payload 序列化和发送 gate）
- [x] 客户端本地结果只用于预测、表现和对账，不决定永久属性、背包、职业、称号或最终战斗结算。（验证：阶段 6/7/10 已记录客户端只提交输入和本地 replay/hash 诊断）
- [x] 第一阶段优先做权威帧 replay，不默认实现激进本地预测。（验证：阶段 7/11 已记录权威帧 replay 和最小 replay 缓存不默认启用激进预测）
- [x] 只处理与 MyServer 的 policy、`sim_input`、snapshot、hash、事件、重连/观战恢复和联调验收相关的客户端开发。（验证：阶段 1-12 已覆盖客户端接入实现；后续联调验收统一迁移到 `summary/共享帧同步调试验证_checklist.md`）
- [x] 每个阶段完成后运行对应客户端构建、测试或手动验收；双客户端和真实服务联调前先列出依赖并等待用户确认。（验证：阶段 1-12 已记录测试/构建结果；阶段 13-15 的真实调试验证迁移到 `summary/共享帧同步调试验证_checklist.md`）

## 阶段 1：外部仓库和现有场景盘点

- 开始时间：2026-07-06 09:47:33 +08:00
- 结束时间：2026-07-06 10:42:03 +08:00
- 开发总结：完成外部 `mybevy` 仓库定位、现有 `arena.robot_sync` 场景与 MyServer authority 流程盘点，确认 `FrameBundlePush`、本地 replay、fixed checksum、HUD 和日志的复用基础；确认当前仅有 `authority-core` path dependency，尚未接入 `sim-core`；后续采用新增 `arena.lockstep_sim` 场景和模块、保留 `arena.robot_sync` 作为回归对照。
- 验证记录：2026-07-06 09:53:58 +08:00 只读盘点完成；未启动 MyServer 服务、未启动真实客户端、未运行联调命令。可复用模块：`project/src/game/myserver/*`、`project/src/game/authority/*`、`project/src/game/scenes/*`、`project/assets/game/scenes.csv`、`project/src/game/features/robot_sync/{plugin.rs,sync.rs,coordinates.rs,hud.rs}`、`project/src/game/screens/gameplay/robot_sync_scene.rs`、`scripts/start-robot-sync-two-clients.ps1`。需要新增模块：`sim-core` path dependency、`arena.lockstep_sim` 场景注册和专属 plugin、lockstep sim 配置、`sim_input` payload 构造、`RoomSnapshot.game_state` 解析、本地 `sim-core` replay / restore / hash 对账、lockstep HUD、事件表现、mismatch / replay 缓存与诊断日志。

- [x] 确认 `MYSERVER_CLIENT_ROOT` 指向的 `mybevy` 仓库路径。（验证：当前环境变量未设置；`local_help.txt:1` 记录 `MYSERVER_CLIENT_ROOT=C:\project\mybevy`；`git -C C:\project\mybevy rev-parse --show-toplevel` 返回 `C:/project/mybevy`）
- [x] 确认用户允许修改外部 `mybevy` 客户端仓库。（验证：用户于 2026-07-06 明确回复“确认，允许修改”；授权范围为后续阶段按本 checklist 修改 `C:\project\mybevy`，仍需避开既有无关改动）
- [x] 盘点现有 `arena.robot_sync` 场景、authority 接入、登录、进房、ready 和 start 流程。（验证：`project/assets/game/scenes.csv:3` 注册 `arena.robot_sync`；`project/src/game/scenes/robot_sync_arena.rs:14` 定义场景 ID；`project/src/game/features/robot_sync/plugin.rs:331` 进入场景后调用 authority；`project/src/game/features/robot_sync/sync.rs:1116` guest login、`:1187` join、`:1202` ready、`:1268` start）
- [x] 盘点现有 `FrameBundlePush` 消费、本地 replay、fixed 坐标 checksum、HUD 和日志实现。（验证：`project/src/game/myserver/protocol.rs:43` 定义 `FrameBundlePush=1203`；`project/src/game/myserver/plugin.rs:2943` 解码 push；`project/src/game/authority/plugin.rs:933` 转为 `AuthorityFrame`；`project/src/game/features/robot_sync/sync.rs:153` 消费 authority 事件，`:456` fixed 坐标 checksum；`project/src/game/features/robot_sync/hud.rs:28` 生成 HUD snapshot；`project/src/game/screens/gameplay/robot_sync_scene.rs:174` 更新 HUD 文本）
- [x] 盘点 `mybevy` 当前对 `authority-core` 或 MyServer packages 的 path dependency 方式。（验证：`project/Cargo.toml:12` 使用 `authority-core = { path = "../../MyServer/packages/authority-core" }`；`project/build.rs:6` 从 `../MyServer/packages/proto/game.proto` 推导协议路径；未检索到 `sim-core` / `lockstep_sim` 客户端接入）
- [x] 明确 `arena.lockstep_sim` 是新增场景还是从 `arena.robot_sync` 演进。（验证：建议新增场景和 `lockstep_sim` 模块，复用 myserver / authority / scenes / robot_sync 经验；原因是目标 policy、输入 action、snapshot / hash / events 契约与 `robot_sync_room` + `robot_move` 差异明显，保留 `arena.robot_sync` 可作为回归对照）
- [x] 验证项：输出客户端可复用模块和需要新增模块清单。（验证：本阶段 `验证记录` 已列出可复用模块和新增模块；worker 只读盘点报告完成且未修改文件）

## 阶段 2：引入 sim-core 依赖

- 开始时间：2026-07-06 10:42:03 +08:00
- 结束时间：2026-07-06 11:15:15 +08:00
- 开发总结：在外部 `mybevy/project` 引入 `sim-core` 相对 path dependency，并新增 `game::features::lockstep_sim::adapter` 作为客户端适配边界；该模块只引用纯 `sim-core` 类型和 API，未接入 Bevy 表现层或网络协议层。阶段验证通过 `cargo test lockstep_sim --lib` 和 `cargo check --lib`，仅保留既有 `checkbox` dead_code warning。
- 验证记录：2026-07-06 在 `C:\project\mybevy\project` 运行 `cargo test lockstep_sim --lib`，结果 1 passed；运行 `cargo check --lib`，结果通过。两条命令均只出现既有 `src/framework/ui/widgets/controls/selection.rs:32` 的 `checkbox` 未使用 warning。

- [x] 在 `mybevy` Cargo 配置中通过 path dependency 引入 `packages/sim-core`。（验证：`C:\project\mybevy\project\Cargo.toml` 增加 `sim-core = { path = "../../MyServer/packages/sim-core" }`，`Cargo.lock` 包含 `sim-core` package）
- [x] 路径配置优先使用相对路径或本地配置，不把 `C:\project\myserver` 写死为唯一路径。（验证：`C:\project\mybevy\project\Cargo.toml` 使用 `../../MyServer/packages/sim-core` 相对路径，未写入本机绝对路径）
- [x] 确认客户端编译时不会引入服务端专用依赖。（验证：`C:\project\mybevy\project\Cargo.lock` 中 `sim-core` 仅依赖 `serde` 和 `serde_json`；`cargo check --lib` 通过）
- [x] 新增客户端适配模块，隔离 `sim-core` 类型和 Bevy / 网络协议类型。（验证：`C:\project\mybevy\project\src\game\features\lockstep_sim\adapter.rs` 提供 `ClientSimConfig`、`ClientSimEntity`、`ClientSimInput`、`build_client_sim_world`、`step_client_sim`，未引用 Bevy `Transform`、UI 或 MyServer 网络协议类型）
- [x] 验证 `Fp`、`Vec2Fp`、`SimWorld`、`SimInput`、`step` 能在客户端构建中使用。（验证：`adapter.rs` 测试 `sim_core_types_and_step_are_available_to_client_build` 构造 `Fp` / `Vec2Fp` / `SimWorld` / `SimInput` 并调用 `step`；`cargo test lockstep_sim --lib` 通过）
- [x] 验证项：运行 `cargo check` 或项目约定客户端构建命令。（验证：`C:\project\mybevy\project` 下 `cargo check --lib` 通过；仅有既有 `checkbox` dead_code warning）

## 阶段 3：arena.lockstep_sim 场景入口

- 开始时间：2026-07-06 11:19:46 +08:00
- 结束时间：2026-07-06 11:42:29 +08:00
- 开发总结：新增 `arena.lockstep_sim` 场景入口、scene manifest / layout、`LOCKSTEP_SIM_ARENA_SCENE_ID`、`LockstepSimPlugin` 和独立 MyServer join 状态机；新入口默认使用 `lockstep_sim_demo` policy，进入场景后执行 GuestLogin、JoinRoom、SetReady、StartRoom 基础流程，退出时清理 scene / join state 并断开 authority / MyServer。旧 `arena.robot_sync` 的常量、policy 和状态机未改动，新增回归测试覆盖隔离行为。
- 验证记录：2026-07-06 在 `C:\project\mybevy\project` 运行 `cargo test lockstep_sim --lib`，13 passed；运行 `cargo test robot_sync --lib`，92 passed；运行 `cargo test lobby --lib`，23 passed；运行 `cargo check --lib` 通过。所有命令仅有既有 `src/framework/ui/widgets/controls/selection.rs:32` 的 `checkbox` 未使用 warning。

- [x] 新增 `arena.lockstep_sim` 场景 ID 或等价入口。（验证：`C:\project\mybevy\project\assets\game\scenes.csv` 新增 `arena.lockstep_sim` 行；`src/game/scenes/lockstep_sim_arena.rs` 定义 `LOCKSTEP_SIM_ARENA_SCENE_ID = "arena.lockstep_sim"`；`assets/scenes/lockstep_sim_arena/{scene.ron,layout.ron}` 存在）
- [x] 复用现有登录、连接、房间加入、ready、start 和 authority event 流程。（验证：`src/game/features/lockstep_sim/sync.rs` 实现 `start_lockstep_sim_authority`、`follow_lockstep_sim_myserver_events`，覆盖 GuestLogin、Authenticated -> JoinRoom、RoomJoined -> SetReady、ReadyChanged / RoomStatePush -> StartRoom；`cargo test lockstep_sim --lib` 相关 lifecycle 测试通过）
- [x] 将 room policy 设置为 `lockstep_sim_demo` 或服务端约定策略。（验证：`src/game/features/lockstep_sim/config.rs` 定义 `LOCKSTEP_SIM_MYSERVER_POLICY_ID = "lockstep_sim_demo"`；测试 `lockstep_sim_config_defaults_to_demo_policy` 和 `authenticated_joins_lockstep_policy_room` 通过）
- [x] 保留 `arena.robot_sync` 原有行为，不把旧场景改坏。（验证：`arena.robot_sync` catalog 行和 `robot_sync_room` 配置未改为 lockstep；测试 `robot_sync_scene_does_not_activate_lockstep_sim`、`robot_sync_catalog_entry_stays_separate_from_lockstep_sim` 通过；`cargo test robot_sync --lib` 92 passed）
- [x] 增加场景切换、退出和资源清理逻辑。（验证：`src/game/screens/lobby/game_list.rs` 新增 `LockstepSimArenaPlayButton` 切换到 `LOCKSTEP_SIM_ARENA_SCENE_ID`；`src/game/screens/lobby/mod.rs` 将新场景路由到现有 RobotSync HUD；`src/game/features/lockstep_sim/plugin.rs` 在 `SceneEvent::Exited` 时 cleanup 并发送 `AuthorityCommand::Leave` / `MyServerCommand::Disconnect`；相关 lobby 和 cleanup 测试通过）
- [x] 验证项：客户端能进入新场景并完成基础连接流程，或在无服务端时给出明确错误。（验证：单元测试覆盖进入场景发送 `AuthorityCommand::Join` 和 `MyServerCommand::GuestLogin`、认证后 join、ready 后 start；`sync.rs` 对 `ConnectionFailed`、`Disconnected`、`AuthFailed`、join / ready / start reject 均有明确 warn/error 日志路径；未启动真实服务联调）

## 阶段 4：初始快照解析和本地 SimWorld 构建

- 开始时间：2026-07-06 11:46:18 +08:00
- 结束时间：2026-07-06 12:25:42 +08:00
- 开发总结：新增 `lockstep_sim::snapshot` 解析模块，可从 `RoomSnapshot.game_state.initialSnapshot` 恢复 `sim-core::SimWorld`，校验 schema、tick rate、config version、config hash、sim schema、frame、rng seed、state hash、实体列表和 control bindings；`LockstepSimSceneState` 保存解析成功的初始快照或稳定错误，`sync.rs` 在 `RoomStatePush` 中做最小解析 wiring。审核中修正了 `configVersion` 契约：客户端只拒绝 0，允许服务端配置表版本等任意非 0 元数据，并继续用 `lockstep_sim_demo.fixed_v1` 镜像配置校验 hash。
- 验证记录：2026-07-06 在 `C:\project\mybevy\project` 运行 `cargo test lockstep_sim --lib`，21 passed；运行 `cargo check --lib` 通过。两条命令均只出现既有 `src/framework/ui/widgets/controls/selection.rs:32` 的 `checkbox` 未使用 warning。

- [x] 解析服务端下发的 `SimInitialSnapshot` 或等价快照 payload。（验证：`C:\project\mybevy\project\src\game\features\lockstep_sim\snapshot.rs` 的 `parse_initial_snapshot_from_game_state` 解析 `initialSnapshot`；`sync.rs` 在 `RoomStatePush` 中调用解析）
- [x] 构建本地 `SimWorld`，包含 schema version、start frame、rng seed 和实体列表。（验证：`snapshot.rs` 通过 `sim_core::restore()` 恢复 `SimWorld`，并在 `ParsedInitialSnapshot` 保存 `start_frame`、`sim_schema_version`、`rng_seed`、`entities` 和 `world`）
- [x] 解析 control bindings，建立本地角色到 entity 的控制映射。（验证：`snapshot.rs` 的 `restore_control_bindings` 生成 `HashMap<String, EntityId>`，并校验空角色、重复角色、重复 entity、不存在 entity 和 owner mismatch）
- [x] 解析 tick rate、config hash、config version 和 movement / combat 配置。（验证：`ParsedInitialSnapshot` 保存 `tick_rate`、`config_version`、`config_hash` 和 `config`；`client_demo_sim_config` 镜像服务端 `lockstep_sim_demo.fixed_v1` movement / combat 配置；测试 `accepts_non_zero_config_version_as_room_metadata` 覆盖非 0 版本）
- [x] 确认服务端恢复快照和客户端本地 snapshot 的 frame 衔接规则一致。（验证：`snapshot.rs` 校验 `startFrame == restored world.frame`；测试 `rejects_start_frame_world_frame_mismatch` 覆盖不连续错误）
- [x] 对 schema version 或 config hash 不匹配给出明确错误或禁止开始 replay。（验证：`LockstepSimSnapshotError` 覆盖 unsupported schema/version、config hash mismatch、sim schema mismatch、frame/rng/hash/entity/control binding 错误；`sync.rs` 将错误记录到 `initial_snapshot_error` 并输出稳定 error code）
- [x] 增加快照解析测试或 fixture。（验证：`snapshot.rs` 测试用本地 `SimWorld + sim-core snapshot + game_state JSON` fixture 覆盖成功解析、schemaVersion 错误、configHash 错误、control binding 错误、startFrame 错误、configVersion 0 和 configVersion 2）
- [x] 验证项：同一快照多次构建本地 world 得到相同初始 hash。（验证：测试 `restoring_same_snapshot_repeats_initial_hash` 覆盖同一 `game_state` 两次恢复后 `initial_hash()` 一致，`cargo test lockstep_sim --lib` 通过）

## 阶段 5：客户端输入量化

- 开始时间：2026-07-06 12:29:07 +08:00
- 结束时间：2026-07-06 12:46:20 +08:00
- 开发总结：新增 `lockstep_sim::input` 纯输入边界，支持 move、stop、face、castSkill 和 noop 意图，提供键盘 / bot 整数方向、物理轴方向和已量化 raw 方向到 `QuantizedDir` 的转换；新增 `LockstepSimInputSeq` 单调 seq resource，并将 `ClientSimInput` 扩展为携带通用 `SimCommand`。本阶段未做 `sim_input` payload 序列化或网络发送。
- 验证记录：2026-07-06 在 `C:\project\mybevy\project` 运行 `cargo test lockstep_sim --lib`，28 passed；运行 `cargo check --lib` 通过。两条命令均只出现既有 `src/framework/ui/widgets/controls/selection.rs:32` 的 `checkbox` 未使用 warning。

- [x] 将键盘或 bot 输入转换为 `move`、`stop`、`face`、`castSkill` 意图。（验证：`C:\project\mybevy\project\src\game\features\lockstep_sim\input.rs` 定义 `LockstepSimInputIntent::{Move, Stop, Face, CastSkill, Noop}`，并通过 `into_sim_command` / `into_sim_input` 转为 `sim-core` 命令）
- [x] 如支持摇杆或鼠标方向，进入 `sim-core` 前量化为 `-1000..=1000` 的整数方向。（验证：`input.rs` 提供 `quantize_axis_dir`、`quantize_keyboard_axis` 和 `quantize_raw_dir`；测试 `quantizes_horizontal_vertical_and_diagonal_axes` 覆盖 1000 / 707 量化）
- [x] 校验量化方向长度平方不超过 `1000 * 1000`。（验证：所有方向最终经 `QuantizedDir::new` 校验；测试 `invalid_axes_return_errors` 覆盖 `(1000, 1000)` 的 `LengthSquaredTooLarge`）
- [x] 生成单调 seq，避免同一 frame 同一角色输入排序不稳定。（验证：`LockstepSimInputSeq::next()` 单调递增；测试 `seq_state_generates_monotonic_values_for_same_frame` 覆盖同 frame seq `0,1,2`）
- [x] 禁止客户端生成命中、伤害、治疗或 Buff 结果字段。（验证：`LockstepSimInputIntent` 仅表达 `Move`、`Stop`、`Face`、`CastSkill`、`Noop`，`CastSkill` 只包含 `skill_id` 和 target 意图；`input.rs` 未定义伤害、治疗、Buff、死亡、stateHash 或最终坐标字段）
- [x] 增加输入量化测试，覆盖水平、垂直、对角线、停止和非法方向。（验证：`input.rs` 测试覆盖水平 / 垂直 / 对角线量化、stop 非零 move 拒绝、face、castSkill、非 finite、零方向、越界和长度超限）
- [x] 验证项：同一物理输入在不同帧率渲染下产生一致 `SimCommand`。（验证：测试 `same_physical_axis_quantizes_independent_of_render_delta` 对 30 / 60 / 144 FPS render delta 得到同一 `SimCommand::Move(UP_RIGHT)`；`cargo test lockstep_sim --lib` 通过）

## 阶段 6：sim_input 上行发送

- 开始时间：2026-07-06 12:52:24 +08:00
- 结束时间：2026-07-06 13:26:35 +08:00
- 开发总结：新增 `lockstep_sim::payload` 纯上行 payload builder 和最小键盘发送系统，将本地 `SimCommand` 严格序列化为 MyServer `sim_input` JSON，并通过 `AuthorityCommand::SendInput` 接入既有 `PlayerInputReq` 链路；发送前会检查场景激活、初始快照、snapshot 错误、本地玩家 control binding 和 sim schema，拒绝无控制实体、缺快照或契约不匹配状态；当前运行时输入先支持 move / stop，face / castSkill 的 wire builder 与测试已覆盖，后续目标选择阶段再接入实际按键或 UI。
- 验证记录：2026-07-06 在 `C:\project\mybevy\project` 运行 `cargo test lockstep_sim --lib`，39 passed；运行 `cargo check --lib` 通过；运行 `cargo test robot_sync --lib`，92 passed。三条命令均只出现既有 `src/framework/ui/widgets/controls/selection.rs:32` 的 `checkbox` 未使用 warning。未启动 MyServer 服务或真实双端联调；本阶段通过客户端 JSON 形状测试和服务端 `RawSimInputPayload` / `RawSimCommand` 静态契约对照验证字段大小写、类型和禁止未知字段。

- [x] 将本地 `SimCommand` 序列化为 `PlayerInputReq(action="sim_input")` payload。（验证：`C:\project\mybevy\project\src\game\features\lockstep_sim\payload.rs:226` 构建 `LockstepSimInputEnvelope`，`:157` 转为 `AuthorityCommand::SendInput { action: SIM_INPUT_ACTION }`；`authority/plugin.rs:691` 在 MyServer endpoint 下转为 `MyServerCommand::SendPlayerInput`，`myserver/plugin.rs:622` 写入 `pb::PlayerInputReq`）
- [x] payload 包含 version、seq 和 commands。（验证：`payload.rs:13` / `:14` 定义 `sim_input` 与 version 1；`:263` 序列化 `SimInputPayload { version, seq, commands }`；测试 `payload.rs:463` 断言 JSON 顶层为 `version`、`seq`、`commands`）
- [x] 支持 move、stop、face、castSkill 的字段格式与 game-server 适配层一致。（验证：`payload.rs:332` 的 `WireSimCommand` 使用 `type` tag、`dirX`、`dirY`、`skillId`、`targetEntityId`；测试 `payload.rs:463` 覆盖 move / stop / face / castSkill 完整 JSON；服务端 `apps/game-server/src/core/system/lockstep_sim/mod.rs:953` 使用同名 `RawSimCommand`）
- [x] 对本地无控制实体、未收到初始快照、版本不匹配等状态禁止发送输入。（验证：`payload.rs:163` 的 `gate_lockstep_sim_input` 检查 active、snapshot error、initial snapshot、local player、control binding、config version/hash 和 sim schema；测试 `payload.rs:666` / `:694` 覆盖缺快照、snapshot error、缺 binding、version/hash/schema mismatch；运行时 `plugin.rs:74` 发送前调用 gate）
- [x] 记录发送日志，包含 frame、seq、command type 和量化方向或 target。（验证：`payload.rs:279` 的 `log_sim_input_send` 对 move / face 输出 frame、seq、command_type、dir_x、dir_y，对 castSkill 输出 skill_id 和 target_entity_id；`plugin.rs:121` 发送前调用）
- [x] 增加 payload 序列化测试或快照测试。（验证：`payload.rs:463`、`:526`、`:537`、`:559`、`:593`、`:627` 覆盖字段格式、可选字段省略、禁止结果字段、服务端限制、unsupported target 和 JSON 契约；`cargo test lockstep_sim --lib` 39 passed）
- [x] 验证项：game-server 能解析客户端发出的 payload，字段大小写和类型一致。（验证：客户端 `payload.rs:627` 检查 `dirX` / `dirY` camelCase 且无 `dir_x`；服务端 `mod.rs:945` / `:953` 对 payload 和 command 使用 `deny_unknown_fields`，`:956` / `:965` / `:973` 要求同名字段，`:994` 校验 move speed、skillId 和 targetEntityId；未启动真实服务联调）

## 阶段 7：权威帧 replay

- 开始时间：2026-07-06 13:31:06 +08:00
- 结束时间：2026-07-06 14:17:18 +08:00
- 开发总结：新增 `lockstep_sim::replay` 权威帧 replay 状态与系统，在 `arena.lockstep_sim` 激活且已有初始快照后消费 `AuthorityEvent::FrameApplied`，解析 `sim_input` payload 为 `SimInput` 并用 `sim_core::step()` 顺序推进本地 world；记录每帧 local hash、可从 `RoomSnapshot.game_state` 提取的 server hash 和事件数，并对重复/乱序、缺帧、world frame 不连续、payload 错误和 step 错误给出明确状态。由于 `FrameBundlePush.FrameInput` proto 不携带 `is_synthetic`，客户端当前按下发 action/payload replay，空 action/非 `sim_input` 不生成命令；该行为与当前 `sim-core` 对 `Noop`/空输入不参与 movement/cast 选择和 hash 的语义一致。
- 验证记录：2026-07-06 在 `C:\project\mybevy\project` 运行 `cargo test lockstep_sim --lib`，47 passed；运行 `cargo test lockstep_sim::replay --lib`，8 passed；运行 `cargo check --lib` 通过；运行 `cargo test robot_sync --lib`，92 passed。所有命令均只出现既有 `src/framework/ui/widgets/controls/selection.rs:32` 的 `checkbox` 未使用 warning。未启动 MyServer 服务或真实联调；server hash 解析基于 `packages/proto/game.proto` 的 `FrameBundlePush.snapshot` 和服务端 `lockstep_sim_demo` 的 `lastFrame.stateHash` / `observerFrame.stateHash` / `lastStateHash` JSON 结构做静态契约对照。

- [x] 消费服务端 `FrameBundlePush` 或等价权威帧输入。（验证：`C:\project\mybevy\project\src\game\authority\plugin.rs:933` 将 MyServer `FrameBundlePush` 转为 `AuthorityEvent::FrameApplied`；`src\game\features\lockstep_sim\replay.rs:199` 的 `apply_lockstep_sim_authority_events` 消费该事件；`plugin.rs:64` 注册 `LockstepSimReplayState`，`:70` 将 replay 系统加入 Update 链）
- [x] 将每帧输入转成 `SimInput` 列表。（验证：`replay.rs:318` `sim_inputs_from_frame` 遍历 `AuthorityFrame.inputs`，`:330` 只解析 `action == SIM_INPUT_ACTION` 且有 control binding 的输入，`:369` 解析 version/seq/commands，`:406` / `:414` 使用 `deny_unknown_fields`，`:430` 将 move/stop/face/castSkill 转为 `SimCommand`）
- [x] 按 frame 顺序调用 `sim_core::step()` 推进本地 world。（验证：`replay.rs:156` 从 `ParsedInitialSnapshot` 初始化 world/config 和 start frame；`:241` 检查 frame 大于上一帧，`:253` 要求下一帧连续，`:288` 调用 `step(world, FrameId::new(frame.frame_id), &sim_inputs, config)`；测试 `replay.rs:577` 覆盖 move replay 与 offline step hash 一致）
- [x] 处理缺帧、重复帧、乱序帧和本地 world frame 不连续错误。（验证：`replay.rs:241` 对重复/旧帧计数并忽略，`:257` 返回 `MissingFrame`，`:272` 返回 `WorldFrameDiscontinuous`；测试 `replay.rs:660` 覆盖重复/跳帧，`:683` 覆盖 world frame 不连续）
- [x] 保存每帧 local hash 和服务端 hash。（验证：`replay.rs:38` 定义 `LockstepSimFrameHash`，`:175` `record_hash` 保存 local hash、server hash 和 event_count，`:500` 从 `observerFrame.lastFrame` / `lastFrame` / `observerFrame.stateHash` / `lastStateHash` 解析 server hash；测试 `replay.rs:701` 覆盖 local/server hash 记录，`:746` 覆盖 hash fallback）
- [x] 增加 replay 测试或 fixture，覆盖移动和基础技能。（验证：`replay.rs:577` 覆盖 move 输入 replay，`:620` 覆盖 castSkill 造成 2 个事件并更新目标 HP，`:651` 覆盖拒绝 result 字段，`:816` 覆盖系统消费 authority frame；`cargo test lockstep_sim::replay --lib` 8 passed）
- [x] 验证项：同一输入帧序列下，客户端 replay hash 与 offline scenario hash 一致。（验证：`replay.rs:577` 中同一初始 snapshot 和 move 输入分别经 replay 与直接 `sim_core::step()` 推进，断言 replay world 与 offline world 相等且 `hash_history.local_hash == offline_result.state_hash`；`cargo test lockstep_sim --lib` 47 passed）

## 阶段 8：实体渲染和表现同步

- 开始时间：2026-07-06 14:20:30 +08:00
- 结束时间：2026-07-06 14:53:19 +08:00
- 开发总结：新增 `lockstep_sim::visual` 表现同步模块，按 `SimWorld` 实体建立稳定的 Bevy entity 映射，使用 KayKit GLB 区分本地玩家、远端玩家、训练目标和敌方实体，并同步 fixed 坐标、朝向、移动状态、相机跟随标记和 debug entries。视觉系统只读 `LockstepSimReplayState.world`，进入新场景时清空映射，退出或无 replay world 时清理表现实体，不将 Bevy `Transform`、动画或 render tick 状态反写模拟。
- 验证记录：2026-07-06 在 `C:\project\mybevy\project` 运行 `cargo test lockstep_sim --lib`，53 passed；运行 `cargo check --lib` 通过；运行 `cargo test robot_sync --lib`，92 passed；运行 `git diff --check -- project/src/game/features/lockstep_sim/mod.rs project/src/game/features/lockstep_sim/plugin.rs project/src/game/features/lockstep_sim/visual.rs` 通过。所有 cargo 命令仅有既有 `src/framework/ui/widgets/controls/selection.rs:32` 的 `checkbox` 未使用 warning。未启动 MyServer 服务或真实客户端联调。

- [x] 将 `SimWorld` 中实体映射到 Bevy entity 或现有表现实体。（验证：`C:\project\mybevy\project\src\game\features\lockstep_sim\visual.rs:30` 的 `LockstepSimVisualState.entity_visuals` 保存 `EntityId -> Entity` 映射，`:100` 的 `sync_lockstep_sim_entity_visuals` 根据 replay world 新增、更新、移除表现实体，测试 `lockstep_sim_visuals_spawn_local_remote_and_training_entities` 通过）
- [x] 使用 `Fp::to_f32_for_render()` 单向转换位置到 `Transform.translation`。（验证：`visual.rs:343` 的 `lockstep_sim_entity_translation` 用 `to_f32_for_render()` 将 sim x/y 映射到 Bevy X/Z，测试 `lockstep_sim_visuals_convert_fixed_position_to_render_transform` 通过）
- [x] 显示本地角色、远端角色、训练假人或敌方实体。（验证：`visual.rs:353` 的 `visual_role_for_entity` 区分 `LocalPlayer`、`RemotePlayer`、`TrainingTarget`、`EnemyEntity`，`:389` 按角色选择 GLB；测试 `lockstep_sim_visuals_spawn_local_remote_and_training_entities` 断言本地、远端和训练目标资产）
- [x] 展示朝向、移动轨迹或基础移动状态。（验证：`visual.rs:278` 的 `visual_snapshot` 保存 `movement_mode`、`moving`、`facing_dir`、`move_dir`，`:350` 根据 fixed 方向更新旋转；测试 `lockstep_sim_visuals_record_facing_and_movement_state` 通过）
- [x] 保证 render tick、动画、插值和物理组件不会反写 `SimWorld`。（验证：`visual.rs:105` 只以 `Res<LockstepSimReplayState>` 读取 world，未取得 `ResMut<LockstepSimReplayState>`；测试 `lockstep_sim_visual_sync_does_not_mutate_replay_world` 对比 `hash_world` 和 raw 坐标不变）
- [x] 增加可观察日志或 debug overlay，显示 fixed raw 坐标。（验证：`visual.rs:143` 写入 `debug_entries`，`:409` 的 `build_visual_debug_entries` 记录 frame、raw_x/raw_y、render_x/render_z、movement/facing；测试 `lockstep_sim_visuals_convert_fixed_position_to_render_transform` 断言 raw 与 render 坐标）
- [x] 验证项：同一 frame 的渲染位置来自 `SimWorld`，不由 Bevy delta time 推进。（验证：`visual.rs:818` 的 `same_frame_visual_position_comes_from_sim_world_not_render_tick` 手动篡改 `Transform.translation` 后连续 update，位置被重置为同一 `SimWorld` fixed 坐标且 replay frame 保持 7；`cargo test lockstep_sim --lib` 53 passed）

## 阶段 9：战斗事件表现

- 开始时间：2026-07-06 14:57:25 +08:00
- 结束时间：2026-07-06 16:25:19 +08:00
- 开发总结：新增 `lockstep_sim::combat_events` 战斗事件表现模块，replay 成功 step 后保存每帧 `SimEvent` 历史，表现层按 frame backlog 只读消费事件并生成稳定的 display entries；运行时进一步将新增 entries 同步为带 `Sprite`、`Transform`、`SceneOwned` 和 `LockstepSimCombatEventVisual` 的轻量 Bevy 表现实体，覆盖技能释放、命中 marker、伤害/治疗数字 marker、Buff apply/tick/expired、死亡状态和技能范围预览。审核中打回两轮：先修正只消费 latest frame 导致多帧 backlog 事件丢失的问题，再补齐实际可见 marker 和场景清理逻辑。
- 验证记录：2026-07-06 在 `C:\project\mybevy\project` 运行 `cargo test lockstep_sim --lib`，63 passed；运行 `cargo check --lib` 通过；运行 `cargo test robot_sync --lib`，92 passed；运行 `git diff --check -- project/src/game/features/lockstep_sim/combat_events.rs project/src/game/features/lockstep_sim/mod.rs project/src/game/features/lockstep_sim/plugin.rs project/src/game/features/lockstep_sim/replay.rs` 通过。所有 cargo 命令仅有既有 `src/framework/ui/widgets/controls/selection.rs:32` 的 `checkbox` 未使用 warning。未启动 MyServer 服务或真实客户端联调。

- [x] 解析并展示 `SkillCast`、`DamageApplied`、`HealApplied`、`BuffApplied`、`BuffTick`、`BuffExpired`、`EntityDied`。（验证：`C:\project\mybevy\project\src\game\features\lockstep_sim\replay.rs:34` 保存 `event_history`；`combat_events.rs:207` 的 `entries_from_sim_event` 分支覆盖七类 `SimEvent`；`:141` 的 `sync_lockstep_sim_combat_event_visuals` 将 entries 同步为 `Sprite` 表现实体；测试 `combat_event_entries_cover_all_sim_event_kinds_in_order` 和 `combat_event_visuals_spawn_markers_with_labels_and_range_preview` 通过）
- [x] 为技能释放、命中、伤害数字、治疗数字和死亡状态提供基础表现。（验证：`combat_events.rs:47` 定义 `SkillCast`、`Hit`、`DamageNumber`、`HealNumber`、`DeathState` 表现类型；`:245` / `:261` 为伤害生成 hit 与 `-{value}` 数字，`:286` 为治疗生成 `+{value}`，`:367` 生成死亡状态；`:463` 使用 `Sprite::from_color` 生成可见 marker；测试 `combat_event_visuals_spawn_markers_with_labels_and_range_preview` 断言 Skill、伤害、治疗和 Dead marker）
- [x] 展示技能范围预览或目标选择反馈。（验证：`combat_events.rs:416` 的 `skill_range_preview` 为 `SkillCast` 生成 center/radius/target，`:175` 为 range preview 额外生成 visual entity 并用 `Transform.scale` 表示半径；测试 `combat_event_visuals_spawn_markers_with_labels_and_range_preview` 断言存在 range preview marker）
- [x] 事件表现只读消费 `SimEvent`，不改变本地模拟状态。（验证：`combat_events.rs:90` 的事件更新系统只以 `Res<LockstepSimReplayState>` 读取 replay world 和 event history，`:141` 的 visual 同步只读 `LockstepSimCombatEventState.entries`；测试 `combat_event_display_does_not_mutate_replay_world` 对比 replay world 未变化）
- [x] 处理同一 frame 多事件的稳定显示顺序。（验证：`combat_events.rs:113` 遍历 `event_history` 中所有未消费 frame，`:120` 以同 frame `enumerate()` 记录 `order_index` 并保留 sim-core 事件顺序；测试 `combat_event_system_consumes_backlog_frames_in_stable_order_without_duplicates` 和 `combat_event_visuals_keep_same_frame_order_fields_stable` 通过）
- [x] 增加事件展示 fixture 或手动验收场景。（验证：`combat_events.rs:765`、`:805`、`:878` 新增 `melee_hit`、`aoe_hit`、`buff_dot` 具名 fixture 测试；`:984`、`:1072`、`:1105` 覆盖 marker 生成、inactive 清理和 visual 顺序）
- [x] 验证项：`melee_hit`、`aoe_hit`、`buff_dot` 等场景能显示对应事件。（验证：测试 `combat_event_fixture_melee_hit_displays_skill_hit_and_damage_number` 覆盖 SkillCast/Hit/DamageNumber，`combat_event_fixture_aoe_hit_displays_multiple_targets_in_stable_order` 覆盖多目标稳定顺序，`combat_event_fixture_buff_dot_displays_apply_tick_damage_and_expire` 覆盖 BuffApplied/BuffTick/DamageNumber/BuffExpired；`cargo test lockstep_sim --lib` 63 passed）

## 阶段 10：HUD 和 mismatch 诊断

- 开始时间：2026-07-06 16:35:24 +08:00
- 结束时间：2026-07-06 17:14:26 +08:00
- 开发总结：新增 `lockstep_sim::hud` 和 `lockstep_sim::diagnostics`，将 replay 的 local/server hash、事件数、实体数、tick/fps、mismatch 状态和首个 mismatch 摘要格式化到 HUD；`robot_sync_scene` 在 lockstep 场景 active 时切换为 lockstep HUD，否则保持旧 robot_sync HUD。replay 每帧记录 hash match 状态，server hash 缺失显示 `no-server-hash` 不误报 mismatch；首个 mismatch 保存 frame、本地/服务端 hash 与只读实体摘要。新增 `LOCKSTEP_SIM_DEBUG_DIAGNOSTICS` 开关，默认关闭正常帧 debug 日志，mismatch 仍输出 warn 诊断。审核中修复了两个非必要格式化噪声文件，并补充 HUD 切换测试。
- 验证记录：2026-07-06 在 `C:\project\mybevy\project` 运行 `cargo test lockstep_sim --lib`，70 passed；运行 `cargo check --lib` 通过；运行 `cargo test robot_sync --lib`，94 passed；运行 `git diff --check -- project/src/game/features/lockstep_sim/diagnostics.rs project/src/game/features/lockstep_sim/hud.rs project/src/game/features/lockstep_sim/config.rs project/src/game/features/lockstep_sim/mod.rs project/src/game/features/lockstep_sim/plugin.rs project/src/game/features/lockstep_sim/replay.rs project/src/game/features/lockstep_sim/state.rs project/src/game/screens/gameplay/robot_sync_scene.rs` 通过。所有 cargo 命令仅有既有 `src/framework/ui/widgets/controls/selection.rs:32` 的 `checkbox` 未使用 warning。未启动 MyServer 服务或真实客户端联调。

- [x] HUD 展示 room、policy、player、frame、fps、entities 和 events。（验证：`C:\project\mybevy\project\src\game\features\lockstep_sim\hud.rs:11` 定义 `LockstepSimHudSnapshot` 字段，`:27` 的 `lockstep_sim_hud_snapshot` 汇总 room/policy/player/frame/fps/entities/events，`:92` 格式化 HUD；`robot_sync_scene.rs:204` 在 lockstep active 时切换到 lockstep HUD；测试 `hud_status_uses_lockstep_snapshot_when_lockstep_scene_is_active` 通过）
- [x] HUD 展示 local hash、server hash、mismatch 状态和 rollback 次数。（验证：`hud.rs:21` 至 `:25` 定义 hash/mismatch/rollback 字段，`:73` 至 `:86` 从 replay hash history 和 diagnostics 生成字段，`:92` 输出到 HUD；测试 `lockstep_hud_formats_hash_and_mismatch_fields`、`lockstep_hud_snapshot_formats_matching_server_hash` 和 `lockstep_hud_snapshot_reports_no_server_hash_without_mismatch` 通过）
- [x] mismatch 时记录首个不一致 frame、双方 hash 和关键实体摘要。（验证：`diagnostics.rs:8` 定义 `LockstepSimDiagnosticsState`，`:20` 的 `record_frame` 在首次 mismatch 时保存 `LockstepSimMismatchDiagnostic`，`:104` 的 `summarize_world_entities` 输出 entity id/kind/owner/fixed pos/hp/alive；测试 `diagnostics_records_first_mismatch_with_entity_summary` 和 `replay_records_first_hash_mismatch_for_hud_and_diagnostics` 通过）
- [x] 日志输出 frame applied、hash、事件数量和差异摘要。（验证：`replay.rs:344` mismatch 时 `warn!` 输出 frame、local_hash、server_hash、hash_status、event_count、diff_summary；`:354` 在 debug 开关启用时输出正常 frame applied 同等字段）
- [x] 避免 HUD 或日志状态进入 `SimWorld` 或 hash 输入。（验证：`hud.rs:27` 只读 `LockstepSimReplayState` 生成 snapshot；`diagnostics.rs:20` 的 `record_frame` 只接收 `&SimWorld` 并生成字符串摘要；`replay.rs:319` 在 `step()` 和 `record_hash` 后记录 diagnostics，未改变 world 或 hash 输入；相关 diagnostics / replay 测试通过）
- [x] 增加 debug 开关，避免正常运行时日志过量。（验证：`config.rs:25` 增加 `debug_diagnostics`，`:69` 读取 `LOCKSTEP_SIM_DEBUG_DIAGNOSTICS`，`replay.rs:228` 同步到 replay state，`:354` 仅在开关开启时输出正常 frame debug；测试 `lockstep_sim_config_reads_debug_diagnostics_switch` 通过）
- [x] 验证项：人为制造 hash 不一致时，客户端能清楚显示和记录 mismatch。（验证：`replay.rs:895` 的 `replay_records_first_hash_mismatch_for_hud_and_diagnostics` 构造 server hash 不一致并断言 mismatch frame/hash/entity summary；`hud.rs:181` 的 `lockstep_hud_formats_hash_and_mismatch_fields` 断言 HUD 包含 `mismatch=mismatch`、双方 hash 和 `first_mismatch frame=7`）

## 阶段 11：最小 replay 缓存

- 开始时间：2026-07-06 17:17:48 +08:00
- 结束时间：2026-07-06 18:06:49 +08:00
- 开发总结：在 `lockstep_sim::replay` 中新增最小 replay 缓存，成功 step 后保存权威 `SimInput`、输入统计、hash 历史、初始和每 10 帧 `SimWorld` snapshot，并提供只读内部接口从最近 snapshot 克隆恢复、按缓存权威输入重放到目标 frame；新增 mismatch coverage 诊断，能报告目标帧 hash、输入、连续 replay 输入和可用 snapshot 情况。缓存能力仅作为诊断和手动恢复 API 暴露，未接入默认本地预测或自动 rollback。
- 验证记录：2026-07-06 在 `C:\project\mybevy\project` 运行 `cargo test lockstep_sim --lib`，75 passed；运行 `cargo check --lib` 通过；运行 `cargo test robot_sync --lib`，94 passed；运行 `git diff --check -- project/src/game/features/lockstep_sim/replay.rs` 通过。所有 cargo 命令仅有既有 `src/framework/ui/widgets/controls/selection.rs:32` 的 `checkbox` 未使用 warning。未启动 MyServer 服务或真实客户端联调。

- [x] 保存最近 N 帧权威输入。（验证：`C:\project\mybevy\project\src\game\features\lockstep_sim\replay.rs:37` 增加 `input_history`，`:48` 定义 `LockstepSimFrameInputs`，`:263` 的 `record_inputs` 保存解析后的 `Vec<SimInput>` 和 raw/action/command 计数，`:511` 在 step 成功后记录；测试 `replay_records_authoritative_inputs_hashes_and_periodic_snapshots` 和 `replay_cache_enforces_history_limits_and_snapshot_interval` 通过）
- [x] 保存最近 N 帧 local hash 和 server hash。（验证：保留并继续使用 `replay.rs` 的 `hash_history` / `record_hash`，`:511` 至 `:514` 在记录输入后继续记录 local/server hash；测试 `replay_records_authoritative_inputs_hashes_and_periodic_snapshots` 断言 hash 历史与当前 world hash 一致）
- [x] 每 10 或 20 帧保存一次 `SimWorld` snapshot。（验证：`replay.rs:26` 定义 `REPLAY_WORLD_SNAPSHOT_INTERVAL = 10`，`:40` 增加 `world_snapshots`，`:71` 定义 `LockstepSimWorldSnapshot`，`:307` 的 `record_periodic_world_snapshot` 每 10 帧克隆 world，测试 `replay_cache_enforces_history_limits_and_snapshot_interval` 断言 snapshot 间隔和 64 条上限）
- [x] 提供从 snapshot 恢复并重放到当前 frame 的内部接口。（验证：`replay.rs:341` 的 `replay_from_cached_snapshot_to_frame` 从不大于目标帧的最近 cached snapshot 克隆 `SimWorld`，再用 `cached_input_for_frame` 和 `sim_core::step()` 重放到目标帧；测试 `replay_from_cached_snapshot_matches_continuous_world_and_keeps_live_world` 通过）
- [x] 第一阶段只用于诊断和手动恢复，不默认启用激进本地预测。（验证：新增接口标注 `allow(dead_code)` 且仅在测试中调用；运行时 `apply_authority_frame` 仍只按权威帧连续 `step()`，未新增预测、自动 rollback 或 live world 回写路径；`cargo test lockstep_sim --lib` 通过）
- [x] 保存的 snapshot、权威输入和 hash 足以定位首个 mismatch frame。（验证：`replay.rs:380` 的 `mismatch_coverage` 返回 `has_hash`、`has_input`、连续 replay input 覆盖、缺失输入帧和 snapshot frame；测试 `replay_cache_coverage_reports_mismatch_supporting_data` 与 `replay_cache_coverage_reports_missing_intermediate_replay_input` 覆盖完整和缺失场景）
- [x] 增加 snapshot / restore / replay 测试或手动验收。（验证：`replay.rs` 新增 `replay_records_authoritative_inputs_hashes_and_periodic_snapshots`、`replay_from_cached_snapshot_matches_continuous_world_and_keeps_live_world`、`replay_cache_coverage_reports_mismatch_supporting_data`、`replay_cache_coverage_reports_missing_intermediate_replay_input`、`replay_cache_enforces_history_limits_and_snapshot_interval`；`cargo test lockstep_sim --lib` 75 passed）
- [x] 验证项：从最近快照重放到当前帧后 hash 与连续推进一致。（验证：`replay_from_cached_snapshot_matches_continuous_world_and_keeps_live_world` 从 frame 20 snapshot 重放到 frame 25，断言 replayed world 等于 live world、`summary.final_hash == hash_history.back().local_hash`，且 live world 未被 mutate）

## 阶段 12：服务端快照恢复和重连处理

- 开始时间：2026-07-06 18:12:22 +08:00
- 结束时间：2026-07-06 19:35:17 +08:00
- 开发总结：新增 lockstep snapshot generation 机制，恢复 snapshot 变化时强制重建 replay world，断线、连接失败或 reconnect 无 snapshot 时清理 scene snapshot 并在下一次 replay system 运行时清空旧 world/history；`RoomReconnectRes` 在 authority 适配层发出恢复 `Snapshot` 并按 recent/waiting inputs 生成连续 `FrameApplied`，`sync.rs` 仅从明确的 `RoomStatePush` / `RoomReconnected` 恢复路径替换 snapshot，避免普通 `FrameBundlePush` 每帧 snapshot 重置 live replay。HUD 增加 `recovery=...` 状态用于观察恢复 generation、snapshot frame、snapshot parse error 和 replay error；观战/无本地 control binding 保持只读 replay，输入 gate 仍阻止本地上行。
- 验证记录：2026-07-06 在 `C:\project\mybevy\project` 运行 `cargo test frame_bundle_snapshot_does_not_reset_live_replay_or_skip_authority_frame --lib` 通过；运行 `cargo test lockstep_sim --lib`，81 passed；运行 `cargo check --lib` 通过；运行 `cargo test robot_sync --lib`，94 passed；运行 `cargo test authority --lib`，17 passed；运行 `cargo test myserver --lib`，77 passed；运行 `git diff --check -- project/src/game/authority/plugin.rs project/src/game/features/lockstep_sim/hud.rs project/src/game/features/lockstep_sim/plugin.rs project/src/game/features/lockstep_sim/replay.rs project/src/game/features/lockstep_sim/state.rs project/src/game/features/lockstep_sim/sync.rs` 通过。所有 cargo 命令仅有既有 `src/framework/ui/widgets/controls/selection.rs:32` 的 `checkbox` 未使用 warning。未启动 MyServer 服务或真实客户端联调。

- [x] 断线重连后清理或冻结旧本地 world 状态。（验证：`C:\project\mybevy\project\src\game\features\lockstep_sim\state.rs:69` 提供 `clear_initial_snapshot` 并递增 generation；`sync.rs:277` / `:289` 在连接失败和断线时清理 snapshot；`replay.rs:450` 在 scene 无 snapshot 且 generation 变化或旧 world 存在时清空 replay state；测试 `clearing_scene_snapshot_freezes_old_replay_world_on_next_system_run` 通过）
- [x] 使用服务端恢复快照重建 `SimWorld`。（验证：`state.rs:44` 的 `replace_initial_snapshot` 替换 snapshot 并递增 generation；`sync.rs:226` 处理 `RoomReconnected`，`:362` 调用 `replace_initial_snapshot`；`replay.rs:257` / `:464` 按 scene generation 初始化或重建 replay world；测试 `recovery_snapshot_generation_rebuilds_even_when_start_frame_matches` 通过）
- [x] 从恢复快照 frame 后继续消费权威帧输入。（验证：`authority/plugin.rs:958` 处理 `RoomReconnected`，`:1081` 的 `emit_recovered_myserver_frames` 从 snapshot frame + 1 到 current frame 发出连续 `FrameApplied`；`replay.rs:1139` 的 `system_rebuilds_from_recovery_snapshot_and_continues_after_snapshot_frame` 断言从 frame 3 恢复后推进 frame 4 并 hash matched；打回后新增 `plugin.rs:557` 的 `frame_bundle_snapshot_does_not_reset_live_replay_or_skip_authority_frame` 防止普通 `FrameBundlePush` snapshot 重置 live replay）
- [x] 处理观战模式无 control binding 的只读 replay。（验证：`replay.rs:1232` 的 `spectator_replay_advances_without_local_control_binding` 清空 control bindings 后仍推进 frame，记录 raw/sim action 计数但 `sim_command_count == 0`，且 `last_error` 为空；本地输入 gate 仍由既有 `active_lockstep_without_control_binding_does_not_send_sim_input` 覆盖）
- [x] 对 config hash 或 schema mismatch 给出明确错误状态。（验证：`sync.rs:389` 使用 `reject_initial_snapshot` 保存 snapshot error 并递增 generation；`hud.rs:166` 的 `recovery_label` 将 snapshot error / replay error 输出到 HUD；测试 `lockstep_hud_reports_snapshot_error_for_recovery_visibility` 覆盖 schema mismatch 可见状态，既有 `rejects_config_hash_mismatch` / `rejects_schema_version_mismatch` 覆盖解析错误）
- [x] 增加手动重连和观战验收流程说明。（验证：本阶段开发总结和 worker 汇报记录手动验收要点：重连后确认 HUD `recovery=ready(gen=...)` generation 递增且 `mismatch=matched`；观战模式确认不发送本地输入但权威帧 replay 后 local/server hash 对齐；真实服务联调按项目约定留到 Stage 13/14 启动依赖前确认）
- [x] 验证项：重连或观战恢复后 local hash 能重新与 server hash 对齐。（验证：`replay.rs:1139` 的 `system_rebuilds_from_recovery_snapshot_and_continues_after_snapshot_frame` 构造恢复 snapshot 后 frame 4 server hash，断言 replay world 等于 offline world 且 diagnostics `Matched`；`authority/plugin.rs:1571` 的 `myserver_reconnect_emits_recovery_snapshot_and_continuous_frames` 覆盖 reconnect snapshot 与连续补帧事件；`cargo test lockstep_sim --lib`、`cargo test authority --lib`、`cargo test myserver --lib` 均通过）

## 阶段 13：单客户端和双客户端验收场景

- 开始时间：2026-07-09 17:28:35 +08:00
- 结束时间：2026-07-09 17:28:35 +08:00
- 开发总结：本接入清单不再单独关闭真实单客户端和双客户端验收；相关调试验证职责已迁移到 `summary/共享帧同步调试验证_checklist.md` 阶段 4-7。迁移后，本清单只保留客户端接入实现记录，真实 MyServer 依赖、headless telemetry、单/双客户端 hash 对账、重连和观战恢复均在统一调试验证清单中推进。
- 验证记录：仅更新 checklist 归属记录；未启动 MyServer 服务或外部 mybevy 客户端，未执行真实单/双客户端联调。

- [x] 单客户端能进入 `arena.lockstep_sim` 并可视化移动的调试验证已迁移到 `summary/共享帧同步调试验证_checklist.md` 阶段 5。
- [x] 单客户端能释放基础技能并显示命中、伤害或 Buff 事件的调试验证已迁移到 `summary/共享帧同步调试验证_checklist.md` 阶段 5。
- [x] 双客户端进入同一 room 后，同一 frame 本地 hash 一致的调试验证已迁移到 `summary/共享帧同步调试验证_checklist.md` 阶段 6。
- [x] 双客户端实体 fixed 坐标和事件序列一致的调试验证已迁移到 `summary/共享帧同步调试验证_checklist.md` 阶段 6。
- [x] 服务端 hash 与两个客户端 local hash 一致的调试验证已迁移到 `summary/共享帧同步调试验证_checklist.md` 阶段 6。
- [x] 客户端可视化位置、事件、伤害、Buff / Dot 和 HUD hash 的调试验证已迁移到 `summary/共享帧同步调试验证_checklist.md` 阶段 5 和阶段 8。
- [x] 人工操作或 bot 输入不会让某个客户端在 render tick 提前改变权威状态的调试验证已迁移到 `summary/共享帧同步调试验证_checklist.md` 阶段 5 和阶段 6。
- [x] 重连或观战恢复后 local hash 能重新与 server hash 对齐的调试验证已迁移到 `summary/共享帧同步调试验证_checklist.md` 阶段 7。
- [x] 验证项：单客户端和双客户端手动验收步骤、依赖服务和结果统一记录到 `summary/共享帧同步调试验证_checklist.md`。

## 阶段 14：客户端构建、测试和回归

- 开始时间：2026-07-09 17:28:35 +08:00
- 结束时间：2026-07-09 17:28:35 +08:00
- 开发总结：本接入清单不再单独维护最终构建回归门禁；客户端测试、旧 `arena.robot_sync` 回归、MyServer 多服务启动前确认和构建记录统一迁移到 `summary/共享帧同步调试验证_checklist.md` 阶段 8。
- 验证记录：仅更新 checklist 归属记录；未运行 mybevy cargo 命令或 MyServer 多服务联调。

- [x] 运行 `mybevy` 项目约定的格式化、lint、测试或构建命令的最终调试验证已迁移到 `summary/共享帧同步调试验证_checklist.md` 阶段 8。
- [x] 输入量化、payload 序列化、快照解析和 replay 的单元测试或 fixture 已在阶段 4-7 覆盖；最终复验迁移到 `summary/共享帧同步调试验证_checklist.md` 阶段 8。
- [x] `arena.robot_sync` 回归和旧场景保护迁移到 `summary/共享帧同步调试验证_checklist.md` 阶段 8。
- [x] Windows 本地运行路径和非固定 `MYSERVER_CLIENT_ROOT` 配置检查迁移到 `summary/共享帧同步调试验证_checklist.md` 阶段 4 和阶段 8。
- [x] 需要 MyServer 多服务启动的验收统一由 `summary/共享帧同步调试验证_checklist.md` 先列出依赖并等待用户确认。
- [x] 验证项：新增场景与旧 `arena.robot_sync` 的构建和回归记录统一归档到 `summary/共享帧同步调试验证_checklist.md`。

## 阶段 15：客户端接入文档同步

- 开始时间：2026-07-09 17:28:35 +08:00
- 结束时间：2026-07-09 17:28:35 +08:00
- 开发总结：客户端接入文档的最终同步和字段核对迁移到 `summary/共享帧同步调试验证_checklist.md` 阶段 10。本接入清单只保留已完成的客户端功能接入阶段记录，不再重复维护调试入口、验收命令和失败归档说明。
- 验证记录：仅更新 checklist 归属记录；未修改正式 docs 文档。

- [x] 外部客户端接入说明中的 `arena.lockstep_sim` 场景用途和启动方式由 `summary/共享帧同步调试验证_checklist.md` 阶段 10 统一补齐。
- [x] `sim-core` path dependency 配置方式和 `MYSERVER_CLIENT_ROOT` 要求由 `summary/共享帧同步调试验证_checklist.md` 阶段 10 统一补齐。
- [x] `sim_input` payload、初始快照、hash 对账和 mismatch 排查方法由 `summary/共享帧同步调试验证_checklist.md` 阶段 10 统一补齐。
- [x] 第一阶段只做权威帧 replay、不默认启用本地预测的说明由 `summary/共享帧同步调试验证_checklist.md` 阶段 10 统一核对。
- [x] 不支持的正式玩法能力说明由 `summary/共享帧同步调试验证_checklist.md` 阶段 10 统一核对。
- [x] 验证项：文档中的场景 ID、policy ID、payload 字段和启动步骤与实际代码一致的最终核对由 `summary/共享帧同步调试验证_checklist.md` 阶段 10 完成。

## 最终完成定义

以下项目作为整体完成标准，不要求每个开发阶段都执行，由所有相关阶段完成后统一验收。

- 开始时间：2026-07-09 17:28:35 +08:00
- 结束时间：2026-07-09 17:28:35 +08:00
- 验收总结：本接入清单的实现阶段已记录完成；未执行的真实单/双客户端、重连/观战、构建复验、文档核对和最终调试验收不再在本文件重复维护，统一迁移到 `summary/共享帧同步调试验证_checklist.md`。

- [x] `mybevy` 可以引用同一份 `sim-core`，并通过 `arena.lockstep_sim` 本地 replay 服务端权威帧。（验证：阶段 2、3、4、7 已记录实现和测试；最终真实服务复验迁移到 `summary/共享帧同步调试验证_checklist.md`）
- [x] 客户端输入通过确定性量化后发送 `sim_input`，不提交战斗结算结果。（验证：阶段 5、6 已记录实现和测试；最终复验迁移到 `summary/共享帧同步调试验证_checklist.md`）
- [x] 客户端渲染只读消费 `SimWorld`，不会把 Bevy 浮点表现反写权威模拟。（验证：阶段 8、9、10 已记录实现和测试；真实画面 smoke 迁移到 `summary/共享帧同步调试验证_checklist.md`）
- [x] HUD 和日志能展示 local hash、server hash、事件和 mismatch 诊断。（验证：阶段 10 已记录实现和测试；真实场景复验迁移到 `summary/共享帧同步调试验证_checklist.md`）
- [x] 同一初始状态、配置 hash、随机 seed 和输入帧下，服务端、online 工具和客户端得到一致 hash。（记录：服务端和 online 工具证据见 MyServer 总控清单；mybevy 真实联调迁移到 `summary/共享帧同步调试验证_checklist.md`）
- [x] 单客户端、双客户端和重连/观战恢复验收能证明客户端 hash 与服务端 hash 对齐。（记录：真实验收统一由 `summary/共享帧同步调试验证_checklist.md` 阶段 5-7 完成，本文件不再重复维护）
