# game-server CSV 配置系统设计

这份文档描述 `apps/game-server/csv` 下配置表的加载、代码生成、索引和热更新方案，并对齐当前 `core/config_table + gameconfig` 分层实现。

术语说明：

- 本文讨论的是 `运行时热更新`
- 不讨论游戏逻辑代码替换、实例切流或滚动重启
- 那一类能力统一归入 `滚动重启 / 灰度发布`
- 统一口径见 `docs/game-server-update-strategy.md`

目标不是做一个完全动态的通用 csv 解析器，而是做一个适合游戏服长期维护的“编译期结构生成 + 运行时数据热更”系统。

## 当前实现状态

当前代码已经落地了这套方案的主干：

- `apps/game-server/tools/csv_codegen.rs` 在编译期扫描 `apps/game-server/csv/*.csv` 并生成 `apps/game-server/src/csv_code/*.rs`。
- `core/config_table/` 提供 `CsvTableLoader`、行解析、字符串池、运行时快照和按文件 reload。
- `gameconfig/registry.rs` 聚合当前业务表，包括场景、物品、技能、Buff 和测试表。
- 运行时 reload 支持结构签名校验、整表构建、成功后替换，失败时保留旧表。

需要注意当前类型支持边界：

- 端到端加载已实现：`int`、`int64`、`float`、`string`、`Array<string>`、`Dict<string,int>`。
- 生成器能解析更多类型声明，但除上述组合外，当前生成的加载表达式仍会落到 `unimplemented!`，不能视为业务可用。
- 当前二级索引只内置在生成器策略里，实际配置集中在 `TestTable_100` 和 `TestTable_110` 样例表；业务表目前主要使用 `id` 查询和 `all()` 遍历。

## 1. 设计目标

CSV 配置系统需要满足以下要求：

- 配置表存放在 `apps/game-server/csv`
- 每张表第一行为字段名
- 每张表第二行为字段类型
- 后续每一行为数据
- 编译期根据 csv 结构生成 Rust 结构文件
- 运行时支持按 `id` 快速获取
- 运行时支持对指定列建立索引并快速获取
- 运行时支持 csv 数据热更新
- 热更新只支持“数据变化”，不支持“结构变化”

## 2. 变更边界

这套系统把 csv 变更分成两类：

### 2.1 数据变更

数据变更指：

- 修改第 3 行及之后的数据值
- 不修改字段名
- 不修改字段类型
- 不增删列

这类变更允许走运行时热更新。

### 2.2 结构变更

结构变更指：

- 修改第一行字段名
- 修改第二行字段类型
- 增加列
- 删除列
- 修改索引配置

这类变更会影响生成代码，因此不走运行时热更新，而是走滚动重启 / 灰度发布。

## 3. 为什么采用编译期代码生成

本项目明确要求预先生成 csv 对应的 Rust 结构文件，原因如下：

- 业务代码需要强类型访问，避免大量字符串列名和运行时类型转换
- 结构变化本来就会触发服务器代码修改，不需要在运行时兼容结构热更
- 数据热更和滚动重启 / 灰度发布的边界清晰，系统更稳定
- 可以在编译期检查字段类型和索引定义是否合法

因此本系统采用：

- 编译期 codegen 处理结构
- 运行时 reload 处理数据

## 4. CSV 格式规范

每张表格式如下：

```csv
Id,Field_0,Field_1,Field_2
int,Array<string>,float,Dict<string,int>
1000,A|B|C,1.5,key1:1|key2:2
1001,D|E,2.5,key3:4
```

### 4.1 第一行

第一行为字段名，要求：

- 不允许为空
- 必须唯一
- 第一列固定为 `Id`

### 4.2 第二行

第二行为字段类型，要求：

- 与字段数量完全一致
- 必须使用受支持的类型声明

### 4.3 数据行

从第三行开始为数据行，要求：

- 列数必须与字段数一致
- `Id` 必须唯一
- 任意字段解析失败时，该表整体加载失败

## 5. 支持的类型语法

当前稳定可用于运行时加载的类型：

- `int`
- `int64`
- `float`
- `string`
- `Array<string>`
- `Dict<string,int>`

