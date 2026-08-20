# 共享帧同步 P2 基础战斗核心 Checklist

## 目标

在 P0/P1 的 `sim-core` 定点移动和 offline 验证基础上，补齐最小战斗闭环：技能、冷却、命中、伤害、治疗、Buff / Dot、战斗事件和战斗 hash。P2 完成后，移动与基础战斗都能通过 `tools/lockstep-client` offline scenario 验证双端一致。

本阶段不做完整 ECS SOA 优化，不接入 `game-server`，不修改外部 `mybevy`，不做技能编辑器、复杂 AI、投射物和生产级 AOI。

## 基础原则

- [x] 战斗距离、范围、半径和位移继续使用 `Fp` / `Vec2Fp`。（验证：`SkillDefinition.cast_range`、`SkillTarget::Position`、`SimTransform.pos/radius` 和移动/命中距离计算均使用 `Fp` / `Vec2Fp`；`f32` 仅保留在渲染转换边界）
- [x] 伤害、治疗、冷却、Buff 层数和持续时间使用整数或 frame。（验证：伤害/治疗为 `i32`，冷却和 Buff duration / interval 为 `u32` frame，Buff stacks 为 `u16`）
- [x] 目标选择、事件输出和 hash 输入必须稳定排序。（验证：AOE 目标按 `(distance_squared, entity_id)` 排序，事件按固定 sort key 排序，`hash_world` 按 entity id 排序并写入原始整数值）
- [x] 客户端输入只表达释放意图，不包含命中结果或伤害值。（验证：`CastSkillCommand` 仅包含 `skill_id` 和 `target`，命中、伤害、治疗和 Buff 结算均在 `step` 内完成）
- [x] 所有新增战斗能力都必须有 offline scenario 或单元测试覆盖。（验证：阶段 1-10 已记录 `sim-core` 单测、`lockstep-client` 单测，以及 `melee_hit`、`aoe_hit`、`skill_cooldown`、`heal_cap`、`buff_dot` 等战斗 scenario）

## 阶段 1：战斗配置模型

- 开始时间：2026-07-03 14:13:56 +08:00
- 结束时间：2026-07-03 14:46:09 +08:00
- 开发总结：新增 `combat` 配置模块，定义技能、Buff、效果、伤害公式和 `CombatConfig`，并将 `SimConfig` 扩展为包含默认空战斗配置；catalog 构造与反序列化按 id 排序，配置校验覆盖重复 id、非法数值和未知 Buff 引用。
- 验证记录：`cargo fmt --manifest-path packages/sim-core/Cargo.toml -- --check` 通过；`cargo test --manifest-path packages/sim-core/Cargo.toml` 通过，58 个测试通过；`cargo test --manifest-path tools/lockstep-client/Cargo.toml` 通过，12 个测试通过。

- [x] 定义 `SkillCatalog` 和 `SkillDefinition`。（验证：`packages/sim-core/src/combat.rs:93` 定义 `SkillCatalog`，`packages/sim-core/src/combat.rs:143` 定义 `SkillDefinition`）
- [x] 技能定义包含 id、cooldown_frames、cast_range、target_type 和 effects。（验证：`packages/sim-core/src/combat.rs:144`-`148` 定义 `id`、`cooldown_frames`、`cast_range`、`target_type`、`effects` 字段）
- [x] 定义 `BuffCatalog` 和 `BuffDefinition`。（验证：`packages/sim-core/src/combat.rs:185` 定义 `BuffCatalog`，`packages/sim-core/src/combat.rs:235` 定义 `BuffDefinition`）
- [x] Buff 定义包含 id、duration_frames、interval_frames、max_stacks 和 effects。（验证：`packages/sim-core/src/combat.rs:236`-`240` 定义 `id`、`duration_frames`、`interval_frames`、`max_stacks`、`effects` 字段）
- [x] 定义 `DamageFormula`，至少支持 Fixed、Scaling、TrueDamage。（验证：`packages/sim-core/src/combat.rs:280`-`283` 定义 `Fixed`、`Scaling { base, attack_scale_bps }`、`TrueDamage`）
- [x] 所有距离和范围字段使用 `Fp`。（验证：`packages/sim-core/src/combat.rs:146` 的 `SkillDefinition.cast_range` 类型为 `Fp`，未新增浮点范围字段）
- [x] 增加技能配置重复 id、非法范围、非法冷却、未知 effect 的校验测试。（验证：`skill_catalog_rejects_duplicate_id`、`skill_validation_rejects_negative_cast_range`、`skill_validation_rejects_zero_cooldown`、`combat_config_rejects_skill_effect_referencing_unknown_buff`、`serde_rejects_unknown_effect_type` 均在 `packages/sim-core/src/combat.rs`，`cargo test --manifest-path packages/sim-core/Cargo.toml` 通过）

