# Registry 与监控读模型设计

> 状态：实现契约。本文定义 Registry 监控超时治理后的目标 Redis schema、读写语义、性能边界和迁移顺序。后续实现必须遵守本文；当前实现以代码为准，尚未自动切换到本文的 v2 metrics / registry index schema。

## 1. 目标与边界

本设计解决监控页和服务发现因 Redis key 数量增长而在请求路径扫描全库的问题。目标是使 Registry、服务总览和最近指标查询的复杂度只与“固定服务数、活动实例数、所请求的时间桶数”相关，而不与 Redis 中历史 key 总量相关。

适用范围：

- `metrics-collector` 写入当前 metrics 快照与最近一小时历史数据。
- `admin-api` 的 Registry、服务总览、指标详情和归档任务读取该读模型。
- `packages/service-registry` 的 Node.js 与 Rust 客户端维护同一实例索引并按索引发现实例。
- `admin-web` 只消费 API 的降级语义，不直接读取 Redis。

不在本设计范围内：玩家 session、ticket、route store、业务 Redis key，以及 metrics payload 字段本身的业务定义。

## 2. 基本术语与硬约束

### 2.1 逻辑前缀

所有示例中的 `<MP>` 和 `<RP>` 都是字面量前缀，不是 Redis database 编号。

| 前缀 | 解析顺序 | 用途 |
| --- | --- | --- |
| `<MP>` | `METRICS_KEY_PREFIX` -> `REDIS_KEY_PREFIX` -> 空字符串 | metrics 当前快照、历史与索引 |
| `<RP>` | `REGISTRY_KEY_PREFIX` -> `REDIS_KEY_PREFIX` -> 空字符串 | Registry payload、heartbeat 与实例索引 |

共享 Redis 时，测试、预发、生产和不同集群必须使用不同且完整的前缀，例如 `test:`, `staging:`, `prod:cn-shenzhen-a:`。仅在 Redis 由单一环境独占时才允许空前缀。`<MP>` 与 `<RP>` 可以不同，但同一环境内所有 metrics producer / consumer 必须使用相同 `<MP>`，所有 Registry producer / consumer 必须使用相同 `<RP>`。

`service_name` 和 `instance_id` 必须满足 `^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`，不得含 `:`、空白或控制字符。实现必须在写入前校验，不能把未经校验的外部值拼接为 Redis key。

### 2.2 禁止全库扫描

以下命令在所有线上请求路径、服务发现路径、metrics 写入路径和归档正常路径中**严禁使用**：

```text
SCAN
SSCAN
HSCAN
ZSCAN
KEYS
```

允许在两类独立、显式命名的离线运维工具中有限使用 `SCAN`：legacy metrics 迁移与 legacy Registry index bootstrap。它们必须支持 `--dry-run`、速率限制、游标 checkpoint、前缀白名单、审计日志和可中断恢复；不能由 HTTP API、页面轮询、服务启动或定时请求隐式触发。

所有读取必须从本文定义的确定 key 开始，使用 `ZRANGEBYSCORE`、`ZRANGE`、`MGET`、`HGETALL`、`HMGET`、`TTL`、pipeline 或 Lua 脚本完成。所有批量读取均必须设定上限。

## 3. Metrics Redis Schema v2

metrics 上报周期固定为 5 秒。`bucket` 是对齐到 5 秒的 Unix 秒时间戳；上游 payload 的 `timestamp` 是 producer 生成时间，collector 写入时额外记录接收时间。

### 3.1 当前实例快照

```text
<MP>metrics:v2:latest:<service_name>:<instance_id>
类型：Hash
TTL：180 秒（METRICS_LATEST_TTL_SECONDS，必须大于 4 个上报周期）
最大：每 service 最多 64 个活动实例（METRICS_MAX_INSTANCES_PER_SERVICE）
```

Hash 必须包含下列保留字段，其余字段是原 metrics payload 的字符串字段：

