# 共享帧同步 P0 定点模拟核心 Checklist

## 目标

交付 `packages/sim-core` 的最小确定性模拟核心，先解决客户端与服务端共用同一套基础状态、输入、定点数、单帧推进和 hash 的问题。

本阶段只建立可被后续移动、战斗、服务端接入和客户端场景复用的底座。不接入网络，不改 `game-server` room policy，不修改外部 `mybevy`，不实现完整战斗规则。

## 基础原则

- [x] `sim-core` 只依赖必要的纯 Rust 库，不依赖 Bevy、Tokio、Redis、PostgreSQL、NATS 或 Protobuf。（验证：`packages/sim-core/Cargo.toml` 仅依赖 `serde` 和 `serde_json`）
- [x] 核心模拟状态推进不使用 `f32` / `f64`。（验证：`packages/sim-core/src/tick.rs` 使用 `Fp` raw 值、`FP_SCALE` 和整数除法推进；`f32` 仅出现在 `math.rs` 的 `to_f32_for_render` 渲染边界）
- [x] 所有状态 hash 输入都使用稳定排序和原始整数 / 定点 raw 值。（验证：`packages/sim-core/src/hash.rs` 按 `entity.id` 排序，并写入 `raw()` / 固定整数字段；`entity_vec_order_does_not_change_hash` 测试通过）
- [x] 所有 public 类型明确 version、单位、边界和序列化语义。（验证：`SIM_CORE_SCHEMA_VERSION`、`SimWorld.schema_version`、`SimSnapshot.schema_version`、`Fp` milli-unit 文档、`QuantizedDir` 边界校验和 serde roundtrip/反序列化测试均已覆盖）
- [x] 每个阶段完成后运行对应 Rust 测试；涉及跨 crate 引用时至少验证依赖方能编译。（验证：阶段 1-8 均记录 `cargo test --manifest-path packages/sim-core/Cargo.toml` 通过；P0 未接入其他 crate，无跨 crate 引用变更）

## 阶段 1：crate 骨架与依赖边界

- 开始时间：2026-07-02 18:24:20 +08:00
- 结束时间：2026-07-02 18:30:43 +08:00
- 开发总结：新增 `packages/sim-core` 独立 Rust crate，完成最小模块骨架、schema version 常量和 crate 级定位说明。
- 验证记录：`cargo test --manifest-path packages/sim-core/Cargo.toml` 通过；`cargo fmt --manifest-path packages/sim-core/Cargo.toml -- --check` 通过。

- [x] 新建 `packages/sim-core/Cargo.toml`，包名、edition 和版本与仓库现有 Rust 包风格一致。（验证：`packages/sim-core/Cargo.toml` 定义 `name = "sim-core"`、`version = "0.1.0"`、`edition = "2024"`，对齐现有 `packages/authority-core/Cargo.toml` 风格）
- [x] 新建 `packages/sim-core/src/lib.rs`，导出 `math`、`ids`、`state`、`input`、`tick`、`hash` 等最小模块。（验证：`packages/sim-core/src/lib.rs` 包含 `pub mod math/ids/state/input/tick/hash`，对应占位文件均存在）
- [x] 在 `sim-core` 中定义 `SIM_CORE_SCHEMA_VERSION` 常量。（验证：`packages/sim-core/src/lib.rs` 定义 `pub const SIM_CORE_SCHEMA_VERSION: u16 = 1;`）
- [x] 限制初始依赖为 `serde`、`serde_json` 或更少；如引入额外依赖，记录原因。（验证：`packages/sim-core/Cargo.toml` 仅包含 `serde` 和 `serde_json` 依赖）
- [x] 增加 crate 级文档注释，说明 `sim` 表示 `simulation`，定位为确定性模拟核心。（验证：`packages/sim-core/src/lib.rs` 顶部 crate doc 说明 `sim` is short for `simulation` and deterministic simulation core）
- [x] 验证 `cargo test --manifest-path packages/sim-core/Cargo.toml` 可执行。（验证：命令通过，unit tests 和 doc-tests 均 ok）

