# admin-api 运营控制面 API

本文记录活动模块当前已落地的公共控制面契约。`admin-api` 已装配 PostgreSQL provider、活动审计和 Redis 刷新通知；数据库初始化或 provider 装配失败时仍以 `503 ACTIVITY_CONTROL_UNAVAILABLE` 安全拒绝。活动控制面不处理玩家领奖、抽奖算法或资产写入，这些行为属于 `game-server` 权威运行时。

## 路由与权限

| 方法 | 路径 | 权限 | 说明 |
| --- | --- | --- | --- |
| GET | `/api/v1/activities` | `activities.read` | 活动列表，支持 `status`、`activityType`、`key`、`limit`、`offset` |
| GET | `/api/v1/activities/:activityId` | `activities.read` | 活动详情和当前版本摘要 |
| POST | `/api/v1/activities/drafts` | `activities.write` | 创建草稿 |
| PATCH | `/api/v1/activities/:activityId/drafts` | `activities.write` | 更新未发布草稿，支持 `ifMatch` |
| POST | `/api/v1/activities/:activityId/drafts` | `activities.write` | 从当前已发布版本 fork 新草稿 |
| POST | `/api/v1/activities/:activityId/preflight` | `activities.publish` | 发布前校验，返回字段级错误 |
| POST | `/api/v1/activities/:activityId/publish` | `activities.publish` | 发布不可变版本 |
| POST | `/api/v1/activities/:activityId/offline` | `activities.offline` | 下线当前发布版本 |
| GET | `/api/v1/activities/:activityId/records` | `activities.records.read` | 只读查询领奖、抽奖和发奖事实 |

`viewer` 只有 `activities.read`；发布和下线权限分离。`admin`/`super_admin` 具备全部活动权限，其他角色必须由策略显式授予。

## 版本与并发

- 草稿更新使用 `ifMatch`（ETag 或 revision），旧值返回 `409 ACTIVITY_VERSION_CONFLICT`。
- 已发布版本不可通过 PATCH 修改，返回 `409 ACTIVITY_PUBLISHED_IMMUTABLE`。
- 从发布版本创建新草稿的请求必须包含 `sourceVersion`、`ifMatch`、`reason` 和 `overrides` 对象；身份字段不可覆盖，旧 source CAS 返回 409。
- 重复发布返回 `409 ACTIVITY_ALREADY_PUBLISHED`；重复下线返回 `409 ACTIVITY_ALREADY_OFFLINE`。
- 发布版本快照不可变；下线不删除历史事实。

## 预检与错误

预检检查公共时间窗、IANA 时区、阶段和奖励组引用、数量、重复键及共享活动类型 schema。类型配置只受 JSON object/大小/深度限制，业务字段由 `packages/activity-contract` validator 决定。

字段级预检失败使用：

```json
{
  "ok": false,
  "error": "ACTIVITY_PRECHECK_FAILED",
  "details": [{ "path": "rewardGroups[0].items[0].quantity", "code": "INVALID" }]
}
```

常见 HTTP 状态：`400` 请求契约错误，`404` 活动不存在，`409` CAS/状态冲突，`422` 发布预检失败，`503` PostgreSQL provider 初始化失败或不可用。

## 事实查询

`records` 只读返回 `claim`、`draw`、`reward_grant` 记录摘要。支持 `version`、`characterId`、`status`、半开时间区间 `[from,to)`、`requestId`、`limit` 和 `offset`；结果和详情均为副本，控制面没有修改或删除历史领奖、抽奖和奖励流水的接口。

成功的写操作、预检和查询，以及 CAS、状态、预检和查询失败，都会通过 `ActivityAuditSink` 写入最小审计事件：动作、活动、actor、原因、版本、结果和错误码。审计摘要不包含完整公共配置、类型配置或奖励 payload；审计存储失败通过响应中的 `audit.status=failed` 表示，不改变已完成的版本状态。

## OpenAPI

Nest 应用通过 `/api/docs` 暴露 OpenAPI 文档。DTO 和 controller 的字段白名单是运行时契约；不要把 fallback `ActivityControlUnavailableService` 的 503 当作业务成功。真实联调需要游戏 PostgreSQL 与 Redis，玩家闭环还需要 NATS、service registry、auth-http、game-server 和 mock-client；当前离线契约测试不替代空库 migration、多实例或外部客户端验收。