| 字段 | 语义 |
| --- | --- |
| `_schema` | 固定 `metrics-v2` |
| `_service` | 已校验的服务名 |
| `_instance_id` | 已校验的实例 ID |
| `_bucket` | 本快照对应的 5 秒 bucket |
| `_reported_at` | producer payload 的 `timestamp` |
| `_received_at` | collector 成功写入 Redis 的 Unix 秒 |
| `instance_id` | 为保持既有聚合逻辑兼容，等于 `_instance_id` |

每次写入必须以 `_bucket` 比较新旧顺序：较旧的迟到消息不得覆盖当前快照；相同 bucket 可以覆盖；较新的 bucket 写入时必须完整替换旧字段，避免已停止上报的指标残留。实现使用单个 Lua 脚本或等价原子操作完成比较、替换、TTL 和索引更新。

### 3.2 当前实例索引

```text
<MP>metrics:v2:latest-index:<service_name>
类型：Sorted Set
member：<instance_id>
score：该实例最新 `_reported_at`
TTL：300 秒（METRICS_LATEST_INDEX_TTL_SECONDS）
最大成员数：METRICS_MAX_INSTANCES_PER_SERVICE，默认 64
```

写入时先删除 score 小于 `now - 180` 的成员，再插入或更新当前实例。新实例会使成员数超过上限时，collector 必须拒绝该消息、记录 `metrics_capacity_rejected_total` 并告警；不得静默淘汰仍在上报的实例。

读取当前服务时，API 以 `ZRANGEBYSCORE now-180 +inf LIMIT 0 64` 获取实例 ID，再 pipeline 读取对应 `latest` hash。hash 缺失、格式错误或索引与 hash 不一致只影响该实例，并产生局部降级，不得转为扫描匹配 key。

### 3.3 最近一小时历史桶

```text
<MP>metrics:v2:history:<service_name>:<bucket>
类型：Hash
field：<instance_id>
value：该实例该 bucket 的完整 metrics 记录 JSON，包含 `_schema`、`_service`、`_instance_id`、`_bucket`、`_reported_at`、`_received_at` 和业务 metrics 字段
TTL：4,500 秒（METRICS_HISTORY_RETENTION_SECONDS，查询窗口 1 小时 + 15 分钟归档缓冲）
最大 field 数：METRICS_MAX_INSTANCES_PER_SERVICE，默认 64
```

历史桶按服务聚合而非按“服务 + 实例”创建独立 key，因而每服务最多约 900 个历史 bucket key。写入已存在 field 时可以覆盖同 bucket 的重复上报；不同实例写入不同 field。每条 JSON 不得超过 16 KiB（`METRICS_MAX_RECORD_BYTES`），超限消息必须拒绝并记录安全日志，不得放大 Redis 内存。

### 3.4 历史桶索引

```text
<MP>metrics:v2:history-index:<service_name>
类型：Sorted Set
member：<bucket>
score：<bucket>
TTL：4,500 秒（与 history bucket 相同）
最大成员数：900（METRICS_HISTORY_INDEX_MAX_MEMBERS）
```

collector 每次成功写入历史桶时 `ZADD` 该 bucket，并清理 score 小于或等于 `now - 4,500` 的成员。成员数超过 900 时只能移除最早的已归档成员；若最早成员尚未归档，写入必须失败并报警，不能删除未归档数据来满足容量。

`GET .../metrics?window=1m|5m|15m|1h` 先用 `ZRANGEBYSCORE` 精确获得窗口 bucket，最多分别读取 12、60、180、720 个 bucket hash，再以 pipeline `HGETALL` 读取并按现有聚合规则聚合。读取路径不会从 PostgreSQL 回填最近一小时缺口，也不会回退扫描 legacy key。

### 3.5 metrics 新鲜度与 NATS 延迟

metrics 新鲜度使用 `_reported_at` 判断，transport lag 使用 `_received_at - _reported_at` 判断，不能用 Redis key 的剩余 TTL 代替。

| 条件 | `metrics_state` | 页面 / 告警语义 |
| --- | --- | --- |
| `report_age <= 30s` 且 `transport_lag <= 10s` | `fresh` | 正常 |
| `report_age <= 120s`，但超过任一上述阈值 | `delayed` | 保留数值，标记数据延迟；lag 超过 30 秒为 critical |
| `120s < report_age <= 180s` | `stale` | 保留最后快照，服务卡片标记陈旧 |
| 无快照或快照已过期 | `missing` | 指标未知，不能推断服务离线 |
| Redis 读取该实例失败 | `unknown` | 指标未知，附局部错误码 |

