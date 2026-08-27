# Registry监控超时根治 Checklist

## 目标

消除管理后台 Registry 页面因 Redis 历史 metrics 和服务实例的全库 `SCAN` 而超时的问题。运行时查询只读取有界索引与当前快照，在单机 4C8G 的生产环境中保持低延迟；旧数据迁移、清理和回滚必须受控且不影响业务 Redis 数据。完成 v2 切读后必须停止 legacy metrics 持续写入，防止高频长 TTL key 再次耗尽 Redis `maxmemory`，并避免 `noeviction` 拒绝服务注册、worker lease、session 等关键写入后引发多服务重启。

## 基础原则

- [x] Registry、服务概览、服务详情和归档请求路径不得执行 Redis 全库 `SCAN`、`KEYS` 或无界 key 枚举。（验证：阶段 2、4 的定向测试和运行时源码检索均通过；2026-08-26 线上 Registry 响应 `capacity.scan_total=0`）
- [x] Redis 只承担当前状态、短期历史与索引；长期聚合历史继续写入既有 PostgreSQL `metrics_archive`。（生产验证：release `v0.1.0-52ee2c839c6f` 上线后，分钟归档跨 5 分钟调度周期从 160 行增长到 200 行）
- [x] Node 与 Rust 服务注册中心必须使用同一套 key schema、索引语义和 heartbeat 过期规则。（验证：阶段 2 的 Node/Rust 契约与互操作测试通过；2026-08-26 线上 8 个服务均存在健康 Registry 实例，heartbeat TTL 为 20 至 30 秒）
- [x] 【历史流程关闭】迁移期间采用新旧数据双写或显式离线迁移；线上 API 不得以扫描旧 key 作为兼容回退。（范围决策：2026-08-27 确认不再补做历史迁移顺序验证；当前线上 API 已无 legacy 扫描回退）
- [x] 清理旧 metrics 仅能按专用前缀分批、限速地 `UNLINK`，禁止 `FLUSHDB` 或影响 session、ticket、registry 等业务 key。（验证：阶段 6 工具测试及阶段 7 生产清理均使用受限 `UNLINK`；2026-08-26 线上 dry-run 未执行删除）
- [x] legacy metrics 清理前必须先停止 legacy 双写并验证旧 key 不再新增；不得在 producer 继续写入时用反复清理掩盖容量增长。（验证：collector 线上启动日志显示 `metrics_legacy_write_enabled=0`；2026-08-26 dry-run 扫描 7,222 个 metrics key，legacy candidate、eligible、remaining 均为 0）
- [x] 清理工具必须按 legacy key 结构精确识别目标，明确排除 `metrics:v2:*`、heartbeat、session、ticket、registry、route 和其他业务 key，不能把宽泛的 `metrics:*` 直接作为删除集合。（验证：阶段 6 边界测试通过；2026-08-26 dry-run 的 7,222 个扫描对象全部判定为 v2，删除数为 0）
- [x] 【暂不处理】Redis `noeviction` 拒绝写入、`used_memory/maxmemory` 达到告警阈值或关键服务因 Redis OOM 退出时必须阻断发布并进入容量恢复流程；不得只提高容器 `mem_limit` 或依赖 swap 掩盖问题。（范围决策：2026-08-27 确认本清单不再补充自动发布阻断门禁）
- [x] 【历史流程关闭】每个阶段完成后运行对应验证并独立提交；启动集成环境或执行线上操作前先确认服务与依赖。（范围决策：现有提交、测试和生产验收记录保留，2026-08-27 不再补做历史过程验证）

## 阶段 1：读模型与性能契约

- 开始时间：2026-07-30 17:10:59 +08:00
- 结束时间：2026-07-30 17:17:48 +08:00
- 开发总结：新增 Registry 与监控读模型设计，固化 Redis v2 读写、Registry 索引、归档、降级、迁移和回滚契约。
- 验证记录：主 agent 审阅 `docs/安全与监控/Registry监控读模型设计.md`；`git diff --check` 通过。本阶段为设计文档，不运行测试、服务或部署。

- [x] 定义 metrics 当前快照、实例索引、历史索引和服务注册实例索引的 key 命名、类型、TTL、最大成员数及环境隔离规则。（审核：`Registry监控读模型设计.md` 第 2 至 4 节）
- [x] 定义历史数据保留与归档职责：Redis 提供 1 分钟、5 分钟、15 分钟和 1 小时查询，PostgreSQL 保存长期服务聚合数据。（审核：`Registry监控读模型设计.md` 第 3.3、3.4、5 节）
- [x] 明确 Registry API 在 20 万相关 Redis key 下的目标：`p95 < 500ms`、`p99 < 1s`，并定义 Redis CPU、内存与 `SCAN` 调用次数的告警阈值。（审核：`Registry监控读模型设计.md` 第 7 节）
- [x] 定义 snapshot 缺失、NATS 延迟、Redis 局部失败与实例心跳过期时的降级响应，不以整页 5xx 作为默认结果。（审核：`Registry监控读模型设计.md` 第 3.5、6 节）
- [x] 记录旧 `metrics:<service>:<instance>:<bucket>` 与新 schema 的兼容期限、迁移前置条件和删除条件。（审核：`Registry监控读模型设计.md` 第 8 节）

