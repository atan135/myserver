# 角色永久四属性模块

## 状态

本模块已完成从旧 `core` 入口的迁移，是 `game-server` 当前业务模块分层的首个样板。本文只描述已落地代码；与真实服务联调、持久离线 push 补偿和跨进程事件发布有关的事项在“非目标与后续项”中单独标记。

## 职责

模块唯一拥有角色永久四属性：

- `affinity`：地、火、水、风四项长期倾向比例；
- `mastery`：地、火、水、风四项长期掌握值；
- 永久四属性的读取、合法变更、可信来源/操作者/原因、提交前后快照和审计日志语义。

状态主体是服务端确认的 `character_id`。其他模块通过模块根导出的 Query、Command 和 `CharacterElementFacade` 读取或请求变更，不能直接更新 `characters` 的八个四属性字段。

## 明确不负责

本模块不拥有、计算或写入以下状态和规则：

- `core/character_element_effective.rs` 的有效属性聚合及其派生战斗属性；
- 职业/流派、称号、背包、仓库、装备、道具实例、Buff、场景上下文和系统临时修正；
- 任务、成就、活动、排行、世界事件为何产生四属性变更的业务决策；
- Protobuf、消息号、会话鉴权、连接响应和角色 push 的传输细节；
- SQLx、`PgPool`、PostgreSQL 连接池、运行时配置和服务装配。

职业条件、称号解锁、道具使用和有效属性计算是读取方或命令发起方；它们不得成为第二个永久四属性状态所有者。角色 push 只消费已提交的业务事实，不拥有状态。

## 核心状态与所有者

| 状态 | 唯一所有者 | 说明 |
| --- | --- | --- |
| `characters.affinity_{earth,fire,water,wind}` | 角色永久四属性模块 | 固定总和的角色长期倾向。 |
| `characters.mastery_{earth,fire,water,wind}` | 角色永久四属性模块 | 非负的角色长期掌握值。 |
| `character_element_logs` 中的变更审计记录 | 角色永久四属性模块 | 与对应永久状态写入在同一 PostgreSQL 事务提交。 |
| 有效属性、装备/职业/Buff/场景修正 | 非本模块 | 只能把永久快照作为输入，不得写回八个永久字段。 |

## 业务不变量

- `affinity` 四项之和始终为 `10000`。
- 每一项 `affinity` 与 `mastery` 均不得为负。
- 变更计算在写入前检查 `i32` 溢出；不得依赖数据库截断或溢出行为。
- 成功结果中的 `before` 和 `after` 来自同一次已提交事务。
- 每次成功永久变更记录 delta、来源、操作者、原因和前后快照；拒绝的变更不留下成功日志、成功结果或成功事件。

## 公开 API

模块根 `business::character_element` 是唯一的外部 Rust 入口，当前导出：

| 类型 | 已落地能力 | 语义 |
| --- | --- | --- |
| Query | `GetCharacterElements` + `CharacterElementFacade::get_character_elements` | 读取服务端已确认角色的永久四属性快照，不产生副作用。 |
| Command | `ApplyCharacterElementChange` + `CharacterElementFacade::apply_character_element_change` | 使用可信上下文提交 delta，返回已提交的 `before`、`after` 与 `character_id`。 |
| Event | `CharacterElementsChanged` | 过去式的已提交业务事实，位于成功 Command 结果中，带不可变的前后快照和变更上下文。 |
| Preview | `CharacterElementFacade::preview_character_element_change` | 仅校验并投影变更，不写入状态、不创建成功事件。 |

`TrustedCharacterElementChangeContext` 由服务端调用方构造：它要求非空 `source_type`、配对的操作者类型/ID，并在进入 repository 前校验审计字段长度、规范化原因。API 不接收 `AuthenticatedSessionIdentity`、Protobuf、连接对象、SQLx row/transaction 或可变 `PlayerData`。

## 目录、可见性与依赖