## 阶段 2：定点数与向量数学

- 开始时间：2026-07-02 18:32:37 +08:00
- 结束时间：2026-07-02 18:42:26 +08:00
- 开发总结：实现 `Fp`、`Vec2Fp`、`QuantizedDir` 三类确定性数学基础，补齐定点构造、受控运算、方向校验、serde 校验反序列化和数学单元测试。
- 验证记录：`cargo test --manifest-path packages/sim-core/Cargo.toml` 通过，9 个测试通过；`cargo fmt --manifest-path packages/sim-core/Cargo.toml -- --check` 通过。

- [x] 实现 `Fp(i64)` 定点数类型，固定 `FP_SCALE = 1000`。（验证：`packages/sim-core/src/math.rs:11` 定义 `FP_SCALE = 1000`，`packages/sim-core/src/math.rs:18` 定义 `Fp(i64)`）
- [x] 实现 `Fp::from_raw`、`raw`、`from_i32`、`from_milli`、`to_f32_for_render`。（验证：`math.rs` 的 `fp_constructors_expose_raw_milli_units` 和 `render_conversion_is_read_only_boundary` 测试通过）
- [x] 实现受控加减乘除方法，明确截断方向和溢出处理策略。（验证：`math.rs:67` 实现 `mul_ratio`，`fp_checked_and_saturating_add_sub_are_explicit` 与 `fp_ratio_and_division_truncate_toward_zero` 测试通过）
- [x] 实现 `Vec2Fp`，包含加减、clamp、distance_squared 和 raw 访问。（验证：`packages/sim-core/src/math.rs:97` 定义 `Vec2Fp`，`math.rs:153` 实现 `distance_squared_raw`，`vec2_add_sub_clamp_and_distance_use_raw_units` 测试通过）
- [x] 实现 `QuantizedDir`，字段范围为 `-1000..=1000`。（验证：`packages/sim-core/src/math.rs:189` 定义私有字段 `QuantizedDir`，`quantized_dir_rejects_out_of_range_or_overlong_directions` 测试覆盖越界拒绝）
- [x] 实现方向合法性校验，拒绝长度平方超过 `1000 * 1000` 的方向。（验证：`QuantizedDir::new(1000, 1000)` 返回 `LengthSquaredTooLarge`，对应测试通过）
- [x] 增加水平、垂直、707 对角线方向的单元测试。（验证：`quantized_dir_accepts_horizontal_and_vertical_unit_directions` 和 `quantized_dir_accepts_707_diagonal_unit_directions` 测试通过）
- [x] 增加渲染转换只读测试，确保 `to_f32_for_render` 不参与核心推进 API。（验证：`packages/sim-core/src/math.rs:397` 的 `render_conversion_is_read_only_boundary` 测试确认转换后 raw 值不变）

## 阶段 3：ID、实体与世界状态

- 开始时间：2026-07-02 18:44:02 +08:00
- 结束时间：2026-07-02 18:50:45 +08:00
- 开发总结：新增 sim-core 的基础 ID newtype、实体状态模型、世界容器和稳定排序/查找能力，世界构造会拒绝重复 entity id。
- 验证记录：`cargo test --manifest-path packages/sim-core/Cargo.toml` 通过，14 个测试通过；`cargo fmt --manifest-path packages/sim-core/Cargo.toml -- --check` 通过。

