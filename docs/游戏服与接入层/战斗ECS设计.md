# 战斗系统 ECS 设计文档

## 1. 概述

本文档描述基于 ECS（Entity-Component-System）架构的战斗系统设计方案，采用 **SOA（Structure of Arrays）** 数据布局实现高性能帧同步游戏。

### 当前实现状态

当前代码已经落地了战斗 ECS 的可运行闭环：

- `core/system/combat/` 已实现 `RoomCombatEcs`、组件、技能、Buff、战斗事件、快照、输入解析和 CSV catalog。
- `SkillBase.csv` / `BufferBase.csv` 会在服务启动时加工成 `CsvCombatCatalog`，`combat_demo` 房间策略使用这份 catalog。
- `CombatDemoLogic` 已接入 `RoomLogic`，支持玩家入房生成实体、训练假人、技能输入、战斗 tick、事件广播和周期快照。
- 客户端输入仍走帧同步的 `PlayerInputReq`，战斗动作通过 `combat_cast_skill` / `combat_apply_buff` 解析。
- 当前战斗事件和快照通过 `GameMessagePush` 承载 JSON payload，并没有独立的战斗 Protobuf 消息。
- `tools/mock-client` 已有 `combat-dual-client` 场景用于双客户端战斗联调。

当前仍属于后续目标的部分：

- 技能/Buff CSV reload 后只更新配置快照，不会自动替换已构造的 `CsvCombatCatalog`。
- 还没有专用战斗协议、AOI 兴趣过滤、战斗状态持久化、技能编辑器或复杂场景碰撞接入。
- 当前实现重视固定容量上限和 SOA 遍历，但并不是严格“运行中零分配”的固定数组实现。

### 1.1 设计目标

- **确定性**：相同输入产生相同输出，保证帧同步一致性
- **高性能**：预分配内存 + SOA 布局 + 批量遍历
- **可扩展**：技能/Buff 配置化，后续可补 catalog 原子替换
- **低开销**：控制运行时分配，优先使用固定上限和连续数组布局

### 1.2 核心约束

| 参数 | 值 | 说明 |
|------|-----|------|
| MAX_ENTITIES | 2048 | 每房间最大实体数（含玩家、NPC、怪物、投射物、召唤物） |
| MAX_PLAYERS | 100 | 每房间最大玩家数目标值，当前由房间策略约束 |
| MAX_NPCS | 500 | 每房间最大 NPC / 怪物规模建议值 |
| MAX_PROJECTILES | 1000 | 投射物与短生命周期实体预算建议值 |
| MAX_SKILLS_PER_ENTITY | 8 | 每个实体最大技能槽数 |
| MAX_BUFFS_PER_ENTITY | 6 | 每个实体最大 Buff 槽数 |
| FPS | 30 | 帧率（帧同步基础） |

---

## 2. 数据布局

### 2.1 AOS vs SOA

传统 AOS（Array of Structures）布局：

```
Entity[0]: { pos_x, pos_y, hp, skill_id, ... }  ← cache miss
Entity[1]: { pos_x, pos_y, hp, skill_id, ... }  ← cache miss
```

SOA（Structure of Arrays）布局：

```
pos_x: [e0_x, e1_x, e2_x, ...]  ← 连续内存，一次加载多个
pos_y: [e0_y, e1_y, e2_y, ...]
hp:    [e0_hp, e1_hp, e2_hp, ...]
```

**优势**：
- CPU cache line 友好（一次加载可处理多个实体）
- 可尝试 SIMD 向量化优化
- 批量操作更高效

### 2.2 组件定义

组件设计目标是保持纯数据、少逻辑，把主要结算放在 System / ECS 层。当前代码使用普通 Rust struct / enum，并为快照与调试保留 `serde` 序列化能力：

```rust
// 位置组件 [x, y]
struct Position {
    x: f32,
    y: f32,
}

// 生命值组件
struct Health {
    current: i32,
    max: i32,
    base_max: i32,
};

// 技能槽组件
struct SkillSlot {
    skill_id: u16,
    cooldown_remaining: u16,  // 帧数，0=可用
};

// Buff 槽组件
struct BuffSlot {
    buff_id: u16,
    duration_remaining: u16,
    stacks: u8,
};

// 移动状态
struct MoveState {
    state_type: u8,   // 0=Idle, 1=Sliding, 2=Knockback
    start_x: f32,
    start_y: f32,
    target_x: f32,
    target_y: f32,
    progress: f32,   // 0.0 ~ 1.0
    speed: f32,
};
```

### 2.3 ECS 容器