late metrics 仍可在其 bucket 位于历史保留窗口时写入 history；但若它比当前 `latest` snapshot 旧，不得改变 latest index 和当前状态。未来时间超过 collector 当前时间 30 秒的 payload 必须拒绝。

### 3.6 写入原子性与容量失败

`metrics-collector` 必须用一个 versioned Lua 脚本或同等原子机制一次处理：payload 校验、latest 顺序比较、latest hash / index、history hash / index、过期时间、索引清理和容量判定。脚本版本应通过应用 metrics 暴露为 `metrics_storage_schema_version=2`。

`METRICS_LEGACY_WRITE_ENABLED` 是 legacy 兼容写的唯一开关，接受 `true/false/1/0`，默认和生产目标值均为 `false`。只有经批准的迁移观察窗口可以显式设为 `true`；关闭时 Lua 不得创建或续期 `metrics:<service>:<instance>:<bucket>`、service heartbeat 或 instance heartbeat。collector 启动日志与存储计数器必须暴露 `metrics_legacy_write_enabled`、`metrics_legacy_writes_total` 和写入拒绝数，便于确认实际切换状态。

Redis 命令或脚本失败时，collector 只记录失败和内部指标，继续处理后续 NATS 消息；不得重试到无限堆积，也不得把失败误报为已落库。NATS 的 at-least-once 重投必须保持幂等：相同 `service + instance + bucket` 最终只保留一份记录。

## 4. Registry 实例索引

现有实例 payload 的 schema version 仍为 `2`，key `service:<service>:instances:<instance>` 与 endpoint 语义保持不变。本节只新增 `registry-index-v1` 读模型；Node 和 Rust 必须实现完全相同的 key、TTL、排序、清理和容量语义。

### 4.1 Key 与容量

```text
<RP>service:<service_name>:instances:<instance_id>
类型：Hash，字段 data 为既有 schema v2 JSON payload
TTL：90 秒（REGISTRY_INSTANCE_TTL_SECONDS）

<RP>heartbeat:<service_name>:<instance_id>
类型：String
TTL：30 秒（REGISTRY_HEARTBEAT_TTL_SECONDS）

<RP>service:<service_name>:instance-index
类型：Sorted Set
member：<instance_id>
score：最后一次成功 heartbeat 的 Unix 秒
TTL：300 秒（REGISTRY_INSTANCE_INDEX_TTL_SECONDS）
最大成员数：64（REGISTRY_MAX_INSTANCES_PER_SERVICE）
```

注册时必须写 payload、写 heartbeat、写 index，并设置上述 TTL。每次 heartbeat 必须同时续期 payload、heartbeat 和 index，并以当前 Unix 秒更新 index score。正常注销必须直接 `DEL` payload / heartbeat 并 `ZREM` index member。

heartbeat 或注册时先移除 score 小于 `now - 90` 的 index member，并直接删除对应的 payload / heartbeat orphan key。注册一个不在 index 中的新实例会超过上限时必须返回明确的 `REGISTRY_CAPACITY_EXCEEDED` 错误；不得淘汰健康实例。

### 4.2 发现算法

1. 对 `<RP>service:<service>:instance-index` 使用 `ZRANGEBYSCORE now-30 +inf LIMIT 0 64` 获取活动实例 ID。
2. pipeline 读取每个 `<RP>service:<service>:instances:<id>` 的 `data` 字段，并检查对应 heartbeat TTL / existence。
3. 仅返回 heartbeat 存在、payload 可按 schema v2 归一化、实例和所需 endpoint 均健康的实例。
4. 实例按既有稳定排序和权重选择规则处理；缓存只缓存上述索引读到的结果，过期不超过 heartbeat TTL。

index 与 payload/heartbeat 发生竞争时，发现结果宁可少一个实例，也不能把无 heartbeat 的实例当健康实例。payload 格式错误只影响该 instance，并在 Registry API 中产生 `schema_parse_failed` 局部告警。服务发现库不得为了找回该实例调用 `SCAN`。