生成器类型系统预留了以下声明，但加载逻辑尚未补齐，新增业务表不要直接依赖：

- `Array<int>`
- `Array<int64>`
- `Array<float>`
- `Dict<string,string>`
- `Dict<int,int>`

后续如果需要，再扩展更多组合。

### 5.1 当前约束

当前只实现明确声明过且已接入 `CsvRowReader` 的组合，不做任意泛型嵌套，不支持：

- `Array<Dict<...>>`
- `Dict<string,Array<...>>`
- 更深层嵌套容器

这样能明显降低生成器复杂度。

## 6. 类型到 Rust 的映射

由于 csv 中 `string` 值往往重复率较高，尤其是：

- 枚举型字符串
- 资源路径
- 文本 key
- 重复的配置标签

如果直接在每一行中保存完整 `String`，会带来明显的重复内存占用。

因此本系统对字符串类字段采用“表级字符串池”方案：

- 每张表额外生成一个字符串池
- 行结构中不直接保存 `String`
- 行结构只保存字符串索引
- 读取时通过索引回到字符串池中获取真实值

### 6.1 表级字符串池

建议每张表生成：

```rust
pub type StringKey = u32;

pub struct TableStringPool {
    pub values: std::collections::HashMap<StringKey, String>,
}
```

加载时再在内部临时维护一个反向表，用于去重：

```rust
HashMap<String, StringKey>
```

流程是：

1. 读取 csv 字符串值
2. 若该值已存在于表级字符串池，则复用已有 `StringKey`
3. 若不存在，则分配新 `StringKey`
4. 行结构只保存 `StringKey`

这样同一张表中重复出现的字符串只保留一份。

### 6.2 设计边界

第一版建议按“单表字符串池”实现，不做跨表共享字符串池。

原因：

- 单表实现简单
- 热更新时单表原子替换更容易
- 跨表共享会让热更新和生命周期复杂很多

后续如果确认跨表字符串复用收益足够大，再考虑升级。

建议映射如下：

```text
int                  -> i32
int64                -> i64
float                -> f32
string               -> StringKey
Array<string>        -> Vec<StringKey>
Array<int>           -> Vec<i32>
Array<int64>         -> Vec<i64>
Array<float>         -> Vec<f32>
Dict<string,int>     -> HashMap<StringKey, i32>
Dict<string,string>  -> HashMap<StringKey, StringKey>
Dict<int,int>        -> HashMap<i32, i32>
```

对外业务访问时，再由生成代码提供字符串解析接口，把 `StringKey` 转成 `&str` 或 `String`。

## 7. 容器字段编码规则

### 7.1 Array

数组统一使用 `|` 分隔：

```text
A|B|C
1|2|3
```

### 7.2 Dict

字典统一使用：

- 项分隔符：`|`
- 键值分隔符：`:`

例如：

```text
key1:1|key2:2
hello:world|foo:bar
1:10|2:20
```

### 7.3 限制

第一版默认约束：

- `string` 值中不允许原样包含 `|` 和 `:`
- 如果未来业务确实需要转义规则，再单独设计转义语法

### 7.4 字符串池对容器字段的影响

如果字段类型包含字符串，则生成后的内存表示也统一改成索引形式：

- `string` -> `StringKey`
- `Array<string>` -> `Vec<StringKey>`
- `Dict<string,int>` -> `HashMap<StringKey, i32>`
- `Dict<string,string>` -> `HashMap<StringKey, StringKey>`

这样能保证整张表内所有重复字符串都被复用，而不是只优化普通 `string` 列。

## 8. 代码生成方案

当前实现使用编译期生成器：

```text
apps/game-server/tools/csv_codegen.rs
```

输入：

- `apps/game-server/csv/*.csv`
- 内置索引配置

输出：

```text
apps/game-server/src/csv_code/mod.rs
apps/game-server/src/csv_code/*.rs
```

当前生成物已经覆盖测试表、场景表、物品表、技能表和 Buff 表；具体文件以 `apps/game-server/src/csv_code/` 为准。

### 8.1 每张表生成内容

每张表建议生成：