- [x] 定义 `EntityId`、`FrameId`、`TeamId` 等基础类型或 newtype。（验证：`packages/sim-core/src/ids.rs:8`、`:23`、`:38` 分别定义 newtype，并有 `id_newtypes_expose_raw_values` 测试）
- [x] 定义 `EntityKind`，至少覆盖 Player、Npc、Monster、Projectile、Summon。（验证：`packages/sim-core/src/state.rs:10` 定义 5 类实体，`all_entity_kinds_are_representable` 测试通过）
- [x] 定义 `SimTransform`，包含 `pos`、`facing` 和 `radius`。（验证：`packages/sim-core/src/state.rs:19` 定义 `SimTransform { pos, facing, radius }`）
- [x] 定义 `MovementState` 最小字段，支持 Idle 和 Controlled。（验证：`packages/sim-core/src/state.rs:36` 定义 `MovementMode::Idle/Controlled`，`:43` 定义 `MovementState`）
- [x] 定义 `CombatState` 最小字段，包含 hp、max_hp 和基础属性占位。（验证：`packages/sim-core/src/state.rs:60` 定义 `CombatState { hp, max_hp, attack, defense, speed }`）
- [x] 定义 `SimEntity`，包含 entity id、kind、character id、team、transform、movement、combat、alive。（验证：`packages/sim-core/src/state.rs:69` 定义完整 `SimEntity` 字段）
- [x] 定义 `SimWorld`，包含 schema version、frame、rng state 占位和 entities。（验证：`packages/sim-core/src/state.rs:87` 定义 `SimWorld { schema_version, frame, rng, entities }`，`world_new_sets_schema_frame_rng_and_sorts_entities` 测试通过）
- [x] 增加实体按 `entity_id` 稳定排序的辅助方法。（验证：`packages/sim-core/src/state.rs:123` 和 `:163` 提供排序方法，世界构造排序测试通过）
- [x] 增加世界构造、实体查找和重复 entity id 拒绝测试。（验证：`world_new_sets_schema_frame_rng_and_sorts_entities`、`world_finds_entities_by_id`、`world_rejects_duplicate_entity_ids_after_sorting` 测试通过）

## 阶段 4：输入模型与命令结构

- 开始时间：2026-07-02 18:52:11 +08:00
- 结束时间：2026-07-02 18:59:29 +08:00
- 开发总结：新增帧输入、移动/停止/朝向/空命令模型，并提供稳定输入排序和同角色同帧移动输入选择辅助。
- 验证记录：`cargo test --manifest-path packages/sim-core/Cargo.toml` 通过，19 个测试通过；`cargo fmt --manifest-path packages/sim-core/Cargo.toml -- --check` 通过。

- [x] 定义 `SimInput`，包含 frame、character id、entity id、seq 和 input source。（验证：`packages/sim-core/src/input.rs:41` 定义 `SimInput { frame, character_id, entity_id, seq, source, command }`）
- [x] 定义 `SimInputSource`，区分 Real、SynthesizedEmpty、SynthesizedRepeatLast。（验证：`packages/sim-core/src/input.rs:9` 定义三类 input source）
- [x] 定义 `SimCommand`，至少包含 Move、Stop、Face、Noop。（验证：`packages/sim-core/src/input.rs:27` 定义 `SimCommand::Move/Stop/Face/Noop`）
- [x] 定义 `MoveCommand`，使用 `QuantizedDir` 和可选速度字段。（验证：`packages/sim-core/src/input.rs:16` 定义 `MoveCommand { dir, speed_per_second: Option<Fp> }`）
- [x] 定义 `FaceCommand`，使用 `QuantizedDir`。（验证：`packages/sim-core/src/input.rs:22` 定义 `FaceCommand { dir }`）
- [x] 实现同帧输入稳定排序规则：frame、character id、seq、original index。（验证：`packages/sim-core/src/input.rs:56` 定义排序 key，`ordered_inputs_sort_by_frame_character_seq_and_original_index` 测试通过）
- [x] 实现同角色同帧移动输入选择规则，seq 最大优先，seq 相同时 original index 最大优先。（验证：`packages/sim-core/src/input.rs:82` 实现 `select_latest_movement_inputs`，`movement_selection_uses_highest_seq_then_highest_original_index` 测试通过）
- [x] 增加重复输入、乱序输入、非法方向和空输入的单元测试。（验证：`movement_selection_*`、`ordered_inputs_sort_*`、`invalid_quantized_direction_is_rejected_during_deserialization`、`empty_input_slices_return_empty_results` 测试通过）

