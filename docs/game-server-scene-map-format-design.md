# game-server 场景地图格式设计（草案）

这份文档用于确认 `apps/game-server` 后续位移同步、阻挡校验、出生点、AOI 分块与场景切换所依赖的地图格式。

目标不是一次性覆盖完整编辑器方案，而是先确定一套**服务端可落地、可热更、可扩展**的场景数据格式。

## 1. 设计目标

场景地图格式需要满足以下要求：

- 支持 `100` 玩家规模，并同时容纳 NPC / 怪物 / 投射物
- 能被服务端直接用于位移校验，而不是只做展示资源
- 能支持出生点、阻挡、传送点、区域触发、AOI 分块
- 能和当前 `core/config_table + gameconfig` 配置体系对齐
- 能区分“轻量元数据热更”和“重型地图拓扑热更”

## 2. 总体结论

场景地图建议拆成 **两层格式**：

### 2.1 场景元数据层：CSV

适合放：

- 场景基础信息
- 出生点
- 传送点
- 区域定义
- AOI 参数
- 怪物刷新点配置

原因：

- 当前项目已经有成熟的 CSV codegen / runtime
- 这些数据是结构化、强类型、行式配置
- 适合按表查询和热更

### 2.2 场景拓扑层：独立网格文件

适合放：

- 可行走 / 不可行走网格
- 阻挡层
- 高度层
- 水域 / 危险区
- 复杂的分块数据

原因：

- 现有 CSV 系统不适合承载大尺寸二维数组
- 地图网格通常体积大，不适合塞进单元格字符串
- 后续如果要做压缩、分块、版本号校验，独立文件更合理

## 3. 目录建议

建议使用如下目录：

```text
apps/game-server/
├── csv/
│   ├── SceneTable.csv
│   ├── SceneSpawnPoint.csv
│   ├── ScenePortal.csv
│   ├── SceneRegion.csv
│   └── SceneMonsterSpawn.csv
└── scene/
    ├── grassland_01.grid.json
    ├── dungeon_01.grid.json
    └── ...
```

服务端代码侧建议对应：

```text
apps/game-server/src/
├── core/system/scene/
│   ├── mod.rs
│   ├── grid.rs
│   ├── query.rs
│   └── validator.rs
└── gameconfig/
    ├── registry.rs
    └── generated/
```

## 4. CSV 元数据表设计

## 4.1 `SceneTable.csv`

用途：定义场景基础元信息。

建议字段：

```text
Id,Code,Name,GridFile,Width,Height,CellSize,AoiBlockSize,DefaultSpawnId,Tags
int,string,string,string,int,int,float,int,int,Array<string>
```

字段含义：

- `Id`
  - 场景数值 ID
- `Code`
  - 场景唯一字符串编码，如 `grassland_01`
- `Name`
  - 场景名称
- `GridFile`
  - 对应网格文件名，如 `grassland_01.grid.json`
- `Width`
  - 场景宽度，单位可统一为“格”
- `Height`
  - 场景高度，单位可统一为“格”
- `CellSize`
  - 单格世界尺寸，例如 `0.5` 米
- `AoiBlockSize`
  - AOI 分块尺寸，单位为“格”
- `DefaultSpawnId`
  - 默认出生点 ID
- `Tags`
  - 场景标签，例如 `pve|outdoor|safe_zone`

## 4.2 `SceneSpawnPoint.csv`

用途：定义玩家、怪物、NPC 的出生点。

建议字段：

```text
Id,SceneId,Code,SpawnType,X,Y,DirX,DirY,Radius,Tags
int,int,string,string,float,float,float,float,float,Array<string>
```

说明：

- `SpawnType`
  - `player`
  - `monster`
  - `npc`
  - `portal_target`
- `X,Y`
  - 世界坐标
- `DirX,DirY`
  - 初始朝向
- `Radius`
  - 出生分布半径，用于随机散点

## 4.3 `ScenePortal.csv`

用途：定义跨场景或场景内传送点。

建议字段：

```text
Id,SceneId,Code,RegionId,TargetSceneId,TargetSpawnId,PortalType,Enabled
int,int,string,int,int,int,string,int
```

说明：

- `RegionId`
  - 当前场景中触发传送的区域
- `TargetSceneId`
  - 目标场景
- `TargetSpawnId`
  - 目标出生点
- `PortalType`
  - `scene_change`
  - `instance_enter`
  - `return_point`

## 4.4 `SceneRegion.csv`

用途：定义逻辑区域。

建议字段：

```text
Id,SceneId,Code,RegionType,MinX,MinY,MaxX,MaxY,Tags
int,int,string,string,float,float,float,float,Array<string>
```

用途示例：

- 安全区
- 战斗区
- 传送区
- 怪物刷新区
- 任务触发区

## 4.5 `SceneMonsterSpawn.csv`

用途：定义刷怪点和刷怪规则。

建议字段：

```text
Id,SceneId,SpawnPointId,MonsterId,RespawnSeconds,MaxAlive,LeashRadius,Tags
int,int,int,int,int,int,float,Array<string>
```

## 5. 网格文件格式设计

## 5.1 为什么不用 CSV 承载整张地图

不建议把阻挡地图直接写成一张 CSV 大表，原因：

- 二维网格在 CSV 中可读性差
- 更新某个格子会导致整张表 diff 噪音很大
- 当前 CSV codegen 不适合承载大型嵌套二维数组
- 后续压缩、版本校验、分块加载都不方便

因此建议场景拓扑使用独立 `.grid.json` 文件。

