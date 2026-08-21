# 活动模块03：运营后台 API Checklist

## 目标

实现 admin-api 活动控制面：草稿创建/编辑、发布预检、发布、下线、版本查询、领奖/发奖记录查询、权限和审计。

后台 API 不执行玩家领奖，不直接修改角色资产，不复制登录或抽奖类型逻辑。

## 基础原则

- [x] 发布、下线和配置修改记录管理员、原因、版本和配置摘要。（验证：`ActivityAuditSink` 记录 actor/action/reason/version/result 最小摘要；控制器注入 `req.admin.sub`）
- [x] 已发布版本不可直接编辑，编辑产生新草稿。（验证：PATCH 返回 `ACTIVITY_PUBLISHED_IMMUTABLE`；`POST /:activityId/drafts` 通过 source CAS fork 新 draft）
- [x] 类型配置通过同一 types/ 目录中的独立类型文件注册 validator。（验证：`packages/activity-contract` 与 `apps/admin-api/src/activity-types.js` 共享 registry validator，unknown type/schema 测试通过）

## 引用文档

- [活动功能总纲](../../docs/游戏服与接入层/活动系统/活动功能总纲.md)：活动后台工作流、版本不可变、预检和审计要求。
- [管理后台设计](../../docs/后台与运维/管理后台设计.md)：admin-api 控制面、管理员权限、审计和服务调用边界。
- [管理权限与高风险操作治理设计](../../docs/后台与运维/管理权限与高风险操作治理设计.md)：发布、下线和高风险运营操作的权限治理。
- [数据库初始化说明](../../docs/数据库/数据库初始化说明.md)：后台活动数据的数据库初始化边界。
- [数据库迁移体系设计](../../docs/数据库/数据库迁移体系设计.md)：活动表结构迁移和版本兼容要求。
- [服务注册中心设计](../../docs/周边服务/服务注册中心设计.md)：admin-api 到内部控制面的服务发现要求。

## 阶段 1：公共 API 契约

- 开始时间：2026-08-21 15:06:47 +08:00
- 结束时间：2026-08-21 15:19:34 +08:00
- 开发总结：完成 admin-api 活动控制面公共契约与路由接线；生产持久化服务暂以显式 unavailable 实现，未执行玩家领奖或资产修改。
- 验证记录：`node --test --test-concurrency=1 src/activity/activity.controller.test.js src/activity-types.test.js src/auth/roles.guard.test.js`（12/12 通过）；`git diff --check` 通过。

- [x] 定义草稿、版本、阶段、奖励组和类型配置 DTO。（验证：`apps/admin-api/src/activity/activity.dto.ts` 定义 DTO；控制器契约测试通过）
- [x] 定义创建、更新、预检、发布、下线、列表、详情和记录查询接口。（验证：`apps/admin-api/src/activity/activity.controller.ts` 注册 8 条控制面路由并接入 `AppModule`）
- [x] 定义分页、筛选、版本冲突和类型 schema 版本错误。（验证：控制器分页边界、`ifMatch`/版本命令和共享类型 schema 错误测试通过）
- [x] 限制 JSON payload 大小、字段深度和未知字段。（验证：`assertStrictJson` 限制 64KiB/深度 8，草稿结构未知字段拒绝；oversize、deep、unknown-field 测试通过）

## 阶段 2：草稿、发布与下线

- 开始时间：2026-08-21 15:22:11 +08:00
- 结束时间：2026-08-21 15:34:28 +08:00
- 开发总结：新增可替换活动控制领域 service/repository/notifier 边界和离线 adapter；生产仍绑定 unavailable provider。支持字段级预检、不可变发布快照、CAS/etag、已发布版本 fork 新草稿、重复发布/下线拒绝及缓存刷新失败不回滚。
- 验证记录：`node --test --test-concurrency=1 src/activity/activity.controller.test.js src/activity/activity-control.service.test.js src/activity-types.test.js src/auth/roles.guard.test.js`（16/16 通过）；`npx tsc --noEmit -p apps/admin-api/tsconfig.json` 通过；`git diff --check` 通过。

