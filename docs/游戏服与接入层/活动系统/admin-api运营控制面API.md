# admin-api 运营控制面 API

本文记录活动模块当前已落地的公共控制面契约。`admin-api` 已装配 PostgreSQL provider、活动审计和 Redis 刷新通知；数据库初始化或 provider 装配失败时仍以 `503 ACTIVITY_CONTROL_UNAVAILABLE` 安全拒绝。活动控制面不处理玩家领奖、抽奖算法或资产写入，这些行为属于 `game-server` 权威运行时。

## 路由与权限

| 方法 | 路径 | 权限 | 说明 |
| --- | --- | --- | --- |
| GET | `/api/v1/activities` | `activities.read` | 活动列表，支持 `status`、`activityType`、`key`、`limit`、`offset` |
| GET | `/api/v1/activities/:activityId` | `activities.read` | 活动详情和当前版本摘要 |
| POST | `/api/v1/activities/drafts` | `activities.write` | 创建草稿 |
| PATCH | `/api/v1/activities/:activityId/drafts` | `activities.write` | 更新未发布草稿，支持 `ifMatch` |
| POST | `/api/v1/activities/:activityId/drafts` | `activities.write` | 从当前已发布或已下线版本 fork 新草稿 |
| POST | `/api/v1/activities/:activityId/preflight` | `activities.publish` | 发布前校验，返回字段级错误 |
| POST | `/api/v1/activities/:activityId/publish` | `activities.publish` | 发布不可变版本 |
| POST | `/api/v1/activities/:activityId/offline` | `activities.offline` | 下线当前发布版本 |
| GET | `/api/v1/activities/:activityId/records` | `activities.records.read` | 只读查询领奖、抽奖和发奖事实 |

`viewer` 只有 `activities.read`；发布和下线权限分离。`admin`/`super_admin` 具备全部活动权限，其他角色必须由策略显式授予。

## 版本与并发

- 草稿更新使用 `ifMatch`（ETag 或 revision），旧值返回 `409 ACTIVITY_VERSION_CONFLICT`。
- 已发布版本不可通过 PATCH 修改，返回 `409 ACTIVITY_PUBLISHED_IMMUTABLE`。
- 从发布或下线版本创建新草稿的请求必须包含 `sourceVersion`、`ifMatch`、`reason` 和 `overrides` 对象；身份字段不可覆盖，旧 source CAS 返回 409。
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

## 自动化调用约定

本文的请求示例以当前代码实际接受的 JSON 为准，示例中的 `activityId`、`etag`、`version`、JWT 和时间均为占位值。自动化程序应始终使用上一步响应中的值，不要自行拼接版本或 ETag。

- 所有路径均相对于 `https://admin.example.test`；本地开发通常为 `http://localhost:3001`。
- 除查询接口外，调用均使用 `Content-Type: application/json`。
- 所有接口都需要管理员 JWT：`Authorization: Bearer <ADMIN_JWT>`。JWT 的获取属于 admin-api 认证流程，不把密码写入活动配置或脚本日志。
- 当前控制器会拒绝未知字段。请求对象中不要附带 `requestId`、`actorId`、`createdBy` 等未列入白名单的字段；`actorId` 由 JWT 中的 `sub`/`username` 注入。
- JSON 对象最大 64 KiB，最大嵌套深度为 8。时间使用可被 `Date.parse` 解析的 ISO-8601 字符串，建议统一使用带 `Z` 的 UTC 时间。
- 发布版本不可变。修改已发布或已下线活动必须先 fork 新草稿；不要尝试 PATCH 已发布版本。
- `ifMatch` 支持详情返回的弱 ETag（例如 `W/"activity/<id>/<revision>"`）或详情返回的数字 `revision`。推荐原样传 ETag。
- 失败响应不是业务成功：调用方必须检查 HTTP 状态和响应中的 `ok`/`error`，不能仅检查 HTTP 请求是否发出。

## 字段字典

### 1. 草稿请求公共字段

以下字段出现在 `POST /activities/drafts` 和 `PATCH /activities/:activityId/drafts` 请求中。PATCH 是完整替换草稿，不是局部 merge；自动化程序应先 GET 详情，再基于 `draft` 完整提交所有字段。