## 阶段 2：战斗状态与技能槽

- 开始时间：2026-07-03 14:55:03 +08:00
- 结束时间：2026-07-03 15:06:36 +08:00
- 开发总结：扩展实体战斗状态，新增暴击字段、技能槽和 Buff slot；`step` 每帧推进后递减技能冷却和 Buff 计时，并清理过期 Buff，同时将新增运行时状态纳入当前 state hash。
- 验证记录：`cargo fmt --manifest-path packages/sim-core/Cargo.toml -- --check` 通过；`cargo test --manifest-path packages/sim-core/Cargo.toml` 通过，61 个测试通过；`cargo fmt --manifest-path tools/lockstep-client/Cargo.toml -- --check` 通过；`cargo test --manifest-path tools/lockstep-client/Cargo.toml` 通过，12 个测试通过。

- [x] 扩展 `CombatState`，包含 attack、defense、crit_rate_bps、crit_damage_bps。（验证：`packages/sim-core/src/state.rs:62`-`70` 定义 `attack`、`defense`、`crit_rate_bps`、`crit_damage_bps`）
- [x] 定义 `SkillSlot`，包含 skill_id 和 cooldown_remaining。（验证：`packages/sim-core/src/state.rs:74`-`78` 定义 `SkillSlot { skill_id, cooldown_remaining }`）
- [x] 定义 `BuffSlot`，包含 buff_id、duration_remaining、interval_remaining、stacks 和 source_entity。（验证：`packages/sim-core/src/state.rs:80`-`87` 定义 `BuffSlot` 全部字段）
- [x] 支持实体初始化时携带技能 slot。（验证：`CombatState.skill_slots` 存储在 `SimEntity.combat` 中，`combat_state_stores_skill_slots_and_buff_slots` 在 `packages/sim-core/src/state.rs:318`-`338` 覆盖初始化与读取）
- [x] 实现每帧冷却递减。（验证：`packages/sim-core/src/tick.rs:423`-`426` 使用 `saturating_sub(1)` 递减技能冷却，`step_keeps_initial_skill_slots_and_decrements_cooldowns` 覆盖 2 帧递减到 0）
- [x] 实现 Buff duration 和 interval 递减。（验证：`packages/sim-core/src/tick.rs:428`-`430` 递减 Buff duration/interval，`step_decrements_buff_timers_and_removes_expired_slots` 覆盖 duration 与 interval 变化）
- [x] 增加技能槽初始化、冷却递减、Buff slot 清理测试。（验证：`combat_state_stores_skill_slots_and_buff_slots`、`step_keeps_initial_skill_slots_and_decrements_cooldowns`、`step_decrements_buff_timers_and_removes_expired_slots` 均通过）

## 阶段 3：战斗输入模型

- 开始时间：2026-07-03 15:08:51 +08:00
- 结束时间：2026-07-03 15:44:40 +08:00
- 开发总结：新增技能释放输入模型和 `CastSkill` 命令，复用稳定输入选择规则选取同角色同帧最新技能释放意图，并在 `validate_step` 中校验未知技能、未装备、冷却中和 target 类型不匹配，阶段内不结算命中或伤害。
- 验证记录：`cargo fmt --manifest-path packages/sim-core/Cargo.toml -- --check` 通过；`cargo test --manifest-path packages/sim-core/Cargo.toml` 通过，68 个测试通过；`cargo fmt --manifest-path tools/lockstep-client/Cargo.toml -- --check` 通过；`cargo test --manifest-path tools/lockstep-client/Cargo.toml` 通过，12 个测试通过。