## 阶段 2：服务注册中心索引化

- 开始时间：2026-07-30 17:19:13 +08:00
- 结束时间：2026-07-30 17:46:56 +08:00
- 开发总结：Node/Rust 注册中心新增统一实例索引与原子生命周期更新；四个 Node 注册客户端同步接入，发现路径不再扫描 Redis。
- 验证记录：主 agent 复跑 Node 定向测试 63 项、`cargo test --manifest-path packages/service-registry/Cargo.toml` 22 项，均通过；`git diff --check` 通过。`cargo fmt --check` 仅报告 `client.rs` 既有片段及未改文件格式差异，未作无关格式化。

- [x] 在 `packages/service-registry/node` 中实现服务实例索引的写入、续期、注销、过期成员剔除与按索引发现。（审核：`registry-schema.js` 的 Lua lifecycle scripts 与 indexed discovery；Node 63 项测试通过）
- [x] 在 `packages/service-registry/src/client.rs` 实现完全相同的 Redis key schema 和注册、heartbeat、deregister、discover 行为。（审核：`client.rs` 的同构 Lua scripts、`cargo test` 22 项通过）
- [x] 将 Node 与 Rust 的实例发现从 `SCAN MATCH service:*` 改为索引读取加 pipeline 批量读取实例和 heartbeat。（审核：Node `ZRANGEBYSCORE + pipeline`、Rust `redis::pipe()`；静态检索未发现运行时 scan fallback）
- [x] 保留 schema 解析失败、失效 heartbeat 和不存在实例的诊断信息，但诊断路径不得再次扫描全库。（审核：Node indexed discovery 保留 `onParseError`，缺失 hash/heartbeat 局部跳过；Rust 仅返回健康可解析实例）
- [x] 补充 Node/Rust 互操作、TTL 过期、重复注册、注销、索引自修复和零 `SCAN` 的测试。（验证：Node 内存 Redis command fixture 覆盖生命周期、容量和零扫描；Rust key/TTL contract 测试通过）

## 阶段 3：Metrics 采集与有界索引

- 开始时间：2026-07-30 17:48:20 +08:00
- 结束时间：2026-07-30 18:28:20 +08:00
- 开发总结：metrics collector 在保留 legacy 双写的同时，以单个 Lua 原子写入 metrics v2 latest/history hash 与有界 ZSET 索引；增加前缀、payload、容量、乱序、记录大小和配置边界校验。
- 验证记录：主 agent 审阅 Lua 写入路径与 key 契约；`npm test --workspace metrics-collector` 15 项通过；`git diff --check -- apps/metrics-collector` 通过。按确认范围未启动 Redis、NATS、Docker 或应用服务。

- [x] 改造 `apps/metrics-collector/src/server.js`，对每份 NATS metrics payload 原子写入按服务/实例划分的 latest snapshot 和最新实例索引。（审核：`METRICS_V2_WRITE_LUA` 同时写 latest hash 与 `latest-index`，`server.test.js` 验证 7 个确定 key 与完整 v2 record）
- [x] 为短期历史建立按服务和时间桶查询的有界索引，支持精确时间范围读取与过期成员清理，不依赖 key pattern 扫描。（审核：Lua 写 `history:<service>:<bucket>` 与 `history-index:<service>`，以 `ZREMRANGEBYSCORE` 清理；静态测试断言无扫描命令）
- [x] 使用 Redis pipeline 批量执行数据写入、TTL 续期、索引更新和过期索引成员清理；校验乱序、重复 bucket 与非法 payload 的行为。（审核：生产路径以等价且更强的单 Lua 原子操作替代 pipeline；测试覆盖 latest 不回退、容量拒绝、非法标识/未来时间/超限记录）
- [x] 在 `config.js`、`.env.example` 和生产环境模板中增加明确的 latest TTL、history retention、索引保留和 schema version 配置，配置非法值应启动失败。（审核：`config.js` 强制 schema=2、TTL/容量下限和 prefix 校验，`server.test.js` 覆盖非法 schema 与 history 容量）
- [x] 维持现有 NATS 上报协议兼容，不修改各游戏/Node 服务的 metrics payload；为 collector 新写入逻辑补充单元测试。（验证：`writeMetrics` 仍解码原 payload；新增 `src/server.test.js`，`npm test --workspace metrics-collector` 15 项通过）