```rust
pub struct RoomCombatEcs {
    // 实体元数据
    next_entity_id: EntityId,
    entities: Vec<EntityMeta>,

    // 战斗组件（SOA 布局）
    positions_x: Vec<f32>,
    positions_y: Vec<f32>,
    directions_x: Vec<f32>,
    directions_y: Vec<f32>,
    healths: Vec<Health>,
    base_stats: Vec<Stats>,
    move_states: Vec<MoveState>,

    // 技能组件
    skill_slots: Vec<[SkillSlot; 8]>,   // 每实体8个技能槽

    // Buff 组件
    buff_slots: Vec<[BuffSlot; 6]>,     // 每实体6个Buff槽

    // 映射表
    character_entity_map: HashMap<String, EntityId>,
    entity_index_map: HashMap<EntityId, DenseIndex>,
    index_entity_map: Vec<EntityId>,

    // 事件系统
    pending_events: Vec<CombatEvent>,
    pending_skill_requests: Vec<SkillCastRequest>,
}
```

### 2.4 实体标识与索引映射

当场景规模提升到 `100` 玩家，并同时存在 NPC / 怪物 / 投射物时，ECS 不建议直接使用 `character_id` 作为组件数组下标。

推荐拆成三层身份：

- `character_id`
  - 游戏内角色身份，仅玩家角色实体拥有
- `entity_id`
  - 统一实体身份，玩家 / NPC / 怪物 / 投射物都拥有
- `dense_index`
  - ECS 内部连续数组下标，专供 `positions_x/y`、`move_states` 等组件数组访问

推荐结构：

```rust
type EntityId = u32;
type DenseIndex = usize;

struct EntityMeta {
    entity_id: EntityId,
    entity_type: EntityType,
    character_id: Option<String>,
    alive: bool,
}

enum EntityType {
    Player,
    Npc,
    Monster,
    Projectile,
    Summon,
}

pub struct RoomCombatEcs {
    entities: Vec<EntityMeta>,
    positions: Vec<Position>,
    move_states: Vec<MoveState>,

    character_entity_map: HashMap<String, EntityId>,
    entity_index_map: HashMap<EntityId, DenseIndex>,
    index_entity_map: Vec<EntityId>,
}
```

访问路径应为：

```text
character_id -> entity_id -> dense_index -> move_states[dense_index]
```

这样可以同时满足：

- 玩家输入入口使用 `character_id`
- ECS tick 时仍可连续遍历 `Vec`
- NPC / 怪物 / 投射物可共享同一套组件结构
- 删除实体时可通过 `swap_remove` 维护紧凑数组

---

## 3. 技能系统

### 3.1 技能定义

技能配置当前主要来自 `SkillBase.csv`，服务启动时由 `CsvCombatCatalog::from_tables` 转成运行时定义；`BuiltinCombatCatalog` 只作为 fallback / 测试辅助。定义形态类似：

```rust
struct SkillDefinition {
    id: u16,
    code: String,
    name: String,
    description: String,
    cooldown_frames: u16,
    cast_frames: u16,
    range: f32,
    target_type: SkillTargetType,
    effects: Vec<SkillEffect>,
}

struct SkillEffect {
    effect_type: SkillEffectType,
    value: i32,
    buff_id: u16,
    buff_duration: u16,
    aoe_radius: f32,
}
```

### 3.2 示例技能模板

| ID | 名称 | 冷却 | 范围 | 效果 |
|----|------|------|------|------|
| 1 | 普通攻击 | 30帧(1s) | 50 | 伤害10 |
| 2 | 火球术 | 90帧(3s) | 300 | 伤害50 + AOE 30 |
| 3 | 治疗术 | 120帧(4s) | 200 | 治疗80 |
| 4 | 冲锋 | 60帧(2s) | 150 | 伤害20 + 击退100 |
| 5 | 灼烧 | 0 | 50 | 伤害5 + Dot 6秒 |

### 3.3 技能释放流程

```
请求 → 冷却检查 → 距离检查 → 消耗冷却 → 应用效果 → 产生事件
```

```rust
// 请求释放技能
fn request_skill(&mut self, request: SkillCastRequest) {
    self.pending_skill_requests.push(request);
}

// 每帧处理
fn process_skill_requests(&mut self) {
    for request in self.drain_skill_requests() {
        if self.can_cast_skill(...) {
            self.cast_skill(&request);
        }
    }
}
```

---

## 4. Buff 系统

### 4.1 Buff 定义

```rust
struct BuffDefinition {
    id: u16,
    code: String,
    name: String,
    description: String,
    buff_type: BuffType,     // Buff/Debuff/Dot/Hot
    max_stacks: u8,
    duration_frames: u16,
    interval_frames: u16,    // Dot/Hot 间隔
    effects: Vec<BuffEffect>,
    can_dispel: bool,
}
```