- [x] 扩展 `SimCommand::CastSkill`。（验证：`packages/sim-core/src/input.rs:47`-`52` 定义 `SimCommand::CastSkill(CastSkillCommand)` 并未在输入中携带命中或伤害结果）
- [x] 定义 `CastSkillCommand`，包含 skill_id 和 target。（验证：`packages/sim-core/src/input.rs:33`-`36` 定义 `skill_id` 和 `target`）
- [x] 定义 `SkillTarget`，支持 None、Entity、Position、Direction。（验证：`packages/sim-core/src/input.rs:39`-`43` 定义四种 target）
- [x] 校验技能 id 必须存在于实体技能槽中。（验证：`packages/sim-core/src/tick.rs:481`-`498` 分别校验 `config.combat.skills` 和实体 `skill_slots`，`step_rejects_unknown_skill_without_updating_world`、`step_rejects_unequipped_skill_without_updating_world` 测试通过）
- [x] 校验冷却中技能不能释放。（验证：`packages/sim-core/src/tick.rs:500`-`506` 返回 `StepError::SkillOnCooldown`，`step_rejects_skill_on_cooldown_without_updating_world` 测试通过）
- [x] 校验 target type 与技能定义匹配。（验证：`packages/sim-core/src/tick.rs:508`-`527` 校验 `SkillTarget` 与 `SkillTargetType`，`step_rejects_mismatched_skill_target_type_without_updating_world` 测试通过）
- [x] 同一角色同一帧多条技能输入按 seq 最大选择。（验证：`packages/sim-core/src/input.rs:116`-`150` 实现 `select_latest_cast_skill_inputs`，`cast_skill_selection_uses_highest_seq_then_highest_original_index` 和 `step_validates_latest_cast_skill_input_per_character` 测试通过）
- [x] 增加未知技能、未装备技能、冷却中释放、target 类型不匹配测试。（验证：`packages/sim-core/src/tick.rs:916`-`1016` 覆盖未知技能、未装备、冷却中、target 类型不匹配且 world 不推进）

## 阶段 4：命中与目标选择

- 开始时间：2026-07-03 16:28:35 +08:00
- 结束时间：2026-07-03 16:45:16 +08:00
- 开发总结：在技能释放校验阶段加入 Entity target 和 Position AOE 的确定性目标解析，使用 fixed raw 距离平方判断命中范围，按 team/self/alive 过滤候选，并按距离与实体 id 稳定排序 AOE 结果。
- 验证记录：`cargo fmt --manifest-path packages/sim-core/Cargo.toml -- --check` 通过；`cargo test --manifest-path packages/sim-core/Cargo.toml` 通过，74 个测试通过；`cargo test --manifest-path tools/lockstep-client/Cargo.toml` 通过，12 个测试通过。

- [x] 实现 Entity target 距离校验。（验证：`packages/sim-core/src/tick.rs:630`-`676` 的 `resolve_entity_skill_target` 校验目标实体、过滤规则和距离范围）
- [x] 实现 Position target AOE 候选搜索。（验证：`packages/sim-core/src/tick.rs:678`-`707` 的 `resolve_position_skill_targets` 遍历候选并返回命中实体列表）
- [x] 命中距离使用 `distance_squared <= range * range`。（验证：`packages/sim-core/src/tick.rs:652`-`667` 和 `694`-`698` 使用 `distance_squared` 与 `range_squared` 比较，`skill_target_distance_squared` 基于 `Vec2Fp::distance_squared_raw`）
- [x] AOE 候选按 distance asc、entity_id asc 稳定排序。（验证：`packages/sim-core/src/tick.rs:705` 使用 `(distance_squared, entity_id)` 排序，`resolve_position_target_aoe_filters_and_sorts_candidates` 断言同距离按 id 排序）
- [x] 支持 team 过滤，避免默认命中友方或自己。（验证：`packages/sim-core/src/tick.rs:722`-`735` 过滤死亡、自身、友敌队伍，`step_rejects_enemy_skill_against_same_team_target_without_updating_world` 和 `resolve_entity_targets_apply_ally_any_and_self_filters` 测试通过）
- [x] 明确死亡实体不可作为有效目标。（验证：`packages/sim-core/src/tick.rs:727` 排除 `!candidate.alive`，`step_rejects_dead_entity_target_without_updating_world` 测试通过）
- [x] 增加近战命中、近战超距、AOE 多目标排序、死亡目标忽略测试。（验证：`step_accepts_entity_target_at_melee_range_boundary`、`step_rejects_entity_target_out_of_melee_range_without_updating_world`、`resolve_position_target_aoe_filters_and_sorts_candidates`、`step_rejects_dead_entity_target_without_updating_world` 均通过）

