# 共享帧同步 P1 移动与离线验证 Checklist

## 目标

在 P0 `sim-core` 底座上完成可用的移动规则，并新增服务端仓库内 Rust 套壳 client 的 offline 模式，用 scenario 快速验证同一输入下 server_sim 和 client_sim 的移动结果与 hash 一致。

本阶段不启动 MyServer 服务，不接入 `game-server` room policy，不修改外部 `mybevy`，不实现完整战斗。

## 基础原则

- [x] 移动逻辑仍全部位于 `packages/sim-core`，工具只负责加载场景和驱动 replay。（验证：`packages/sim-core/src/tick.rs` 实现移动推进、速度校验和边界 clamp；`tools/lockstep-client/src/offline.rs` 仅加载 scenario、构建双 world 并驱动 replay）
- [x] offline 验证不依赖 Redis、PostgreSQL、NATS、auth-http、game-proxy 或 game-server。（验证：`tools/lockstep-client/Cargo.toml` 仅依赖 `sim-core`、`serde`、`serde_json`；最终验收仅运行 cargo test/run 命令，未启动任何 MyServer 服务）
- [x] scenario 文件字段带 version，非法字段和非法数值应给出明确错误。（验证：`tools/lockstep-client/src/scenario.rs` 定义 `version` 并使用 `deny_unknown_fields`，`unsupported_version_is_rejected`、`invalid_input_fixture_is_rejected_with_readable_error` 测试通过）
- [x] mismatch 输出必须能定位到首个不一致帧和关键实体差异。（验证：`tools/lockstep-client/src/offline.rs` 的 `MismatchDiff` 输出首个 mismatch frame、server/client hash、实体数量和实体字段差异；`frame_hash_mismatch_display_includes_readable_diff` 测试通过）
- [x] 每个阶段完成后运行对应 Rust 测试或工具命令。（验证：阶段 1-8 均记录 `cargo test`、`cargo fmt --check` 或 offline scenario 运行结果；最终验收再次运行 sim-core、lockstep-client 测试和 5 个正式移动 scenario）

## 阶段 1：移动配置与输入细化

- 开始时间：2026-07-03 09:12:36 +08:00
- 结束时间：2026-07-03 09:26:48 +08:00
- 开发总结：新增 `MovementConfig` 并将 `SimConfig` 收拢为 `movement` 配置，补齐移动速度单位注释、Move 缺省速度解析、速度/方向校验和 Stop 清空移动状态行为。
- 验证记录：`cargo test --manifest-path packages/sim-core/Cargo.toml` 通过，37 个测试通过；`cargo fmt --manifest-path packages/sim-core/Cargo.toml -- --check` 通过。

- [x] 扩展 `SimConfig.movement`，包含 tick rate、默认速度、最大速度和场景边界。（验证：`packages/sim-core/src/tick.rs` 定义 `SimConfig { movement: MovementConfig }`，`MovementConfig` 包含 `tick_rate`、`default_speed_per_second`、`max_speed_per_second`、`bounds`）
- [x] 明确速度单位为 fixed raw per second 或 sim unit per second，并在代码注释中固定。（验证：`packages/sim-core/src/tick.rs`、`input.rs`、`state.rs` 注释说明速度为 simulation units per second represented as `Fp` raw milli-units）
- [x] 扩展 Move 输入，支持仅提交方向时由实体或配置决定速度。（验证：`resolve_movement_input` 对 `speed_per_second: None` 优先使用实体当前正速度，否则使用 `config.movement.default_speed_per_second`；`step_move_without_speed_*` 测试通过）
- [x] 校验 `speed > 0` 时方向不能为零向量。（验证：`validate_move_speed` 在正速度且 `QuantizedDir::ZERO` 时返回 `StepError::ZeroDirectionMove`；`step_rejects_positive_speed_with_zero_direction_without_updating_world` 测试通过）
- [x] 校验速度不能超过配置最大速度。（验证：`validate_move_speed` 对比 `config.movement.max_speed_per_second` 并返回 `StepError::MovementSpeedTooHigh`；显式速度和解析后速度越界测试均通过）
- [x] 支持 Stop 输入将实体速度和方向置为停止。（验证：`step` 对 `MovementSelection::Stop` 设置 `Idle`、`QuantizedDir::ZERO`、`Fp::ZERO`；`step_stop_keeps_entity_idle_until_a_new_move_input_arrives` 测试通过）
- [x] 增加速度合法、速度越界、零方向移动拒绝、Stop 生效测试。（验证：`cargo test --manifest-path packages/sim-core/Cargo.toml` 通过 37 个测试，覆盖默认速度、实体速度复用、速度越界、零/负速度、零方向拒绝和 Stop）