| 字段 | 类型 | 必填 | 含义与约束 |
| --- | --- | --- | --- |
| `key` | string | 是 | 运营可读的活动唯一键。同一数据库内不能重复；建议使用稳定的业务命名，如 `login_2026_summer`。长度不超过 64 个字符。已发布版本不能修改身份键。 |
| `activityType` | string | 是 | 已注册的活动类型。当前只有 `login_reward`（登录奖励）和 `lottery`（随机抽奖）。未知类型返回 `ACTIVITY_UNKNOWN_TYPE`。 |
| `schemaVersion` | positive integer | 是 | 公共活动类型契约版本。当前支持 `1`，并且必须等于 `typeConfig.schema_version`。 |
| `startAt` | ISO timestamp | 是 | 活动开始时间，数据库按 UTC `TIMESTAMPTZ` 保存。活动时间窗为 `[startAt, endAt)`。 |
| `endAt` | ISO timestamp | 是 | 活动结束时间，必须晚于 `startAt`。等于结束时间的瞬间已不可产生新参与。 |
| `claimDeadline` | ISO timestamp | 是 | 已产生资格的最晚领取时间，必须不早于 `endAt`。等于 `endAt` 表示结束后不保留领取宽限期；`offline` 后不允许领取。 |
| `timezone` | string | 是 | 合法 IANA 时区，如 `Asia/Shanghai`、`UTC`。登录奖励的自然日按该时区计算，不使用服务器本地时区。 |
| `publicConfig` | object | 是 | 面向客户端展示的公共配置。当前常用字段为 `title`、`resources`、`show_before_start`；未被服务端业务使用的展示字段可作为 JSON 保存，但不能放入类型规则或服务器状态。 |
| `typeConfig` | object | 是 | 活动类型专属规则。字段由共享 `packages/activity-contract` schema 严格校验；服务器状态字段不可提交。详见“类型配置字典”。 |
| `stages` | array | 是 | 通用阶段容器。登录奖励通常每个阶段对应一天/档位；抽奖通常为空数组。每个元素只能包含 `stageId`、`stageNo`、`rewardGroupKey`、`qualification`、可选 `display`。 |
| `rewardGroups` | array | 是 | 奖励组容器。每组必须至少有一个奖励项，`key` 唯一，并被阶段或抽奖奖池引用。 |
| `reason` | string | 是 | 本次创建或修改原因，用于审计。长度不超过 512；不能是空白字符串。 |
| `ifMatch` | string | PATCH 可选、并发时实际必需 | 草稿 CAS 值。使用 GET 详情中的 `etag` 或 `revision`。缺失或过期会返回 `ACTIVITY_VERSION_CONFLICT`；创建草稿时不要传。 |

`publicConfig` 的几个已知字段：

| 字段 | 类型 | 含义 |
| --- | --- | --- |
| `title` | string | 客户端展示标题，不参与活动规则计算。 |
| `resources` | array/object | 客户端展示资源引用，例如图标、横幅或文案 key。控制面只保存 JSON，不下载或验证资源。 |
| `show_before_start` | boolean | 是否允许活动在开始前展示。展示不等于可参与或可领取；不填默认为 `false`。 |
| `claimMode` | string | 前端兼容字段，可用于展示手动/自动领取意图；当前数据库真正写入的领取模式来自 `typeConfig.claim_mode`。不要让它与类型配置冲突。 |

PostgreSQL 适配器会把请求中的 `rewardGroups` 规范化后，以 `publicConfig.reward_groups` 的镜像形式保存到发布快照；`rewardGroups` 请求字段才是控制面输入的权威来源。自动化程序不要手工维护两份奖励组，也不要把 `publicConfig.reward_groups` 当作替代字段。

### 2. 阶段字段

| 字段 | 类型 | 必填 | 含义与约束 |
| --- | --- | --- | --- |
| `stageId` | string | 是 | 阶段稳定标识，如 `day-1`。同一版本内唯一，建议只使用字母、数字、`.`、`_`、`-`，最长 64。领奖语义和审计会引用它。 |
| `stageNo` | positive integer | 是 | 阶段顺序号。同一版本内唯一；`login_reward` 必须按升序提交。 |
| `rewardGroupKey` | string | 是 | 指向 `rewardGroups[].key` 的外键式引用。不存在时预检返回 `UNKNOWN_REFERENCE`。登录奖励还要求它与 `typeConfig.stages[].reward_group_key` 一致。 |
| `qualification` | object | 是 | 服务端资格条件。首期可为空 `{}`；仓储会识别 `periodStrategy`、`maxClaims`、`resetPolicy` 作为阶段持久化策略，其余内容由运行时类型处理器解释。不能放入玩家提交的进度或奖励结果。 |
| `display` | object | 否 | 阶段展示信息，例如 `{ "title": "第 1 天", "description": "登录领取" }`。仅用于展示/快照，不替代 `qualification`。 |

`qualification` 当前被仓储读取的控制字段：

| 字段 | 类型 | 默认/可选值 | 含义 |
| --- | --- | --- | --- |
| `periodStrategy` | string | `once`；也可为 `natural_day`、`natural_week`、`activity_stage` | 领取周期策略。登录奖励通常由 `typeConfig.cycle_unit` 设为 `natural_day`，阶段中不填时自动继承。 |
| `maxClaims` | positive integer | `1` | 同一周期最多领取次数。 |
| `resetPolicy` | string | `none`；也可为 `reset`、`carry` | 断签/周期切换时的进度处理策略；登录奖励未填写时继承 `typeConfig.miss_policy`。 |

