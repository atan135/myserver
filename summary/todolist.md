# 项目未完成任务清单

更新时间：2026-06-12 18:41:16 +08:00

## 协作流程

- 主 agent 负责维护本清单、派发子 subagent、复核代码、启动服务和 mock-client 做验收。
- 每个功能点由新的 subagent 承接，完成后由主 agent 复核验收。
- 每完成一项任务，主 agent 更新本文件中的状态、结束时间、验收说明和相关提交，然后提交 git。
- 再将下一项任务、本次完成说明和当前上下文综合后交给新的 subagent。
- 本次会话已授权主 agent 在验收阶段自动跑集成、启动 server 和 mock-client。

## 状态说明

- `待开始`：尚未派发或开发。
- `进行中`：已开始开发或验收，记录开始时间。
- `待验收`：已有实现，等待主 agent 复核、测试和确认。
- `已完成`：已通过验收并提交 git，记录结束时间和提交。
- `阻塞`：存在明确外部依赖或无法继续推进的问题。

## 任务列表

### 1. 本轮未提交：rollout route metadata 丢失故障演练

- 状态：已完成
- 开始时间：2026-06-12 18:29:02 +08:00
- 结束时间：2026-06-12 18:41:16 +08:00
- 优先级：P0
- 范围：
  - `tools/mock-client/src/rollout-transfer.js`
  - `tools/mock-client/src/rollout-fault-drill.js`
  - `tests/rollout-fault-drill.test.mjs`
  - `docs/rollout-fault-drill-runbook.md`
- 目标：
  - 新增 `route-metadata-missing` 故障演练。
  - 当 proxy 查询不到既有 room route metadata 时，应停在 `proxy_route_upsert`。
  - 不允许将缺失 metadata 静默当作首次创建 route 成功。
  - 不允许继续 retire old room。
- 当前说明：
  - 已派发给第 1 个 subagent 复核并补齐演练实现。
  - `apps/chat-server/*` 当前显示 modified，但 `git diff --stat` 没有内容，本项提交不纳入这些文件。
- 验收计划：
  - 静态复核故障注入路径和 mock 调用顺序。
  - 运行 `rollout-fault-drill` 相关 Node 测试。
  - 如需要，启动相关服务和 mock-client 做可选 execute 验证。
- 验收说明：
  - 新增 `route-metadata-missing` drill，并在 `proxy.getRoomRoute` 返回缺失后直接以 `ROOM_ROUTE_METADATA_MISSING` 停在 `proxy_route_upsert`。
  - dry-run 计划不包含 `proxy.upsertRoomRoute`；simulate 和 orchestrator 测试确认不会 upsert，也不会 retire old room。
  - 已运行并通过：
    - `node --test --experimental-test-isolation=none --test-concurrency=1 tests/rollout-fault-drill.test.mjs`
    - `node --test --experimental-test-isolation=none --test-concurrency=1 tests/rollout-transfer-cli.test.mjs tests/room-transfer-orchestrator.test.mjs`
    - `node --test --experimental-test-isolation=none --test-concurrency=1 tests/rollout-fault-drill.test.mjs tests/rollout-transfer-cli.test.mjs tests/room-transfer-orchestrator.test.mjs`
    - `npm run rollout:fault-drill -- --simulate --drill route-metadata-missing --rollout-epoch rollout-test --room-id room-test`
  - 未执行真实 `--execute`，因为该演练会故意停在迁移中间阶段，真实环境需要人工准备和清理；真实 route metadata 丢失后的端到端恢复仍保留为第 4 项。
- 相关提交：本次提交 `test(rollout): 增加 route metadata 缺失演练`

### 2. 真实 old/new/proxy 三进程 rollout 联调

- 状态：待开始
- 开始时间：待填写
- 结束时间：待填写
- 优先级：P0
- 范围：
  - `scripts/rollout-three-process-drill.ps1`
  - `tools/mock-client/src/rollout-transfer-cli.js`
  - `apps/game-server`
  - `apps/game-proxy`
- 目标：
  - 在真实 old/new/proxy 三进程环境中跑通第一阶段 rollout。
  - 验证 old freeze/export、new import/confirm ownership、proxy route upsert、old retire、complete-if-drained 的端到端链路。
- 验收计划：
  - 启动 old game-server、new game-server、game-proxy 及必要依赖。
  - 执行三进程演练脚本的非 destructive 路径。
  - 用 mock-client 验证 room transfer 后同 `room_id` 进入新服。
- 验收说明：待填写
- 相关提交：待填写

### 3. redirect 后真实客户端重连验证

- 状态：待开始
- 开始时间：待填写
- 结束时间：待填写
- 优先级：P0
- 范围：
  - `tools/mock-client`
  - 外部客户端 `mybevy` 的接入验证说明
  - `apps/game-server`
  - `apps/game-proxy`
- 目标：
  - 验证 `ServerRedirectPush` 下发后，客户端断开旧连接并通过 proxy 重连到新服。
  - 明确 mock-client 与 mybevy 的适配边界。
