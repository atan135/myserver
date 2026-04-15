# 背包系统设计

## 1. 概述

背包系统管理玩家的物品存储、装备穿戴和属性计算。玩家物品分散存放于三个位置：**背包**（随身）、**仓库**（固定位置）、**装备栏**（穿戴中）。

### 1.1 核心设计原则

- **服务端权威**：所有物品操作以服务端数据为准
- **预计算属性**：装备/Buff 变更时重新计算属性，战斗时直接读取
- **来源追踪**：属性变化记录具体来源，用于面板展示
- **延迟写库**：非关键操作批量写库，减少 IO 压力
- **变更检测**：脏标记机制驱动通知和持久化

---

## 2. 存储架构

### 2.1 数据流

```
DB（MySQL）                    游戏服务内存                    Client
   │                                │                           │
   │  登录时加载 ◄───────────────────┤                           │
   │                                │                           │
   │                    ┌───────────┴───────────┐               │
   │                    │  InventoryManager      │               │
   │                    │  - inventory: Vec<Item>│               │
   │                    │  - warehouse: Vec<Item>│               │
   │                    │  - equipment: EqSlots  │               │
   │                    │  - player_attr          │               │
   │                    │  - dirty_flags          │               │
   │                    └───────────┬───────────┘               │
   │                                │                           │
   │                    帧末检查dirty ◄─── 每帧 tick             │
   │                                │                           │
   │  写库 ◄────────────────────────┤ 延迟批量                  │
   │                                │                           │
   │                    ┌───────────┴───────────┐               │
   │                    │  NotificationDispatcher│              │
   │                    │  - attr → 本人         │               │
   │                    │  - visual → 视野内玩家  │               │
   │                    └────────────────────────┘               │
```

### 2.2 三种存储位置

| 位置 | 说明 | 访问限制 |
|------|------|----------|
| 背包 (Inventory) | 随身携带，存放道具、材料、任务物品 | 在线即可操作 |
| 仓库 (Warehouse) | 固定于主城/据点 NPC 处 | 必须位于仓库 NPC 附近 |
| 装备栏 (Equipment) | 穿戴中的装备，影响角色属性和外观 | 在线即可穿戴/卸下 |

### 2.3 物品堆叠规则

- 不同物品有各自的堆叠上限（MaxStack）
- 不可堆叠物品（装备）每个占用独立格子
- 堆叠物品达到上限后自动占用新格子

---

## 3. 数据结构

### 3.1 物品定义

```rust
struct Item {
    item_id: u32,           // 物品配置ID
    uid: u64,               // 唯一实例ID（用于区分同名物品）
    count: u32,             // 数量（堆叠物品）
    binded: bool,           // 是否绑定（绑定后不可交易）
}

struct ItemDefine {
    id: u32,
    name: String,
    max_stack: u32,         // 最大堆叠数，1 表示不可堆叠
    type_: ItemType,        // 装备/道具/材料/任务物品
    quality: Quality,       // 白色/蓝色/紫色/橙色/红色
    // 装备属性（仅装备类型）
    equip_attr: Option<EquipAttr>,
}
```

### 3.2 装备槽位

```rust
enum EquipSlot {
    Weapon,    // 武器
    Armor,     // 衣服
    Helmet,    // 头盔
    Pants,     // 裤子
    Shoes,     // 鞋子
    Accessory, // 饰品
    // ... 可扩展
}

struct EquipmentSlots {
    slots: HashMap<EquipSlot, Option<Item>>,
}
```

### 3.3 属性与来源记录

```rust
// 属性来源
enum AttrSource {
    Base,              // 基础属性（升级/转职）
    Equipment(u64),    // 装备（按实例ID）
    Buff(u32),         // Buff
    Skill(u32),        // 技能
    Food,              // 临时消耗品
}

// 单条属性记录
struct AttrRecord {
    source: AttrSource,
    attr_type: AttrType,
    value: i32,
}

// 完整属性面板
struct PlayerAttr {
    base: AttrPanel,           // 基础属性（升级点满）
    bonus: Vec<AttrRecord>,    // 所有附加（用于面板展示）
    final: AttrPanel,          // 最终属性（战斗用）
}

// 战斗属性子集
struct AttrPanel {
    hp: i64,
    max_hp: i64,
    attack: i64,
    defense: i64,
    speed: i32,
    crit_rate: f32,
    crit_dmg: f32,
    // ...
}
```

### 3.4 外观与Buff

```rust
struct PlayerVisual {
    appearance: Appearance,      // 当前外观
    active_buffs: Vec<BuffId>,   // 激活的Buff列表
}

struct Buff {
    id: u32,
    name: String,
    duration_ms: u64,
    attr_bonus: AttrPanel,       // 属性加成
    visual_effect: Option<u32>, // 视觉特效ID
}
```

### 3.5 玩家数据与脏标记

