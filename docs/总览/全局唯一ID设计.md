# 全局唯一 ID 设计

## 1. 文档定位

本文定义 MyServer 中玩家、物品实例、房间、邮件、公告、聊天消息等持久业务对象的统一 ID 生成口径。

目标：

- 同一业务对象 ID 在全服、跨服务、多实例下唯一。
- 合服时不需要重写历史 ID。
- 可以从 ID 中解析出生服 `origin_id`，并结合世界归并表追溯合服前后的归属。
- 避免把数据库自增主键、进程实例 ID 或当前服务注册 ID 当作跨服业务 ID。

当前项目仍处于新项目阶段，落地全局 ID 时不保留旧 ID 兼容层。已有开发数据、旧格式测试数据和旧生成逻辑可以直接清空或删除，重新按本文机制初始化。

本文只约束持久业务 ID。房间内临时实体 ID、连接 session ID、帧号等局部运行时 ID 不属于本文范围。

## 2. 核心概念

### 2.1 origin_id

`origin_id` 表示 ID 的出生服或初始数据来源，写入全局 ID 的 bit 字段中，永久不变。

规则：

- 一个正式初始服分配一个唯一 `origin_id`。
- `origin_id` 一旦分配，不再复用给另一个正式初始服。
- 合服不会修改历史 ID 中的 `origin_id`。
- 合服后新生成 ID 默认复用目标世界的 `active_origin_id`，通常是主服 `origin_id`。

`origin_id` 不是当前世界 ID，也不是服务实例 ID。

### 2.2 world_id

`world_id` 表示当前运营世界或逻辑服，可随合服变化。

示例：

```text
合服前：
S1: world_id=1, active_origin_id=1
S2: world_id=2, active_origin_id=2

合服后：
S1+S2: world_id=10001, active_origin_id=1
历史 S1 数据：ID 内 origin_id=1
历史 S2 数据：ID 内 origin_id=2
合服后新数据：ID 内 origin_id=1
```

### 2.3 worker_id

`worker_id` 表示同一个 `origin_id` 下的某个发号器实例，用于避免多个进程在同一毫秒内生成相同 ID。

规则：

- `worker_id` 只在同一个 `origin_id` 内要求唯一。
- `worker_id` 不等于 `SERVICE_INSTANCE_ID`。
- 不生成持久业务 ID 的服务实例不需要占用 `worker_id`。
- 生产环境应通过 Redis lease 或等价机制分配和续租 `worker_id`，避免重复。

## 3. ID 位布局

统一使用 63-bit 正整数，最高位保持 0，便于存入 PostgreSQL `bigint`，并兼容 Protobuf `uint64`。

推荐布局：

```text
| 41 bits time_ms | 10 bits origin_id | 6 bits worker_id | 6 bits sequence |
```

字段含义：

| 字段 | 位数 | 范围 | 说明 |
|------|------|------|------|
| `time_ms` | 41 | 约 69 年 | 相对项目自定义 epoch 的毫秒数 |
| `origin_id` | 10 | `0-1023` | ID 出生服或初始数据来源 |
| `worker_id` | 6 | `0-63` | 同一 origin 下的发号器实例 |
| `sequence` | 6 | `0-63` | 同一 worker 同一毫秒内递增序号 |

建议保留 `origin_id=0` 给本地开发、测试或无效占位，正式服从 `1` 开始分配。

默认 epoch 建议固定为：

```text
2026-01-01T00:00:00Z
```

对应毫秒值：

```text
1767225600000
```

## 4. 业务 ID 规范

### 4.1 数字 ID 与字符串 ID

底层统一生成 `u64`/`bigint` 数字 ID。

对客户端、HTTP API、日志和数据库字符串字段可使用带前缀的字符串形式：

```text
<prefix>_<base32(global_id)>
```

推荐前缀：