## 阶段 5：伤害与治疗结算

- 开始时间：2026-07-03 16:47:19 +08:00
- 结束时间：2026-07-03 17:11:41 +08:00
- 开发总结：`step` 对合法 `CastSkill` 执行基础伤害与治疗结算，支持 Fixed、Scaling、TrueDamage、整数防御减伤、治疗封顶、死亡状态更新和技能冷却写入；同帧结算顺序固定为技能输入选择顺序、目标顺序、效果 vector 顺序。
- 验证记录：`cargo fmt --manifest-path packages/sim-core/Cargo.toml -- --check` 通过；`cargo test --manifest-path packages/sim-core/Cargo.toml` 通过，83 个测试通过；`cargo test --manifest-path tools/lockstep-client/Cargo.toml` 通过，12 个测试通过。

- [x] 实现 Fixed 伤害。（验证：`packages/sim-core/src/tick.rs:832`-`842` 计算 Fixed 公式，`step_applies_fixed_damage_after_defense_reduction` 测试通过）
- [x] 实现 Scaling 伤害，比例使用 basis points。（验证：`packages/sim-core/src/tick.rs:835`-`840` 使用 `attack_scale_bps / 10000` 整数公式，`step_applies_scaling_damage_with_basis_points_and_integer_truncation` 测试通过）
- [x] 实现 TrueDamage。（验证：`packages/sim-core/src/tick.rs:807`-`815` 对 `TrueDamage` 跳过防御减伤，`step_applies_true_damage_without_defense_reduction` 测试通过）
- [x] 实现防御减伤公式，避免浮点。（验证：`packages/sim-core/src/tick.rs:868`-`874` 使用纯整数 `max(0, raw_damage - max(0, defense))`，`step_clamps_defense_reduced_damage_to_zero` 测试通过）
- [x] 实现治疗效果，不能超过 max_hp。（验证：`packages/sim-core/src/tick.rs:877`-`898` 对治疗 clamp 到 `max_hp`，`step_applies_heal_without_exceeding_max_hp` 测试通过）
- [x] 实现 hp 归零后 alive=false。（验证：`packages/sim-core/src/tick.rs:860`-`864` 和 `kill_entity` 将 hp 置 0 且 `alive=false`，`step_sets_alive_false_and_clamps_hp_to_zero_when_damage_kills` 测试通过）
- [x] 明确同一帧多个伤害事件的排序。（验证：`packages/sim-core/src/tick.rs:430`-`433` 注释固定 selected cast input、resolved target、effect vector 顺序，`step_applies_same_frame_casts_in_selected_input_order` 和 `step_applies_skill_effects_in_vector_order` 测试通过）
- [x] 增加固定伤害、缩放伤害、真实伤害、防御减伤、治疗封顶、死亡测试。（验证：`packages/sim-core/src/tick.rs:1375`-`1575` 覆盖固定伤害、Scaling bps、真实伤害、防御减伤、治疗封顶、不复活死亡实体和致死）

## 阶段 6：Buff 与 Dot 最小闭环

- 开始时间：2026-07-03 17:13:32 +08:00
- 结束时间：2026-07-03 17:42:24 +08:00
- 开发总结：实现 `AddBuff`、Buff 叠层/刷新、Dot/Hot 周期效果和过期清理；固定同一目标上 `(buff_id, source_entity)` 合并规则，Buff tick 按层数倍数结算，并加入最小 `BuffTick` / `BuffExpired` 事件用于确定性验证。
- 验证记录：`cargo fmt --manifest-path packages/sim-core/Cargo.toml -- --check` 通过；`cargo test --manifest-path packages/sim-core/Cargo.toml` 通过，90 个测试通过；`cargo test --manifest-path tools/lockstep-client/Cargo.toml` 通过，12 个测试通过。