```text
business/character_element/
  MODULE.md
  api/{contracts.rs, facade.rs}
  application/{mod.rs, ports.rs}
  domain/mod.rs
adapters/persistence/character_element_repository.rs
gameservice/character_element/mod.rs
```

- `api` 和 `domain` 是模块私有目录；模块根再导出外部需要的契约和值类型。
- `application` 使用 `pub(super)`，只向模块根开放；它组织 Query/Command 和 repository 失败语义。
- repository port 由模块根以 `pub(crate)` 重新导出，只允许 crate 内的 persistence adapter 装配；业务调用方不得导入 port、`application`、`domain` 或 adapter 内部路径。
- `domain` 不依赖 SQLx、Tokio、Protobuf、会话、配置运行时、网络或其他业务模块。
- `adapters/persistence/character_element_repository.rs` 是唯一的 PostgreSQL/SQLx 实现；`server.rs` 创建 `PgCharacterElementStore`，注入 facade，并在关闭时释放它。
- `gameservice/character_element` 从已鉴权会话提取角色与操作者，完成协议映射和已提交事实的 push 适配。

`game-server` 是二进制 crate，因此此处 `pub(crate)` 是当前跨模块可见边界，不表示外部 Rust crate 可直接调用的 SDK。

## 当前调用方

- `gameservice/character_element` 提供 `GetCharacterElementsReq/Res(1413/1414)` 和受控 debug 变更 `1415/1416` 的协议适配。
- `core/character_progress`、`core/character_discipline` 和 `core/character_title_unlock` 使用 facade 读取或请求永久变更。
- 背包道具流程构造四属性 delta；它不持有永久状态。
- `core/character_element_effective`、场景和战斗仅消费永久快照，计算临时有效属性。
- `server.rs` 进行 PostgreSQL adapter、facade 和 `ServiceContext` 的运行时装配；它不包含四属性领域规则。

## 事务、事件与失败语义

生产 PostgreSQL adapter 的 `apply_change` 在一个事务内执行：

1. 开启 PostgreSQL 事务并以 `SELECT ... FOR UPDATE` 锁定未软删除角色行；
2. 映射永久状态，执行领域变更和不变量校验；
3. 更新八个 `affinity_*` / `mastery_*` 字段；
4. 插入包含来源、操作者、delta、`before_json`、`after_json` 与原因的 `character_element_logs`；
5. 明确提交成功后返回 `before`/`after`，facade 才构造 `CharacterElementsChanged`。

`CharacterElementsChanged` 不是写入请求、不是 NATS 消息，也不等同于已经送达的玩家 push。`gameservice` 只在收到这个已提交事实后记录并发送 `CharacterElementsChangePush(1505)`。角色不存在、领域校验失败、repository 不可用、repository 失败或 `OutcomeUnknown` 都不能返回成功事件或发送成功 push。提交后的 push 失败不回滚数据库；重试/补偿属于传输层后续职责。

## 非目标与后续项

以下不是当前已落地能力：

- durable outbox、NATS 发布器或跨进程四属性事件订阅；当前 `CharacterElementsChanged` 只是成功结果中的进程内业务事实。
- 跨实例持久离线 push 补偿、push 重试和送达确认。
- 对 PostgreSQL、Redis、Core NATS、`auth-http`、`game-proxy`、`game-server` 的本次真实联调；启动依赖或执行联调必须先获得用户确认。
- 将有效属性计算、职业、称号、背包或奖励来源迁入本模块。

后续扩展必须继续通过模块根公开 API 访问永久四属性，不得恢复旧 `core::character_element` 或 `core::service::character_element_service` 写入口，也不得建立双写兼容路径。

## 相关协议和专题文档

- `docs/游戏服与接入层/游戏业务模块开发规范.md`
- `docs/游戏服与接入层/Rust游戏服开发指南.md`
- `docs/游戏服与接入层/角色体系与四属性设计.md`
- `docs/游戏服与接入层/checklists/角色体系P1四属性基础_checklist.md`
- `summary/角色永久四属性模块迁移_checklist.md`