| 对象 | 推荐字段 | 格式 | 说明 |
|------|----------|------|------|
| 玩家 | `player_id` | `plr_<base32>` | 新玩家直接使用该格式，不保留旧 `player-uuid` 兼容 |
| 物品实例 | `item_uid` / `uid` | `uint64` | 协议当前已是 `uint64` |
| 房间 | `room_id` | `room_<base32>` | 客户端和 route store 使用字符串 |
| 邮件 | `mail_id` | `mail_<base32>` | 新邮件直接使用该格式 |
| 公告 | `announce_id` | `ann_<base32>` | 新公告直接使用该格式 |
| 聊天消息 | `msg_id` | `msg_<base32>` | 新消息直接使用该格式 |
| 聊天群 | `group_id` | `grp_<base32>` | 新群组直接使用该格式 |
| GM/业务幂等请求 | `request_id` | 外部传入或 `req_<base32>` | 外部系统请求仍可使用其自身幂等 ID |

### 4.2 配置 ID 不使用全局发号器

配置表 ID 不是业务实例 ID。

例如：

- `item_id`：物品配置 ID，对应 `ItemTable.csv`。
- `skill_id`：技能配置 ID。
- `scene_id`：场景配置 ID。
- `buff_id`：Buff 配置 ID。

这些 ID 应由配置表管理，不通过全局 ID 生成器生成。

### 4.3 局部运行时 ID 不使用全局发号器

以下 ID 只在进程、房间或一次连接内有效，不需要全局唯一：

- `session_id`
- 帧号 `frame_id`
- ECS 局部 `entity_id`
- 容器格子下标
- 临时请求序号 `seq`

如果某类对象未来需要持久化、跨服追踪或进入交易/审计链路，再升级为全局业务 ID。

## 5. 生成器规则

生成器必须满足：

1. 同一 `origin_id + worker_id` 内 ID 单调递增。
2. 同一毫秒内 `sequence` 从 `0` 递增。
3. `sequence` 溢出时等待下一毫秒。
4. 系统时间短暂回拨时使用上次逻辑时间继续生成，不允许生成小于等于已发出的 ID。
5. 系统时间严重回拨时应拒绝发号并告警，避免长期时间漂移。

伪代码：

```text
now = current_ms() - EPOCH_MS
if now < last_time_ms:
  if last_time_ms - now <= MAX_CLOCK_BACKWARD_MS:
    now = last_time_ms
  else:
    fail CLOCK_MOVED_BACKWARD

if now == last_time_ms:
  sequence += 1
  if sequence > MAX_SEQUENCE:
    wait until next millisecond
else:
  sequence = 0

last_time_ms = now
id = (time_ms << 22) | (origin_id << 12) | (worker_id << 6) | sequence
```

其中 `22 = 10 + 6 + 6`。

## 6. worker_id 分配

生产环境推荐使用 Redis lease。

示例 key：

```text
id:worker:<origin_id>:<worker_id>
id:last-ts:<origin_id>:<worker_id>
id:origin:<origin_id>
```

启动流程：

1. 读取 `GLOBAL_ID_ORIGIN_ID`。
2. 若显式配置 `GLOBAL_ID_WORKER_ID`，尝试抢占对应 worker lease。
3. 若未配置 worker，则在允许范围内寻找空闲 worker。
4. 抢占成功后定期续租。
5. 抢占失败或 Redis 不可用时，生产环境应启动失败。
6. 运行中若续租失败或发现 lease 已被其它实例抢占，发号器必须立即 fail-closed，拒绝继续生成业务 ID。

本地开发可允许手动配置固定 worker，例如：

```text
GLOBAL_ID_ORIGIN_ID=0
GLOBAL_ID_WORKER_ID=1
```

注意：

- `SERVICE_INSTANCE_ID=game-server-001` 用于服务注册、路由和运维识别。
- `GLOBAL_ID_WORKER_ID=3` 只用于 ID 生成。
- 二者不要混用。

## 7. 合服追溯模型

合服时不改历史 ID，不新建 `origin_id`。合服后新 ID 复用目标世界的 `active_origin_id`。

推荐表结构：