- [x] 实现 AddBuff effect。（验证：`packages/sim-core/src/tick.rs:867`-`918` 对 `CombatEffect::AddBuff` 调用 `add_or_refresh_buff` 并写入 `BuffSlot`）
- [x] 支持 Buff 层数上限。（验证：`packages/sim-core/src/tick.rs:904`-`907` 将 stacks 增加后 clamp 到 `max_stacks`，`step_reapplies_same_source_buff_refreshes_duration_interval_and_caps_stacks` 测试通过）
- [x] 支持重复添加刷新持续时间或叠层，规则必须固定。（验证：`packages/sim-core/src/tick.rs:902`-`909` 注释并实现同 `(target, buff_id, source)` 刷新 duration/interval 且叠层，`step_reapplies_same_source_buff_refreshes_duration_interval_and_caps_stacks` 测试通过）
- [x] 实现 Dot periodic damage。（验证：`packages/sim-core/src/tick.rs:1186`-`1228` 在 Buff tick 时应用 Damage effect，`step_applies_dot_periodic_damage_multiplied_by_stacks` 测试通过）
- [x] 实现 Hot periodic heal。（验证：`packages/sim-core/src/tick.rs:1186`-`1228` 在 Buff tick 时应用 Heal effect，`step_applies_hot_periodic_heal_multiplied_by_stacks_and_clamped_to_max_hp` 测试通过）
- [x] Buff interval 到期时产生确定性事件。（验证：`packages/sim-core/src/tick.rs:1138`-`1150` 生成 `SimEvent::BuffTick`，`step_emits_buff_tick_events_by_entity_id_then_slot_order` 验证实体 id 与 slot 顺序）
- [x] Buff duration 到期时清理 slot 并产生可选事件。（验证：`packages/sim-core/src/tick.rs:1157`-`1169` 生成 `SimEvent::BuffExpired` 并移除 slot，`step_decrements_buff_timers_and_removes_expired_slots` 测试通过）
- [x] 增加 Buff 添加、叠层上限、刷新、Dot tick、Hot tick、过期清理测试。（验证：`step_adds_buff_from_skill_and_decrements_new_slot_same_frame`、`step_reapplies_same_source_buff_refreshes_duration_interval_and_caps_stacks`、`step_applies_dot_periodic_damage_multiplied_by_stacks`、`step_applies_hot_periodic_heal_multiplied_by_stacks_and_clamped_to_max_hp`、`step_decrements_buff_timers_and_removes_expired_slots` 均通过）

## 阶段 7：战斗事件模型

- 开始时间：2026-07-03 17:44:44 +08:00
- 结束时间：2026-07-03 18:21:26 +08:00
- 开发总结：扩展 `SimEvent` 为完整基础战斗事件输出，覆盖技能释放、伤害、治疗、Buff 应用 / tick / 过期和死亡；事件携带 frame、来源、目标、技能 / Buff 标识、value 与 sequence，并在返回前按固定 key 稳定排序，同时明确事件只作为本帧输出，不参与 `hash_world`。
- 验证记录：`cargo fmt --manifest-path packages/sim-core/Cargo.toml -- --check` 通过；`cargo test --manifest-path packages/sim-core/Cargo.toml` 通过，92 个测试通过；`cargo test --manifest-path tools/lockstep-client/Cargo.toml` 通过，12 个测试通过。

- [x] 定义 `SimEvent`，至少包含 SkillCast、DamageApplied、HealApplied、BuffApplied、BuffExpired、EntityDied。（验证：`packages/sim-core/src/tick.rs:433`-`485` 定义 `SkillCast`、`DamageApplied`、`HealApplied`、`BuffApplied`、`BuffExpired`、`EntityDied`，并保留 `BuffTick`）
- [x] 事件字段包含 frame、source_entity、target_entity、skill_id / buff_id、value。（验证：`packages/sim-core/src/tick.rs:434`-`493` 的事件字段覆盖 frame、source_entity、target_entity、skill_id / buff_id、value 与 sequence）
- [x] 事件输出按 frame、event_type、source、target、序号稳定排序。（验证：`packages/sim-core/src/tick.rs:506`-`517` 使用 `(frame, event_type_code, source_entity, target_entity, sequence)` 排序，`packages/sim-core/src/tick.rs:533`-`541` 固定事件类型编码）
- [x] `SimStepResult.events` 返回本帧事件。（验证：`packages/sim-core/src/tick.rs:496`-`502` 定义 `SimStepResult.events` 为本帧事件输出，`packages/sim-core/src/tick.rs:663`-`668` 在 `step` 返回 events）
- [x] 明确事件进入 hash 或只进入 event hash 的规则。（验证：`packages/sim-core/src/tick.rs:498`-`502` 注释明确 events 不进入 `hash_world`，`packages/sim-core/src/tick.rs:668` 仍以 `hash_world(world)` 计算 `state_hash`，`step_state_hash_comes_from_world_state_not_events` 测试通过）
- [x] 增加同一帧多事件排序测试。（验证：`packages/sim-core/src/tick.rs:2709` 的 `step_sorts_same_frame_events_by_type_source_target_and_sequence` 覆盖同帧 SkillCast、BuffTick、DamageApplied、HealApplied 排序）