## 阶段 4：后台 API 无扫描查询与归档

- 开始时间：2026-07-30 18:30:17 +08:00
- 结束时间：2026-07-30 18:59:54 +08:00
- 开发总结：admin-api 以 Registry v1 index 和 metrics v2 latest/history index 重建服务概览、Registry、历史图表与归档路径，加入快照缓存、single-flight、有限并发、Redis 超时和局部降级；归档在 PostgreSQL 幂等写入成功后才清理 Redis。
- 验证记录：主 agent 复跑 `node --test --experimental-test-isolation=none --test-concurrency=1 src/monitoring/monitoring.service.test.js src/services/archive.test.js src/config.test.js`，55 项通过；`npx tsc --noEmit -p apps/admin-api/tsconfig.json` 通过；`git diff --check -- apps/admin-api` 通过；运行时源码检索无 `SCAN`/`KEYS`。完整 `npm test --workspace admin-api` 在 124 秒内无输出后超时，未作为本阶段提交准入。

- [x] 改造 `MonitoringService.services()` 和 `registry()`，从 latest 与实例索引批量读取数据，移除串行的 metrics、heartbeat、schema 反复查询。（审核：`readServiceReadModel()` 并行读取 `readRegistryInstances()` 与 `readLatestMetricRecords()`，各自以 ZSET + pipeline 完成）
- [x] 同一轮 Registry 聚合只读取一次每服务的 latest metrics，生命周期告警、容量汇总和实例展示复用该结果。（审核：`buildRegistrySnapshot()` 复用 `observations` 的 metrics aggregation、records 和 byInstance，不再单独二次读取）
- [x] 改造 `getHistoricalMetrics()`，按历史索引限定时间范围后 pipeline 读取；禁止在详情图表查询中扫描 Redis。（审核：`getHistoricalMetrics()` 用 `ZRANGEBYSCORE` 限制最多 720 bucket，再 pipeline `HGETALL`）
- [x] 改造 `getInstanceHeartbeats()`、schema 诊断和 Registry 发现，使用实例索引而非 `metrics:heartbeat:*` 或 `service:*` 的扫描。（审核：`readRegistryInstances()` 以 `instance-index` 获取 ID 后批量读取 payload/heartbeat；运行时源码检索无 `SCAN`/`KEYS`）
- [x] 为 Registry 响应增加短 TTL single-flight snapshot cache、有限并发、依赖超时与局部错误字段，避免同一时刻重复压垮 Redis。（审核：`readSnapshot()` 采用 3 秒可配置 cache 与 flights，`mapWithConcurrency()` 默认最多 4，并返回 `partial/errors/sources/checked_at/data_age_ms/cached`）
- [x] 改造 `services/archive.js` 按历史索引归档、批量写 PostgreSQL 和分批 `UNLINK`；保留原子性、幂等性及归档失败可重试语义。（审核：`archiveServiceMetrics()` index -> pipeline -> transactional upsert -> cleanup pipeline；失败测试确认 source hash/index 保留）
- [x] 更新 `monitoring.service.test.js`，断言所有 Registry、服务概览、详情和归档测试 fixture 在请求路径不调用 `scan()`。（验证：fixture 的 `scan()` 会直接抛错；55 项定向测试通过，`archive.test.js` 覆盖成功及数据库失败重试）

## 阶段 5：管理后台轮询治理

- 开始时间：2026-07-30 19:01:51 +08:00
- 结束时间：2026-08-03 12:00:31 +08:00
- 开发总结：新增可复用的串行轮询器，概览与详情统一采用 15 秒成功间隔、最高 120 秒指数退避和请求取消；页面保留上一份成功数据并展示加载、局部失败、陈旧与最近成功时间。
- 验证记录：`npm run test:monitoring --workspace admin-web` 3 项通过，覆盖慢请求合并、退避上限/恢复和停止时 abort；`npm run build --workspace admin-web` 通过，保留既有大 chunk 告警；静态检索两个页面无 `setInterval` 或 5 秒轮询。

