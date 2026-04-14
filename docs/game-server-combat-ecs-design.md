# 战斗系统 ECS 设计文档

## 1. 概述

本文档描述基于 ECS（Entity-Component-System）架构的战斗系统设计方案，采用 **SOA（Structure of Arrays）** 数据布局实现高性能帧同步游戏。

### 1.1 设计目标

- **确定性**：相同输入产生相同输出，保证帧同步一致性
- **高性能**：预分配内存 + SOA 布局 + 批量遍历
- **可扩展**：技能/Buff 配置化，支持运行时扩展
- **低开销**：无运行时内存分配（GC friendly）

### 1.2 核心约束

| 参数 | 值 | 说明 |
|------|-----|------|
| MAX_ENTITIES | 16 | 每房间最大实体数（含玩家、NPC、投射物） |
| MAX_PLAYERS | 8 | 每房间最大玩家数 |
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

所有组件均为 `#[repr(C)]` 纯数据，无逻辑方法（逻辑在 System 层）：

```rust
// 位置组件 [x, y]
#[repr(C)]
struct Position([f32; 2]);

// 生命值组件
#[repr(C)]
struct Health {
    current: i32,
    max: i32,
    base_max: i32,
};

// 技能槽组件
#[repr(C)]
struct SkillSlot {
    skill_id: u16,
    cooldown_remaining: u16,  // 帧数，0=可用
};

// Buff 槽组件
#[repr(C)]
struct BuffSlot {
    buff_id: u16,
    duration_remaining: u16,
    stacks: u8,
};

// 移动状态
#[repr(C)]
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
    entities: Vec<Entity>,

    // 战斗组件（SOA 布局）
    positions: Vec<Position>,
    velocities: Vec<Velocity>,
    healths: Vec<Health>,
    stats: Vec<Stats>,
    move_states: Vec<MoveState>,

    // 技能组件
    skill_slots: Vec<[SkillSlot; 8]>,   // 每实体8个技能槽

    // Buff 组件
    buff_slots: Vec<[BuffSlot; 6]>,     // 每实体6个Buff槽
    dot_contexts: Vec<DotContext>,

    // 事件系统
    pending_events: Vec<CombatEvent>,
    pending_skill_requests: Vec<SkillCastRequest>,
}
```

---

## 3. 技能系统

### 3.1 技能定义

技能配置在编译期确定，支持模板定义：

```rust
struct SkillDefinition {
    id: u16,
    name: &'static str,
    cooldown_frames: u16,
    cast_frames: u16,
    range: f32,
    target_type: u8,       // 0=敌方, 1=友方, 2=自己, 3=地面
    effects: &'static [SkillEffect],
}

struct SkillEffect {
    effect_type: SkillEffectType,
    value: i32,
    buff_id: u16,
    buff_duration: u16,
    aoe_radius: f32,
}
```

### 3.2 内置技能模板

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
    name: &'static str,
    buff_type: BuffType,     // Buff/Debuff/Dot/Hot
    max_stacks: u8,
    duration_frames: u16,
    interval_frames: u16,    // Dot/Hot 间隔
    effects: &'static [BuffEffect],
    can_dispel: bool,
}
```

### 4.2 内置 Buff 模板

| ID | 名称 | 类型 | 层数 | 持续 | 效果 |
|----|------|------|------|------|------|
| 1 | 灼烧 | Dot | 1 | 180帧(6s) | 伤害5/30帧 |
| 2 | 护盾 | Buff | 1 | 300帧(10s) | 防御+5 |
| 3 | 减速 | Debuff | 3 | 120帧(4s) | 移速-20 |
| 4 | 攻击强化 | Buff | 5 | 180帧(6s) | 攻击+10 |
| 5 | 回复 | Hot | 1 | 180帧(6s) | 治疗/30帧 |

### 4.3 Dot/Hot 处理

```rust
struct DotContext {
    dots: [DamageOverTime; 4],  // 最多4个Dot
    dot_count: usize,
}

fn tick_all(&mut self) -> i32 {
    let mut total = 0;
    for dot in &mut self.dots[..self.dot_count] {
        if dot.is_active() {
            total += dot.tick();
        }
    }
    self.clear_expired();
    total
}
```

---

## 5. 伤害结算

### 5.1 伤害公式

```rust
enum DamageFormula {
    Fixed(i32),                          // 固定伤害
    Scaling { base: i32, attack_scale: f32 },  // 缩放伤害
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

- 伤害随机浮动使用 `frame_id % N` 作为种子，避免真随机
- 所有计算使用整数 i32，避免浮点精度问题
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
    for i in 0..MAX_ENTITIES {
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

---

## 7. 帧循环集成

### 7.1 RoomLogic 帧循环

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

每帧收集战斗事件，广播给客户端：

```rust
struct CombatEvent {
    event_type: u8,     // DAMAGE=1, HEAL=2, BUFF_APPLY=3, ...
    source_entity: u16,
    target_entity: u16,
    value: i32,
    extra: u16,
}

// 每帧 drain 并广播
let events = self.combat.drain_events();
for event in events {
    self.broadcast_to_all(event);
}
```

---

## 8. 性能优化

### 8.1 内存预分配

```rust
impl RoomCombatEcs {
    pub fn new() -> Self {
        Self {
            entities: vec![Entity::default(); MAX_ENTITIES],
            positions: vec![Position::default(); MAX_ENTITIES],
            healths: vec![Health::default(); MAX_ENTITIES],
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

- 实体 ID 使用 `u16`（16实体只需2字节）
- 技能/Buff ID 使用 `u16`
- 位图标记存活状态替代 `Vec<bool>`

---

## 9. 文件结构

```
apps/game-server/src/core/system/combat/
├── mod.rs        # 模块入口，CombatSystem trait
├── components.rs # 组件定义（Position, Health, SkillSlot...）
├── skills.rs     # 技能定义与技能模板
├── buffs.rs      # Buff 定义与 Buff 模板
└── ecs.rs        # RoomCombatEcs 容器实现
```

---

## 10. 下一步

1. **与 RoomLogic 集成**：将 CombatSystem 接入现有 RoomRuntimePolicy
2. **协议对接**：定义战斗事件的 Protobuf 消息格式
3. **配置表支持**：从 CSV/JSON 加载技能和 Buff 配置
4. **技能编辑器**：可视化编辑技能模板
