# 角色永久四属性模块

## 状态

本档案在迁移阶段 1 建立，用于冻结现有行为和目标边界。本文中“目标”描述后续阶段将落地的公开契约；除非明确标记为“当前”，不得将目标设计视为已实现能力。

## 职责

模块唯一拥有角色永久四属性：

- `affinity`：地、火、水、风四项倾向比例；
- `mastery`：地、火、水、风四项长期掌握值；
- 永久四属性的读取、合法变更、变更来源、操作者、原因、提交前后快照和审计日志语义。

模块以 `character_id` 为状态主体。当前实现位于 `core::character_element`，后续迁入本模块后，永久四属性只能由本模块的公开变更能力写入。

## 明确不负责

本模块不拥有、计算或写入以下状态和规则：

- `character_element_effective.rs` 的有效属性聚合及由其派生的战斗属性；
- 职业/流派状态、职业学习条件和职业修正；
- 称号状态、称号解锁策略和称号效果；
- 背包、仓库、装备、道具实例、Buff、场景上下文和系统临时修正；
- 任务、成就、活动、排行、世界事件等产生四属性变更的业务决策；
- 玩家协议、Protobuf 映射、连接响应、角色 push 传输和 revision 记录；
- PostgreSQL、SQLx、连接池、配置运行时与服务装配。

其中职业条件、称号解锁、物品使用和有效属性计算是读取方或命令发起方；角色 push 是已经提交结果的协议适配，不是本模块的状态所有者。

## 核心状态与所有者

| 状态 | 唯一所有者 | 说明 |
| --- | --- | --- |
| `characters.affinity_{earth,fire,water,wind}` | 角色永久四属性模块 | 固定总和的角色长期倾向。 |
| `characters.mastery_{earth,fire,water,wind}` | 角色永久四属性模块 | 非负的角色长期掌握值。 |
| `character_element_logs` 中的变更审计记录 | 角色永久四属性模块 | 与对应永久状态变更在同一事务提交。 |
| 有效属性、装备/职业/Buff/场景修正 | 非本模块 | 只可把永久四属性快照作为输入，不得写回上述八个字段。 |

## 业务不变量

- `affinity` 四项之和始终为 `10000`。
- 每一项 `affinity` 均不得为负。
- 每一项 `mastery` 均不得为负。
- 变更计算必须在写入前检查 `i32` 溢出；不得依赖数据库截断或溢出行为。
- 成功结果中的 `before` 和 `after` 必须是同一次已提交事务的快照。
- 每次成功永久变更都必须记录 delta、来源、操作者、原因和前后快照；拒绝的变更不得留下成功日志或成功事件。

## 目标公开 API

以下是迁移完成后的目标公开能力，名称可以随 Rust 代码规范调整，但语义不得扩大或缩小：

| 类型 | 目标能力 | 语义 |
| --- | --- | --- |
| Query | `GetCharacterElements` | 按服务端已确认的 `character_id` 查询永久四属性快照，不产生副作用。 |
| Command | `ApplyCharacterElementChange` | 提交永久四属性 delta、可信来源/操作者上下文和可选原因；返回已经提交的 `before`、`after` 与 `character_id`。 |
| Event | `CharacterElementsChanged` | 过去式业务事实，仅在上述变更事务提交成功后表达；携带不可变的提交前后快照和变更上下文。 |

`CharacterElementsChanged` 不是同步写入请求，也不等同于玩家 push。阶段 1 尚未实现事件发布器；后续 application 只能在 repository 明确报告提交成功后构造该事实。事务失败、提交结果未知或查询失败时，均不得产生该事件或成功 push。

### 公开 API 输入边界

公开 API 只接收业务值：服务端确定的 `character_id`、不可变四属性值/变更、可信变更上下文和可选原因。它不得接收或暴露：

- `AuthenticatedSessionIdentity`；
- Protobuf 请求/响应或消息号；
- SQLx row、`PgPool`、连接/事务或数据库错误对象；
- 可变 `PlayerData`、背包集合或其他模块的内部领域对象；
- 网络连接、`ConnectionContext` 或角色 push 记录。

`gameservice` 等接入层从可信会话提取 `character_id` 和操作者上下文，负责协议映射、鉴权和 push 适配。客户端提交的角色标识不得覆盖接入层确定的目标角色。

## 目标目录与允许依赖

目标结构遵循《游戏业务模块开发规范》：

```text
business/character_element/
  MODULE.md
  api/             # 对外 Command、Query、Event、Facade
  application/     # 用例和 repository port
  domain/          # 永久状态、转换和不变量
```