## 阶段 2：移动推进规则

- 开始时间：2026-07-03 09:28:43 +08:00
- 结束时间：2026-07-03 09:38:04 +08:00
- 开发总结：收口确定性移动推进公式和截断规则，补齐水平/垂直/对角线、首次缺失输入、连续缺失输入、Stop 后缺失输入和边界 clamp 测试，并验证 debug/release hash 一致。
- 验证记录：`cargo test --manifest-path packages/sim-core/Cargo.toml` 通过，43 个测试通过；`cargo fmt --manifest-path packages/sim-core/Cargo.toml -- --check` 通过；debug/release 定向运行 `step_hash_is_stable_across_matching_worlds_and_frames` 均通过。

- [x] 使用固定公式推进：`delta = dir * speed / (FP_SCALE * fps)`。（验证：`packages/sim-core/src/tick.rs` 的 `movement_delta_raw` 注释和实现固定该公式）
- [x] 明确除法采用 toward zero 截断。（验证：`movement_delta_raw` 注释说明 Rust signed integer division truncates toward zero；`movement_delta_raw_uses_fixed_formula_and_truncates_toward_zero` 覆盖正负截断）
- [x] 实现缺失输入保持上一移动状态的行为。（验证：`step_reuses_previous_movement_state_across_consecutive_missing_inputs` 连续 3 帧空输入仍沿用上一移动状态）
- [x] 实现首次无输入保持静止的行为。（验证：`step_first_missing_movement_input_keeps_entity_stationary` 测试通过）
- [x] 实现显式 Stop 后后续缺失输入继续静止。（验证：`step_stop_after_movement_makes_following_missing_inputs_stationary` 和 `step_stop_keeps_entity_idle_until_a_new_move_input_arrives` 测试通过）
- [x] 对 position clamp 到场景边界。（验证：`step_clamps_position_to_scene_bounds` 和 `step_clamps_position_to_scene_min_bounds` 覆盖 max/min 边界 clamp）
- [x] 增加水平移动、垂直移动、对角线移动、连续缺失输入、Stop 后缺失输入测试。（验证：`step_moves_horizontal_vertical_and_707_diagonal_by_fixed_formula`、`step_reuses_previous_movement_state_across_consecutive_missing_inputs`、`step_stop_after_movement_makes_following_missing_inputs_stationary` 测试通过）
- [x] 增加 debug 和 release 下结果一致的验证记录。（验证：`cargo test --manifest-path packages/sim-core/Cargo.toml step_hash_is_stable_across_matching_worlds_and_frames` 与 `cargo test --release --manifest-path packages/sim-core/Cargo.toml step_hash_is_stable_across_matching_worlds_and_frames` 均通过）

## 阶段 3：简单空间边界与碰撞预留

- 开始时间：2026-07-03 09:40:03 +08:00
- 结束时间：2026-07-03 09:50:15 +08:00
- 开发总结：明确 `SceneBounds` 矩形语义，新增静态障碍数据结构预留但 P1 不启用碰撞，并将边界 clamp 改为按实体半径约束中心点。
- 验证记录：`cargo test --manifest-path packages/sim-core/Cargo.toml` 通过，46 个测试通过；`cargo fmt --manifest-path packages/sim-core/Cargo.toml -- --check` 通过。