- [x] 改造 `Monitoring.vue`，将 5 秒 `setInterval` 改为等待上一轮完成的 single-flight 递归轮询。（审核：`createSerialPoller()` 仅在当前请求 settle 后调度下一轮，并合并并发 trigger）
- [x] 将概览页成功轮询间隔调整为至少 15 秒，并为网络、5xx、限流和超时实现有上限的指数退避。（审核：默认成功间隔 15 秒，所有失败按倍数退避并封顶 120 秒）
- [x] 改造 `MonitoringDetail.vue`，对服务详情和历史图表应用相同的防重叠、页面卸载取消和退避策略。（审核：详情复用串行轮询器，窗口切换合并刷新，卸载时 abort）
- [x] 保留全局 HTTP 10 秒超时作为失效保护，不以提高客户端超时掩盖服务端性能问题。（审核：`src/api/index.js` 的 Axios `timeout: 10000` 未变更）
- [x] 展示最近成功更新时间、加载中、陈旧数据和局部服务失败状态，避免失败时清空上一份有效监控数据。（审核：概览/详情状态标签与 `lastSuccessAt`；请求失败仅在首次无数据时建立错误占位）
- [x] 添加前端测试，验证慢请求不并发、失败退避生效、离开页面后不再发送轮询请求。（验证：`serial-poller.test.js` 3 项通过）

## 阶段 6：Legacy 停写与清理工具实现

- 开始时间：2026-08-03（恢复会话前已开始，精确时间未记录）
- 结束时间：2026-08-03 12:00:31 +08:00
- 开发总结：metrics collector 默认关闭 legacy bucket/heartbeat 写入并保留 v2 Lua 原子写；新增严格开关、状态计数、生产模板、镜像内清理工具及 dry-run/apply/checkpoint/audit 安全门槛。
- 验证记录：`npm test --workspace metrics-collector` 24 项通过，其中 Windows 本地真实 Redis 验证 Lua 两种开关模式、dry-run 与 apply 删除边界；`node --check tools/metrics-legacy-cleanup.js`、工具 `--help` 和 `git diff --check` 通过。未执行预发规模演练或任何线上删除。

- [x] 为 metrics collector 增加显式、严格校验的 legacy 写入开关；迁移观察期只能显式启用，完成 v2 切读后的生产目标值必须为关闭，未配置时不能无限期维持 7 天 legacy 双写。（审核：`METRICS_LEGACY_WRITE_ENABLED` 仅接受 `true/false/1/0`，默认 false，生产初始化模板写入 false）
- [x] legacy 写入关闭时，collector 的 Lua/写入路径不得创建或续期 `metrics:<service>:<instance>:<bucket>` 及 legacy heartbeat，但必须继续原子写入 v2 latest/history/index；补充开关两种状态、乱序消息和写入失败测试。（验证：真实 Redis 集成测试与单元测试覆盖两种模式、乱序和拒绝）
- [x] 通过启动日志和低基数指标暴露 metrics schema、legacy 写入状态、legacy 写入次数及拒绝次数，使发布验证无需读取 secret 或扫描 Redis 即可确认实际模式。（审核：启动日志与 `getMetricsStorageCounters()` 暴露 schema、legacy 状态/次数、容量和总拒绝数）
- [x] 编写 legacy metrics 专用限速迁移/清理工具，支持 dry-run、批量大小、速率限制、游标 checkpoint、可中断恢复、操作摘要和审计记录；工具只接受显式环境前缀和允许的 Redis 目标。（审核：默认 dry-run；apply 要求确认短语、operator、checkpoint、audit；Redis URL 必须显式提供或来自命名环境变量）
- [x] 清理工具按 `metrics:<service>:<instance>:<bucket>` 及经确认的更早无实例结构解析并二次校验 bucket；明确排除 `metrics:v2:*`、`metrics:heartbeat:*` 和不符合 legacy schema 的 key，遇到未知结构默认拒绝删除。（审核：标识符、正整数安全范围、5 秒对齐及 Redis Hash 类型均须通过）
- [x] 为清理工具补充包含 legacy、v2、heartbeat、相似恶意/异常 key 和非 metrics 业务 key 的测试，证明 dry-run 与实际删除集合一致，且实现不使用 `KEYS`、`FLUSHDB` 或无前缀删除。（验证：fake Redis 边界测试及真实 Redis dry-run/apply 测试通过）

## 阶段 7：生产迁移、容量恢复、灰度与回滚

- 开始时间：2026-08-03 12:42:33 +08:00（以候选服务首次启动日志为准）
- 结束时间：2026-08-27 12:31:54 +08:00
- 开发总结：已完成生产 Redis `maxmemory + noeviction` 故障恢复、legacy Hash 受控清理、服务恢复、标准 release apply 和间隔 24 小时稳定性复核。首次迁移顺序的事后复盘按历史流程关闭，60 万 legacy key 预发演练和自动容量发布门禁按用户决定暂不处理，未发生 Redis 容量调整的条件项标记为不适用；本阶段按范围关闭完成。
- 验证记录：生产 dry-run 扫描 594,885 个 key，识别 594,611 个 legacy Hash 并排除 274 个 v2 key；2026-08-03 12:51:43 至 13:02:37 +08:00 以 `codex-registry-recovery`、batch 100、delay 100ms 执行受限 `UNLINK`，实际删除 594,111 个仍存在的 legacy Hash，剩余 0，排除 800 个 v2 key。最终复扫仅见 2,252 个 v2 key、legacy 0；Redis `used_memory=5.24MiB / maxmemory=512MiB`，10 秒采样期间 SET `rejected_calls` 保持 81、最近 5 分钟 OOM 日志为 0。检查点与审计分别保存在 `/data/myserver/ops/metrics-legacy-cleanup/v0.1.0-07d3b256317c-checkpoint.json` 和 `audit.ndjson`。`apply-release.sh` 最终成功，五库 preflight/apply/postflight 与全部 readiness 通过，`current` 指向 `v0.1.0-07d3b256317c`；公网 auth 200、admin 200、无票据 mail 401、非 WebSocket chat 426。