### 4.3 Node / Rust 互操作

- Node `registry-schema.js`、所有 Node registry client 与 Rust `packages/service-registry` 必须共享本节常量和 key builder 的测试向量。
- 环境变量解析顺序、TTL、容量、过期清理、注册/heartbeat/deregister 的原子顺序必须一致。
- 仅改 Node 或仅改 Rust 都是不完整变更，不能发布到 strict discovery 环境。
- Redis 最低版本为 7.x；Lua 脚本使用的命令必须在该版本可用。

## 5. PostgreSQL 归档职责

Redis 只提供最近 `1m`、`5m`、`15m`、`1h` 查询，API 不接受超过一小时的 Redis history window。`metrics_archive`（auth 库）保存服务聚合后的长期数据：一条 `service_name + bucket_time` 记录对应一分钟，`bucket_time` 是分钟起点。`qps` 是该分钟请求数除以 60，`latency_ms` 按请求数加权，`online_value` 取该分钟样本平均值；样本数、请求总数、在线峰值和低基数扩展字段写入 `extra`。

归档核心由后台调度器和受权限保护的手动接口共用，二者先竞争同一个带 token 校验和续租的 Redis 锁。后台调度器在 `admin-api` 启动后立即执行，此后默认每 5 分钟执行一次。归档按 service 的 `history-index` 用 `ZRANGEBYSCORE` 分批读取已达到 `METRICS_ARCHIVE_AFTER_SECONDS=3600` 的 5 秒 bucket：

1. pipeline 读取 bucket hash，先聚合同一 5 秒桶的实例 JSON，再合并为分钟记录。
2. 对 `metrics_archive` 按 `service_name + bucket_time` 执行幂等 upsert。
3. 仅在数据库提交成功后，`UNLINK` 对应的 5 秒 history hash 并 `ZREM` index member。
4. PostgreSQL 失败或某一分钟内存在 Redis 局部读取/解析失败时保留该分钟全部源 bucket，记录失败并在下一轮重试。

4,500 秒 retention 为一小时查询窗口之外留出 15 分钟归档缓冲。长期归档只保存服务分钟聚合值，不承诺按实例重放；PostgreSQL 自动删除策略当前未实现，需要按实例长期审计或限制长期保留周期时必须另行设计表和清理策略。

## 6. API 读模型与局部降级

### 6.1 API cache 与读取上限

`GET /api/admin/monitoring/registry`、`services` 及其共享底层读模型使用进程内 3 秒快照缓存（`MONITORING_SNAPSHOT_CACHE_TTL_MS=3000`）和 single-flight。相同 cache key 的并发请求必须复用同一个 Redis 读取 Promise；缓存 key 至少隔离 `<MP>`、`<RP>` 和服务集合。

缓存不是正确性的来源：快照响应必须返回 `checked_at`、`data_age_ms`、`cached` 和 `partial`。缓存未命中时，单 service 的 Redis 批量操作上限为 64 实例和 720 个一小时 bucket；跨服务读取可以有限并发（默认 4），不得串行地把所有服务读完后才返回首个可用结果。

### 6.2 Registry 响应语义

Registry 实例应分别暴露 `registry_state` 与 `metrics_state`，不得把“没有 metrics”误报为“没有注册”。总体 `status` 语义如下：

| 条件 | `status` | HTTP / 页面语义 |
| --- | --- | --- |
| Registry heartbeat 活跃，metrics fresh | `healthy` | 正常展示 |
| Registry heartbeat 活跃，metrics delayed / stale / missing | `degraded` | 保留注册详情，指标区域标记延迟、陈旧或未知 |
| Registry payload 存在但 heartbeat 已失效 | `unhealthy` | 实例显示失联，不返回给服务发现消费者 |
| 单实例 payload 或 Redis 读取失败 | `unknown` | 仅该实例显示未知，保留同服务其他实例 |
| 服务 index 无活动实例 | `missing` | 服务无注册实例；与 metrics 是否存在分别展示 |