- [x] 定义 `SceneBounds`，支持矩形边界。（验证：`packages/sim-core/src/tick.rs` 的 `SceneBounds { min, max }` 注释说明 axis-aligned rectangular scene bounds，min/max 按轴归一化）
- [x] 定义 `StaticObstacle` 数据结构，至少预留 circle 或 axis-aligned rect 类型。（验证：`packages/sim-core/src/tick.rs` 定义 `StaticObstacle` 和 `StaticObstacleShape::Circle` / `AxisAlignedRect`，并在 `lib.rs` 顶层导出）
- [x] 第一阶段如不实现障碍碰撞，明确返回 unsupported 或保留结构但不启用。（验证：`MovementConfig.static_obstacles` 注释明确 P1 movement does not apply static obstacles，结构仅为 serializable configuration）
- [x] 如果实现障碍碰撞，固定实体与障碍处理顺序。（验证：本阶段未实现障碍碰撞；`MovementConfig.static_obstacles` 注释预留后续按 sorted `EntityId` 推进、按 vector order 解析障碍）
- [x] 定义实体半径参与边界 clamp 的规则。（验证：`SceneBounds::clamp_center_with_radius` 注释定义中心点 clamp 到 `[min + radius, max - radius]`，半径超出空间时按轴退化到 bounds midpoint）
- [x] 增加靠近边界时半径不越界的测试。（验证：`step_clamps_near_boundary_so_entity_radius_stays_inside` 和 `step_collapses_oversized_radius_axis_to_bounds_midpoint` 测试通过）
- [x] 增加障碍未启用时不影响移动 hash 的测试。（验证：`static_obstacles_are_reserved_and_do_not_affect_p1_movement_or_hash` 测试确认有/无 obstacle 的 world 和 state_hash 一致）

## 阶段 4：scenario schema

- 开始时间：2026-07-03 09:52:29 +08:00
- 结束时间：2026-07-03 10:12:14 +08:00
- 开发总结：新增 `tools/lockstep-client` Rust 库 crate，在工具侧定义 scenario schema、反序列化、基础校验和 sim-core 类型转换接口。
- 验证记录：`cargo test --manifest-path tools/lockstep-client/Cargo.toml` 通过，6 个测试通过；`cargo fmt --manifest-path tools/lockstep-client/Cargo.toml -- --check` 通过。

- [x] 在 `sim-core` 或 `tools/lockstep-client` 定义 scenario schema。（验证：`tools/lockstep-client/src/scenario.rs` 定义 `Scenario` schema，工具 crate 通过 path dependency 引用 `sim-core`，未扩大核心 crate 边界）
- [x] scenario 包含 version、tickRate、config、initial、inputs、assertions。（验证：`Scenario` 使用 `serde(rename_all = "camelCase")` 定义 `version`、`tick_rate`、`config`、`initial`、`inputs`、`assertions`）
- [x] initial 支持声明实体 id、kind、characterId、teamId、x、y、radius、hp。（验证：`ScenarioInitialEntity` 定义 `id/kind/character_id/team_id/x/y/radius/hp`，并转换为 `SimEntity`）
- [x] inputs 支持按 frame 声明 Move、Stop、Face。（验证：`ScenarioInput` 定义 `frame/character_id/entity_id/seq/command`，`ScenarioCommand` 支持 `Move`、`Stop`、`Face` 并转换为 `SimCommand`）
- [x] assertions 支持 finalFrame、finalHash、可选实体位置断言。（验证：`ScenarioAssertions` 定义 `final_frame`、`final_hash`、`entity_positions`，`expected_final_hash` 解析 16 位 hex）
- [x] 实现 scenario 反序列化和基本校验。（验证：`Scenario::from_json_str` 调用 serde 反序列化和 `validate`，校验 version、tickRate、移动配置、重复实体、输入实体/角色映射、速度/方向、finalHash 和位置断言实体存在）
- [x] 增加合法 scenario、缺字段、version 不支持、重复 entity id、非法输入的测试。（验证：`valid_scenario_deserializes_validates_and_converts_to_sim_types`、`missing_required_field_reports_missing_field`、`unsupported_version_is_rejected`、`duplicate_entity_id_is_rejected`、`invalid_input_is_rejected`、`invalid_direction_is_rejected` 测试通过）