- `Row` 结构体
- `Table` 结构体
- `StringPool` 结构
- `load_from_csv()` 加载函数
- `get(id)` 查询函数
- 索引查询函数
- schema signature 常量

## 9. 生成代码示例

以 `TestTable_100.csv` 为例，可生成类似：

```rust
pub type StringKey = u32;

pub struct TestTable100Row {
    pub id: i32,
    pub field_0: Vec<StringKey>,
    pub field_1: f32,
    pub field_2: i32,
    pub field_3: f32,
    pub field_4: std::collections::HashMap<StringKey, i32>,
    pub field_5: f32,
    pub field_6: i64,
    pub field_7: i32,
}

pub struct TestTable100 {
    string_pool: std::collections::HashMap<StringKey, String>,
    rows: Vec<TestTable100Row>,
    by_id: std::collections::HashMap<i32, usize>,
    by_field_2: std::collections::HashMap<i32, Vec<usize>>,
    by_field_6: std::collections::HashMap<i64, Vec<usize>>,
}
```

同时生成辅助访问函数，例如：

```rust
impl TestTable100 {
    pub fn resolve_string(&self, key: StringKey) -> Option<&str> {
        self.string_pool.get(&key).map(|s| s.as_str())
    }
}
```

## 10. 索引设计

不是所有列都自动建索引，只对配置指定的列建索引。

原因：

- 降低内存占用
- 避免无意义索引
- 编译期能更清晰控制接口生成

### 10.1 索引配置来源

第一版建议把索引配置内置在生成器中，例如：

```rust
pub struct TableCodegenPolicy {
    pub indexed_columns: &'static [&'static str],
}
```

例如：

```rust
TestTable_100 => ["Id", "Field_2", "Field_6"]
TestTable_110 => ["Id", "Field_0"]
```

### 10.2 索引支持范围

第一版建议只支持以下字段做索引：

- `int`
- `int64`
- `string`

第一版不建议支持以下字段建索引：

- `float`
- `Array<...>`
- `Dict<...>`

这里的 `string` 索引指“原始字符串值语义上的索引”，而不是直接按 `StringKey` 暴露给业务层。

也就是说，生成表内部可以用字符串池降低内存，但对外索引接口仍建议保持：

```rust
find_by_name("abc")
```

而不是：

```rust
find_by_name_key(17)
```

这样业务代码不需要感知字符串池实现细节。

## 11. 查询接口设计

每张生成表至少提供：

```rust
impl TestTable100 {
    pub fn get(&self, id: i32) -> Option<&TestTable100Row>;
}
```

对索引列再生成：

```rust
impl TestTable100 {
    pub fn find_by_field_2(&self, value: i32) -> Vec<&TestTable100Row>;
    pub fn find_by_field_6(&self, value: i64) -> Vec<&TestTable100Row>;
}
```

如果索引列是字符串，则建议生成：

```rust
impl SomeTable {
    pub fn find_by_name(&self, value: &str) -> Vec<&SomeTableRow>;
}
```

对外业务代码应直接使用这些强类型接口，而不是再手写字段字符串访问。

### 11.1 字符串字段访问接口

由于行结构内部保存的是 `StringKey`，生成器还应为字符串列生成友好的 getter，例如：

```rust
impl SomeTableRow {
    pub fn name<'a>(&self, table: &'a SomeTable) -> Option<&'a str> {
        table.resolve_string(self.name)
    }
}
```

这样可以同时满足：

- 内存去重
- 业务层强类型访问
- 不暴露太多底层实现细节

## 12. 运行时注册中心

当前实现已经把“通用 CSV runtime”和“具体游戏表装配”拆开：

- `core/config_table/`
  - 放通用 CSV 解析、trait、runtime、reload
- `gameconfig/registry.rs`
  - 放具体游戏的 `ConfigTables` 装配

建议统一注册中心保持为强类型结构，例如：

```rust
pub struct ConfigTables {
    pub testtable_100: Arc<TestTable100>,
    pub testtable_110: Arc<TestTable110>,
}
```

当前代码中，`ConfigTables` 已位于 `apps/game-server/src/gameconfig/registry.rs`，而不是继续放在 `src/config_table/` 下。

或者后续如果表数量很多，再包装成统一管理器。

