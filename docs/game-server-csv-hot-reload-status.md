# game-server CSV 热更现状清单

这份文档只回答一个问题:

当前 `apps/game-server/csv` 里的各张表, 哪些在运行中修改后会真正影响在线服务, 哪些只是被 `ConfigTableRuntime` 重载了, 但业务侧没有消费路径。

## 判定口径

这里把“支持热更”拆成三档:

| 状态 | 含义 |
|------|------|
| `可直接生效` | 请求处理或房间逻辑会读取最新 `RuntimeGameConfig` / `ConfigTables` 快照, 修改 CSV 后无需重启即可影响后续边界 |
| `派生配置热更生效` | CSV reload 成功后会重建并原子替换派生 catalog, 新建房间和运行中房间会在下一次读取边界看到新配置 |
| `当前未接入业务` | 表会被加载和 reload, 但当前主业务链路里没有找到有效消费点, 修改后通常看不到运行时效果 |

需要特别区分两件事:

1. `ConfigTableRuntime` reload 成功
2. 业务逻辑真正开始使用 reload 后的新数据

当前 runtime 已把原始 `ConfigTables`、`SceneCatalog`、`CsvCombatCatalog` 和 `RoomPolicyRegistry` 收敛到版本化 `RuntimeGameConfig`。reload 时先构建候选原始表和派生 catalog, 全部成功后才替换当前版本; 任一阶段失败都会保留旧版本。

## 总览清单

| CSV 文件 | 当前状态 | 主要原因 | 推荐验证方式 |
|------|------|------|------|
| `TestTable_100.csv` | `可直接生效` | `GetRoomData` 请求处理时会重新读取最新 raw table 快照 | `mock-client --scenario get-room-data` |
| `ItemTable.csv` | `部分可直接生效` | 背包/物品相关请求会重新读取最新 raw table 快照 | `inventory-add` / `inventory-equip` / `inventory-use` |
| `TestTable_110.csv` | `当前未接入业务` | 当前只看到装载与 reload, 没有实际业务消费路径 | 暂无业务验证入口 |
| `SceneTable.csv` | `派生配置热更生效` | reload 成功后重建并替换 `SceneCatalog` | movement demo 新玩家出生 / 后续移动校验 |
| `SceneSpawnPoint.csv` | `派生配置热更生效` | reload 成功后重建并替换 `SceneCatalog` | movement demo 新玩家出生点 |
| `ScenePortal.csv` | `当前未接入业务` | 当前未找到主业务消费路径 | 暂无业务验证入口 |
| `SceneRegion.csv` | `当前未接入业务` | 当前未找到主业务消费路径 | 暂无业务验证入口 |
| `SceneMonsterSpawn.csv` | `当前未接入业务` | 当前未找到主业务消费路径 | 暂无业务验证入口 |
| `SkillBase.csv` | `派生配置热更生效` | reload 成功后重建并替换 `CsvCombatCatalog` | combat demo 后续技能执行 / 快照 |
| `BufferBase.csv` | `派生配置热更生效` | reload 成功后重建并替换 `CsvCombatCatalog` | combat demo 后续 Buff 效果 |

## 可直接生效

### `TestTable_100.csv`

当前 `GetRoomData` 请求会在处理时重新读取最新配置表快照:

- `apps/game-server/src/gameservice/room_query/mod.rs`
- `services.config_tables.tables_snapshot().await`
- `tables.testtable_100`

这意味着:

- 修改 `TestTable_100.csv` 数据行
- 等待 `csv runtime config hot reload succeeded` 日志
- 再次发起 `GetRoomData`

后续请求能直接读到新值。

### `ItemTable.csv`

`ItemTable` 的热更是“请求链路生效”, 不是“全局状态自动回填”。

当前会重新读取最新 `item_table` 的路径包括:

- `apps/game-server/src/core/service/inventory_service.rs`
  - `handle_item_equip`
  - `handle_item_use`
  - `handle_item_add`
- `apps/game-server/src/admin_server.rs`
  - GM 发奖前校验物品配置是否存在

边界:

- CSV reload 不会主动遍历在线玩家并重算当前装备属性
- 在线玩家已经穿上的装备, 不会因为 reload 完成就自动刷新属性
- 需要后续再次触发装备/卸下/使用等路径, 才会重新走 `item_table` 逻辑

## 派生配置热更生效

### `SceneTable.csv` 与 `SceneSpawnPoint.csv`