```rust
struct PlayerData {
    // 物品存储
    inventory: Vec<Option<Item>>,  // 背包（固定格子数，可扩展）
    warehouse: Vec<Option<Item>>,  // 仓库
    equipment: EquipmentSlots,     // 装备栏

    // 角色状态
    attr: PlayerAttr,
    visual: PlayerVisual,

    // 脏标记
    attr_dirty: bool,      // 属性变化，需要通知本人
    visual_dirty: bool,   // 外观变化，需要通知周围
    data_dirty: bool,      // 数据变化，需要写库
}

impl PlayerData {
    fn new() -> Self { ... }

    fn set_attr_dirty(&mut self) {
        self.attr_dirty = true;
        self.data_dirty = true;
    }

    fn set_visual_dirty(&mut self) {
        self.visual_dirty = true;
        self.data_dirty = true;
    }
}
```

---

## 4. 属性计算

### 4.1 计算公式

```
最终属性 = 基础属性 + Σ(装备附加) + Σ(Buff附加) + Σ(技能附加) + Σ(临时消耗品)
```

### 4.2 重算触发时机

| 触发时机 | 影响的dirty |
|----------|-------------|
| 穿/脱装备 | attr_dirty + visual_dirty |
| 升级/转职 | attr_dirty |
| 添加/移除Buff | attr_dirty + visual_dirty |
| 使用属性类消耗品 | attr_dirty |

### 4.3 重算逻辑

```rust
impl PlayerAttr {
    fn recalculate(&mut self, equipment: &EquipmentSlots, buffs: &[Buff]) {
        // 1. 收集所有来源
        let mut all_bonus: Vec<AttrRecord> = vec![];

        // 装备附加
        for (slot, item_opt) in &equipment.slots {
            if let Some(item) = item_opt {
                if let Some(equip_attr) = &item.equip_attr {
                    all_bonus.push(AttrRecord {
                        source: AttrSource::Equipment(item.uid),
                        attr_type: AttrType::Attack,
                        value: equip_attr.attack,
                    });
                    // ... 其他属性
                }
            }
        }

        // Buff附加
        for buff in buffs {
            for (attr_type, value) in &buff.attr_bonus {
                all_bonus.push(AttrRecord {
                    source: AttrSource::Buff(buff.id),
                    attr_type,
                    value: *value,
                });
            }
        }

        // 2. 聚合到 final
        let mut final_panel = self.base.clone();
        for record in &all_bonus {
            final_panel.add(record.attr_type, record.value);
        }

        // 3. 更新
        self.bonus = all_bonus;
        self.final = final_panel;
    }
}
```

### 4.4 战斗时属性读取

```rust
// 伤害计算
fn calculate_damage(attacker: &PlayerData, defender: &PlayerData) -> Damage {
    let attack = attacker.attr.final.attack;
    let defense = defender.attr.final.defense;
    // ...
}

// 属性面板展示（发送给客户端）
fn build_attr_panel_for_client(attr: &PlayerAttr) -> AttrPanelClient {
    AttrPanelClient {
        base: attr.base.clone(),
        bonus: attr.bonus.clone(),  // 包含来源信息
        final: attr.final.clone(),
    }
}
```

---

## 5. 变更通知

### 5.1 通知类型

| 消息 | 方向 | 内容 | 接收者 |
|------|------|------|--------|
| `AttrChangePush` | Server→Client | 完整属性面板 | 本人 |
| `VisualChangePush` | Server→Client | 外观/Buff变化（增量） | 视野内玩家 |
| `InventoryUpdatePush` | Server→Client | 背包变化（增量） | 本人 |

### 5.2 帧末处理

```rust
impl PlayerData {
    fn tick(&mut self, notifier: &mut Notifier) {
        if self.attr_dirty {
            notifier.notify_attr_change(self.player_id, &self.attr);
            self.attr_dirty = false;
        }

        if self.visual_dirty {
            notifier.notify_visual_change(self.player_id, &self.visual);
            self.visual_dirty = false;
        }

        // data_dirty 在下一节处理
    }
}
```

### 5.3 视野广播

```rust
fn notify_visual_change(&mut self, player_id: u64, visual: &PlayerVisual) {
    let nearby_players = self.scene.get_nearby_players(player_id);
    for pid in nearby_players {
        self.send_to_player(pid, VisualChangePush {
            player_id,
            visual: visual.clone(),
        });
    }
}
```

---

## 6. 核心操作

### 6.1 装备穿戴/卸下

```rust
fn equip_item(&mut self, slot: EquipSlot, item: Item) -> Result<()> {
    // 1. 检查背包是否有该物品
    let idx = self.find_item(item.uid)?;
    if idx.is_none() { return Err(ErrCode::ItemNotFound); }

    // 2. 检查槽位是否匹配
    if !slot.matches_item_type(item.type_) {
        return Err(ErrCode::SlotMismatch);
    }

    // 3. 卸下当前装备到背包
    let old_equipment = self.equipment.slots.remove(&slot);
    if let Some(old) = old_equipment {
        self.inventory.add_item(old)?;
    }

    // 4. 穿上新装备
    self.inventory.remove_item(item.uid)?;
    self.equipment.slots.insert(slot, Some(item));

    // 5. 触发重算和脏标记
    self.attr.recalculate(&self.equipment, &self.visual.active_buffs);
    self.set_attr_dirty();
    self.set_visual_dirty();

    Ok(())
}
```