### 4.2 示例 Buff 模板

| ID | 名称 | 类型 | 层数 | 持续 | 效果 |
|----|------|------|------|------|------|
| 1 | 灼烧 | Dot | 1 | 180帧(6s) | 伤害5/30帧 |
| 2 | 护盾 | Buff | 1 | 300帧(10s) | 防御+5 |
| 3 | 减速 | Debuff | 3 | 120帧(4s) | 移速-20 |
| 4 | 攻击强化 | Buff | 5 | 180帧(6s) | 攻击+10 |
| 5 | 回复 | Hot | 1 | 180帧(6s) | 治疗/30帧 |

### 4.3 Dot/Hot 处理

当前实现不单独维护 `DotContext` 数组，而是在 `BuffSlot` 中保存 `duration_remaining`、`interval_remaining` 和 `stacks`，由 `tick_buffs` 按间隔应用 `DamagePeriodic` / `HealPeriodic` 效果。设计形态类似：

```rust
struct BuffSlot {
    buff_id: u16,
    duration_remaining: u16,
    stacks: u8,
    interval_remaining: u16,
}

fn tick_buffs(&mut self, frame_id: u32) {
    for slot in active_buff_slots {
        if slot.interval_remaining == 0 {
            apply_periodic_effect(slot.buff_id, frame_id);
            slot.interval_remaining = buff.interval_frames;
        }
    }
}
```

---

## 5. 伤害结算

### 5.1 伤害公式

```rust
enum DamageFormula {
    Fixed(i32),                          // 固定伤害
    Scaling { base: i32, attack_scale_bps: u16 },  // 缩放伤害
    TrueDamage(i32),                      // 真实伤害
}

// 简易减伤公式
fn apply_damage(&mut self, target: EntityIndex, base_damage: i32) {
    let defense = self.stats[target].defense;
    let damage_reduction = defense as f32 / (defense as f32 + 200.0);
    let final_damage = (base_damage as f32 * (1.0 - damage_reduction)) as i32;

    self.healths[target].take_damage(final_damage);
}
```

### 5.2 确定性保障

- 伤害随机浮动如需引入，应使用 `frame_id % N` 或房间固定种子，避免真随机
- 核心伤害结算尽量使用整数和 basis points 表达比例，减少浮点差异
- 服务器权威模式，客户端仅预表现

---

## 6. 位移系统

### 6.1 移动状态机

```rust
enum MoveState {
    Idle,                                // 静止
    Sliding { start, end, progress, speed },  // 滑行
    Knockback { start, dir, dist, progress, speed },  // 击退
}
```

### 6.2 位移更新

```rust
fn tick_movements(&mut self) {
    for i in 0..self.positions.len() {
        let state = &mut self.move_states[i];
        if !state.is_active() { continue; }

        // 进度增量
        let total_dist = distance(state.start, state.end);
        let progress_delta = state.speed / 30.0 / total_dist;
        state.progress = (state.progress + progress_delta).min(1.0);

        // 更新位置
        self.positions[i] = state.current_position();
    }
}
```

### 6.3 位移同步职责

位移系统建议采用：

- **输入帧同步** 作为主链路
- **权威位置校正** 作为纠偏链路

具体含义：

- 客户端每帧发送移动输入，不直接发送当前位置
- 服务端在 `on_tick()` 中处理输入并更新 `Position / MoveState`
- 平时通过 `FrameBundlePush` 同步输入
- 每 `N` 帧或超阈值时，通过单独快照消息校正权威位置

不建议一开始就：

- 每帧全量广播所有实体位置
- 让客户端直接上传权威位置

### 6.4 场景与阻挡校验

服务端位移结算必须结合场景配置做校验，至少包括：

- 可行走区域
- 障碍 / 阻挡
- 出生点
- 越界约束
- 冲刺 / 击退的特殊碰撞规则

也就是说，位移同步的核心不是“同步坐标”，而是：

```text
同步输入 + 服务端按场景规则结算位置 + 低频校正
```

### 6.5 广播建议

在 `100` 玩家 + NPC / 怪物场景下：

- 玩家位置校正建议每 `3-5` 帧发送一次
- NPC / 怪物位置校正建议每 `5-10` 帧发送一次
- 投射物优先事件化广播
- 后续必须接入 AOI / 兴趣管理，只同步视野内实体

---

## 7. 帧循环集成

### 7.1 RoomLogic 帧循环