## 阶段 5：最小 tick 推进

- 开始时间：2026-07-02 19:00:58 +08:00
- 结束时间：2026-07-02 19:11:25 +08:00
- 开发总结：实现最小 `step` 流程、移动/停止/朝向命令应用、定点移动推进、边界 clamp、连续帧校验和占位 step 结果。
- 验证记录：`cargo test --manifest-path packages/sim-core/Cargo.toml` 通过，23 个测试通过；`cargo fmt --manifest-path packages/sim-core/Cargo.toml -- --check` 通过。

- [x] 定义 `SimConfig` 最小结构，包含 tick rate、默认移动速度和场景边界。（验证：`packages/sim-core/src/tick.rs:18` 定义 `SimConfig { tick_rate, default_move_speed_per_second, bounds }`）
- [x] 定义 `SimStepResult`，包含 frame、events 和 state hash 占位。（验证：`packages/sim-core/src/tick.rs:28` 定义 `SimStepResult { frame, events, state_hash }`，`packages/sim-core/src/hash.rs` 提供 `SimHash::placeholder`）
- [x] 实现 `step(world, frame, inputs, config)` 基础流程。（验证：`packages/sim-core/src/tick.rs:73` 定义 `step`，测试 `step_moves_in_a_straight_line_and_reuses_missing_movement_input` 通过）
- [x] 校验传入 frame 必须等于 `world.frame + 1`。（验证：`packages/sim-core/src/tick.rs:151` 返回 `StepError::NonSequentialFrame`，`step_rejects_non_sequential_frame_without_updating_world` 测试通过）
- [x] 将 Move、Stop、Face 输入应用到实体 movement / facing 状态。（验证：`packages/sim-core/src/tick.rs:90`、`:97`、`:108` 分别处理 Move、Stop、Face，直线移动/停止测试通过）
- [x] 使用定点公式推进 Controlled 移动。（验证：`packages/sim-core/src/tick.rs:180` 推进 Controlled 实体，`step_moves_in_a_straight_line_and_reuses_missing_movement_input` 测试验证 60 tick 下 6 unit/s 每帧移动 100 raw）
- [x] 对位置执行场景边界 clamp。（验证：`packages/sim-core/src/tick.rs:199`、`:204` 调用 clamp，`step_clamps_position_to_scene_bounds` 测试通过）
- [x] 更新 `world.frame`。（验证：`packages/sim-core/src/tick.rs` 在 step 成功后设置 `world.frame = frame`，直线移动测试断言 frame 更新）
- [x] 对缺失输入保持上一帧 movement 状态，除非 Stop 已生效。（验证：`step_moves_in_a_straight_line_and_reuses_missing_movement_input` 覆盖缺失输入继续移动，`step_stop_keeps_entity_idle_until_a_new_move_input_arrives` 覆盖 Stop 后保持 Idle）
- [x] 增加直线移动、停止、边界 clamp 和 frame 不连续拒绝测试。（验证：`step_moves_in_a_straight_line_and_reuses_missing_movement_input`、`step_stop_keeps_entity_idle_until_a_new_move_input_arrives`、`step_clamps_position_to_scene_bounds`、`step_rejects_non_sequential_frame_without_updating_world` 均通过）

## 阶段 6：状态 hash

- 开始时间：2026-07-02 19:13:07 +08:00
- 结束时间：2026-07-02 19:18:54 +08:00
- 开发总结：实现稳定状态 hash，覆盖现有 world schema/frame/rng/entity/movement/combat 状态，并让 `step` 成功结果返回真实 hash。
- 验证记录：`cargo test --manifest-path packages/sim-core/Cargo.toml` 通过，27 个测试通过；`cargo fmt --manifest-path packages/sim-core/Cargo.toml -- --check` 通过。