单个 service、单个实例、metrics source 或 NATS 延迟异常时，Registry / services API 返回 HTTP `200`，在响应中设置 `partial=true`、稳定 `error_code`、`sources[]` 和受影响对象的 `status=unknown/degraded`。页面保留上一次成功数据并明确显示数据时间，不以清空卡片或整页 5xx 作为默认行为。

只有请求参数非法、认证/授权失败，或 Redis 完全不可用且进程内不存在可用快照时，接口才返回相应 `4xx` / `503`。`503` 必须是小 JSON 错误体，包含 `error_code=MONITORING_DATA_UNAVAILABLE`，不能伪造健康或离线结论。

## 7. 性能 SLO、容量与告警

在 Redis 中存在 20 万个相关 key 的压力场景下，Registry API（不含浏览器和 TLS）目标为：

| 指标 | 目标 | 告警阈值 |
| --- | --- | --- |
| `GET /monitoring/registry` p95 | 小于 500 ms | 5 分钟 p95 >= 500 ms warning |
| `GET /monitoring/registry` p99 | 小于 1 s | 5 分钟 p99 >= 1 s critical |
| Registry / services 数据读取 | 每请求 0 次 `SCAN` / `KEYS` | 应用 `monitoring_forbidden_scan_total > 0` 立即 critical |
| Redis `cmdstat_scan` 增量 | 常规运行每 5 分钟为 0 | 非维护窗口 > 10 warning，> 100 critical |
| Redis CPU | 小于 60% 持续 5 分钟 | >= 60% warning，>= 80% critical |
| Redis used memory / maxmemory | 小于 75% | >= 75% warning，>= 85% critical |
| metrics NATS transport lag | 小于等于 10 秒 | > 10 秒 warning，> 30 秒 critical |
| metrics archive lag | 小于等于 10 分钟 | > 10 分钟 warning，> 15 分钟 critical |
| index / hash 不一致、容量拒绝 | 0 | 任意非零 warning；持续增长 critical |

运维平台必须区分离线 migration 产生的 `SCAN` 与常规运行：离线工具执行前写入审计事件和维护窗口；窗口外出现 scan 增量视为回归。不能仅通过把 Axios 超时调大来宣称满足本 SLO。

## 8. Legacy 兼容、迁移、清理与回滚

旧 metrics key 为 `metrics:<service>:<instance>:<bucket>`（以及更早的无实例形式），旧 Registry 没有 `instance-index`。两者只在以下受控阶段兼容，线上 API 不允许以 legacy `SCAN` 作为 fallback。

### 8.1 发布顺序

1. **准备**：上线支持 v2 写入和 registry index 双写的 producer 版本；保持现有读路径不变。所有 Node 与 Rust registry producer 都必须覆盖。
2. **bootstrap**：执行一次离线 `registry-index-bootstrap`，只扫描 `<RP>service:<service>:instances:*`，只把 heartbeat 仍存在且 payload 合法的实例写入 index。执行一次离线 `metrics-v2-backfill`，只处理 `<MP>metrics:<service>:*` 中最近 1 小时数据并按 v2 bucket 写入。两工具都先 dry-run，再限速实际执行并保存 checkpoint。
3. **验证**：比较旧/新每 service 的活跃实例数、实例 ID 集合、最近 bucket 聚合结果、history index 连续性和归档幂等性；所有差异必须先处理。
4. **切读**：发布 admin-api 与所有 Node/Rust consumers，使其只读 v2 metrics / registry index。此时任何正常请求路径均无 legacy scan fallback。
5. **观察**：保持 metrics legacy 双写至少 24 小时且覆盖一个 archive 周期；保持 Registry legacy payload 兼容至少一个完整发布窗口，确认所有实例的 build version 都支持 index。
6. **删除**：将 `METRICS_LEGACY_WRITE_ENABLED=false` 并至少观察两个上报周期；确认 legacy 最新 bucket 不再前移、key 数不再增长后，再等待 TTL 自然过期或用离线限速清理。确认所有消费者不再调用 legacy scan 后，删除 legacy Registry 扫描代码。完成前保留文档化回滚开关。

