# game-server 更新策略拆分

这份文档用于把当前仓库里的两类能力明确拆开:

1. `运行时热更新`
2. `滚动重启 / 灰度发布`

这两个概念在之前的文档里经常被混写, 但它们解决的问题并不一样。

## 1. 术语定义

### 1.1 运行时热更新

这里的“运行时热更新”指:

- 不重启 `game-server` 进程
- 直接在当前在线服务里更新运行时配置
- 或更新那些会在请求链路 / 房间链路里重新读取最新快照的 CSV 数据

当前仓库里, 它主要对应两类东西:

- `game-server admin` 可修改的运行时配置项
- `ConfigTableRuntime` 能 reload 且业务侧会重新 `snapshot()` 的 CSV 表

它**不**等于:

- 任意代码热替换
- 任意房间逻辑热替换
- 任意场景/战斗对象的在线重建

### 1.2 滚动重启 / 灰度发布

这里的“滚动重启 / 灰度发布”指:

- 启动一个带新代码 / 新配置 / 新 CSV 的 `game-server` 实例
- 让新连接优先进入新实例
- 旧实例进入摘流或保留老连接直到自然结束
- 最终让旧实例下线

它主要解决的是:

- 游戏逻辑代码更新
- 启动时固化的派生对象更新
- 不能在当前房间内安全在线切换的数据更新

## 2. 你的理解与当前代码的关系

你的理解大方向是对的, 但要补一条边界:

- `运行时热更新` 不是“所有 room 内 CSV”
- 更准确地说, 它是“那些在 room / request 链路里会重新读取最新快照的 CSV”

原因是:

- 有些 CSV 虽然和房间逻辑有关
- 但它们会在服务启动时先被加工成长期持有对象
- 这类表当前不会随着 CSV reload 自动替换

所以当前更准确的拆法是:

- `运行时热更新`
  - 运行时配置项
  - 可按最新快照读取的 CSV 数据
- `滚动重启 / 灰度发布`
  - 游戏逻辑代码
  - 启动时固化的派生对象
  - 不能在当前房间内安全热切的 CSV 数据

## 3. 当前代码里的对应关系

### 3.1 属于运行时热更新

#### 运行时配置项

当前 `game-server admin` 已支持至少这些配置项的在线更新:

- `max_body_len`
- `heartbeat_timeout_secs`

对应代码:

- `apps/game-server/src/admin_server.rs`

#### 可按最新快照读取的 CSV

当前明确属于这一类的有:

- `TestTable_100.csv`
- `ItemTable.csv`

原因是这些请求路径会在处理时重新读取:

- `services.config_tables.snapshot().await`

对应代码:

- `apps/game-server/src/gameservice/room_query/mod.rs`
- `apps/game-server/src/core/service/inventory_service.rs`
- `apps/game-server/src/admin_server.rs`

### 3.2 属于滚动重启 / 灰度发布

当前明确属于这一类的有:

- `game-server` 业务代码和玩法逻辑代码
- `SceneTable.csv`
- `SceneSpawnPoint.csv`
- `SkillBase.csv`
- `BufferBase.csv`

原因是这些数据会在启动时被加工成长期持有对象:

- `SceneCatalog`
- `CsvCombatCatalog`
- `GameRoomLogicFactory` 持有的依赖

对应代码:

- `apps/game-server/src/server.rs`
- `apps/game-server/src/gameroom/factory.rs`
- `apps/game-server/src/core/system/scene/query.rs`
- `apps/game-server/src/core/system/combat/catalog.rs`

### 3.3 当前未适合作为更新能力验收对象

这些表当前更适合视为“已加载, 但未形成明确业务热切换路径”:

- `TestTable_110.csv`
- `ScenePortal.csv`
- `SceneRegion.csv`
- `SceneMonsterSpawn.csv`

它们不应被用来证明“运行时热更新”或“滚动重启后业务已接入”是否完整。

## 4. 文档分工

当前建议按下面方式理解相关文档:

- `docs/game-server-csv-config-design.md`
  - 讲 CSV 配置系统本身
  - 属于 `运行时热更新` 侧
- `docs/game-server-csv-hot-reload-status.md`
  - 讲当前哪些 CSV 能在线生效, 哪些不能
  - 属于 `运行时热更新` 侧
- `docs/game-proxy-hot-update-design.md`
  - 讲新老 `game-server` 实例切流、摘流、维护模式
  - 属于 `滚动重启 / 灰度发布` 侧

## 5. 变更归类速查

| 变更类型 | 归类 | 当前建议 |
|------|------|------|
| `max_body_len` / `heartbeat_timeout_secs` | 运行时热更新 | 直接走 admin 配置更新 |
| `TestTable_100.csv` 数据值 | 运行时热更新 | 直接改 CSV, 等 reload 生效 |
| `ItemTable.csv` 中新物品/道具效果配置 | 运行时热更新 | 直接改 CSV, 通过背包链路复验 |
| `SceneTable.csv` / `SceneSpawnPoint.csv` | 滚动重启 / 灰度发布 | 新实例加载后切流 |
| `SkillBase.csv` / `BufferBase.csv` | 滚动重启 / 灰度发布 | 新实例加载后切流 |
| `gameroom/*` 玩法逻辑代码 | 滚动重启 / 灰度发布 | 新实例上线, 旧实例摘流 |
| 协议结构变化 / CSV schema 变化 | 滚动重启 / 灰度发布 | 不走运行时热更新 |

## 6. 当前项目里的推荐口径

后续在项目文档里建议统一使用下面的表述:

- `运行时热更新`
  - 专指运行中进程内可直接生效的配置项和 CSV 数据更新
- `滚动重启 / 灰度发布`
  - 专指带新代码 / 新启动期配置 / 新固化对象的新实例替换

不建议继续把这两类能力合并写成单个“热更新”概念。