### 3. 奖励组和奖励项字段

| 字段 | 类型 | 必填 | 含义与约束 |
| --- | --- | --- | --- |
| `rewardGroups[].key` | string | 是 | 奖励组唯一键，如 `day-1`、`pool`。最长 64，建议使用字母数字和 `._-`。 |
| `rewardGroups[].selectionMode` | enum | 是 | `fixed` 表示固定发放组内奖励；`weighted` 表示按奖励项 `weight` 做整数权重选择。 |
| `rewardGroups[].items` | array | 是 | 奖励项列表，不能为空。 |
| `items[].item_id` | positive int32 | 是 | `ItemTable.csv` 中的物品 ID。服务端预检会对照权威奖励目录；不存在返回 `UNKNOWN_REWARD_ITEM`。 |
| `items[].quantity` | positive uint32 | 是 | 发放数量，必须大于 0。 |
| `items[].weight` | positive safe integer | weighted 时是 | 权重，不是百分比；只接受正整数。固定组可省略。 |
| `items[].binding` | enum | 否 | `unbound` 或 `character_bound`。若与权威物品 BindType 不兼容，预检返回 `REWARD_BINDING_MISMATCH`。 |

当前 API 不接受 `reward_type`、`asset_key`、`reward_json` 等数据库内部列，也不接受奖励项中的未知字段。活动奖励最终仍由 `game-server` 的统一资产交付能力处理。

### 4. 版本命令字段

预检、发布和下线共用以下请求体：

| 字段 | 类型 | 必填 | 含义 |
| --- | --- | --- | --- |
| `version` | positive integer | 是 | 要操作的草稿版本或当前发布版本。必须等于详情返回的 `version`；过期返回 409。 |
| `ifMatch` | string | 发布/下线时实际必需；预检建议携带 | 详情返回的 `etag` 或 `revision`。发布和下线会用它做 CAS；预检当前主要校验 `version`，但携带同一值可以让自动化请求保持一致。 |
| `reason` | string | 是 | 预检、发布或下线原因，写入审计。 |

从已发布或已下线版本 fork 新草稿时，请求体不同：

| 字段 | 类型 | 必填 | 含义 |
| --- | --- | --- | --- |
| `sourceVersion` | positive integer | 是 | 要复制的当前发布/下线版本。 |
| `ifMatch` | string | 推荐/并发时必需 | 当前活动详情的 ETag/revision。 |
| `reason` | string | 是 | fork 原因。新草稿会把它写入版本变更原因。 |
| `overrides` | object | 是 | 对发布快照的浅层字段覆盖。不能包含 `activityId`、`key`、`activityType`；需要修改嵌套对象时提交完整的新对象，避免误解为深 merge。 |

### 5. 列表和记录查询参数

| 参数 | 类型 | 含义 |
| --- | --- | --- |
| `status` | string | 活动状态过滤。当前控制面常用 `draft`、`published`、`offline`；数据库还可能计算 `running`、`ended`、`archived`。 |
| `activityType` | string | `login_reward` 或 `lottery`。 |
| `key` | string | 精确匹配活动键。 |
| `version` | positive integer | 记录查询限定配置版本。 |
| `characterId` | string | 记录查询限定角色。控制面只读，不接受玩家身份替换。 |
| `status`（records 接口） | string | 记录状态过滤，如 `processing`、`granted`、`retryable_failure`、`permanent_failure`、`manual_review`。列表接口的同名参数过滤活动生命周期状态。 |
| `from` / `to` | ISO timestamp | 记录创建时间半开区间 `[from,to)`；两者同时出现时 `from < to`。 |
| `requestId` | string | 原始玩家请求号过滤。PostgreSQL 适配器对请求号和邮件引用做哈希/脱敏后返回。 |
| `limit` | integer | 每页 1-100，默认 50。 |
| `offset` | integer | 非负偏移，默认 0。 |

## 类型配置字典

### `login_reward` schema 1

```json
{
  "schema_version": 1,
  "event_source": "game_entry",
  "cycle_unit": "natural_day",
  "progression": "consecutive",
  "miss_policy": "reset",
  "claim_mode": "manual",
  "stages": [
    { "stage_no": 1, "required_count": 1, "reward_group_key": "day-1" }
  ]
}
```