- [x] 定义 `SimHash { frame, value }`。（验证：`packages/sim-core/src/hash.rs:8` 定义 `SimHash { frame, value }`）
- [x] 实现稳定 hash 算法，避免使用平台默认 hasher。（验证：`packages/sim-core/src/hash.rs:88` 定义固定 FNV-1a `StableHasher`，源码未使用 `DefaultHasher`）
- [x] hash 输入覆盖 schema version、frame、config hash 或 config version、实体基础状态、位置 raw、朝向 raw、移动状态和 hp。（验证：`packages/sim-core/src/hash.rs:23` 的 `hash_world` 覆盖 schema/frame/rng；`hash_entity` 覆盖 id/kind/team/owner/alive/pos/facing/radius/movement/combat；当前 world 尚无 config 字段，未伪造 config hash）
- [x] hash 前按 `entity_id` 升序遍历实体。（验证：`packages/sim-core/src/hash.rs` 在 `hash_world` 中按 `entity.id` 排序，`entity_vec_order_does_not_change_hash` 测试通过）
- [x] 确认 hash 不包含日志、渲染、HashMap 内存顺序或 transient debug 字段。（验证：`hash.rs` 仅写入显式 world/entity 字段和固定字节序；不读取日志、渲染值、HashMap 或 debug 字段）
- [x] 增加同状态 hash 相同、实体顺序不同 hash 相同、位置变化 hash 不同的测试。（验证：`same_state_hashes_the_same`、`entity_vec_order_does_not_change_hash`、`position_change_changes_hash` 测试通过）
- [x] 增加多帧推进 hash 稳定性测试。（验证：`packages/sim-core/src/tick.rs:330` 的 `step_hash_is_stable_across_matching_worlds_and_frames` 测试通过）

## 阶段 7：序列化与快照骨架

- 开始时间：2026-07-02 19:20:23 +08:00
- 结束时间：2026-07-02 19:27:16 +08:00
- 开发总结：新增 snapshot 模块，提供确定性 world 快照、restore 校验和 JSON roundtrip 测试，并在模块边界明确排除渲染/网络/资源状态。
- 验证记录：`cargo test --manifest-path packages/sim-core/Cargo.toml` 通过，31 个测试通过；`cargo fmt --manifest-path packages/sim-core/Cargo.toml -- --check` 通过。

- [x] 为核心 snapshot 类型补充 `Serialize` / `Deserialize`。（验证：`packages/sim-core/src/snapshot.rs:16` 的 `SimSnapshot` derive `Serialize` / `Deserialize`，JSON roundtrip 测试通过）
- [x] 定义 `SimSnapshot`，包含 schema version、frame、world state 和 hash。（验证：`packages/sim-core/src/snapshot.rs:16` 定义 `schema_version`、`frame`、`world`、`hash` 字段）
- [x] 实现 `snapshot(world, config)`。（验证：`packages/sim-core/src/snapshot.rs:83` 实现 `snapshot(world, _config)` 并生成 `hash_world(world)`）
- [x] 实现 `restore(snapshot)`，校验 schema version。（验证：`packages/sim-core/src/snapshot.rs:94` 实现 restore，校验 snapshot schema、world schema、frame 和 hash）
- [x] 增加 snapshot roundtrip 测试。（验证：`snapshot_roundtrips_through_json_and_restores_world` 测试通过）
- [x] 增加 schema version 不匹配时失败的测试。（验证：`restore_rejects_unsupported_snapshot_schema_version` 和 `restore_rejects_world_schema_mismatch` 测试通过）
- [x] 明确 snapshot 不包含渲染状态、网络连接状态或外部资源路径。（验证：`packages/sim-core/src/snapshot.rs:4` 模块注释明确 excludes rendering state、network connection state、handles、external resource paths）

## 阶段 8：文档与对外 API 收口