## 阶段 8：战斗 hash 覆盖

- 开始时间：2026-07-03 18:24:08 +08:00
- 结束时间：2026-07-03 19:02:09 +08:00
- 开发总结：补齐战斗状态 hash 专项测试，确认 `hash_world` 覆盖 hp、max_hp、基础属性、技能冷却和 Buff slot 字段；Buff slot 按当前 vector slot 顺序写入 hash，使 hash 与 Buff tick 运行时顺序保持一致；新增测试确认释放技能输入不会作为 pending request 残留进下一帧 hash。
- 验证记录：`cargo fmt --manifest-path packages/sim-core/Cargo.toml -- --check` 通过；`cargo test --manifest-path packages/sim-core/Cargo.toml` 通过，98 个测试通过；`cargo test --manifest-path tools/lockstep-client/Cargo.toml` 通过，12 个测试通过。

- [x] 扩展状态 hash，覆盖 hp、max_hp、基础属性、技能冷却、Buff slot。（验证：`packages/sim-core/src/hash.rs:64`-`76` 写入 hp、max_hp、attack、defense、speed、crit、skill_id 和 cooldown_remaining，`packages/sim-core/src/hash.rs:78`-`85` 写入 Buff slot 字段）
- [x] 确认 Buff slot hash 按 buff_id、source_entity 或固定 slot index 稳定。（验证：`packages/sim-core/src/hash.rs:78`-`85` 按当前 Buff vector slot 顺序写入字段，`buff_slot_order_changes_hash_because_tick_order_uses_slot_order` 测试确认 slot 顺序不同 hash 不同）
- [x] 确认 pending skill request 不残留到下一帧 hash，除非设计要求。（验证：`packages/sim-core/src/tick.rs:2368` 的 `pending_cast_skill_request_does_not_affect_next_frame_hash` 覆盖释放技能后一帧空输入 hash 只反映 cooldown 世界状态）
- [x] 增加 hp 变化 hash 不同测试。（验证：`packages/sim-core/src/hash.rs:274` 的 `combat_hp_max_hp_and_base_stats_change_hash` 覆盖 hp 和 max_hp 改变 hash）
- [x] 增加技能冷却变化 hash 不同测试。（验证：`packages/sim-core/src/hash.rs:307` 的 `skill_slot_id_and_cooldown_change_hash` 覆盖 skill_id 与 cooldown_remaining 改变 hash）
- [x] 增加 Buff 变化 hash 不同测试。（验证：`packages/sim-core/src/hash.rs:324` 的 `buff_slot_fields_change_hash` 覆盖 buff_id、source_entity、duration_remaining、interval_remaining、stacks 改变 hash）
- [x] 增加实体顺序不同但战斗状态相同 hash 一致测试。（验证：`packages/sim-core/src/hash.rs:363` 的 `entity_vec_order_with_combat_state_does_not_change_hash` 覆盖实体 vector 顺序反转后 hash 一致）

## 阶段 9：战斗 scenario

- 开始时间：2026-07-03 19:23:20 +08:00
- 结束时间：2026-07-03 20:33:31 +08:00
- 开发总结：扩展 `lockstep-client` scenario schema，支持可选 combat 配置、初始战斗状态、`CastSkill` 输入、事件断言和预期 StepError；新增 6 个战斗 scenario 并写入真实 final hash，同时刷新旧正向移动 scenario 的 final hash，使移动与战斗场景均可 offline 回放验证。
- 验证记录：`cargo fmt --manifest-path tools/lockstep-client/Cargo.toml -- --check` 通过；`cargo test --manifest-path tools/lockstep-client/Cargo.toml` 通过，13 个测试通过；`cargo fmt --manifest-path packages/sim-core/Cargo.toml -- --check` 通过；`cargo test --manifest-path packages/sim-core/Cargo.toml` 通过，98 个测试通过；新增 6 个战斗 scenario 与旧正向移动 scenario `smoke`、`move_straight`、`move_stop`、`move_missing_input_continue`、`move_diagonal`、`move_boundary_clamp` 均通过 `cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode offline --scenario <name>`。

