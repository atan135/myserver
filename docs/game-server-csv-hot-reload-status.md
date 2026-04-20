# game-server CSV 热更现状清单

这份文档只回答一个问题:

当前 `apps/game-server/csv` 里的各张表, 哪些在运行中修改后会真正影响在线服务, 哪些只是被 `ConfigTableRuntime` 重载了, 但业务侧不会自动生效。

术语说明:

- 本文属于 `运行时热更新` 范畴
- 不讨论游戏逻辑代码和实例替换
- 那类能力统一归入 `滚动重启 / 灰度发布`
- 统一口径见 `docs/game-server-update-strategy.md`

## 判定口径

这里把“支持热更”拆成三档:

| 状态 | 含义 |
|------|------|
| `可直接生效` | 请求处理时会重新读取最新 `ConfigTableRuntime` 快照, 修改 CSV 后无需重启即可影响后续请求 |
| `仅重载快照, 不自动生效` | CSV 文件会被 reload, 但服务启动时已经把数据加工成长期持有对象, 当前不会随着 reload 自动替换 |
| `当前未接入业务` | 表会被加载和 reload, 但当前主业务链路里没有找到有效消费点, 修改后通常看不到运行时效果 |

需要特别区分两件事:

1. `ConfigTableRuntime` reload 成功
2. 业务逻辑真正开始使用 reload 后的新数据

第一件事不等于第二件事。

## 总览清单

| CSV 文件 | 当前状态 | 主要原因 | 推荐验证方式 |
|------|------|------|------|
| `TestTable_100.csv` | `可直接生效` | `GetRoomData` 请求处理时会重新 `snapshot()` | `mock-client --scenario get-room-data` |
| `ItemTable.csv` | `部分可直接生效` | 背包/物品相关请求会重新 `snapshot()` 取最新表 | `inventory-add` / `inventory-equip` / `inventory-use` |
| `TestTable_110.csv` | `当前未接入业务` | 当前只看到装载与 reload, 没有实际业务消费路径 | 暂无业务验证入口 |
| `SceneTable.csv` | `仅重载快照, 不自动生效` | 启动时被加工进 `SceneCatalog`, 当前不会在 reload 后自动重建 | 需要滚动重启 / 灰度发布 |
| `SceneSpawnPoint.csv` | `仅重载快照, 不自动生效` | 启动时被加工进 `SceneCatalog`, 当前不会在 reload 后自动重建 | 需要滚动重启 / 灰度发布 |
| `ScenePortal.csv` | `当前未接入业务` | 当前未找到主业务消费路径 | 暂无业务验证入口 |
| `SceneRegion.csv` | `当前未接入业务` | 当前未找到主业务消费路径 | 暂无业务验证入口 |
| `SceneMonsterSpawn.csv` | `当前未接入业务` | 当前未找到主业务消费路径 | 暂无业务验证入口 |
| `SkillBase.csv` | `仅重载快照, 不自动生效` | 启动时被加工进 `CsvCombatCatalog`, 当前不会在 reload 后自动替换 | 需要滚动重启 / 灰度发布 |
| `BufferBase.csv` | `仅重载快照, 不自动生效` | 启动时被加工进 `CsvCombatCatalog`, 当前不会在 reload 后自动替换 | 需要滚动重启 / 灰度发布 |

## 可直接生效

### `TestTable_100.csv`

当前 `GetRoomData` 请求会在处理时重新读取最新配置快照:

- `apps/game-server/src/gameservice/room_query/mod.rs`
- `services.config_tables.snapshot().await`
- `tables.testtable_100`

这意味着:

- 修改 `TestTable_100.csv` 数据行
- 等待 `csv reload` 日志成功
- 再次发起 `GetRoomData`

后续请求能直接读到新值。

推荐验证:

```powershell
node tools/mock-client/src/index.js --scenario get-room-data `
  --http-base-url http://127.0.0.1:3000 `
  --host 127.0.0.1 --port 7000 `
  --login-name test001 --password Passw0rd! `
  --id-start 1000 --id-end 1000