reload 成功后会重建 `SceneCatalog`:

- `apps/game-server/src/core/config_table/runtime.rs`
- `RuntimeGameConfig::build(...)`
- `SceneCatalog::load_from_dir(...)`

运行中 `movement_demo` 不再长期持有启动时的 `SceneCatalog`, 而是在玩家生成和每帧移动校验时读取当前 `RuntimeGameConfig`:

- `apps/game-server/src/gameroom/movement_demo/mod.rs`

生效边界:

- 新建房间会使用最新 `SceneCatalog`
- 运行中房间后续 tick 的阻挡 / walkable 校验会读取最新 `SceneCatalog`
- 运行中房间新加入玩家的出生点会读取最新 `SceneSpawnPoint`
- 已经存在的实体不会因为 reload 自动迁移位置或重置场景状态

### `SkillBase.csv` 与 `BufferBase.csv`

reload 成功后会重建 `CsvCombatCatalog`:

- `apps/game-server/src/core/config_table/runtime.rs`
- `CsvCombatCatalog::from_tables(...)`
- `apps/game-server/src/core/system/combat/catalog.rs`

运行中 `combat_demo` 会在命令执行、tick 和快照生成时读取当前 `RuntimeGameConfig` 的 `combat_catalog`:

- `apps/game-server/src/gameroom/combat_demo/mod.rs`

生效边界:

- 新建房间会使用最新 `CsvCombatCatalog`
- 运行中房间下一次技能执行、Buff tick、战斗快照会读取最新 catalog
- 已经存在的战斗实体不会因为 reload 自动重建
- 已经挂在实体上的运行时 Buff 状态不会被自动清空, 但后续按 catalog 查询到的定义会来自当前版本

## 失败策略与日志

热更失败不会污染当前有效配置。流程是:

```text
旧 RuntimeGameConfig 继续服务
构建候选 ConfigTables
构建候选 SceneCatalog / CsvCombatCatalog / RoomPolicyRegistry
全部成功后替换当前版本
任一失败则丢弃候选版本
```

成功日志会包含:

- `csv_dir`
- `scene_dir`
- `changed_files`
- `config_version`
- 各表行数

失败日志会包含:

- `csv_dir`
- `scene_dir`
- `changed_files`
- `error`
- `current_config_version`
- “keeping previous config version” 语义

常见失败阶段:

- `reload ConfigTables`: CSV 文件缺失、schema 不匹配、行解析失败
- `build SceneCatalog`: 场景表和 grid 文件不一致、默认出生点缺失、场景引用错误
- `build CsvCombatCatalog`: 技能/Buff 字段越界、脚本解析失败、重复 ID

## 当前未接入业务

### `TestTable_110.csv`

当前能看到:

- 表会装载
- reload 日志会输出它的行数

但当前主业务链路里没有找到对 `testtable_110` 的有效消费点。

### `ScenePortal.csv` / `SceneRegion.csv` / `SceneMonsterSpawn.csv`

当前能看到:

- 这三张表会被 `ConfigTables` 装载
- reload 时也会进入 `reload_changed`

但在当前主业务链路里, 没有找到有效的运行时消费点。

`SceneCatalog` 当前只明确消费:

- `SceneTable.csv`
- `SceneSpawnPoint.csv`

因此这三张表当前不适合作为业务热更验证对象。

## 房间类型默认值

房间 tick、输入等待、人数、销毁、移动校正等默认值当前由 `RoomRuntimePolicy` / `RoomPolicyRegistry` 提供, 并通过 `RuntimeGameConfig` 暴露共享策略注册表:

- `apps/game-server/src/core/runtime/room_policy.rs`
- `apps/game-server/src/core/runtime/room_manager.rs`
- `apps/game-server/src/core/config_table/runtime.rs`

当前默认策略仍是代码内置模板, 已覆盖:

- `max_members`
- `min_start_players`
- `silent_room_fps` / `idle_room_fps` / `active_room_fps` / `busy_room_fps`
- `input_delay_frames`
- `wait_timeout_ms`
- `wait_strategy`
- `missing_input_strategy`
- `snapshot_interval_frames`

后续如果新增 `RoomType.csv` 或同类玩法配置表, 应在 `RuntimeGameConfig::build(...)` 中从 raw table 构建新的 `RoomPolicyRegistry`, 并沿用当前共享 registry 的原子替换入口。