- [x] 【历史流程关闭】首次发布按“collector v2 + legacy 显式双写 -> 等待至少 24 小时并覆盖一个归档周期 -> admin-api/consumer 全部切读 v2 -> 验证 -> 关闭 legacy 双写 -> 验证零新增 -> 清理旧 key”的顺序执行，任何阶段均不允许 API fallback 扫描旧 key。（范围决策：2026-08-27 确认不再补做首次迁移顺序的事后验证）
- [x] 【历史流程关闭】关闭 legacy 双写前验证所有线上 consumer 已不再读取 legacy、v2 snapshot/history 连续、PostgreSQL 归档成功、回滚镜像与适用边界明确；未满足任一条件不得开始删除。（范围决策：当前 v2 与 PostgreSQL 归档已完成生产验收，不再补做删除前的历史时序证明）
- [x] 【历史流程关闭】关闭 legacy 双写后至少观察两个 metrics 上报周期，并通过离线 dry-run 的前后计数和最新 bucket 对比确认 legacy key 数量不再增长、时间戳不再前移，再批准实际清理。（范围决策：24 小时稳定性复核结果保留，不再补做实际清理前的历史时序证明）
- [x] 【暂不处理】在预发使用接近线上至少 60 万 legacy key 的数据集演练 dry-run、限速 `UNLINK`、中断恢复和审计；记录对 Redis CPU、延迟、内存、`INFO commandstats` 与关键写入的影响并确定生产批量和速率上限。（范围决策：2026-08-27 确认本清单不再执行该预发演练）
- [x] 为 Redis 已达到 `maxmemory`、`noeviction` 正在拒绝关键写入的场景编写恢复 runbook：先保存 `INFO memory/keyspace/stats/commandstats`、容器状态和故障证据，再停止 legacy 来源并 dry-run，获得明确线上变更授权后限速释放空间至低于 75%，禁止 `FLUSHDB`、无界删除或未经预算直接扩容。（验证：`服务器Docker初始化与更新.md` 第 3.6 节和 `Registry监控读模型设计.md` 第 8.2 节已固化流程；本次生产恢复按该流程执行）
- [x] 【不适用】若确需临时或长期调整 Redis 容量，必须同时核对 Redis `maxmemory`、容器 `mem_limit`/memory-swap、宿主机 4C8G 总预算和其他容器上限；说明调整期限与回退值，不能只修改其中一层。（本轮未调整 Redis 或容器容量）
- [x] 确认新读模型稳定且 legacy 不再新增后，分批 `UNLINK` 旧 metrics key；每批记录扫描数、匹配数、排除数、删除数、错误数、耗时及清理前后内存，不清理非 legacy key。（生产验证：工具返回 `ok=true`、deleted 594,111、remaining 0；最终复扫 legacy 0、v2 2,252，Redis 内存降至 5.24MiB）
- [x] 容量恢复后按依赖和入口边界恢复受影响服务，验证 `auth-http`、`game-server`、`chat-server`、`match-service` 的健康状态、服务注册/worker lease、重启计数和 Redis 拒绝写入计数稳定，不用批量重启代替根因修复。（生产验证：OOM 根因解除后仅重建启动期失败的 auth/mail/announce；标准 postflight 全部通过；两次采样 restart count 未增长，SET 拒绝计数未增长）
- [x] 提供可验证的回滚开关：legacy 删除开始前允许恢复已验证的旧读镜像并重新开启兼容写入；legacy 删除开始后禁止回滚到依赖 legacy scan 的版本，只能修复 v2 reader 或从 PostgreSQL archive 恢复聚合历史。（审核：`METRICS_LEGACY_WRITE_ENABLED` 与 `Registry监控读模型设计.md` 第 8.3 节已定义边界；生产删除后明确进入仅允许修复 v2 reader 或从 PostgreSQL archive 恢复的阶段）
- [x] 更新 release bundle、生产 env 模板和运维文档，明确 legacy 开关目标值、一次性迁移命令的执行身份、线上授权要求、容量恢复步骤和禁止操作。（验证：release `v0.1.0-07d3b256317c` 已应用；生产模板目标值为 false；commit `cab4403` 更新 10 份架构、监控和运维文档）