## 5.2 `.grid.json` 顶层结构

建议格式：

```json
{
  "version": 1,
  "scene_code": "grassland_01",
  "width": 256,
  "height": 256,
  "cell_size": 0.5,
  "layers": {
    "walkable": "base64-or-rle-data",
    "block": "base64-or-rle-data",
    "water": "base64-or-rle-data"
  },
  "aoi": {
    "block_size": 16
  }
}
```

字段说明：

- `version`
  - 地图格式版本
- `scene_code`
  - 必须与 `SceneTable.Code` 一致
- `width / height`
  - 网格尺寸
- `cell_size`
  - 单格世界尺寸
- `layers`
  - 各逻辑层数据
- `aoi.block_size`
  - AOI 分块大小，通常应和 `SceneTable.AoiBlockSize` 一致

## 5.3 推荐层定义

第一版建议支持这些层：

- `walkable`
  - 可行走层，`1=可走`，`0=不可走`
- `block`
  - 阻挡层，`1=阻挡`
- `water`
  - 水域层，可选
- `height`
  - 高度层，可选，后续再扩展

第一版最小可落地只需要：

- `walkable`
- `block`

## 5.4 编码方式

第一版建议支持两种实现方案中的一种：

### 方案 A：直接数组

调试友好，适合早期验证：

```json
{
  "layers": {
    "walkable": [1,1,1,0,0,1],
    "block":    [0,0,0,1,1,0]
  }
}
```

优点：

- 简单直观
- 容易调试

缺点：

- 文件偏大

### 方案 B：RLE / Base64 压缩

适合后期正式服或大地图：

- `walkable`: RLE 字符串
- `block`: RLE 字符串

第一阶段建议先用 **直接数组或简单 RLE**，不要一开始就上复杂二进制格式。

## 6. 世界坐标与网格坐标约定

必须统一坐标换算规则。

建议：

- 原点：场景左下角
- `x` 向右增长
- `y` 向上增长
- 网格索引：
  - `cell_x = floor(world_x / cell_size)`
  - `cell_y = floor(world_y / cell_size)`

示例：

```text
cell_size = 0.5
world = (10.3, 4.1)
cell = (20, 8)
```

服务端所有阻挡校验、AOI 计算、出生点合法性校验都必须基于同一规则。

## 7. 服务端使用方式

场景系统至少需要提供这些查询接口：

```rust
trait SceneQuery {
    fn is_walkable(&self, scene_id: i32, x: f32, y: f32) -> bool;
    fn is_blocked(&self, scene_id: i32, x: f32, y: f32) -> bool;
    fn clamp_position(&self, scene_id: i32, x: f32, y: f32) -> (f32, f32);
    fn resolve_aoi_block(&self, scene_id: i32, x: f32, y: f32) -> (i32, i32);
}
```

用于：

- 普通移动校验
- 冲刺 / 击退落点校验
- 出生点合法性校验
- AOI 分块归属

## 8. 位移同步中的使用原则

这份文档和位移同步设计配套使用时，应遵守：

- 客户端发送的是输入，不是坐标
- 服务端在 `on_tick()` 中根据输入推进位移
- 位移推进必须结合 `SceneQuery` 做阻挡与越界校验
- 低频校正消息发送的是**权威结果**

也就是说，地图格式的价值在于让服务端能够回答：

- 这个位置能不能走
- 这条位移路径是否穿墙
- 冲刺能否落在目标点
- 某玩家现在属于哪个 AOI 分块

## 9. 热更新策略

场景地图建议区分两类热更：

### 9.1 元数据热更

可通过现有 CSV 热更机制支持：

- 场景名称
- 出生点
- 传送点
- 区域定义
- 刷怪配置

### 9.2 拓扑热更

网格文件热更需要更谨慎：

- 若只更新 `walkable/block` 数据，允许整体替换
- 若宽高、坐标系、层定义变化，视为结构变更
- 结构变更不建议在线热更，建议走整服切换

## 10. 校验规则

服务端启动时建议做这些校验：

- `SceneTable.Code` 与 `grid.json.scene_code` 必须一致
- `SceneTable.Width / Height / CellSize` 与网格文件一致
- 默认出生点必须存在
- 所有出生点必须落在可行走区域
- 所有传送点目标出生点必须存在
- 所有区域框必须在场景边界内
- AOI 分块大小必须大于 `0`

## 11. 第一阶段范围建议

第一阶段先落下面这些能力：

- `SceneTable.csv`
- `SceneSpawnPoint.csv`
- `ScenePortal.csv`
- `SceneRegion.csv`
- 单独的 `.grid.json`
- 服务端 `is_walkable / is_blocked / resolve_aoi_block`
- 出生点和阻挡校验

先不要做：

- 多层高度导航
- 斜坡 / 楼梯
- 客户端可视化编辑器
- 二进制专有地图格式
- 寻路网格和导航网格双轨

## 12. 关键设计结论

这套场景地图格式的核心结论是：

1. 场景地图拆成 **CSV 元数据 + 独立网格文件** 两层
2. 出生点、传送点、区域、刷怪配置走 CSV
3. 阻挡和可行走信息走 `.grid.json`
4. 位移同步依赖的是服务端基于地图做权威校验，而不是客户端直接发坐标
5. AOI 分块属于场景数据的一部分，不能后期临时硬编码

如果这五点定下来，后续 `core/system/scene`、位移系统、AOI 系统和怪物刷新系统都能沿同一套格式继续扩展。