旧 metrics key 的默认兼容期限为从切读成功起 7 天；不得因 Redis 空间压力直接 `FLUSHDB` 或批量无前缀删除。legacy 清理只允许对已验证的 `<MP>metrics:` 精确前缀执行，使用 `UNLINK`，每批有上限并记录删除数。

### 8.2 Legacy 清理工具

`tools/metrics-legacy-cleanup.js` 是唯一允许执行 legacy metrics 删除的仓库工具。它默认 dry-run，只接受精确的 `metrics:<service>:<instance>:<bucket>` 和经确认的旧 `metrics:<service>:<bucket>` Hash；`metrics:v2:*`、`metrics:heartbeat:*`、未知结构、非 Hash 和其他业务 key 一律排除。生产通过 metrics-collector 镜像在 Compose internal network 内执行，连接串从容器的 `REDIS_URL` 环境变量读取，不出现在命令参数或报告中。

空生产前缀的 dry-run 示例：

```bash
mkdir -p /data/myserver/ops/metrics-legacy-cleanup
docker compose --env-file ./compose.production.env -f ./compose.production.yml run --rm --no-deps \
  -v /data/myserver/ops/metrics-legacy-cleanup:/ops \
  metrics-collector node /app/tools/metrics-legacy-cleanup.js \
  --redis-url-env REDIS_URL --key-prefix '' --allow-empty-prefix --pretty
```

实际清理还必须提供变更单或操作者标识、确认短语、checkpoint 和 NDJSON 审计文件，并使用经预发验证的批量与间隔：

```bash
docker compose --env-file ./compose.production.env -f ./compose.production.yml run --rm --no-deps \
  -v /data/myserver/ops/metrics-legacy-cleanup:/ops \
  metrics-collector node /app/tools/metrics-legacy-cleanup.js \
  --redis-url-env REDIS_URL --key-prefix '' --allow-empty-prefix \
  --apply --confirm legacy-metrics-unlink --operator '<change-id-or-operator>' \
  --batch-size 100 --delay-ms 100 \
  --checkpoint /ops/checkpoint.json --audit-log /ops/audit.ndjson --pretty
```

dry-run 的 `eligibleHashes` 必须与预期 legacy 数量相符，且 `excluded.v2`、`excluded.heartbeat`、`excluded.wrong_type` 和异常结构统计经过人工复核。中断后仅可使用同一 Redis 目标、前缀、模式和 checkpoint 加 `--resume` 继续；已完成 checkpoint 不可重复执行。

### 8.3 回滚顺序

- **切读前失败**：停止 bootstrap / backfill，保留 legacy 读写；v2 key 可自然过期，不影响线上路径。
- **切读后、legacy 双写期内失败**：先把 consumers 切回已验证的 legacy 版本，再停止 v2 读取；collector 继续 legacy 双写，禁止执行 legacy 删除。
- **legacy 删除开始后**：不再允许回滚到需要 legacy scan 的版本；只能修复 v2 reader 或从 PostgreSQL archive 恢复聚合历史。故 legacy 删除必须在至少 24 小时稳定观察、SLO 达标和明确变更批准后执行。

迁移和清理工具必须把执行者、环境前缀、开始/结束 bucket、checkpoint、读写/删除数量、错误样本和版本写入审计日志；不允许连接错误环境或跨前缀操作。

## 9. 实现验收条件

后续阶段的代码审查和测试至少证明：

- 所有 Node 与 Rust registry producer / consumer 对相同输入生成相同 index key，并在注册、heartbeat、注销和过期时得到相同可见实例集合。
- 在构造 20 万 unrelated / legacy key 的 Redis fixture 后，Registry、services 和 metrics API 不调用 `SCAN` / `KEYS`，且满足本节 SLO。
- v2 latest 不会被乱序或延迟消息回退；history 可精确返回 1 分钟、5 分钟、15 分钟、1 小时 bucket。
- Redis 单 key 失败、单实例坏 payload、NATS 延迟、heartbeat 过期、完整 Redis 不可用且有/无缓存，均符合第 6 节 HTTP 与页面状态语义。
- archive 失败不删除 Redis history；成功重试不会重复产生长期数据；legacy 迁移可 dry-run、断点续跑、限速、审计且不使用 `FLUSHDB`。