- [x] 扩展 scenario schema，支持技能配置和 Buff 配置。（验证：`tools/lockstep-client/src/scenario.rs:218` 定义 `ScenarioCombatConfig`，`tools/lockstep-client/src/scenario.rs:258` 定义技能配置，`tools/lockstep-client/src/scenario.rs:285` 定义 Buff 配置，`Scenario::to_sim_config` 转换为 `CombatConfig`）
- [x] 新增 `melee_hit.json`。（验证：`tools/lockstep-client/scenarios/melee_hit.json:77` 记录 final hash `c177c3e96f8c0ba8`，offline 运行通过）
- [x] 新增 `melee_out_of_range.json`。（验证：`tools/lockstep-client/scenarios/melee_out_of_range.json:90` 记录 final hash `7330eab0eeb1e8d4`，`tools/lockstep-client/scenarios/melee_out_of_range.json:91` 声明预期 `SkillTargetOutOfRange`，offline 运行通过）
- [x] 新增 `aoe_hit.json`。（验证：`tools/lockstep-client/scenarios/aoe_hit.json:90` 记录 final hash `786129b1d444f47f`，events 断言两个 `damageApplied`，offline 运行通过）
- [x] 新增 `skill_cooldown.json`。（验证：`tools/lockstep-client/scenarios/skill_cooldown.json:80` 记录 final hash `feb7cdd176ed00ff`，`tools/lockstep-client/scenarios/skill_cooldown.json:81` 声明预期 `SkillOnCooldown`，offline 运行通过）
- [x] 新增 `heal_cap.json`。（验证：`tools/lockstep-client/scenarios/heal_cap.json:60` 记录 final hash `d4b38fbea41e708d`，`tools/lockstep-client/scenarios/heal_cap.json:72` 断言实际治疗量 `15`，offline 运行通过）
- [x] 新增 `buff_dot.json`。（验证：`tools/lockstep-client/scenarios/buff_dot.json:81` 记录 final hash `34066c992f46cc52`，events 断言 `buffApplied`、`buffTick`、`damageApplied`、`buffExpired`，offline 运行通过）
- [x] 为每个 scenario 记录 final hash 和关键事件断言。（验证：六个新增 JSON 的 `assertions.finalHash` 均为非零真实 hash，`tools/lockstep-client/src/offline.rs:343` 的 `assert_events` 按子序列校验 `assertions.events`）
- [x] 使用 `tools/lockstep-client --mode offline` 跑通全部战斗 scenario。（验证：`melee_hit`、`melee_out_of_range`、`aoe_hit`、`skill_cooldown`、`heal_cap`、`buff_dot` 六条 `cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode offline --scenario <name>` 均通过）

## 阶段 10：战斗边界文档与旧实现对照

- 开始时间：2026-07-03 20:36:10 +08:00
- 结束时间：2026-07-03 20:39:11 +08:00
- 开发总结：在 checklist 阶段 10 内记录 P2 战斗核心边界：当前交付的是 `packages/sim-core` 的共享确定性基础战斗能力和 `tools/lockstep-client` offline scenario，不接入 `game-server`，不替换现有 `apps/game-server/src/core/system/combat` 与 `apps/game-server/src/gameroom/combat_demo`；旧 combat demo 暂时保留作为现有演示/联调实现。记录后续迁移风险：旧实现的 `Position`、`MoveState`、技能 `range`、AOE 半径、击退距离、目标距离比较和 combat demo 初始坐标仍使用 `f32`，迁移到共享核心时需要统一到 `Fp` / `Vec2Fp` 或明确转换边界，避免回放 hash、距离边界和跨平台浮点差异。明确 P2 不支持投射物、AI、复杂碰撞、生产级 AOI、技能编辑器和 Protobuf 专用战斗协议。
- 验证记录：`cargo test --manifest-path packages/sim-core/Cargo.toml` 通过，98 个测试通过；`cargo test --manifest-path tools/lockstep-client/Cargo.toml` 通过，13 个测试通过；全部正向移动和战斗 offline scenario `smoke`、`move_straight`、`move_stop`、`move_missing_input_continue`、`move_diagonal`、`move_boundary_clamp`、`melee_hit`、`melee_out_of_range`、`aoe_hit`、`skill_cooldown`、`heal_cap`、`buff_dot` 均通过。`move_invalid_input` 为负例，按阶段要求未作为成功 scenario 运行。