## 阶段 5：`tools/lockstep-client` offline 骨架

- 开始时间：2026-07-03 10:14:36 +08:00
- 结束时间：2026-07-03 10:31:39 +08:00
- 开发总结：为 `tools/lockstep-client` 新增 offline CLI 入口、scenario 路径解析、双 world replay、逐帧 hash 比对和基础成功/失败输出。
- 验证记录：`cargo test --manifest-path tools/lockstep-client/Cargo.toml` 通过，10 个测试通过；`cargo fmt --manifest-path tools/lockstep-client/Cargo.toml -- --check` 通过；`cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode offline --scenario smoke` 通过并输出 final frame/hash；非法 mode 返回非 0。

- [x] 新建 `tools/lockstep-client/Cargo.toml`。（验证：`tools/lockstep-client/Cargo.toml` 已存在并定义 `lockstep-client` crate）
- [x] 新建 CLI 入口，支持 `--mode offline` 和 `--scenario <path-or-name>`。（验证：`tools/lockstep-client/src/main.rs` 调用 `offline::run_cli`，`CliOptions::parse` 支持 `--mode offline` 和 `--scenario`）
- [x] 通过 path dependency 引用 `packages/sim-core`。（验证：`tools/lockstep-client/Cargo.toml` 包含 `sim-core = { path = "../../packages/sim-core" }`）
- [x] 实现 scenario 路径解析，默认查找 `tools/lockstep-client/scenarios/`。（验证：`resolve_scenario_path` 优先读现有路径，否则查找 `env!("CARGO_MANIFEST_DIR")/scenarios` 并支持补 `.json`；路径解析测试通过）
- [x] 根据 scenario 构建 server_sim 和 client_sim 两个 `SimWorld`。（验证：`replay_scenario` 使用 `Scenario::to_initial_world()` 构建 `server_sim` 并 clone 为 `client_sim`）
- [x] 按帧驱动两个 world 分别调用 `sim_core::step()`。（验证：`replay_scenario` 按 `initial.frame + 1..=finalFrame` 遍历并分别调用 `step(&mut server_sim, ...)` 和 `step(&mut client_sim, ...)`）
- [x] 每帧比对 hash。（验证：`replay_scenario` 每帧比较 `server_result.state_hash` 和 `client_result.state_hash`，不一致返回 `OfflineError::FrameHashMismatch`）
- [x] 成功时输出 scenario、final frame、final hash。（验证：`OfflineReport` 的 `Display` 输出 scenario、final frame、final hash；smoke CLI 输出 `final frame: 5` 和 `final hash: cc4b59cef0123a23`）
- [x] 失败时返回非 0 退出码。（验证：`main.rs` 在 `Err` 时 `std::process::exit(1)`；非法 `--mode invalid` 运行返回 exit code 1）

## 阶段 6：mismatch diff 输出

- 开始时间：2026-07-03 10:34:03 +08:00
- 结束时间：2026-07-03 13:23:42 +08:00
- 开发总结：增强 offline replay 的 hash mismatch 诊断，新增 `MismatchDiff`、实体差异和输入摘要输出，CLI 错误信息可定位首个不一致帧及关键实体字段差异。
- 验证记录：`cargo test --manifest-path tools/lockstep-client/Cargo.toml` 通过，11 个测试通过；`cargo fmt --manifest-path tools/lockstep-client/Cargo.toml -- --check` 通过；`cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode offline --scenario smoke` 通过。