| 字段 | 可选值/类型 | 含义 |
| --- | --- | --- |
| `schema_version` | `1` | 类型规则版本，必须等于公共 `schemaVersion`。 |
| `event_source` | `game_entry` | 当前唯一支持的登录事件来源。 |
| `cycle_unit` | `natural_day` | 按 `timezone` 的自然日推进。 |
| `progression` | `consecutive` / `cumulative` | 连续登录计数，或累计登录计数。 |
| `miss_policy` | `reset` / `carry` | 连续模式断签后重置，或保留可继续推进的状态。具体玩家状态由运行时处理器维护。 |
| `claim_mode` | `manual` / `automatic` | 阶段资格产生后，玩家手动领取或由运行时自动触发领取。 |
| `stages` | 非空数组 | 类型规则中的阶段门槛；每个 `stage_no` 唯一、`required_count >= 1`，且 `reward_group_key` 必须与通用阶段对应。 |

服务端拥有的 `progress`、`state`、`last_period_key`、`consecutive_count`、`cumulative_count`、`claimed_stage_ids`、`current_stage_id`、`today_period_key`、`reward_items` 等字段不能出现在请求中。

### `lottery` schema 1

```json
{
  "schema_version": 1,
  "draw_source": "player_action",
  "pool_version": 1,
  "free_draw_count": 1,
  "voucher_item_id": 2001,
  "daily_draw_limit": 10,
  "total_draw_limit": 100,
  "pool_items": [
    { "item_id": 1001, "quantity": 1, "weight": 80 },
    { "item_id": 1002, "quantity": 1, "weight": 20 }
  ],
  "pity": { "enabled": false, "threshold": 0 },
  "limited_stock": { "enabled": false, "stock": 0 }
}
```

| 字段 | 类型/范围 | 含义 |
| --- | --- | --- |
| `schema_version` | `1` | 类型规则版本。 |
| `draw_source` | `player_action` | 当前唯一支持的抽奖来源；抽奖由玩家动作触发。 |
| `pool_version` | uint32，至少 1 | 奖池版本。已发布版本中，同一 `pool_version` 的奖池项和权重不可改变；修改奖池必须递增版本。 |
| `free_draw_count` | uint32 | 每个活动状态可用的免费抽奖次数。 |
| `voucher_item_id` | 可选正 int32 | 抽奖券物品 ID；不填表示不配置抽奖券消耗。 |
| `daily_draw_limit` | uint32 | 按活动时区自然日计算的抽奖次数上限；0 表示不允许抽奖，而不是无限。 |
| `total_draw_limit` | uint32 | 活动总抽奖次数上限；0 表示不允许抽奖。 |
| `pool_items` | 非空数组 | 奖池项。`item_id` 必须唯一，`quantity` 和 `weight` 必须为正整数；概率由整数权重计算，不提交百分比。 |
| `pity` | 可选 object | 预留保底策略字段，只能包含 `enabled`、`threshold`。是否生效以当前运行时实现为准。 |
| `limited_stock` | 可选 object | 预留限量库存字段，只能包含 `enabled`、`stock`。是否生效以当前运行时实现为准。 |

服务端拥有的抽奖结果、随机值、消费记录、剩余次数和 `result_state` 等字段不能提交。`pool_items` 中的每个 `item_id` 还必须出现在 `rewardGroups[].items` 奖励目录中。

## 响应对象

成功响应统一包一层 `ok: true`。以下是当前 PostgreSQL 控制面返回的关键结构；字段可能因状态省略：

```json
{
  "ok": true,
  "activityId": "8a1c...",
  "key": "login_2026_summer",
  "activityType": "login_reward",
  "status": "draft",
  "revision": 42,
  "version": 1,
  "configDigest": "sha256:<64 hex>",
  "etag": "W/\"activity-8a1c...-42\"",
  "draft": {
    "key": "login_2026_summer",
    "activityType": "login_reward",
    "schemaVersion": 1,
    "startAt": "2026-09-01T00:00:00.000Z",
    "endAt": "2026-09-08T00:00:00.000Z",
    "claimDeadline": "2026-09-10T00:00:00.000Z",
    "timezone": "Asia/Shanghai",
    "publicConfig": {},
    "typeConfig": {},
    "stages": [],
    "rewardGroups": [],
    "reason": "..."
  },
  "audit": { "status": "sent" }
}
```

详情中的 `snapshot` 是当前已发布或已下线的不可变快照；`draft` 是当前可编辑版本。活动处于 `draft` 时通常只有 `draft`，发布或下线后通常只有 `snapshot`。控制台应将没有 `draft` 的详情按只读快照展示。`configDigest` 是服务端对规范化公共配置和类型配置计算的 SHA-256 摘要，不应由自动化程序自行重算后作为身份依据。

列表成功响应为 `{ "ok": true, "items": [...], "total": 1, "limit": 50, "offset": 0, "audit": { "status": "sent" } }`；列表项是摘要，不保证含完整 `draft`/`snapshot`，需要详情时调用 GET。