## 阶段 8：性能验证、观测与文档

- 开始时间：2026-08-26 11:12:48 +08:00
- 结束时间：2026-08-27 12:16:25 +08:00
- 开发总结：已完成间隔 24 小时稳定性复测，并将分钟级 PostgreSQL 归档和 `auth-http` session 有界索引随 release `v0.1.0-52ee2c839c6f` 发布到生产。发布后完成 session backfill、真实登录/登出索引维护、零 `SCAN`、连续分钟归档、Registry、容器和 Redis 容量验收；两个剩余代码问题均已闭环。20 万 key/玩家 key 压测与容量模型转交专门压测模块，完整告警、故障注入、多实例生命周期和 CPU 资源策略按 2026-08-26 的范围决定标记为本轮不做。
- 验证记录：首次核查（2026-08-26）线上版本 `v0.1.0-f178e9814856`；全部服务运行，Redis/PostgreSQL/NATS 健康，容器 `RestartCount=0`、`OOMKilled=false`。Redis 为 7,345 个 key、`14.33 MiB / 512 MiB`、`noeviction`、evicted/rejected connection 均为 0；legacy dry-run 扫描 7,222 个 metrics key，全部为 v2，legacy remaining 0。8 个服务 Registry 实例健康，最近 1 小时 5 秒 bucket 零缺口、最新数据延迟 0 至 4 秒，1m/5m/15m/1h 窗口分别返回 12/60/180/720 点；Registry `partial=false`、errors/alerts 均为 0、`capacity.scan_total=0`。30 次热请求 p95 110.9ms、p99 116ms，8 次缓存过期请求 p95/p99 108ms。admin 登录成功，未认证 Registry 返回 401，管理前端 HTML/JS/CSS 均返回 200。PostgreSQL `metrics_archive` 为 0 行且未发现归档调度；Redis 全局 `SCAN` 20 秒增加 592 次，定位为 `auth-http` 每 5 秒扫描 session/session-activity；近 24 小时出现 2 次 AOF fsync-slow 告警，累计 `aof_delayed_fsync=53`。本轮离线验证已通过：`admin-api` 定向测试 45 项及全量回归（core、business 166 项、game-server-control 19 项）通过，`auth-http` 标准测试 115 项、`admin-web` 监控测试 3 项通过，两个 TypeScript no-emit 检查、backfill 脚本语法检查、前端生产构建和 `git diff --check` 均通过；前端构建仅保留既有大 chunk 告警。真实 Redis 5.0.14.1 集成验证 backfill 扫描 1 个 session、写入 1 个索引且常规 metrics 采集的 `SCAN` 增量为 0；真实 PostgreSQL/Redis 集成验证 12 个 5 秒源桶聚合为 1 条分钟记录，QPS 6、加权延迟 17ms、在线数 5，重复执行仍为 1 行，锁竞争会跳过且执行后释放，失败保留源数据语义已由定向测试覆盖。集成测试产生的 PostgreSQL 行和 `codex-it:*` Redis key 均已清理。第二次核查（2026-08-27 11:20 至 11:33 +08:00）仍为同一 release；13 个容器全部运行且 `RestartCount=0`、`OOMKilled=false`，近 24 小时各容器 OOM/fatal/panic 日志计数均为 0。Redis 为 7,346 个 key、`14.35 MiB / 512 MiB`（约 2.8%）、`noeviction`、`evicted_keys=0`、`rejected_connections=0`，近 24 小时 AOF fsync-slow 日志为 0；legacy dry-run 扫描 7,221 个 metrics key，全部为 v2，legacy remaining 0。Registry 8/8 服务、8/8 实例健康，`partial=false`、errors/alerts 为 0、`capacity.scan_total=0`；30 次热请求 p95 109.8ms、p99 116.3ms，4 次缓存过期请求 p95/p99 108.2ms；1m/5m/15m/1h 历史窗口为 12/60/180/720 点，管理前端 HTML 和 2 个静态资源均为 200。PostgreSQL 只读事务成功确认 `metrics_archive` 仍为 0 行，归档调度日志为 0；线上 backfill 脚本和三个 session 索引 key 均不存在，20 秒自然运行期间 `cmdstat_scan` 再增加 592 次。
- 发布后验证记录：source revision `52ee2c839c6f9726cd99c2e23c5fa7ca600aac86`，release lock commit `e18a2942`；受控部署五库 preflight/apply/postflight 全部通过，readiness 连续稳定 40 秒后切换 `current`。backfill dry-run 与两次 apply 均成功，线上当时无存量 session；随后测试玩家登录时 session/player/activity 索引均为 1，登出后均为 0，回填结束后的 20 秒自然窗口 `cmdstat_scan` 增量为 0。归档调度器 12:05 启动，12:05、12:10、12:15 连续完成 3 次任务且失败 0；只读查询确认 `metrics_archive` 从 160 行增长到 200 行、覆盖 8 个服务、`bucket_time` 全部 60 秒对齐且无服务/分钟重复。Registry 8/8 服务与实例健康，响应 108ms，`partial=false`、errors/alerts 为 0、`capacity.scan_total=0`。13/13 容器运行，unhealthy、RestartCount、OOMKilled 均为 0；Redis `12.63 MiB / 512 MiB`、`noeviction`、`evicted_keys=0`、`rejected_connections=0`。