- [x] 输出首个 hash 不一致帧。（验证：`MismatchDiff::fmt` 输出 `hash mismatch: first mismatch frame ...`，`frame_hash_mismatch_display_includes_readable_diff` 覆盖 frame 7）
- [x] 输出 server_hash 和 client_hash。（验证：`MismatchDiff` 保存 `server_hash` / `client_hash` 并在 Display 中输出 `server_hash:`、`client_hash:`）
- [x] 输出实体数量差异。（验证：`MismatchDiff` 输出 `entity count: server=... client=... diff=...`，测试覆盖 `server=2 client=1 diff=-1`）
- [x] 输出位置不同的实体 id、server pos、client pos。（验证：`EntityDiff` 输出 entity id、`server_pos`、`client_pos`，测试覆盖 entity 1001 的 `(1000, 2000)` 与 `(3000, 2000)` 以及缺失实体 `<missing>`）
- [x] 输出 hp / alive / movement 状态差异。（验证：`EntityDiff` 输出 `server_hp/client_hp`、`server_alive/client_alive`、`server_movement/client_movement`，测试断言相关字段）
- [x] 输出输入集合摘要，包含 frame、character id、entity id、seq 和 command。（验证：`InputSummary` Display 输出 `frame`、`character_id`、`entity_id`、`seq`、`command`，测试覆盖 Move 输入摘要）
- [x] 增加人为制造 mismatch 的测试或 fixture，验证 diff 可读。（验证：`frame_hash_mismatch_display_includes_readable_diff` 人为构造 server/client world 差异并检查错误输出关键字段）

## 阶段 7：移动 scenario 集合

- 开始时间：2026-07-03 13:26:22 +08:00
- 结束时间：2026-07-03 13:36:25 +08:00
- 开发总结：新增 5 个正式移动 offline scenario 和 1 个非法输入 fixture，为正式 scenario 写入实际 final hash，并补充非法 fixture 被拒绝的测试。
- 验证记录：`cargo test --manifest-path tools/lockstep-client/Cargo.toml` 通过，12 个测试通过；`cargo fmt --manifest-path tools/lockstep-client/Cargo.toml -- --check` 通过；5 个正式 scenario 逐个 offline 运行通过；`move_invalid_input` 返回非 0 且错误信息可读。

- [x] 新增 `move_straight.json`，验证固定方向直线移动。（验证：`tools/lockstep-client/scenarios/move_straight.json` 存在，offline 运行 final hash `f70bc6733be8be87`）
- [x] 新增 `move_stop.json`，验证显式停止。（验证：`tools/lockstep-client/scenarios/move_stop.json` 存在，offline 运行 final hash `cc4b59cef0123a23`）
- [x] 新增 `move_diagonal.json`，验证 707 对角线。（验证：`tools/lockstep-client/scenarios/move_diagonal.json` 存在，offline 运行 final hash `c6a4919b89dd5a4b`）
- [x] 新增 `move_boundary_clamp.json`，验证边界 clamp。（验证：`tools/lockstep-client/scenarios/move_boundary_clamp.json` 存在，offline 运行 final hash `3d50c94ac9d52792`）
- [x] 新增 `move_missing_input_continue.json`，验证缺失输入延续上一移动状态。（验证：`tools/lockstep-client/scenarios/move_missing_input_continue.json` 存在，offline 运行 final hash `121d14a3cd4c91a1`）
- [x] 新增 `move_invalid_input.json` 或测试 fixture，验证非法方向 / 速度被拒绝。（验证：`tools/lockstep-client/scenarios/move_invalid_input.json` 存在，`invalid_input_fixture_is_rejected_with_readable_error` 测试通过，CLI 运行返回 exit code 1）
- [x] 为每个 scenario 记录预期 final hash。（验证：5 个正式 scenario 的 `assertions.finalHash` 均为非零实际 hash，逐个 offline 运行通过）
- [x] 确认 bless / 更新 hash 必须是显式命令或手工动作，不默认覆盖。（验证：工具代码未新增自动 bless/覆盖逻辑；hash 由 scenario JSON 显式记录，`finalHash = 0000000000000000` 仍仅作为占位跳过校验）

## 阶段 8：工具验证命令与文档