当前 `combat_demo` 已按这个模式接入 `RoomLogic::on_tick`：先解析帧输入，再执行战斗命令和 tick，随后 drain 事件并按间隔推送快照。

```rust
impl RoomLogic {
    pub fn tick(&mut self, frame_id: u32) {
        // 1. 输入处理（已由 input_system 处理）

        // 2. 战斗系统 tick
        self.combat.tick_combat(frame_id);

        // 3. 位置同步
        self.tick_movements();

        // 4. 快照广播
        self.broadcast_frame_state(frame_id);
    }
}
```

### 7.2 事件广播

每帧收集战斗事件，当前通过 `GameMessagePush { event: "combat", action, payload_json }` 广播给客户端：

```rust
struct CombatEvent {
    event_type: u8,     // DAMAGE=1, HEAL=2, BUFF_APPLY=3, ...
    source_entity: u32,
    target_entity: u32,
    value: i32,
    extra: u16,
}

// 每帧 drain 并广播
let events = self.combat.drain_events();
for event in events {
    self.broadcast_to_all(event);
}
```

### 7.3 迁移状态边界

当前 `RoomCombatEcs` 的 `combat_state_json` 使用 `room-combat-ecs.v1`，导出所有 ECS 实体的基础战斗状态，包括玩家和 Monster 的 entity meta、位置、朝向、血量、基础属性、移动状态、技能冷却、Buff slot、角色实体映射和待处理技能请求。`pending_events` 不迁移，避免导入后重复广播已经产生过的副作用。

NPC / Monster 的非玩家运行态通过 `RoomLogicTransferState.npc_state_json` 承载，当前通用 schema 为 `room-transfer.npc-state.v1`。该契约可表达 entity id、entity kind、position、hp/max hp、target、threat/aggro、behavior node、blackboard/context、rng、path、wait timer 和技能冷却。`combat_demo` 目前只把 training dummy / Monster 导出为 demo 级 `training_dummy.idle`，并在导入时与 `combat_state_json` 恢复出的 ECS 实体做 entity id、类型、位置和血量一致性校验。

这仍是生产方向的运行态契约骨架，不代表完整行为树引擎、AI timer、路径推进或 RNG 状态已经恢复；后续真实 AI 系统接入时应在该 schema 下补齐具体字段含义和版本兼容策略。

---

## 8. 性能优化

### 8.1 内存预分配

```rust
impl RoomCombatEcs {
    pub fn new() -> Self {
        Self {
            entities: Vec::with_capacity(MAX_ENTITIES),
            positions_x: Vec::with_capacity(MAX_ENTITIES),
            positions_y: Vec::with_capacity(MAX_ENTITIES),
            healths: Vec::with_capacity(MAX_ENTITIES),
            // ...
        }
    }
}
```

### 8.2 批量处理

```rust
// 批量更新冷却
fn tick_cooldowns(&mut self) {
    for slots in &mut self.skill_slots {
        for slot in slots.iter_mut() {
            slot.tick();
        }
    }
}

// 批量处理 Dot
fn tick_dots(&mut self) {
    for i in 0..MAX_ENTITIES {
        if !self.entities[i].alive { continue; }
        let dot_damage = self.dot_contexts[i].tick_all();
        if dot_damage != 0 {
            self.healths[i].take_damage(dot_damage);
        }
    }
}
```

### 8.3 标识符优化

- 实体 ID 当前使用 `u32`
- 技能/Buff ID 使用 `u16`
- 存活状态保存在实体元数据中，后续如果需要可再引入位图优化

---

## 9. 文件结构

```
apps/game-server/src/core/system/combat/
├── mod.rs        # 模块入口与导出
├── components.rs # 组件定义（Position, Health, SkillSlot...）
├── skills.rs     # 技能定义与 EffectScript 解析
├── buffs.rs      # Buff 定义与 EffectScript 解析
├── catalog.rs    # CSV / 内置 CombatCatalog
├── input.rs      # 玩家帧输入解析
└── ecs.rs        # RoomCombatEcs 容器、tick、事件和快照
```

---

## 10. 下一步

1. **专用协议对接**：评估是否把当前 `GameMessagePush` JSON payload 收敛为专用 Protobuf 战斗消息。
2. **配置派生对象热更**：CSV reload 后重建并原子替换 `CsvCombatCatalog`，同时定义旧房间版本策略。
3. **AOI 与场景接入**：战斗事件、快照和位置校正按视野过滤，并接入场景阻挡 / 击退落点校验。
4. **状态迁移与持久化**：为大世界常驻房间补战斗状态快照、恢复和跨版本迁移策略。
5. **技能编辑器**：可视化编辑技能模板，并导出当前 CSV / EffectScript 格式。