```

### `ItemTable.csv`

`ItemTable` 的热更比 `TestTable_100` 更接近真实业务, 但要注意它是“请求链路生效”, 不是“全局状态自动回填”。

当前会重新读取最新 `item_table` 的路径包括:

- `apps/game-server/src/core/service/inventory_service.rs`
  - `handle_item_equip`
  - `handle_item_use`
  - `handle_item_add`
- `apps/game-server/src/admin_server.rs`
  - GM 发奖前校验物品配置是否存在

因此这些行为会受到 reload 后新表的影响:

- 新物品 ID 是否存在
- 装备槽位解析
- 消耗品效果解析
- GM 发奖校验

但有一个重要边界:

- CSV reload 本身不会主动遍历在线玩家并重算当前装备属性
- 也不会自动给在线玩家推送新的属性面板

也就是说:

- 改了 `ItemTable.csv`
- 在线玩家已经穿上的装备, 不会因为 reload 完成就自动刷新属性
- 需要后续再次触发装备/卸下/使用等路径, 才会重新走 `item_table` 逻辑

推荐验证:

```powershell
node tools/mock-client/src/index.js --scenario inventory-add `
  --http-base-url http://127.0.0.1:3000 `
  --host 127.0.0.1 --port 7000 `
  --login-name test001 --password Passw0rd! `
  --add-item-id 9999 --add-count 1
```

先得到 `ITEM_NOT_FOUND`, 再把 `9999` 加进 `ItemTable.csv`, 等待 reload 成功后重试。

## 仅重载快照, 不自动生效

### `SceneTable.csv` 与 `SceneSpawnPoint.csv`

服务启动时, `server.rs` 会先取一次 `tables_snapshot`, 再构造 `SceneCatalog`:

- `apps/game-server/src/server.rs`
- `config_tables.snapshot().await`
- `SceneCatalog::load_from_dir(...)`

之后 `GameRoomLogicFactory` 会长期持有这个 `SceneCatalog`:

- `apps/game-server/src/gameroom/factory.rs`

因此当前行为是:

- CSV 文件会被 reload
- `ConfigTableRuntime` 里的快照会更新
- 但 `SceneCatalog` 不会自动重建
- 运行中的房间逻辑和后续新建房间仍会继续使用旧 `SceneCatalog`

结论:

- 当前不能把这两张表当成“在线热更可生效”的场景表
- 要生效, 需要通过滚动重启 / 灰度发布让新实例加载新表

### `SkillBase.csv` 与 `BufferBase.csv`

这两张表在启动时会被加工成 `CsvCombatCatalog`:

- `apps/game-server/src/server.rs`
- `CsvCombatCatalog::from_tables(...)`
- `apps/game-server/src/core/system/combat/catalog.rs`

之后 `GameRoomLogicFactory` 会长期持有该 `combat_catalog`:

- `apps/game-server/src/gameroom/factory.rs`
- `CombatDemoLogic::new(self.combat_catalog.clone())`

因此当前行为是:

- CSV 文件会被 reload
- `ConfigTableRuntime` 快照会更新
- 但已构造出的 `combat_catalog` 不会自动替换

结论:

- 当前不能把技能/Buff 配置当成“在线热更立即生效”
- 要让战斗逻辑稳定使用新表, 需要通过滚动重启 / 灰度发布让新实例加载新表

## 当前未接入业务

### `TestTable_110.csv`

当前能看到:

- 表会装载
- reload 日志会输出它的行数

但当前主业务链路里没有找到对 `testtable_110` 的有效消费点。

结论:

- 技术上可以 reload
- 但当前没有可见业务效果

### `ScenePortal.csv` / `SceneRegion.csv` / `SceneMonsterSpawn.csv`

当前能看到:

- 这三张表会被 `ConfigTables` 装载
- reload 时也会进入 `reload_changed`

但在当前主业务链路里, 没有找到有效的运行时消费点。

`SceneCatalog` 当前只明确消费了:

- `scenetable`
- `scenespawnpoint`

结论:

- 这三张表当前不适合作为热更验证对象
- 即使 reload 成功, 也通常看不到实际业务效果

## 推荐测试顺序

如果只是验证“当前热更链路是否真的生效”, 建议按下面顺序测:

1. `TestTable_100.csv`
2. `ItemTable.csv`
3. `max_body_len` / `heartbeat_timeout_secs` 运行时配置更新

不建议先拿下面这些做首轮验证:

1. `SceneTable.csv`
2. `SceneSpawnPoint.csv`
3. `SkillBase.csv`
4. `BufferBase.csv`

原因不是它们不会 reload, 而是当前代码结构下 reload 后不会自动进入实际运行对象。

## 后续改造方向

如果后续希望让“启动时固化”的表也支持真正在线热更, 需要补下面的能力:

1. 为 `SceneCatalog` 提供可原子替换的共享引用
2. 为 `CsvCombatCatalog` 提供可原子替换的共享引用
3. 在 CSV reload 成功后重建并替换这些派生对象
4. 明确旧房间是继续使用旧版本, 还是切到新版本
5. 为 reload 后的房间/战斗行为补专项回归测试

在这些能力落地前, 当前文档中的状态应视为准确信息。