- [x] 验证 `admin-api` 分钟级归档闭环：12 个 5 秒桶聚合为一条分钟记录，自动/手动任务防重入，PostgreSQL 失败保留源数据，成功后清理 Redis，并在线上观察 `metrics_archive` 持续增长。（生产验证：release `v0.1.0-52ee2c839c6f` 上线后调度连续运行，归档从 160 行增长到 200 行，8 个服务的分钟记录均对齐且无重复）
- [x] 验证 `auth-http` session 指标索引替代全库扫描：登录、续期、活动刷新、替换登录、登出和密码修改均维护索引，常规采集只使用 `ZREMRANGEBYSCORE + ZCARD`，发布期完成存量 backfill，线上 `cmdstat_scan` 不再每 5 秒增长。（生产验证：backfill dry-run 与两次 apply 成功；真实登录三个索引均为 1、登出均为 0；20 秒自然窗口 `SCAN` 增量由旧版 592 降为 0）
- [x] 【转交专门压测模块】使用接近生产规模的 Redis fixture 验证 20 万相关 key 下 Registry、服务概览和服务详情的延迟、返回正确性和 Redis 命令分布。（当前进展：2026-08-26 在 7,345 个线上 key 下实测 Registry 热请求 p95 110.9ms、p99 116ms，缓存过期请求 p95/p99 108ms。范围决策：metrics v2 key 受 TTL 和索引上限约束，当前约 7 千 key 已接近稳态；20 万 key/玩家 key 场景转交专门压测模块，本轮不处理且不作为验收阻塞项）
- [x] 【本轮不做】增加 Redis 指标：Registry 请求耗时、snapshot cache 命中、索引成员数、过期成员清理、collector 写入失败、legacy 写入状态与剩余量、`used_memory/maxmemory`、`rejected_connections`、`total_error_replies` 和关键 OOM 错误；达到 75% warning、85% critical 时告警。（当前进展：线上可读取内存、连接拒绝、淘汰、legacy 状态和 Registry 容量数据；近 24 小时有 2 次 AOF fsync-slow 告警。范围决策：2026-08-26 暂不处理完整指标、75%/85% 告警闭环及 AOF 专项排查，不作为本轮验收阻塞项）
- [x] 【转交专门压测模块】建立 metrics key 容量模型，按服务数、实例数、5 秒 bucket、TTL 和单记录大小计算迁移期峰值与 v2 稳态；验证生产容量预算不依赖 legacy key 自然过期碰运气。（当前进展：线上稳态样本为 7,345 个 key、14.33 MiB/512 MiB。范围决策：容量与玩家 key 峰值验证转交专门压测模块，本轮不处理且不作为验收阻塞项）
- [x] 【本轮不做】增加 Redis `maxmemory + noeviction` 故障测试，证明 collector 容量异常会被观测和隔离，服务注册/worker lease 失败能产生明确告警，并验证恢复后不会继续生成 legacy key。（当前进展：线上为 `noeviction` 且当前无拒绝、淘汰或 OOM；本次只读核查未执行故障注入。范围决策：2026-08-26 暂不处理，不作为本轮验收阻塞项）
- [x] 【本轮不做】验证 Redis、NATS、PostgreSQL 短暂不可用时的 API 局部退化、前端错误状态和恢复后的自动刷新。（当前进展：三个依赖当前均健康，Registry 返回 `partial=false`；本次只读核查未中断任何依赖。范围决策：2026-08-26 暂不处理，不作为本轮验收阻塞项）
- [x] 【本轮不做】验证多实例服务的注册、心跳失效、重启、注销和 metrics snapshot 过期不会产生假健康或索引泄漏。（当前进展：线上 8 个服务实例健康、heartbeat TTL 20 至 30 秒、容器重启数为 0；未执行过期、重启或注销演练。范围决策：2026-08-26 暂不处理，不作为本轮验收阻塞项）
- [x] 在生产观察窗口验证 legacy key 连续 24 小时零新增、Redis 内存低于 75%、无 `OOM command not allowed`、关键服务重启计数不增长；保存脱敏后的验证记录。（验证：按用户确认的间隔 24 小时两次采样口径，2026-08-26 11:12:48 至 2026-08-27 11:33 +08:00 两次 legacy dry-run 均为 remaining 0；Redis 内存从 14.33 MiB 增至 14.35 MiB，始终约为上限的 2.8%；13 个容器两次 `RestartCount=0`、`OOMKilled=false`，第二次核查近 24 小时 OOM/fatal/panic 日志均为 0）
- [x] 【本轮不做】核对线上容器实际 `HostConfig` 与资源契约一致，记录内存硬限制、memory-swap、CPU quota/shares/cpuset 的实际值；CPU 继续不设限时必须明确记录这是经压测确认的策略而非遗漏配置。（当前进展：已核对容器无 OOM 且重启数为 0；CPU 仍未设限，尚无目标规模压测依据支持该策略。范围决策：2026-08-26 暂不处理，不作为本轮验收阻塞项）
- [x] 更新监控设计、服务发现设计、Docker 部署文档和线上排障 runbook，写明禁止在请求路径使用 Redis `SCAN` 的约束。（验证：commit `cab4403` 已同步 `Registry监控读模型设计.md`、`监控设计.md`、服务发现/注册、Docker 初始化与整体架构等 10 份文档）

