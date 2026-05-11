# 大世界常驻 Room 热更新设计

本文档描述大世界类型游戏中，常驻 Room（持久化 Zone）的热更新策略。

术语说明：

- 本文讨论的是大世界持久化场景下的热更新方案
- 与 `运行时热更新`（CSV 数据 / 运行时配置）和 `滚动重启 / 灰度发布`（实例切流）均有关联
- 统一口径见 `docs/game-server-update-strategy.md`

## 1. 问题背景

大世界游戏中，Zone 是按区块切割的常驻 Room，具有以下特征：

- 房间永远不会自然结束
- 内部有持久化实体状态（NPC、怪物、资源点）
- 玩家随时进出

这意味着现有的"等房间结束再用新实例"策略不适用，需要专门的热更新方案。

## 2. 设计约束

- 不引入第二种开发语言（无 Lua / Rhai / WASM）
- 不接受因脚本层带来的性能损耗
- 全程纯 Rust 实现
- 尽可能复用现有框架基础设施（game-proxy 路由、CSV 热更、Admin API）

## 3. 三层分治架构

```
┌─────────────────────────────────────────────────┐
│  Layer 3: 编译逻辑层 (Rust 二进制)               │  ← 极少变更，走状态迁移
│  帧循环、网络、ECS 调度、物理引擎               │
├─────────────────────────────────────────────────┤
│  Layer 2: 行为配置层 (CSV/JSON 数据文件)         │  ← 中频变更，Catalog 原子替换
│  技能公式、AI 行为树、状态机转换表、Buff 规则   │
├─────────────────────────────────────────────────┤
│  Layer 1: 数值配置层 (CSV 数据行)                │  ← 高频变更，当前已支持热更
│  伤害系数、刷怪间隔、掉落概率、区域参数        │
└─────────────────────────────────────────────────┘
```

核心原则：

- 让尽可能多的"逻辑"不写死在代码里，而是由数据表达
- 大部分日常迭代落在 Layer 1 和 Layer 2，不需要重启
- 只有极少数结构性变更才需要走 Layer 3 的实例替换

## 4. Layer 1：数值配置热更（当前已支持）

这是现有 `ConfigTableRuntime` 已实现的能力：

- 轮询 CSV 文件变化
- 原子替换表快照（`Arc<RwLock<Arc<ConfigTables>>>`）
- 请求链路在下一次处理时读到新值

覆盖范围：伤害系数、刷怪间隔、掉落概率、区域参数等纯数值调整。

详见 `docs/game-server-csv-hot-reload-status.md`。

## 5. Layer 2：行为配置热更（需要扩展）

### 5.1 核心思路

将"看起来是逻辑"的东西用数据表达，代码只实现通用执行引擎。

### 5.2 可数据化的游戏逻辑

| 逻辑类型 | 数据表达方式 |
|---------|------------|
| AI 行为 | 行为树 JSON/CSV（节点类型 + 参数） |
| 状态机转换 | 转换表（当前状态 + 条件 → 下一状态） |
| 技能效果 | 公式字符串 + 效果列表 |
| Buff 规则 | 触发条件 + 修改属性 + 叠加规则 |
| 刷怪规则 | 刷新点 + 时间表 + 条件表达式 |
| 任务触发 | 事件类型 + 条件 + 动作列表 |
| 区域规则 | 进入/离开触发器 + 效果列表 |

### 5.3 技能效果示例

不要这样写（每个技能一个函数，改逻辑必须改代码）：

```rust
fn skill_fireball(caster: &Entity, target: &Entity) {
    let damage = caster.atk * 2.5 - target.def * 0.3;
    apply_damage(target, damage);
    apply_buff(target, "burn", 5.0);
}
```

改为数据驱动：

```csv
SkillId,DamageFormula,Buffs
1001,atk*2.5-target.def*0.3,burn:5.0
1002,atk*1.8+int*0.5,freeze:3.0|slow:2.0
```

```rust
// 代码只实现通用公式引擎（编译一次，不再变）
fn execute_skill(caster: &Entity, target: &Entity, skill_cfg: &SkillRow) {
    let damage = evaluate_formula(&skill_cfg.damage_formula, caster, target);
    apply_damage(target, damage);
    for buff in &skill_cfg.buffs {
        apply_buff(target, buff.id, buff.duration);
    }
}
```

### 5.4 公式引擎

在 Rust 内实现轻量表达式求值器，不属于"引入第二种语言"，而是游戏数据格式的基础设施：

```rust
pub struct FormulaEngine {
    // 支持: 四则运算、变量引用、函数调用(min/max/clamp/rand)
}

impl FormulaEngine {
    /// 公式在 CSV 加载时预编译为 AST/字节码，运行时求值开销极小
    pub fn evaluate(&self, expr: &CompiledFormula, ctx: &FormulaContext) -> f64;
}
```

### 5.5 Catalog 原子替换

当前 `GameRoomLogicFactory` 持有 `Arc<SceneCatalog>` 是启动时一次性构造的，需要改为可原子替换：

```rust
// 当前
struct GameRoomLogicFactory {
    scene_catalog: Arc<SceneCatalog>,
    combat_catalog: Arc<CsvCombatCatalog>,
}

// 改为
struct GameRoomLogicFactory {
    scene_catalog: Arc<RwLock<Arc<SceneCatalog>>>,
    combat_catalog: Arc<RwLock<Arc<CsvCombatCatalog>>>,
}
```

CSV reload 成功后重建 catalog 并原子替换指针，常驻 Room 在下一次 tick 或请求时读到新版本。