- [x] 实现草稿保存和发布前公共校验。（验证：`ActivityControlDomainService` 的 `validateDraft` 与 create/update/preflight/publish 测试覆盖时间窗、时区、阶段和奖励组引用）
- [x] 调用类型独立文件中的 validator，并返回字段级预检结果。（验证：共享 `activity-types.js` validator 接入；错误包含 `path/code/message`，domain test 通过）
- [x] 实现发布事务、不可变版本和缓存刷新通知。（验证：`InMemoryActivityControlRepository` 保存 immutable snapshot；`ActivityRefreshNotifier` 通知成功/失败测试通过，通知失败不回滚版本）
- [x] 实现下线原因、紧急关闭和重复操作保护。（验证：offline 保存 reason，重复 publish/offline 返回稳定错误并映射 HTTP 409）
- [x] 拒绝过期版本覆盖和并发编辑丢失。（验证：PATCH 草稿透传 `ifMatch`；发布/下线和 fork 新草稿均执行 revision/source CAS，stale 测试通过）

## 阶段 3：权限、审计与查询

- 开始时间：2026-08-21 15:35:02 +08:00
- 结束时间：2026-08-21 15:47:16 +08:00
- 开发总结：活动控制面接入独立 publish/offline RBAC 权限；新增 append-only 事实记录只读查询端口、全量过滤/分页和有界审计 sink。生产仍需后续接入真实活动数据库 adapter，当前不可用 provider 不冒充事实源。
- 验证记录：`node --test --test-concurrency=1 src/activity/activity.controller.test.js src/activity/activity-control.service.test.js src/activity-types.test.js src/auth/roles.guard.test.js`（18/18 通过）；`npx tsc --noEmit -p apps/admin-api/tsconfig.json` 通过；`git diff --check` 通过。

- [x] 接入现有 admin RBAC，区分查看、编辑、发布和下线权限。（验证：`ActivityController` 使用 `activities.read/write/publish/offline/records.read`，`roles.guard.test.js` 覆盖 viewer/admin 能力矩阵）
- [x] 查询领奖记录、抽奖结果、发奖流水和失败状态。（验证：`ActivityRecord.recordType/status` 与 records 只读查询覆盖 claim/draw/reward_grant 事实模型）
- [x] 支持按活动、版本、角色、状态、时间和 request_id 查询。（验证：repository.records 过滤 activity/version/character/status/from/to/requestId，并执行 limit/offset；service test 通过）
- [x] 禁止修改 append-only 奖励流水和历史领奖结果。（验证：生产接口仅暴露 records read；内存 adapter 仅提供 appendRecordForTest fixture，返回深拷贝且无 update/delete）

## 阶段 4：测试与接口文档

- 开始时间：2026-08-21 15:47:41 +08:00
- 结束时间：2026-08-21 15:53:08 +08:00
- 开发总结：补齐 controller 稳定 HTTP 状态、Swagger/OpenAPI 元数据、权限/并发/预检/重复操作/类型 schema/审计/分页测试，并新增运营控制面 API 文档。
- 验证记录：`npm --prefix apps/admin-api run --silent test:core`（166/166 通过）；活动定向组合测试（20/20 通过）；`npx tsc --noEmit -p apps/admin-api/tsconfig.json` 通过；`git diff --check` 通过。

- [x] 覆盖权限拒绝、并发更新、预检失败、重复发布和重复下线。（验证：activity service/controller tests 与 roles guard tests 覆盖 409/422/403 语义）
- [x] 覆盖未知类型、schema 不兼容、审计写入和分页查询。（验证：activity-types、audit sink、records filter/pagination tests 通过）
- [x] 同步 admin-api 文档、OpenAPI/接口测试和前端契约。（验证：`docs/游戏服与接入层/活动系统/admin-api运营控制面API.md`、README 更新；controller `ApiOperation/ApiResponse` 元数据已补齐）

## 最终完成定义

- 开始时间：2026-08-21 15:06:47 +08:00
- 结束时间：2026-08-21 15:54:02 +08:00
- 验收总结：活动 admin-api 控制面公共契约、草稿/版本生命周期、RBAC、审计和 append-only 事实查询均已完成离线实现与验证。生产 provider 仍显式 unavailable，真实 PostgreSQL/Redis adapter、玩家运行时和资产交付不在本轮启用。

- [x] 后台 API 可以完整管理活动草稿和发布版本。（验收：create/update/fork/preflight/publish/offline/list/detail 路由、CAS 和 immutable snapshot 测试通过）
- [x] 发布/下线具备权限、预检、审计和版本保护。（验收：独立 publish/offline RBAC、field-level preflight、audit sink、重复/CAS 保护测试通过）
- [x] 管理员可以查询事实但不能改写历史流水。（验收：records 只读过滤/分页/深拷贝视图，无 update/delete API；生产 provider 明确未启用）