## 最终完成定义

- 开始时间：2026-07-30 17:10:59 +08:00
- 结束时间：2026-08-27 12:31:54 +08:00
- 验收总结：Registry 请求路径已消除 Redis 全库扫描，分钟级 PostgreSQL 归档和 `auth-http` session 有界索引已发布并通过生产验收；legacy 连续 24 小时零新增，Redis 容量、服务健康和 Registry 状态稳定。20 万玩家 key 压测转交专门压测模块，自动容量发布门禁、60 万 legacy key 预发演练、故障注入和多实例生命周期等事项按用户决定暂不处理；历史迁移顺序以范围关闭记录留档。本清单按当前约定完成。

- [x] Registry、服务概览、服务详情和归档的运行时路径均经测试证明不执行 Redis `SCAN`、`KEYS` 或无界 key 枚举。（验证：阶段 2 Node/Rust 定向测试、阶段 4 monitoring/archive 55 项定向测试和运行时源码检索通过；离线 legacy 清理工具除外）
- [x] 【转交专门压测模块】生产 Redis 在目标数据规模下 Registry API 满足 `p95 < 500ms`、`p99 < 1s`，前端不再出现轮询重叠导致的 Registry 获取超时。（当前验证：7,345 个线上 key 下热请求 p95 110.9ms、p99 116ms，缓存过期请求 p95/p99 108ms。范围决策：20 万 key/玩家 key 目标规模验收转交专门压测模块，本轮不处理且不作为验收阻塞项）
- [x] Node/Rust 服务注册互操作、当前指标展示、历史图表和 Registry 生命周期告警均通过回归验证。（验证：阶段 2 Node/Rust 契约与生命周期测试通过；2026-08-26 线上 8 个服务 Registry 实例健康，当前指标新鲜度 0 至 4 秒，1m/5m/15m/1h 历史窗口分别返回 12/60/180/720 点，Registry errors/alerts 均为 0）
- [x] collector 生产配置已停止 legacy metrics 双写，连续 24 小时未产生新的 legacy bucket；v2 latest/history/index 与 PostgreSQL 归档保持连续。（生产验证：24 小时两次 legacy dry-run 均为 0；release `v0.1.0-52ee2c839c6f` 上线后分钟归档从 160 行增长到 200 行）
- [x] 旧 metrics 已按受控流程迁移或清理，清理工具证明未匹配或删除 `metrics:v2:*`、heartbeat、session、ticket、服务注册或其他业务 key。（生产验证：最终 legacy 0、v2 2,252；apply 排除项只有 v2，heartbeat/异常结构/wrong type/非 metrics 均为 0）
- [x] Redis `used_memory/maxmemory` 在生产观察窗口持续低于 75%，无 `OOM command not allowed` 或关键写入拒绝；`auth-http`、`game-server`、`chat-server`、`match-service` 健康且重启计数不再增长。（验证：2026-08-26 与 2026-08-27 间隔 24 小时采样内存均约 2.8%，第二次采样 `evicted_keys=0`、`rejected_connections=0`，近 24 小时无 OOM 日志；四个关键服务及其余生产容器均运行且重启数保持 0）
- [x] 【本轮不做】灰度、回滚、容量观测和线上排障流程均已文档化并完成一次受控演练。（范围决策：文档化成果保留；2026-08-26 决定暂不补充故障与生命周期受控演练，不作为本轮验收阻塞项）