- 验收计划：
  - 启动必要服务。
  - 用 mock-client 覆盖 redirect/reconnect 场景。
  - 如 `MYSERVER_CLIENT_ROOT` 可用，再检查 mybevy 侧配置和协议适配。
- 验收说明：待填写
- 相关提交：待填写

### 4. route metadata 真实丢失后的恢复演练

- 状态：待开始
- 开始时间：待填写
- 结束时间：待填写
- 优先级：P1
- 范围：
  - `game-proxy` route store
  - Redis route metadata
  - `tools/mock-client` rollout fault drill
  - 对应 runbook
- 目标：
  - 在真实 route metadata 缺失场景下验证控制面行为。
  - 明确恢复策略：人工恢复、重新导出导入、重新 upsert，或保守中止。
- 验收计划：
  - 构造真实 Redis/proxy route metadata 缺失。
  - 运行 fault drill 或手动控制面调用。
  - 确认不会错误 retire old room。
- 验收说明：待填写
- 相关提交：待填写

### 5. 部署平台自动停旧进程

- 状态：待开始
- 开始时间：待填写
- 结束时间：待填写
- 优先级：P1
- 范围：
  - `RequestServerShutdownReq/Res`
  - `auth-http` shutdown-if-drained 内部接口
  - 部署脚本或进程管理器集成点
  - 运维 runbook
- 目标：
  - 在灰度结束且旧服真实排空后，接入部署平台或进程管理器自动停止旧服进程。
  - 保留安全闸：drain enabled、connection/owned/migrating 均为 0。
- 验收计划：
  - 启动服务并构造已排空状态。
  - 验证安全闸不满足时不会触发停服。
  - 验证安全闸满足时可请求 graceful shutdown。
- 验收说明：待填写
- 相关提交：待填写

### 6. 完整 NPC/AI 行为迁移

- 状态：待开始
- 开始时间：待填写
- 结束时间：待填写
- 优先级：P1
- 范围：
  - `apps/game-server/src/core/logic/room_logic.rs`
  - `apps/game-server/src/gameroom/combat_demo`
  - 未来真实行为树、AI timer、path、RNG 状态
- 目标：
  - 从当前 demo 级 NPC 迁移契约推进到真实行为树恢复点、AI timer、path、RNG 的完整迁移。
  - 导入后行为继续推进并保持可验证一致性。
- 验收计划：
  - 增加至少一个真实或接近真实的 AI 状态 roundtrip 测试。
  - 验证导出、导入、继续 tick 后状态一致。
- 验收说明：待填写
- 相关提交：待填写

### 7. RoomManager 扩展性改造

- 状态：待开始
- 开始时间：待填写
- 结束时间：待填写
- 优先级：P1
- 范围：
  - `apps/game-server/src/core/runtime/room_manager.rs`
- 目标：
  - 降低当前全局 `Mutex<HashMap<...>>` 带来的锁热点。
  - 评估并落地 actor、shard 或拆锁方案。
- 验收计划：
  - 保持现有房间生命周期和 transfer 测试通过。
  - 增加并发房间操作或压力型单元测试。
  - 记录改造前后风险和行为边界。
- 验收说明：待填写
- 相关提交：待填写

### 8. 协议生成与一致性检查

- 状态：待开始
- 开始时间：待填写
- 结束时间：待填写
- 优先级：P2
- 范围：
  - `packages/proto`
  - `tools/mock-client`
  - 外部 `mybevy`
  - `apps/chat-server` 本地 proto
- 目标：
  - 建立 mock-client、mybevy、Rust、Node 之间的协议生成或一致性校验。
  - 降低手写消息号、字段和枚举漂移风险。
- 验收计划：
  - 增加 `check:proto` 或等价脚本。
  - 覆盖 mock-client 与 `packages/proto` 的消息号/字段校验。
  - 明确 chat proto 迁入共享包的路线。
- 验收说明：待填写
- 相关提交：待填写

### 9. DB migration 体系

- 状态：待开始
- 开始时间：待填写
- 结束时间：待填写
- 优先级：P2
- 范围：
  - `db/`
  - Node 服务启动期 schema 初始化
  - 本地脚本和文档
- 目标：
  - 从单一 `db/init.sql` 和局部 `ALTER TABLE IF NOT EXISTS` 过渡到版本化 migration。
  - 支持增量迁移、回滚或至少明确的修复脚本。
- 验收计划：
  - 新增 migration 目录和版本表。
  - 提供本地执行脚本。
  - 验证空库初始化和已有库增量升级路径。
- 验收说明：待填写
- 相关提交：待填写

### 10. match-service 多实例与恢复

- 状态：待开始
- 开始时间：待填写
- 结束时间：待填写
- 优先级：P2
- 范围：
  - `apps/match-service`
  - Redis/DB 状态存储
  - match 与 game-server 建房协作
- 目标：
  - 设计并逐步实现匹配池、任务状态和事件流的多实例/重启恢复能力。
  - 引入租约、状态机和超时补偿。
- 验收计划：
  - 先形成设计文档和最小状态持久化方案。
  - 覆盖撮合成功、失败、超时、取消和重启恢复测试。
- 验收说明：待填写
- 相关提交：待填写