```sql
CREATE TABLE id_origins (
  origin_id smallint PRIMARY KEY,
  origin_key varchar(64) NOT NULL UNIQUE,
  created_at timestamptz NOT NULL,
  retired_at timestamptz NULL
);

CREATE TABLE worlds (
  world_id bigint PRIMARY KEY,
  world_key varchar(64) NOT NULL UNIQUE,
  active_origin_id smallint NOT NULL,
  created_at timestamptz NOT NULL,
  retired_at timestamptz NULL
);

CREATE TABLE world_origin_memberships (
  world_id bigint NOT NULL,
  origin_id smallint NOT NULL,
  joined_at timestamptz NOT NULL,
  left_at timestamptz NULL,
  PRIMARY KEY (world_id, origin_id, joined_at)
);

CREATE TABLE world_merge_events (
  merge_id bigint PRIMARY KEY,
  target_world_id bigint NOT NULL,
  active_origin_id smallint NOT NULL,
  source_world_ids bigint[] NOT NULL,
  source_origin_ids smallint[] NOT NULL,
  merged_at timestamptz NOT NULL,
  operator varchar(64) NULL,
  details_json jsonb NULL
);
```

追溯流程：

1. 解码业务 ID，得到 `origin_id` 和 `created_at_ms`。
2. 查询 `id_origins`，确认该 `origin_id` 的初始服。
3. 查询 `world_origin_memberships`，找到该时间点 `origin_id` 属于哪个 `world_id`。
4. 如需合服详情，再查询 `world_merge_events`。

业务表建议保存当前世界归属字段，例如 `current_world_id` 或可通过玩家账号关系查询到当前世界，避免每次业务查询都解码 ID。

## 8. 合服示例

初始状态：

```text
origin_id=1 -> cn-s001
origin_id=2 -> cn-s002

world_id=1 -> cn-s001, active_origin_id=1
world_id=2 -> cn-s002, active_origin_id=2
```

合服到 `cn-s001-s002`：

```text
target_world_id=10001
active_origin_id=1
source_world_ids=[1,2]
source_origin_ids=[1,2]
```

合服后：

- 历史 S1 ID 仍解码出 `origin_id=1`。
- 历史 S2 ID 仍解码出 `origin_id=2`。
- 新玩家、新物品、新邮件默认使用 `origin_id=1`。
- 判断 `origin_id=1` 的某个 ID 是合服前还是合服后生成，需要结合 ID 时间和 `world_merge_events.merged_at`。

## 9. 后台展示与查询

全局 ID 和合服来源追踪需要在 `admin-web` 上可见、可查，避免只能通过脚本或数据库手工解码。

### 9.1 后台页面

建议在 `admin-web` 增加“全局 ID”页面，入口权限使用 `id.read`。

页面至少包含三个视图：

| 视图 | 能力 | 说明 |
|------|------|------|
| ID 解码 | 输入任意业务 ID，展示解析结果 | 支持 `plr_`、`room_`、`mail_`、`ann_`、`msg_`、`grp_` 和纯数字 `item_uid` |
| Origin / World 查询 | 查看 origin、world 和当前 active origin | 支持按 `origin_id`、`origin_key`、`world_id`、`world_key` 搜索 |
| 合服事件 | 查看合服历史 | 展示源 world/origin、目标 world、active origin、合服时间、操作者和备注 |

ID 解码结果建议展示：

| 字段 | 说明 |
|------|------|
| `raw_id` | 原始输入 |
| `normalized_id` | 规范化后的 ID |
| `id_kind` | 玩家、物品、房间、邮件、公告、聊天消息、群组等 |
| `numeric_id` | 底层 `u64` 数字 ID |
| `created_at` | 从 ID 时间字段解析出的创建时间 |
| `origin_id` | ID 出生 origin |
| `origin_key` | 出生服标识 |
| `worker_id` | 发号器实例 |
| `sequence` | 同毫秒序号 |
| `world_at_create` | 生成时所属 world |
| `current_world` | 当前所属 world，若对象可从业务表关联到玩家或 world |
| `merge_context` | 是否处于某次合服前后，来自 `world_merge_events` |