- 开始时间：2026-07-02 19:28:35 +08:00
- 结束时间：2026-07-02 19:38:42 +08:00
- 开发总结：收口 `sim-core` P0 对外 API，新增顶层 re-export，补充关键 public 类型/函数文档注释，并在 crate doc 中明确 P0 能力边界。
- 验证记录：`cargo test --manifest-path packages/sim-core/Cargo.toml` 通过，31 个测试通过；`cargo fmt --manifest-path packages/sim-core/Cargo.toml -- --check` 通过。

- [x] 为 `Fp`、`Vec2Fp`、`QuantizedDir`、`SimWorld`、`SimInput`、`step` 补充必要文档注释。（验证：`packages/sim-core/src/math.rs` 覆盖 `Fp`/`Vec2Fp`/`QuantizedDir`，`state.rs` 覆盖 `SimWorld`，`input.rs` 覆盖 `SimInput`，`tick.rs` 覆盖 `step`）
- [x] 在 `lib.rs` 中整理 public exports，避免外部依赖内部模块路径。（验证：`packages/sim-core/src/lib.rs` 顶层 re-export `Fp`、`Vec2Fp`、`QuantizedDir`、`SimWorld`、`SimInput`、`step`、`SimConfig`、`SimSnapshot` 等 P0 API）
- [x] 检查 public API 命名是否与 [共享帧同步移动战斗核心设计](../docs/游戏服与接入层/共享帧同步移动战斗核心设计.md) 保持一致。（验证：顶层 API 沿用设计中的 `sim-core`、`Fp`、`Vec2Fp`、`QuantizedDir`、`SimWorld`、`SimInput`、`step`、`SimHash`、`SimSnapshot` 命名）
- [x] 记录 P0 不支持战斗、碰撞、服务端接入和客户端接入。（验证：`packages/sim-core/src/lib.rs` crate doc 明确 P0 不实现 full combat、entity/map collision、server room policy integration、Bevy scene/client integration）
- [x] 运行 `cargo test --manifest-path packages/sim-core/Cargo.toml`。（验证：31 个 unit tests 和 doc-tests 通过）
- [x] 如仓库有格式化要求，运行对应 Rust format 检查。（验证：`cargo fmt --manifest-path packages/sim-core/Cargo.toml -- --check` 通过）

## 最终完成定义

以下项目作为 P0 整体完成标准，由所有阶段完成后统一验收。

- 开始时间：2026-07-02 19:40:38 +08:00
- 结束时间：2026-07-02 19:41:02 +08:00
- 验收总结：`packages/sim-core` P0 已形成可测试的独立确定性模拟核心，覆盖定点数、世界状态、帧输入、最小移动 tick、稳定 hash、snapshot 和对外 API 收口；完整 Rust 测试与格式检查通过。

- [x] `packages/sim-core` 独立 crate 存在且可测试。（验证：`packages/sim-core/Cargo.toml` 存在；`cargo test --manifest-path packages/sim-core/Cargo.toml` 通过 31 个 unit tests 和 doc-tests）
- [x] 最小模拟核心全程使用定点数和整数推进状态。（验证：`packages/sim-core/src/math.rs` 定义 `Fp`/`Vec2Fp`/`QuantizedDir`，`packages/sim-core/src/tick.rs` 使用 `FP_SCALE` 与 raw 定点值推进移动）
- [x] 同一初始状态和输入多次运行得到相同 hash。（验证：`packages/sim-core/src/hash.rs` 的 `same_state_hashes_the_same` 和 `packages/sim-core/src/tick.rs` 的 `step_hash_is_stable_across_matching_worlds_and_frames` 测试通过）
- [x] 核心 API 足以支撑 P1 移动离线验证。（验证：`packages/sim-core/src/lib.rs` 顶层导出 `SimWorld`、`SimInput`、`SimConfig`、`step`、`hash_world`、`snapshot`、`restore` 等离线推进/校验 API）
- [x] 文档明确 P0 边界和后续阶段关系。（验证：`packages/sim-core/src/lib.rs` crate doc 明确 P0 支持范围，并排除完整战斗、碰撞、服务端 room policy 和 Bevy 客户端接入）