第一版不需要做完全动态的表注册系统，强类型字段访问更符合当前目标。

字符串池也应跟随每张表一起挂在注册中心对应表实例上，热更新时整张表连同字符串池一起原子替换。

## 13. 运行时热更新

虽然结构是编译期生成的，但数据仍需要支持热更新。

### 13.1 热更新流程

每次某个 csv 文件变化时：

1. 读取变更文件的前两行
2. 计算 schema signature
3. 与编译期生成的 signature 常量比较
4. 如果一致，则继续解析数据区
5. 基于旧 `ConfigTables` 和变更文件构建候选 `ConfigTables`
6. 基于候选 `ConfigTables` 重建派生配置
   - `SceneCatalog`
   - `CsvCombatCatalog`
   - `RoomPolicyRegistry`
7. 全部构建成功后生成新的 `RuntimeGameConfig` 版本
8. 原子替换当前 `RuntimeGameConfig`
9. 如果任一阶段失败，则保留旧版本

### 13.2 原子替换原则

热更新必须按版本替换，不允许边解析边覆盖旧数据，也不允许只替换 raw table 后让派生 catalog 停留在旧版本。

正确流程是：

```text
旧 RuntimeGameConfig 继续服务
候选 ConfigTables 构建
候选派生 catalog 构建
成功后原子替换
失败则丢弃候选版本
```

当前 `ConfigTableRuntime` 持有版本化快照：

```rust
pub struct RuntimeGameConfig {
    pub version: u64,
    pub tables: Arc<ConfigTables>,
    pub scene_catalog: Arc<SceneCatalog>,
    pub combat_catalog: SharedCombatCatalog,
    pub room_policies: Arc<RoomPolicyRegistry>,
}
```

运行中系统应按使用边界读取当前快照：

- 请求处理只需要 raw table 时，使用 `tables_snapshot()`
- 房间逻辑需要场景或战斗派生配置时，使用 `current_snapshot()`
- 需要跨多次操作保持同一版本时，先取一次 `Arc<RuntimeGameConfig>` 并在本次操作内复用

### 13.3 运行中房间生效边界

当前已落地的边界是：

- 新建房间通过 `GameRoomLogicFactory` 读取当前配置
- `movement_demo` 在玩家出生和每帧移动校验时读取当前 `SceneCatalog`
- `combat_demo` 在命令执行、tick 和快照生成时读取当前 `CsvCombatCatalog`
- `RoomManager` 通过共享 `RoomPolicyRegistry` 读取房间 tick、输入和人数默认值

这不是“强制重建运行中房间状态”。已经存在的实体、Buff 实例、房间帧号和成员状态不会因为 reload 自动重置。后续玩法如果需要在 reload 后迁移状态，应在各自 `RoomLogic` 内显式实现版本检测和迁移逻辑。

## 14. Schema Signature

为了区分“数据变更”和“结构变更”，每张表都需要编译期签名。

例如：

```rust
pub const TEST_TABLE_100_SCHEMA_SIGNATURE: &str =
    \"Id:int|Field_0:Array<string>|Field_1:float|Field_2:int|Field_3:float|Field_4:Dict<string,int>|Field_5:float|Field_6:int64|Field_7:int\";
```

运行时热更新时，如果当前 csv 的签名与编译期不一致，则：

- 直接拒绝热更新
- 记录错误日志
- 等待滚动重启 / 灰度发布

## 15. 失败策略

### 15.1 启动时

启动时如果任意关键配置表加载失败，应直接启动失败。

### 15.2 运行时热更

运行时热更失败时：

- 保留旧 `RuntimeGameConfig` 版本
- 打日志，包含变更文件、失败阶段、错误原因和当前版本号
- 不影响当前在线逻辑

### 15.3 结构不匹配

如果热更新时发现结构不匹配：

- 不尝试兼容
- 不允许半解析
- 直接报“需要整服替换”

## 16. 与现有样例表的关系

当前 `apps/game-server/csv` 下样例表已按新规则调整：

- `Array` 改为 `Array<string>`
- `Dict` 改为 `Dict<string,int>`

后续如果出现新表使用：