## 6. Layer 3：编译逻辑层状态迁移

当确实需要修改编译逻辑（新增系统、修改帧循环、重构 ECS 结构）时，走状态快照迁移。

### 6.1 迁移流程

```
1. game-proxy 标记目标 Zone 为 Draining（不再路由新玩家进入）
2. game-server(旧) 收到迁移指令
3. Room 执行 serialize_world_state() → 写入 Redis/文件
4. game-server(旧) 通知 proxy："Zone X 状态已持久化"
5. game-server(新) 收到恢复指令
6. Room 执行 restore_world_state() → 从持久化数据重建
7. game-proxy 将 Zone X 路由切换到新实例
8. 缓冲队列中的玩家输入 flush 到新 Room
9. 旧实例释放该 Zone
```

### 6.2 迁移窗口期

```
时间线:
  t0 ──── freeze 帧推进（~100-500ms）
  t1 ──── 序列化完成
  t2 ──── 新实例恢复完成
  t3 ──── proxy 切换路由
  t4 ──── 恢复帧推进
           ↑
      玩家感知: 一次短暂卡顿（类似网络波动）
```

对于大世界探索类游戏，100-500ms 的冻结窗口通常可接受。

### 6.3 需要在框架里补的 trait

```rust
pub trait PersistentRoomLogic: RoomLogic {
    /// 完整序列化当前世界状态（实体、定时器、全局变量等）
    fn serialize_state(&self) -> Result<RoomStateSnapshot, SerializeError>;

    /// 从快照恢复世界状态
    fn restore_state(&mut self, snapshot: RoomStateSnapshot) -> Result<(), RestoreError>;

    /// 状态 schema 版本（用于兼容性判断）
    fn state_version(&self) -> u32;

    /// 版本迁移：将旧版本状态升级到当前版本
    fn migrate_state(old: RoomStateSnapshot, from_version: u32) -> Result<RoomStateSnapshot, MigrateError>;
}
```

### 6.4 状态 Schema 版本管理

每次修改状态结构时递增版本号，新版本实例加载旧快照时执行迁移链：

```
v1 → v2 → v3（当前）
```

迁移函数逐版本升级，确保任意历史版本都能恢复到最新。

## 7. 运营场景对照

| 变更类型 | 频率 | 走哪层 | 玩家感知 |
|---------|------|--------|---------|
| 调整怪物伤害数值 | 日常 | Layer 1（CSV 数据热更） | 无感知 |
| 修改 AI 行为树逻辑 | 每周 | Layer 2（行为配置热更） | 无感知 |
| 新增技能效果组合 | 每周 | Layer 2（公式/效果表热更） | 无感知 |
| 修改刷怪时间表 | 日常 | Layer 1（数值配置） | 无感知 |
| 新增全新系统（如天气系统） | 月级 | Layer 3（状态迁移） | ~200ms 卡顿 |
| 重构 ECS 组件结构 | 季度级 | Layer 3（状态迁移） | ~200ms 卡顿 |
| 大版本更新 | 季度级 | Layer 3 + 公告维护 | 短暂维护 |

## 8. 与业务层驱逐方案的对比

业务层驱逐（通过游戏惩罚机制强制玩家离开 Zone，清空后冷替换）作为 Layer 3 的**替代方案**也可行，但有以下取舍：

| 维度 | 状态迁移 | 业务层驱逐 |
|------|---------|-----------|
| 实现复杂度 | 高（序列化/版本管理） | 低（清空后新建） |
| 玩家体验 | 极短卡顿（~200ms） | 被迫离开当前区域 |
| 非持久化状态 | 可保留 | 丢失 |
| 清空时间可控性 | 完全可控 | 不可控（需超时强踢兜底） |
| 批量更新扩展性 | 好（可并行迁移多 Zone） | 差（大量驱逐影响体验） |
| 适用频率 | 可频繁使用 | 适合低频使用 |

建议：

- 低频、非核心区域 → 业务层驱逐（简单够用）
- 高频、核心区域 → 状态迁移（体验优先）

两种方案可以在同一套系统里并存。

## 9. 框架改动优先级

| 优先级 | 改动项 | 复杂度 | 说明 |
|--------|--------|--------|------|
| P0 | `SceneCatalog` / `CsvCombatCatalog` 原子替换 | 低 | 改为 `Arc<RwLock<Arc<T>>>` |
| P0 | CSV 行为配置表设计（公式、行为树、状态机） | 中 | 设计表结构和数据格式 |
| P1 | 轻量公式/条件求值引擎（纯 Rust） | 中 | CSV 加载时预编译，运行时求值 |
| P1 | `PersistentRoomLogic` trait + 序列化/恢复接口 | 中 | 定义迁移协议 |
| P1 | Room 级 proxy 路由切换 API | 低 | proxy 已有基础设施 |
| P2 | 状态 schema 版本管理 + 迁移链 | 中 | 支持跨版本恢复 |
| P2 | 迁移期间输入缓冲队列 | 低 | freeze 窗口内缓冲玩家输入 |
| P2 | 行为树执行器 | 中 | 通用行为树 runtime |

## 10. 与现有文档的关系

- `docs/game-server-update-strategy.md`：定义运行时热更新与滚动重启的基本拆分
- `docs/game-server-csv-config-design.md`：Layer 1 的技术实现
- `docs/game-server-csv-hot-reload-status.md`：Layer 1 当前各表状态
- `docs/game-proxy-hot-update-design.md`：Layer 3 依赖的代理层基础设施
- 本文档：在以上基础上，针对大世界常驻 Room 场景的完整方案
