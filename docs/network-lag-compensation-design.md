# 网络延迟补偿算法设计文档

## 概述

本文档定义游戏服务端网络延迟补偿算法的设计方案，作为后续开发的参考依据。

延迟补偿算法分为三种类型，分别解决不同的网络同步问题：

1. **帧同步输入延迟处理**：解决 lockstep 帧同步中各玩家输入到达时间不一致的问题
2. **状态同步权威矫正**：解决状态同步中客户端预测与服务器权威状态不一致的问题
3. **射击命中判决回溯**：解决射击游戏中开枪时刻与服务器判定时刻不一致的问题

---

## 1. 帧同步输入延迟处理

### 1.1 背景与问题

Lockstep 帧同步要求"所有玩家在同一帧看到相同的游戏状态"。服务器必须等到"所有玩家本帧的输入都到达"后，才能推进帧。

```
时间线：
帧 10 开始 ────────────────────────────────────────── 帧 10 结束
    │                    │                    │
  t=0ms                t=100ms              t=200ms
  服务器开始            玩家A的输入           玩家B的输入
  等待本帧输入          到达                  到达
                           │                    │
                           └────── 服务器等到这里才推进帧 10 ────┘
```

### 1.2 服务端职责

服务器维护：
- `current_frame: u32` - 当前帧号
- `pending_inputs: HashMap<player_id, PlayerInput>` - 本帧已收到的输入
- `future_inputs: HashMap<frame_id, HashMap<player_id, PlayerInput>>` - 提前到达的输入
- `wait_deadline: Instant` - 本帧截止时间

**等待策略A：严格帧同步（等待所有玩家）**
```
每帧 tick 时：
  if pending_inputs.count() == all_players.count():
     // 所有玩家输入都到了，立即推进
     on_tick(current_frame, pending_inputs)
     current_frame += 1
     pending_inputs.clear()
  else if now() > wait_deadline:
     // 等待超时，用空输入或上一帧输入代替
     on_tick(current_frame, pending_inputs)
     current_frame += 1
     pending_inputs.clear()
```

**等待策略B：乐观推进（不等所有人）**
```
每帧 tick：
  if pending_inputs.count() >= min_players_threshold:
     on_tick(current_frame, pending_inputs)
     current_frame += 1
     pending_inputs.clear()
  else:
     // 玩家不够，发送空帧或暂停
```

**关键参数**：
| 参数 | 含义 | 典型值 |
|------|------|--------|
| `frame_interval` | 每帧时长 | 100ms (10fps), 33ms (30fps) |
| `input_delay` | 客户端领先帧数 | 1-3 帧 |
| `wait_timeout` | 等待输入超时时间 | frame_interval × 1.5 |

### 1.3 客户端职责

客户端维护一个"本地帧号"，可以比服务器领先 1-3 帧：

```
服务器帧: 8   9   10  11  12  13  ...
客户端帧: 8   9   10  11  12  13  14  15  16  // 客户端领先2帧

玩家按下"前进"键：
  客户端立即在本地帧16应用输入（预测）
  同时发送 frame_id=16 的输入到服务器

服务器收到提前到的帧16输入：
  暂存到 future_inputs[frame_id=16]
  等服务器推进到帧16时才处理
```

### 1.4 丢帧处理

当某个玩家的输入在 `wait_timeout` 内未到达：
- **策略1**：用上一帧的输入代替（输入重复）
- **策略2**：用空输入代替（丢失该帧操作）
- **策略3**：直接踢出长时间未响应的玩家

---

## 2. 状态同步权威矫正

### 2.1 背景与问题

状态同步中，服务器维护权威游戏状态并定期广播给客户端。客户端在服务器状态到达之前做"预测"，收到服务器状态后发现预测错误时需要回滚重演。

```
时间线：
服务器:  [state@frame10] ─────── [state@frame11] ─────── [state@frame12]
           │                          │                          │
           ▼                          ▼                          ▼
客户端:   显示frame10 ──预测frame11─▶ 显示frame11 ──预测frame12─▶ 显示frame12
           │        ▲                │        ▲                │
           │        │                │        │                │
        服务器状态  客户端预测      服务器状态  客户端预测      服务器状态
        到达        回滚+重演      到达        回滚+重演        到达
```

### 2.2 服务端职责

- 每帧生成权威游戏状态
- 广播 `StateUpdate` 给所有客户端（可优化为只发送增量变化）
- 接收客户端输入，应用输入生成新状态

### 2.3 客户端职责