- `Array<int>`
- `Array<int64>`
- `Array<float>`
- `Dict<string,string>`
- `Dict<int,int>`

则生成器和解析器都需要补齐端到端加载逻辑，而不只是让类型声明通过解析。

## 17. 内存优化补充说明

采用表级字符串池后，这套系统的内存模型变成：

- 重复字符串在单表中只保留一份
- 行对象更轻，只保存整数索引
- `Array<string>` 和 `Dict<string,*>` 也能享受去重收益

这个方案特别适合：

- 配置量大
- 文本列重复率高
- 房间逻辑频繁读表

代价是：

- 读取字符串时多一次间接查表
- 生成器和加载器复杂度会上升

对于游戏配置系统，这是可接受的折中。

## 18. 模块划分建议

当前推荐目录结构如下：

```text
apps/game-server/src/core/config_table/
  mod.rs
  parser.rs
  runtime.rs
  reload.rs
  traits.rs

apps/game-server/src/csv_code/
  mod.rs
  ...

apps/game-server/src/gameconfig/
  mod.rs
  registry.rs
  generated/
    mod.rs
```

职责建议：

- `core/config_table/`
  - 通用 CSV runtime、reload、trait、parser
- `gameconfig/registry.rs`
  - 当前游戏的 `ConfigTables` 注册中心和表装配入口
- `gameconfig/generated/`
  - 预留给游戏侧生成装配代码或手工聚合导出
- `csv_code/`
  - 编译期生成的具体表实现

补充说明：

- `apps/game-server/src/config_table/mod.rs` 目前仅作为兼容层 re-export，方便旧代码平滑迁移
- 后续新增文档、代码或扩展模块时，应优先引用 `core/config_table` 和 `gameconfig` 的新结构

## 19. 统一 trait 建议

建议每张生成表实现统一 trait：

```rust
pub trait CsvTableLoader: Sized {
    const TABLE_NAME: &'static str;
    const SCHEMA_SIGNATURE: &'static str;

    fn load_from_csv(path: &std::path::Path) -> Result<Self, CsvLoadError>;
}
```

这样运行时热更新逻辑可以复用，不用每张表手写一套。

## 20. 当前阶段范围与后续扩展

当前已经落地的范围：

- 读取 `apps/game-server/csv/*.csv`
- 生成 Rust 结构代码
- 支持 `id` 查询
- 支持指定列索引查询
- 支持 raw table 数据热更新
- 支持 `SceneCatalog` / `CsvCombatCatalog` 派生配置随 reload 重建并原子替换
- 支持房间默认策略通过共享 `RoomPolicyRegistry` 暴露可替换入口
- 支持 schema signature 校验
- 支持单表字符串池

仍不属于当前能力边界：

- 任意泛型嵌套
- float 索引
- Array / Dict 索引
- 跨表引用校验
- 自动从运行时动态推断结构
- 除 `Array<string>`、`Dict<string,int>` 外的容器类型端到端加载

## 21. 当前落地说明

当前代码中的职责划分如下：

- `core::config_table::ConfigTableRuntime`
  - 负责配置表目录加载、快照读取、按文件变更热更新
- `core::config_table::CsvTableLoader`
  - 约束每张生成表的统一加载接口
- `gameconfig::ConfigTables`
  - 负责把当前游戏实际使用的表聚合成强类型注册中心
- `csv_code::*`
  - 负责承载编译期生成出的具体表结构、字符串池、加载逻辑和查询函数

因此后续扩展时的建议是：

- 新增通用 CSV 能力时，优先改 `core/config_table/`
- 新增某个游戏具体表时，优先改 `gameconfig/registry.rs`
- 新增 codegen 产物或生成规则时，改 `tools/csv_codegen.rs` 和 `csv_code/`

## 22. 关键设计结论

这套 csv 配置系统的核心结论是：

1. csv 结构由编译期 codegen 固化
2. csv 数据由运行时热更新加载
3. 结构变更不走运行时热更，只走滚动重启 / 灰度发布
4. 字符串类字段采用单表字符串池压缩内存
5. 查询接口以强类型访问为主
6. 二级索引按配置生成，不做全列自动索引

如果这六点定下来，后续实现就比较直接了。