### 9.2 admin-api 接口

建议由 `admin-api` 提供只读查询接口，统一要求 JWT 鉴权和 `id.read` 权限。

```text
GET /api/v1/global-id/decode?id=<id>
GET /api/v1/global-id/origins?origin_id=&origin_key=&limit=&offset=
GET /api/v1/global-id/worlds?world_id=&world_key=&origin_id=&limit=&offset=
GET /api/v1/global-id/merge-events?world_id=&origin_id=&limit=&offset=
```

`decode` 接口只做解析和元数据查询，不创建 ID、不修改业务数据。对于无法识别的旧格式或非法 ID，直接返回明确错误；本项目不保留旧 ID 兼容转换。

### 9.3 权限与审计

新增后台权限：

| 权限 | viewer | operator | admin / super_admin | 说明 |
|------|--------|----------|---------------------|------|
| `id.read` | 是 | 是 | 是 | 查看全局 ID 解析、origin、world 和合服事件 |
| `id.manage` | 否 | 否 | 是 | 后续如需在后台维护 origin/world 元数据再启用 |

第一阶段只要求 `id.read`。`id.manage` 仅作为后续元数据维护入口预留，正式开放前必须补充写操作审计、输入校验和审批/变更流程。

查询类接口一般不写 `admin_audit_logs`，但管理型写操作必须写入审计。非法 ID、越权访问和异常查询失败按现有后台安全日志策略处理。

## 10. 当前项目落地建议

### 10.1 第一阶段：清空旧数据并建立基础工具

- 清空旧格式开发数据和测试数据。
- 删除旧的 ID 生成逻辑，不保留旧格式兼容读取路径。
- 新增共享 `global-id` 实现，分别提供 Rust 和 Node.js API。
- 提供 encode/decode 工具，用于将 `u64` 转成前缀字符串，以及反向解析。
- 提供 CLI 或脚本解码 ID，输出 `time_ms`、`origin_id`、`worker_id`、`sequence`。

### 10.2 第二阶段：替换所有持久业务 ID 生成点

直接替换当前所有持久业务 ID 生成点：

- `game-server` 物品实例 `uid` 的时间戳生成。
- GM 发物品 `next_item_uid`。
- `auth-http` 新玩家 `player_id`。
- `mail-service` `mail_id`。
- `announce-service` `announce_id`。
- `chat-server` `msg_id` / `group_id`。
- `match-service` 自动创建的 `room_id`。

替换后不再允许新代码使用 UUID、时间戳、随机字符串或数据库自增主键生成跨服业务 ID。

### 10.3 第三阶段：数据库、后台与合服表

- 在 `db/init.sql` 中加入 `id_origins`、`worlds`、`world_origin_memberships`、`world_merge_events`。
- 为玩家、邮件、公告等需要世界归属的业务表补充 `world_id` 或明确归属查询路径。
- 在 `admin-api` 增加全局 ID 查询接口。
- 在 `admin-web` 增加全局 ID 解码、origin/world 查询和合服事件页面。
- 合服工具只迁移数据和归属关系，不重写业务 ID。

## 11. 设计约束汇总

| 问题 | 决策 |
|------|------|
| 持久业务 ID 生成 | 统一 63-bit Snowflake 风格 ID |
| 初始服容量 | `origin_id` 保留 10 bits，最多 1024 个 origin |
| 合服后是否新建 origin | 默认不新建，复用目标世界 `active_origin_id` |
| 历史 ID 是否重写 | 不重写 |
| 旧格式兼容 | 新项目不兼容旧格式，旧开发数据直接清空 |
| 如何追溯来源 | 解码 ID 的 `origin_id`，再查 world/merge 表 |
| 后台查询 | `admin-web` 提供 ID 解码、origin/world 和合服事件查询 |
| worker_id 含义 | 同一 origin 下的发号器实例，不是 server_id |
| 配置表 ID | 不使用全局发号器 |
| 局部运行时 ID | 不使用全局发号器 |