- 开始时间：2026-07-03 13:39:30 +08:00
- 结束时间：2026-07-03 13:46:08 +08:00
- 开发总结：新增 `tools/lockstep-client/README.md`，记录 offline replay、单 scenario、全量 scenario、测试命令、常见失败原因和 finalHash 手工更新原则。
- 验证记录：`cargo test --manifest-path packages/sim-core/Cargo.toml` 通过，46 个测试通过；`cargo test --manifest-path tools/lockstep-client/Cargo.toml` 通过，12 个测试通过；`cargo fmt --manifest-path tools/lockstep-client/Cargo.toml -- --check` 通过；`move_straight`、`move_stop`、`move_diagonal` 三个 scenario offline 运行通过。

- [x] 在工具 README 或 checklist 备注中记录 offline 运行命令。（验证：`tools/lockstep-client/README.md` 的 `Offline replay` 章节记录 `cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode offline --scenario ...`）
- [x] 记录单个 scenario 运行命令。（验证：README 记录按名称运行 `move_straight` 和按 JSON path 运行单个 scenario）
- [x] 记录全量 scenario 运行命令。（验证：README `Run all passing scenarios` 章节提供 PowerShell 批量运行命令并排除 `move_invalid_input`）
- [x] 记录常见失败原因：frame 不连续、hash 不一致、非法输入、scenario version 不匹配。（验证：README `Common failures` 章节覆盖 non-contiguous frame、hash mismatch、final hash mismatch、invalid input、scenario version mismatch）
- [x] 运行 `cargo test --manifest-path packages/sim-core/Cargo.toml`。（验证：命令通过，46 个测试通过）
- [x] 运行 `cargo test --manifest-path tools/lockstep-client/Cargo.toml`。（验证：命令通过，12 个测试通过）
- [x] 运行至少 3 个移动 scenario offline。（验证：`move_straight`、`move_stop`、`move_diagonal` offline 运行通过，hash 分别为 `f70bc6733be8be87`、`cc4b59cef0123a23`、`c6a4919b89dd5a4b`）

## 最终完成定义

以下项目作为 P1 整体完成标准，由所有阶段完成后统一验收。

- 开始时间：2026-07-03 13:48:21 +08:00
- 结束时间：2026-07-03 13:50:31 +08:00
- 验收总结：P1 已完成共享移动规则、scenario schema、offline 双 world replay、mismatch diff、移动 scenario 集合和工具 README；最终验收中 sim-core 与 lockstep-client 测试均通过，5 个正式移动 scenario offline 运行通过，非法输入 fixture 按预期失败。

- [x] `sim-core` 移动规则可覆盖直线移动、停止、缺失输入和边界 clamp。（验证：`cargo test --manifest-path packages/sim-core/Cargo.toml` 通过 46 个测试，覆盖水平/垂直/对角线移动、Stop、缺失输入延续和边界 clamp）
- [x] `tools/lockstep-client` offline 可加载 scenario 并驱动双 world replay。（验证：`cargo test --manifest-path tools/lockstep-client/Cargo.toml` 通过 12 个测试；`replay_scenario` 构建 server_sim/client_sim 并逐帧调用 `sim_core::step`）
- [x] 移动 scenario 多次运行 hash 稳定。（验证：`move_straight`、`move_stop`、`move_diagonal`、`move_boundary_clamp`、`move_missing_input_continue` offline 均通过，final hash 分别为 `f70bc6733be8be87`、`cc4b59cef0123a23`、`c6a4919b89dd5a4b`、`3d50c94ac9d52792`、`121d14a3cd4c91a1`）
- [x] mismatch 输出足以定位首个差异帧和实体差异。（验证：`frame_hash_mismatch_display_includes_readable_diff` 覆盖首个 mismatch frame、server/client hash、实体数量、位置、hp、alive 和 movement 差异输出）
- [x] P1 不依赖启动任何 MyServer 服务。（验证：最终验收只运行 `cargo test` 与 `cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode offline --scenario ...`；未启动 Redis、PostgreSQL、NATS、auth-http、game-proxy 或 game-server）