记录成功响应为 `{ "ok": true, "items": [...], "total": ..., "limit": ..., "offset": ... }`。记录项至少包含 `recordId`、`activityId`、`version`、`recordType`、`status`、`createdAt`、`details`，有角色或请求号时还会包含 `characterId`、`requestId`。`recordType` 当前包括 `claim`、`draw`、`player_state`、`reward_grant`、`reward_mail`、`manual_review`。

## 全流程 API 参考样例

下面的样例创建一个 7 天、手动领取的连续登录奖励活动。样例采用 Bash 风格的 `curl` 环境变量；Windows PowerShell 请使用同名环境变量（例如 `$env:BASE`、`$env:MYSERVER_ADMIN_JWT`），或将命令中的 `$BASE` / `$MYSERVER_ADMIN_JWT` 替换为对应的 PowerShell 引用。

```bash
export BASE="https://admin.example.test"
export MYSERVER_ADMIN_JWT="<从密钥管理器读取>"
```

### 0. 获取管理员 JWT（前置步骤）

认证接口属于 admin-api 通用认证，不是活动控制面专属接口。自动化程序可复用已有登录凭据，但不应把密码写进后续活动请求：

```bash
curl -sS -X POST "$BASE/api/v1/auth/login" \
  -H 'Content-Type: application/json' \
  -d '{"username":"ops-bot","password":"<从密钥管理器读取>"}'
# 保存返回的 JWT 到 MYSERVER_ADMIN_JWT，然后再调用以下活动接口。
```

调用前应确认该主体拥有 `activities.write`、`activities.publish`、`activities.offline` 和 `activities.records.read`；只读机器人只需要 `activities.read`。

### 1. 创建草稿

创建请求必须一次提交完整草稿。这里使用 `activityType=login_reward`，通用阶段和类型阶段保持一一对应，奖励目录使用 `ItemTable.csv` 中已存在的物品 ID。

```bash
curl -sS -X POST "$BASE/api/v1/activities/drafts" \
  -H "Authorization: Bearer $MYSERVER_ADMIN_JWT" \
  -H 'Content-Type: application/json' \
  -d '{
    "key": "login_2026_summer",
    "activityType": "login_reward",
    "schemaVersion": 1,
    "startAt": "2026-09-01T00:00:00.000Z",
    "endAt": "2026-09-08T00:00:00.000Z",
    "claimDeadline": "2026-09-10T00:00:00.000Z",
    "timezone": "Asia/Shanghai",
    "publicConfig": {
      "title": "夏日登录礼",
      "show_before_start": true,
      "resources": [
        { "kind": "banner", "key": "activity/login_2026_summer/banner" }
      ]
    },
    "typeConfig": {
      "schema_version": 1,
      "event_source": "game_entry",
      "cycle_unit": "natural_day",
      "progression": "consecutive",
      "miss_policy": "reset",
      "claim_mode": "manual",
      "stages": [
        { "stage_no": 1, "required_count": 1, "reward_group_key": "day-1" },
        { "stage_no": 2, "required_count": 2, "reward_group_key": "day-2" },
        { "stage_no": 3, "required_count": 3, "reward_group_key": "day-3" }
      ]
    },
    "stages": [
      { "stageId": "day-1", "stageNo": 1, "rewardGroupKey": "day-1", "qualification": {}, "display": { "title": "第 1 天" } },
      { "stageId": "day-2", "stageNo": 2, "rewardGroupKey": "day-2", "qualification": {}, "display": { "title": "第 2 天" } },
      { "stageId": "day-3", "stageNo": 3, "rewardGroupKey": "day-3", "qualification": {}, "display": { "title": "第 3 天" } }
    ],
    "rewardGroups": [
      { "key": "day-1", "selectionMode": "fixed", "items": [{ "item_id": 1001, "quantity": 1 }] },
      { "key": "day-2", "selectionMode": "fixed", "items": [{ "item_id": 1002, "quantity": 2 }] },
      { "key": "day-3", "selectionMode": "fixed", "items": [{ "item_id": 1003, "quantity": 1, "binding": "character_bound" }] }
    ],
    "reason": "创建 2026 夏日登录活动"
  }'
```

保存响应中的 `activityId`、`version` 和 `etag`，后续请求都从响应读取。创建时会执行一次服务端草稿校验；物品不存在、阶段引用错误或类型 schema 错误会直接返回 400/422，不会创建可用草稿。

### 2. 读取详情并建立 CAS 基线

```bash
curl -sS "$BASE/api/v1/activities/$ACTIVITY_ID" \
  -H "Authorization: Bearer $MYSERVER_ADMIN_JWT"
```

自动化程序应解析：

```text
revision = response.revision
etag     = response.etag
version  = response.version
draft    = response.draft
```