- `api` 依赖本模块 `application` 的受控入口，不执行领域计算或存储操作。
- `application` 依赖本模块 `domain` 与自身 `ports`；负责用例、提交顺序和失败语义。
- `domain` 不依赖 SQLx、Tokio、Protobuf、会话、配置运行时、网络或其他业务模块；后续若需要共享标识，只依赖稳定的 `business::shared` 值对象。
- PostgreSQL repository 实现在模块外的 adapter，通过 application port 接入；`server.rs` 和 `internal_server.rs` 负责装配。
- 外部业务模块、协议层和运行时只能使用从 `business::character_element` 模块根导出的 API，不得导入 `domain`、`application` 或 adapter 内部类型。

## 当前核心文件与调用方

| 路径 | 当前角色 | 迁移中的定位 |
| --- | --- | --- |
| `apps/game-server/src/core/character_element.rs` | 同时包含四属性领域模型、`CharacterElementService`、`PgCharacterElementStore`、SQL 和测试替身。 | 分离为 domain、application/port 和外部 PostgreSQL adapter 的主要来源。 |
| `apps/game-server/src/core/service/character_element_service.rs` | 四属性查询与 debug 变更协议处理、可信会话读取、Protobuf 映射及成功后 push。 | 迁为 `gameservice` 协议适配；不进入 domain。 |
| `apps/game-server/src/core/character_progress.rs` | 进度条件读取永久四属性，并将进度奖励变更委托给当前服务。 | 迁后作为公开 Query/Command 调用方。 |
| `apps/game-server/src/core/character_discipline.rs` | 职业学习/条件逻辑读取永久四属性。 | 迁后作为公开 Query 调用方；职业规则不迁入。 |
| `apps/game-server/src/core/character_title_unlock.rs` | 称号解锁规则按需读取永久四属性。 | 迁后作为公开 Query 调用方；解锁策略不迁入。 |
| `apps/game-server/src/core/inventory/player_data.rs` | 解析道具 `CharacterElementChange` 使用效果，并拥有装备相关状态。 | 保留为物品流程输入；不得把可变 `PlayerData` 传入模块 API。 |
| `apps/game-server/src/core/character_element_effective.rs` | 组合永久快照、职业、装备、Buff、场景和系统修正，生成临时有效属性。 | 不迁移；后续按跨模块只读投影单独评估。 |
| `apps/game-server/src/core/context.rs` | `ServiceContext` 持有并向调用方提供当前四属性服务。 | 后续改为持有 facade，不暴露具体 store。 |
| `apps/game-server/src/server.rs` | 生产 PostgreSQL store 和服务装配、协议分发、关闭资源。 | 保留为运行时装配和协议分发位置。 |
| `apps/game-server/src/internal_server.rs` | 内部服务/测试场景使用 disabled PostgreSQL store 装配上下文。 | 保留为测试装配位置；后续通过 facade 与可替换 port 装配。 |

## 当前事务、提交与审计语义

当前 `PgCharacterElementStore::apply_change` 的原子写入顺序是：

1. 开启 PostgreSQL 事务；
2. 对未软删除角色执行 `SELECT ... FOR UPDATE`，锁定角色行；
3. 将行映射为永久状态并执行领域变更与不变量校验；
4. 在同一事务更新 `characters` 的八个 `affinity_*`/`mastery_*` 字段；
5. 在同一事务插入 `character_element_logs`，记录来源、操作者、八项 delta、`before_json`、`after_json` 和原因；
6. 提交事务后返回 `CharacterElementApplyResult` 的 `before`/`after` 快照。

角色不存在或领域校验失败时，当前实现回滚事务并返回稳定业务错误。任何失败路径均不应被下游解释为已提交。后续 adapter 必须保持该锁、校验、八字段更新、日志插入和提交的单一原子边界，不修改现有 schema 或日志字段。

## 失败与回滚策略

- 每个切换阶段只保留一个实际写入口：要么旧 facade，要么新 facade；禁止为“回滚”同时调用两套写入路径或双写数据库和日志。
- 阶段切换失败时，恢复上一版本或让兼容 facade 委托回旧实现；恢复后重新验证请求仍只经过一个写入口。
- 没有明确提交成功的结果不得触发 `CharacterElementsChanged`、成功响应或成功 push。
- 已经提交的数据库变更不因后续 push 失败而回滚；push 的重试/补偿属于协议或传输层后续职责。
- 删除旧路径前必须完成调用方切换和回归验证；兼容导出仅可转发，不能复制规则或建立第二个状态所有者。

## 相关协议和专题文档

- `docs/游戏服与接入层/游戏业务模块开发规范.md`
- `docs/游戏服与接入层/角色体系与四属性设计.md`
- `docs/游戏服与接入层/checklists/角色体系P1四属性基础_checklist.md`
- `summary/角色永久四属性模块迁移_checklist.md`