- [x] 在 checklist 或设计文档中记录 P2 战斗能力边界。（验证：本阶段 `开发总结` 已记录 P2 只交付共享确定性基础战斗核心与 offline scenario，不接入 `game-server`）
- [x] 记录 P2 与现有 `apps/game-server/src/core/system/combat` 的关系：先新增共享核心，不立即删除旧 combat demo。（验证：本阶段 `开发总结` 已记录旧 `combat` 与 `combat_demo` 暂时保留）
- [x] 标记现有 combat demo 中 `f32` 位置和范围后续迁移风险。（验证：`apps/game-server/src/core/system/combat/components.rs` 的 `Position`、`MoveState` 使用 `f32`，`apps/game-server/src/core/system/combat/skills.rs` 的 `range`、`aoe_radius`、`displacement_distance` 使用 `f32`，`apps/game-server/src/gameroom/combat_demo/mod.rs` 的初始坐标使用 `f32` 字面量）
- [x] 记录不支持投射物、AI、复杂碰撞、AOI、技能编辑器和 Protobuf 专用协议。（验证：本阶段 `开发总结` 已记录 P2 非目标）
- [x] 运行 `cargo test --manifest-path packages/sim-core/Cargo.toml`。（验证：98 个测试通过）
- [x] 运行 `cargo test --manifest-path tools/lockstep-client/Cargo.toml`。（验证：13 个测试通过）
- [x] 运行全部移动和战斗 offline scenario。（验证：`smoke`、`move_straight`、`move_stop`、`move_missing_input_continue`、`move_diagonal`、`move_boundary_clamp`、`melee_hit`、`melee_out_of_range`、`aoe_hit`、`skill_cooldown`、`heal_cap`、`buff_dot` 均通过）

## 最终完成定义

以下项目作为 P2 整体完成标准，由所有阶段完成后统一验收。

- 开始时间：2026-07-03 20:41:31 +08:00
- 结束时间：2026-07-03 20:41:31 +08:00
- 验收总结：P2 已完成最小基础战斗闭环：`packages/sim-core` 支持技能释放、目标选择、伤害、治疗、Buff / Dot、战斗事件和覆盖关键战斗状态的 hash；战斗距离和范围使用 `Fp` / `Vec2Fp`，伤害、治疗、冷却和 Buff 计时使用整数 / frame；`tools/lockstep-client` 已支持移动与战斗 scenario 双端 offline 回放、事件断言和预期错误场景。旧 `game-server` combat demo 保留，后续 P3 再接入共享核心。

- [x] `sim-core` 支持最小技能、命中、伤害、治疗、Buff / Dot 和战斗事件。（验证：阶段 3-7 已实现并提交，`cargo test --manifest-path packages/sim-core/Cargo.toml` 98 个测试通过）
- [x] 战斗计算不使用浮点参与状态推进。（验证：阶段 1/4/5/6 记录距离范围使用 `Fp` / `Vec2Fp`，伤害、治疗、冷却、Buff 计时使用整数或 frame）
- [x] 战斗状态和事件排序稳定。（验证：阶段 4 AOE 按 distance/entity_id 排序，阶段 7 事件按 frame/type/source/target/sequence 排序，相关测试通过）
- [x] 战斗 hash 覆盖 hp、技能冷却和 Buff 等关键状态。（验证：阶段 8 已覆盖 hp、max_hp、基础属性、技能冷却和 Buff slot，`cargo test --manifest-path packages/sim-core/Cargo.toml` 98 个测试通过）
- [x] `tools/lockstep-client` offline 可跑通移动和战斗 scenario。（验证：阶段 9/10 已跑通 `smoke`、`move_straight`、`move_stop`、`move_missing_input_continue`、`move_diagonal`、`move_boundary_clamp`、`melee_hit`、`melee_out_of_range`、`aoe_hit`、`skill_cooldown`、`heal_cap`、`buff_dot`）
- [x] P2 完成后具备进入 P3 `game-server` 接入的核心能力。（验证：阶段 10 已记录 P2 与旧 combat demo 的关系和后续 f32 迁移风险，P3 可在保留旧实现基础上接入共享核心）