不要从 `configDigest` 推导 `ifMatch`，也不要使用旧响应中的 ETag。多个操作员同时修改时，先 GET 再提交是必要步骤。

### 3. 更新草稿

PATCH 是完整草稿替换。下面示例把标题更新为“夏日连续登录礼”；实际脚本应把 GET 返回的 `draft` 全量复制后只修改目标字段，并补充新的 `reason` 和 `ifMatch`。

```bash
curl -sS -X PATCH "$BASE/api/v1/activities/$ACTIVITY_ID/drafts" \
  -H "Authorization: Bearer $MYSERVER_ADMIN_JWT" \
  -H 'Content-Type: application/json' \
  -d '{
    "key": "login_2026_summer",
    "activityType": "login_reward",
    "schemaVersion": 1,
    "startAt": "2026-09-01T00:00:00.000Z",
    "endAt": "2026-09-08T00:00:00.000Z",
    "claimDeadline": "2026-09-10T00:00:00.000Z",
    "timezone": "Asia/Shanghai",
    "publicConfig": {
      "title": "夏日连续登录礼",
      "show_before_start": true,
      "resources": [{ "kind": "banner", "key": "activity/login_2026_summer/banner" }]
    },
    "typeConfig": {
      "schema_version": 1,
      "event_source": "game_entry",
      "cycle_unit": "natural_day",
      "progression": "consecutive",
      "miss_policy": "reset",
      "claim_mode": "manual",
      "stages": [
        { "stage_no": 1, "required_count": 1, "reward_group_key": "day-1" },
        { "stage_no": 2, "required_count": 2, "reward_group_key": "day-2" },
        { "stage_no": 3, "required_count": 3, "reward_group_key": "day-3" }
      ]
    },
    "stages": [
      { "stageId": "day-1", "stageNo": 1, "rewardGroupKey": "day-1", "qualification": {}, "display": { "title": "第 1 天" } },
      { "stageId": "day-2", "stageNo": 2, "rewardGroupKey": "day-2", "qualification": {}, "display": { "title": "第 2 天" } },
      { "stageId": "day-3", "stageNo": 3, "rewardGroupKey": "day-3", "qualification": {}, "display": { "title": "第 3 天" } }
    ],
    "rewardGroups": [
      { "key": "day-1", "selectionMode": "fixed", "items": [{ "item_id": 1001, "quantity": 1 }] },
      { "key": "day-2", "selectionMode": "fixed", "items": [{ "item_id": 1002, "quantity": 2 }] },
      { "key": "day-3", "selectionMode": "fixed", "items": [{ "item_id": 1003, "quantity": 1, "binding": "character_bound" }] }
    ],
    "reason": "优化活动展示标题",
    "ifMatch": "W/\"activity/<activityId>/<revision-from-step-2>\""
  }'
```

成功后用新响应覆盖本地的 `revision`、`version`、`etag` 和 `draft`。如果返回 409，说明基线过期；必须重新 GET、重新合并业务变更，再提交，不能循环重放旧 PATCH。

### 4. 发布前预检

预检只接受版本命令，不接受完整草稿。它会读取服务端当前草稿，因此步骤 3 成功后才执行：

```bash
curl -sS -X POST "$BASE/api/v1/activities/$ACTIVITY_ID/preflight" \
  -H "Authorization: Bearer $MYSERVER_ADMIN_JWT" \
  -H 'Content-Type: application/json' \
  -d '{
    "version": 2,
    "ifMatch": "W/\"activity/<activityId>/<revision-after-step-3>\"",
    "reason": "发布前自动预检"
  }'
```

通过时响应类似：

```json
{
  "ok": true,
  "activityId": "<activityId>",
  "version": 2,
  "valid": true,
  "errors": [],
  "audit": { "status": "sent" }
}
```

失败时通常返回 HTTP 422 和 `ACTIVITY_PRECHECK_FAILED`，`details`/`errors` 中的 `path` 是可直接定位到请求 JSON 的路径，例如 `rewardGroups[0].items[0].quantity`。自动化程序应把错误整理给操作者，不要自动修改奖励数量、权重或时间。

### 5. 发布不可变版本

活动发布属于高风险操作。除了上一步的活动配置预检外，必须先用唯一 `requestId` 请求发布接口取得控制面预检，再由操作者确认影响摘要，最后复用同一 `requestId`、`preflightNonce` 和 `preflightSummarySha256` 执行。第一次请求只创建预检，不会发布版本：

```bash
curl -sS -X POST "$BASE/api/v1/activities/$ACTIVITY_ID/publish" \
  -H "Authorization: Bearer $MYSERVER_ADMIN_JWT" \
  -H 'Content-Type: application/json' \
  -d '{
    "version": 2,
    "ifMatch": "W/\"activity/<activityId>/<revision-after-step-3>\"",
    "reason": "预检通过，发布夏日登录活动",
    "requestId": "activity-publish-20260901-0001"
  }'
```