### 6.2 仓库存取

```rust
fn can_access_warehouse(player: &Player) -> bool {
    let dist = player.pos.distance(WAREHOUSE_POSITION);
    dist <= ACCESS_DISTANCE
}

fn warehouse_deposit(&mut self, item_uid: u64, count: u32) -> Result<()> {
    // 1. 位置校验
    if !can_access_warehouse(&self.player) {
        return Err(ErrCode::NotAtWarehouse);
    }

    // 2. 从背包移到仓库
    let item = self.inventory.remove_item(item_uid, count)?;
    self.warehouse.add_item(item)?;
    self.data_dirty = true;

    Ok(())
}

fn warehouse_withdraw(&mut self, item_uid: u64, count: u32) -> Result<()> {
    // 1. 位置校验
    if !can_access_warehouse(&self.player) {
        return Err(ErrCode::NotAtWarehouse);
    }

    // 2. 从仓库移到背包
    let item = self.warehouse.remove_item(item_uid, count)?;
    self.inventory.add_item(item)?;
    self.data_dirty = true;

    Ok(())
}
```

### 6.3 物品使用

```rust
fn use_item(&mut self, item_uid: u64) -> Result<()> {
    let item = self.inventory.get_item(item_uid)?;

    match item.def.type_ {
        ItemType::Food => {
            // 消耗品：直接移除，产生Buff/属性效果
            self.inventory.remove_item(item_uid, 1)?;
            self.apply_food_effect(&item.def)?;
            self.set_attr_dirty();
        }
        ItemType::Quest => {
            // 任务物品：触发任务条件，不移除
            self.trigger_quest_condition(&item.def)?;
        }
        _ => {
            return Err(ErrCode::CannotUse);
        }
    }

    Ok(())
}
```

### 6.4 交易

```rust
fn trade_request(&mut self, target_id: u64) -> Result<()> {
    // 1. 双方都在线
    // 2. 双方距离校验
    // 3. 创建交易锁定（双人确认机制）
    // 4. 交易期间物品冻结
    Ok(())
}

fn trade_confirm(&mut self, trade_id: u64) -> Result<()> {
    // 确认后交换物品，扣除并发放
    self.apply_trade_result(trade_id)?;
    self.set_attr_dirty();  // 可能涉及装备
    self.data_dirty = true;
    Ok(())
}
```

### 6.5 PK掉落

```rust
fn handle_pk_defeat(&mut self, killer_id: u64) -> Result<Vec<ItemDrop>> {
    // 1. 根据配置概率决定是否掉落
    // 2. 优先掉落非绑定物品
    // 3. 装备绑定状态可能变化
    let drops = self.calculate_drop()?;
    self.remove_items(&drops);
    self.set_visual_dirty();  // 可能外观变了
    self.data_dirty = true;
    Ok(drops)
}
```

---

## 7. 数据库持久化

### 7.1 写库策略

| 操作类型 | 策略 |
|----------|------|
| 交易、合成、PK掉落 | 实时写库 |
| 物品使用、Buff变更 | 延迟批量写库 |
| 登录登出 | 强制写库 |

### 7.2 脏数据收集

```rust
struct DirtyCollector {
    pending_save: Vec<(u64, PlayerData)>,  // player_id, data
}

impl PlayerData {
    fn tick_save(&mut self, collector: &mut DirtyCollector) {
        if self.data_dirty {
            collector.pending_save.push((self.player_id, self.clone()));
            self.data_dirty = false;
        }
    }
}

impl DirtyCollector {
    fn flush(&self, db: &DbPool) {
        // 批量写入 MySQL
    }
}
```

---

## 8. 协议设计

### 8.1 背包相关协议

| 协议 | 方向 | 说明 |
|------|------|------|
| `WarehouseAccessReq/Res` | Client→Server | 仓库存取请求（含位置校验） |
| `ItemEquipReq/Res` | Client→Server | 装备穿戴/卸下 |
| `ItemUseReq/Res` | Client→Server | 物品使用 |
| `InventoryUpdatePush` | Server→Client | 背包增量更新 |
| `AttrChangePush` | Server→Client | 属性面板更新 |
| `VisualChangePush` | Server→Client | 外观/Buff变化 |

---

## 9. 待设计项

以下模块与背包系统交互，需要后续详细设计：

- **Buff系统**：Buff 的具体定义、叠加规则、驱散机制
- **交易系统**：双人锁定、物品冻结、取消惩罚
- **合成系统**：材料消耗、产物生成、成功率
- **商店系统**：购买/出售价格、货币类型、限购
- **掉落系统**：怪物掉落表、PK掉落概率

---

## 10. 设计决策汇总

| 问题 | 决策 |
|------|------|
| 背包数据存储位置 | 独立于 RoomState，登录时从 DB 加载到内存 |
| 属性计算方式 | 预计算 + 来源记录，变更时重算并标记 dirty |
| 仓库访问限制 | 必须位于仓库 NPC 附近，支持反作弊校验 |
| 通知范围 | 属性→本人，外观→视野内玩家 |
| DB 写入策略 | 关键操作实时写，其余延迟批量 |