**预测流程**：
```
1. 玩家输入
2. 客户端立即应用输入，更新本地状态（预测）
3. 同时发送输入到服务器
4. 继续预测下一帧
```

**回滚重演流程**：
```
收到服务器状态后：
  if 本地预测状态 == 服务器状态:
     // 预测正确，无事发生
     nothing
  else:
     // 预测错误
     rollback_to(server_state)  // 回滚到服务器状态
     replay_recent_inputs()      // 重演本地最近输入
```

### 2.4 具体示例

```
帧10: 服务器状态 [pos_A: (0,0), pos_B: (100,0)]
       客户端显示 [pos_A: (0,0), pos_B: (100,0)]

帧11:
  客户端: 玩家A按下"右移"，预测 pos_A = (10,0)
          客户端显示 [pos_A: (10,0), pos_B: (100,0)]
          发送 input: {player: A, action: move_right, frame: 11}

  服务器: 收到A的输入，但B同时按下了"左移"
          权威状态: pos_A = (10,0), pos_B = (90,0)
          广播 state@11

帧11(收到服务器状态后):
  客户端对比发现 B 的位置不同
  回滚: pos_B = (90,0)
  重演: B向左移的输入
  结果: [pos_A: (10,0), pos_B: (90,0)] ← 与服务器一致
```

### 2.5 带宽优化

服务器可只发送变化的部分（增量状态）：

```rust
struct StateUpdate {
    frame_id: u32,
    changes: Vec<PropertyChange>,
}

struct PropertyChange {
    entity_id: u64,
    field: String,
    value: Value,
}
```

### 2.6 本项目中的落地方式

对当前 `game-server`，状态同步权威矫正不建议取代帧同步主链路，而应作为**位移纠偏辅链路**使用。

推荐组合：

- 输入同步：继续使用按帧输入广播
- 本地预测：客户端领先 `1-3` 帧做移动预测
- 状态校正：服务端按 `N` 帧或超阈值发送权威位置快照

也就是说，位移的主驱动力仍然是：

```text
本地输入 -> 本地预测 -> 服务端 on_tick() 权威结算
```

而不是：

```text
服务端每帧广播所有实体完整位置
```

### 2.7 位移校正策略

位移校正建议采用“固定频率 + 异常立即校正”双轨制：

#### 策略 A：固定 N 帧校正

- 每 `3-5` 帧对玩家发送一次位置校正
- 每 `5-10` 帧对 NPC / 怪物发送一次位置校正

适合用来兜底修复客户端预测漂移。

#### 策略 B：误差超阈值立即校正

如果服务端检测到任一实体预测误差超过阈值，则立即发送校正。

典型阈值可按玩法调节，例如：

- 普通移动：`0.3m ~ 0.5m`
- 冲刺 / 击退：`0.5m ~ 1.0m`

#### 策略 C：关键事件强校正

以下情况即使未达到固定校正周期，也建议立即发送：

- 冲刺结束
- 击退结束
- 服务端碰撞修正
- 穿墙 / 越界修正
- 传送
- 重连恢复

### 2.8 客户端处理建议

客户端收到权威位置校正后，不应一律瞬移。

建议策略：

- 小误差：插值拉回
- 中误差：短时间加速追赶
- 大误差：立即硬修正

这样可以兼顾一致性和观感。

### 2.9 100 人场景下的补充要求

在 `100` 玩家并带有 NPC / 怪物的场景下，状态校正必须结合 AOI / 兴趣管理，否则带宽成本会过高。

建议约束：

- 只校正当前玩家视野或战斗相关实体
- 远距离实体降低校正频率
- 投射物优先事件同步，而不是高频位置同步

---

## 3. 射击命中判决回溯

### 3.1 背景与问题

射击游戏中，"命中"是极其敏感的操作。由于网络延迟，玩家开枪时客户端显示的位置与服务器收到命令时服务器上的位置可能不同。

```
玩家A视角:
  t=0ms:   看到玩家B在位置 (100, 0)
  t=50ms:  按下射击键，瞄准 (100, 0)
  t=100ms: 子弹到达位置 (100, 0)，显示命中

服务器视角（延迟200ms）:
  t=0ms:   收到A的射击命令，但服务器上B已经移动到 (80, 0)
  t=100ms: 服务器处理射击，判定未命中（因为B不在原位了）
```

如果不回溯，玩家会感觉"明明瞄准了打中了，为什么不中"。

### 3.2 服务端职责

服务器在收到射击命令时，**回溯到"子弹实际发射"的时刻**来判定是否命中：