响应中的 `preflight.nonce`、`preflight.summarySha256` 和 `preflight.impactSummary` 仅用于本次确认。确认后执行第二次请求：

```bash
curl -sS -X POST "$BASE/api/v1/activities/$ACTIVITY_ID/publish" \
  -H "Authorization: Bearer $MYSERVER_ADMIN_JWT" \
  -H 'Content-Type: application/json' \
  -d '{
    "version": 2,
    "ifMatch": "W/\"activity/<activityId>/<revision-after-step-3>\"",
    "reason": "预检通过，发布夏日登录活动",
    "requestId": "activity-publish-20260901-0001",
    "preflightNonce": "<response.preflight.nonce>",
    "preflightSummarySha256": "<response.preflight.summarySha256>"
  }'
```

成功响应会返回 `status: "published"`、`snapshot`、`configDigest`，以及配置刷新通知结果：`notification.status` 为 `sent` 或 `failed`。如果执行结果为 `execution_uncertain`，不要直接重试，应先查询操作审计和活动详情。数据库版本已经发布后，Redis 刷新通知失败不会回滚版本；自动化程序应告警并检查各 `game-server` 实例的刷新指标。

### 6. 发布后核验和记录查询

先读取详情确认快照，再查询控制面事实记录：

```bash
curl -sS "$BASE/api/v1/activities/$ACTIVITY_ID" \
  -H "Authorization: Bearer $MYSERVER_ADMIN_JWT"

curl -sS "$BASE/api/v1/activities/$ACTIVITY_ID/records?version=2&limit=50&offset=0" \
  -H "Authorization: Bearer $MYSERVER_ADMIN_JWT"
```

按角色和时间窗口查询示例：

```bash
curl -G -sS "$BASE/api/v1/activities/$ACTIVITY_ID/records" \
  -H "Authorization: Bearer $MYSERVER_ADMIN_JWT" \
  --data-urlencode "version=2" \
  --data-urlencode "characterId=character-10001" \
  --data-urlencode "status=granted" \
  --data-urlencode "from=2026-09-01T00:00:00Z" \
  --data-urlencode "to=2026-09-08T00:00:00Z" \
  --data-urlencode "limit=50" \
  --data-urlencode "offset=0"
```

记录查询是只读的。它不能补发奖励、修改领取状态或删除历史；需要补偿时必须走独立 GM/资产纠正流程，并保留关联审计。

### 7. 下线当前发布版本

下线同样属于高风险操作。下线前重新 GET 详情，使用最新的当前版本和 ETag；先请求一次下线接口取得预检，再复用同一 `requestId`、nonce 和摘要执行。不要沿用发布响应之前缓存的值：

```bash
curl -sS -X POST "$BASE/api/v1/activities/$ACTIVITY_ID/offline" \
  -H "Authorization: Bearer $MYSERVER_ADMIN_JWT" \
  -H 'Content-Type: application/json' \
  -d '{
    "version": 2,
    "ifMatch": "W/\"activity/<activityId>/<revision-after-publish>\"",
    "reason": "活动结束，按计划下线",
    "requestId": "activity-offline-20260908-0001"
  }'
```

第一次响应为控制面预检时，将 `preflight.nonce` 和 `preflight.summarySha256` 写入第二次请求的 `preflightNonce`、`preflightSummarySha256`。执行成功后下线不会删除配置、玩家状态、领取记录、抽奖记录或奖励流水。响应状态为 `offline`，并带有 `offlineReason`；下线后的活动不能再领取，即使 `claimDeadline` 尚未到达。若返回 `execution_uncertain`，先核对审计和详情，不要直接重试。

### 8. 修改已发布或已下线活动：fork 新草稿并发布新版本

已发布或已下线版本不能 PATCH，也不能直接恢复上线。以下示例把第 3 天奖励从 `item_id=1003` 改为 `item_id=2001`，同时把奖池/奖励内容作为完整嵌套对象覆盖：

```bash
curl -sS -X POST "$BASE/api/v1/activities/$ACTIVITY_ID/drafts" \
  -H "Authorization: Bearer $MYSERVER_ADMIN_JWT" \
  -H 'Content-Type: application/json' \
  -d '{
    "sourceVersion": 2,
    "ifMatch": "W/\"activity/<activityId>/<revision-after-publish>\"",
    "reason": "调整第 3 天奖励",
    "overrides": {
      "rewardGroups": [
        { "key": "day-1", "selectionMode": "fixed", "items": [{ "item_id": 1001, "quantity": 1 }] },
        { "key": "day-2", "selectionMode": "fixed", "items": [{ "item_id": 1002, "quantity": 2 }] },
        { "key": "day-3", "selectionMode": "fixed", "items": [{ "item_id": 2001, "quantity": 1, "binding": "character_bound" }] }
      ],
      "stages": [
        { "stageId": "day-1", "stageNo": 1, "rewardGroupKey": "day-1", "qualification": {}, "display": { "title": "第 1 天" } },
        { "stageId": "day-2", "stageNo": 2, "rewardGroupKey": "day-2", "qualification": {}, "display": { "title": "第 2 天" } },
        { "stageId": "day-3", "stageNo": 3, "rewardGroupKey": "day-3", "qualification": {}, "display": { "title": "第 3 天" } }
      ],
      "publicConfig": { "title": "夏日连续登录礼（调整版）", "show_before_start": true },
      "typeConfig": {
        "schema_version": 1,
        "event_source": "game_entry",
        "cycle_unit": "natural_day",
        "progression": "consecutive",
        "miss_policy": "reset",
        "claim_mode": "manual",
        "stages": [
          { "stage_no": 1, "required_count": 1, "reward_group_key": "day-1" },
          { "stage_no": 2, "required_count": 2, "reward_group_key": "day-2" },
          { "stage_no": 3, "required_count": 3, "reward_group_key": "day-3" }
        ]
      }
    }
  }'
```

fork 成功后服务端返回新的 `draft` 和新 `version`（通常为 `sourceVersion + 1`）。随后重复步骤 2、4、5、6；每一个版本都必须重新预检、发布并记录审计。若修改的是 `lottery` 奖池，必须同时递增 `typeConfig.pool_version`，否则预检会返回 `POOL_VERSION_IMMUTABLE` 或 `POOL_VERSION_ROLLBACK`。

## 自动化编排伪代码

以下逻辑表达了推荐的最小安全编排，不代表新增接口：

```text
token = login_from_secret_manager()
draft = POST /activities/drafts(full_config, reason)
activity_id = draft.activityId

detail = GET /activities/{activity_id}
edit = merge_full(detail.draft, operator_changes)
updated = PATCH /activities/{activity_id}/drafts(edit + reason, ifMatch=detail.etag)

check = POST /activities/{activity_id}/preflight(
  version=updated.version, ifMatch=updated.etag, reason="preflight")
assert check.valid == true

published = POST /activities/{activity_id}/publish(
  version=updated.version, ifMatch=updated.etag, reason="publish")
assert published.status == "published"
alert_if(published.notification.status == "failed")

GET /activities/{activity_id}
GET /activities/{activity_id}/records(version=published.version, ...)

latest = GET /activities/{activity_id}
POST /activities/{activity_id}/offline(
  version=latest.version, ifMatch=latest.etag, reason="offline")
```

遇到 `409` 时停止当前写流程，重新读取详情并让操作者确认如何合并；遇到 `422` 时只修复返回的字段级配置错误；遇到 `503 ACTIVITY_CONTROL_UNAVAILABLE` 时不要降级到内存或本地假成功，应等待控制面恢复后重试查询/写入。

## 常见错误与处理建议

| HTTP | `error` | 自动化处理 |
| --- | --- | --- |
| 400 | `ACTIVITY_UNKNOWN_FIELD`、`ACTIVITY_INVALID_REQUEST`、`ACTIVITY_INVALID_CONFIG` | 修正请求结构或删除未知字段；不要盲目重试。 |
| 403 | `FORBIDDEN` 或权限策略错误 | 停止写操作，申请对应活动权限；不要更换玩家或服务端凭据绕过权限。 |
| 404 | `ACTIVITY_NOT_FOUND` | 确认 `activityId` 是否来自当前环境；不要用活动 `key` 代替路径 ID。 |
| 409 | `ACTIVITY_VERSION_CONFLICT`、`ACTIVITY_PUBLISHED_IMMUTABLE`、`ACTIVITY_ALREADY_PUBLISHED`、`ACTIVITY_ALREADY_OFFLINE` | 重新 GET 详情；冲突由人工合并，已发布版本走 fork。重复状态可视为幂等结果，但仍应核对详情。 |
| 422 | `ACTIVITY_PRECHECK_FAILED` | 读取 `details` 中的 `path/code/message`，修复草稿后重新预检。 |
| 503 | `ACTIVITY_CONTROL_UNAVAILABLE` | 控制面依赖 PostgreSQL/Redis provider 不可用；保留请求上下文，恢复后重新 GET 确认是否已提交，不能直接重复创建。 |

自动化系统应把 `activityId`、目标版本、配置摘要、操作原因、调用者和最终响应状态写入自己的任务日志，但不要记录 JWT、密码、完整奖励 payload 或玩家敏感身份信息。