```rust
fn resolve_shot(
    shooter_id: u64,
    target_id: u64,
    aim_direction: Vec3,
    client_shoot_time: u64,      // 客户端发送的开枪时刻
    server_current_time: u64,    // 服务器当前时刻
    frame_interval_ms: u64,
) -> ShotResult {
    // 1. 估算网络延迟
    let estimated_latency_ms = (server_current_time - client_shoot_time) / 2;

    // 2. 回溯到开枪时刻的帧号
    let server_frame_at_shoot = calculate_frame_at_time(
        server_current_time - estimated_latency_ms,
        frame_interval_ms,
    );

    // 3. 获取开枪时刻目标的位置（需要历史帧位置数据）
    let target_pos_at_shoot = position_history
        .get_position_at_frame(target_id, server_frame_at_shoot)
        .unwrap_or_else(|| current_position(target_id));

    // 4. 在开枪时刻的位置上做命中判定
    let hit = raycast(shooter_position(shooter_id), aim_direction, target_pos_at_shoot);

    ShotResult {
        hit,
        hit_frame: server_frame_at_shoot,
        hit_position: if hit { target_pos_at_shoot } else { Vec3::ZERO },
    }
}
```

### 3.3 位置历史存储

要实现回溯，服务器必须保存历史帧的玩家位置：

```rust
struct FrameSnapshot {
    frame_id: u32,
    entities: HashMap<entity_id, EntityState>,
}

struct EntityState {
    position: Vec3,
    rotation: Quat,
    velocity: Vec3,
}

struct PositionHistory {
    snapshots: VecDeque<FrameSnapshot>,  // 最大保存 N 帧
    max_frames: u32,
}
```

### 3.4 命中框与判定半径

实际游戏中使用比角色模型稍大的命中框：

```rust
struct HitboxConfig {
    standing_radius: f32,   // 站立时命中框半径
    standing_height: f32,   // 站立时命中框高度
    moving_radius: f32,     // 移动时（略大，因为移动中更难命中）
    airborne_radius: f32,   // 跳跃/下落时
}
```

### 3.5 客户端职责

- 发送射击命令时附带 `client_timestamp`（开枪时刻）
- 收到服务器判定结果后，在 `hit_position` 播放命中特效
- 如果命中了，但 `hit_position` 与客户端当前显示位置不同，仍在 `hit_position` 显示特效（即使敌人已经跑远了）

```
服务器: 收到射击 → 回溯t=100ms时刻 → 命中 → 广播 {hit: true, hit_pos: (100,0)}
客户端: 收到结果 → 在(100,0)播放命中特效 → 即使敌人已经跑到(80,0)
```

### 3.6 副作用："子弹转弯"（bullet bending）

如果目标在墙角后只露出一个角，玩家开枪时瞄准那个角，但服务器回溯后判定目标在墙角后（未露出），会判定未命中。客户端上玩家看到的是命中了，但实际判定未命中。

解决方案：
- 对"擦边球"做特殊处理，即使判定未命中也播放"擦过"的音效/特效
- 客户端也做命中预测，收到服务器结果后做平滑过渡

---

## 三种算法对比

| 维度 | 帧同步输入延迟处理 | 状态同步权威矫正 | 射击命中判决回溯 |
|------|-------------------|-----------------|-----------------|
| **目标** | 保证所有玩家看到相同帧 | 保证流畅体验，减少卡顿 | 保证射击手感公平 |
| **核心矛盾** | 网络延迟导致输入到达时间不同 | 客户端预测与服务器权威的差异 | 延迟导致开枪和判定时刻不一致 |
| **服务端职责** | 等待所有玩家输入，按帧推进 | 定期广播权威状态 | 回溯到开枪时刻判定 |
| **客户端职责** | 本地帧领先1-3帧预测 | 预测、显示、回滚重演 | 收到结果后显示特效 |
| **典型问题** | 等待超时、丢帧 | 回滚范围、累积误差 | 子弹转弯、违和感 |
| **适用场景** | MOBA、RTS、格斗游戏 | MMORPG、MMO | FPS、TPS |
| **带宽压力** | 低（只传输入） | 高（传完整/增量状态） | 中（射击事件+位置历史） |

---

## 后续开发优先级建议

| 优先级 | 类型 | 说明 |
|--------|------|------|
| 高 | 帧同步输入延迟处理 | 已有帧同步基础，只需扩展等待策略 |
| 中 | 状态同步权威矫正 | 需评估是否采用状态同步模式 |
| 低 | 射击命中判决回溯 | 仅 FPS/TPS 类型游戏需要 |
