# 项目未完成任务清单

更新时间：2026-06-13 19:41:40 +08:00

## 协作流程

- 主 agent 负责读取仓库上下文和本清单，拆分并派发新的 subagent，复核 subagent 产出的 diff，按需启动必要服务做验收，更新本清单，并在确认范围后使用 `$mygit-skill` 暂存和提交。
- subagent 只负责一个明确子任务的实现、局部文档或验证准备，交付完成上下文和建议验收方式；subagent 不直接 commit、不 push、不回滚或覆盖他人改动。
- 每个功能点由新的 subagent 承接，完成后由主 agent 复核验收；如验收需要启动 old/new/proxy/auth、Redis、MySQL、NATS 或 mock-client，由主 agent 统一确认并执行。
- 每完成一项任务，主 agent 更新本文件中的状态、结束时间、验收说明和相关提交，再按 `$mygit-skill` 检查工作区、隔离无关改动、暂存本项文件并提交 git。
- 再将下一项任务、本次完成说明、未提交上下文、运行服务/依赖和验收风险综合后交给新的 subagent。

## 状态说明

- `待开始`：尚未派发或开发。
- `进行中`：已开始开发或验收，记录开始时间。
- `待验收`：已有实现，等待主 agent 复核、测试和确认。
- `已完成`：已通过验收并提交 git，记录结束时间和提交。
- `阻塞`：存在明确外部依赖或无法继续推进的问题。

## 任务执行模板

每个任务至少维护以下字段；已完成任务保留历史说明，不强制反复改写。

- 状态：待开始 / 进行中 / 待验收 / 已完成 / 阻塞。
- 开始时间：派发或开始实现的本地时间，格式 `YYYY-MM-DD HH:mm:ss +08:00`。
- 结束时间：主 agent 验收并提交后的本地时间，未完成填 `待填写`。
- 优先级：P0 / P1 / P2。
- 范围：涉及文件、模块、文档、外部仓库或服务边界。
- 目标：该任务完成后必须达到的行为或交付结果。
- 派发说明：主 agent 给 subagent 的具体边界、禁止事项和期望输出。
- 已完成上下文：当前已有实现、已知未提交改动、前置提交或相关观察。
- 验收计划：主 agent 计划如何复核 diff、启动服务、运行脚本或做人工检查。
- 验收说明：实际执行的检查、结果、未覆盖风险和是否通过。
- 运行服务/依赖：需要启动或确认可用的服务、数据库、中间件、外部客户端和环境变量。
- 测试命令：可运行的单测、脚本、dry-run、execute 或人工命令；未运行时写明原因。
- 阻塞/回退：阻塞条件、失败时保守回退方式、避免破坏现有环境的要求。
- 交给下一 subagent 的上下文：完成后必须交接的剩余问题、文件状态和下一步入口。
- 相关提交：已落地的 commit 标题或 hash；未提交填 `待填写`。

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
- 派发说明：
  - 补齐 route metadata 缺失故障演练和对应测试，不扩大到真实 execute 联调。
- 已完成上下文：
  - 已完成并提交，后续任务不要重复纳入本项 diff。
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
- 运行服务/依赖：
  - 本项已通过模拟和单测完成；真实服务 execute 不属于本项完成条件。
- 测试命令：
  - `node --test --experimental-test-isolation=none --test-concurrency=1 tests/rollout-fault-drill.test.mjs`
  - `node --test --experimental-test-isolation=none --test-concurrency=1 tests/rollout-transfer-cli.test.mjs tests/room-transfer-orchestrator.test.mjs`
  - `npm run rollout:fault-drill -- --simulate --drill route-metadata-missing --rollout-epoch rollout-test --room-id room-test`
- 阻塞/回退：
  - 无当前阻塞；真实 route metadata 丢失恢复仍放在第 4 项。
- 交给下一 subagent 的上下文：
  - 从第 2 项继续真实 old/new/proxy 三进程 rollout 联调，不要把第 1 项重新打开。
- 相关提交：本次提交 `test(rollout): 增加 route metadata 缺失演练`

### 2. 真实 old/new/proxy 三进程 rollout 联调

- 状态：进行中
- 开始时间：2026-06-12 18:43:54 +08:00
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
- 派发说明：
  - 交给单个 subagent 时，只处理真实三进程 rollout 联调的一个明确子范围，例如脚本参数修正、CLI envelope/report 输出、actor header 贯通或 runbook 补齐。
  - subagent 不提交 git、不 push、不启动长期服务；完成后交还 diff 说明、已运行命令和下一步 execute 验收入口。
- 已完成上下文：
  - 第 1 项 route metadata 缺失演练已完成并提交：`test(rollout): 增加 route metadata 缺失演练`。
  - 当前工作区已有未提交改动，集中在 `scripts/rollout-three-process-drill.ps1`、`docs/rollout-three-process-drill-runbook.md`、`tools/mock-client/src/rollout-transfer-cli.js`、`tools/mock-client/src/rollout-transfer.js`、相关 Node tests 和 `summary/todolist.md`。
  - 这些未提交改动主要增强 dry-run/report/envelope/actor header，不能视为真实 old/new/proxy 三进程 `-ExecuteSteps` 联调已完成。
  - 真实 old/new/proxy 管理面链路的 execute 验收仍未完成，不能把本项状态改为 `已完成`。
- 验收计划：
  - 启动 old game-server、new game-server、game-proxy 及必要依赖。
  - 执行三进程演练脚本的非 destructive 路径。
  - 用 mock-client 验证 room transfer 后同 `room_id` 进入新服。
  - 在主 agent 复核 diff 后，按 runbook 启动 auth-http、old/new game-server、game-proxy，以及 Redis/MySQL/NATS 等脚本实际需要的依赖。
  - 先运行 dry-run/report，确认 envelope、actor header、端口和 route metadata 参数正确，再执行 `-ExecuteSteps`。
  - execute 通过后检查 old freeze/export、new import/ownership confirm、proxy route upsert、old retire、complete-if-drained 的结果和日志。
- 验收说明：待填写
- 运行服务/依赖：
  - 需要 old game-server、new game-server、game-proxy、auth-http。
  - 需要 Redis；如登录、ticket、审计或脚本路径要求，还需要 MySQL/MariaDB 和 Core NATS。
  - 需要 mock-client 可用，并按实际端口配置 old/new/proxy/admin/auth 地址。
  - 本次文档改造不实际启动任何服务。
- 测试命令：
  - 待主 agent 确认当前未提交 diff 后运行相关 Node 单测，例如 rollout transfer CLI / orchestrator 覆盖。
  - dry-run/report 命令以 `scripts/rollout-three-process-drill.ps1` 当前参数为准。
  - 真实验收必须包含 `scripts/rollout-three-process-drill.ps1` 的 `-ExecuteSteps` 路径；本次文档改造不运行。
- 阻塞/回退：
  - 如 old/new/proxy/auth 或 Redis/MySQL/NATS 未就绪，保持 `进行中` 或转 `阻塞`，不要标记完成。
  - 如 execute 中途失败，保留日志和 report，优先确认 old room 未被错误 retire；必要时按 runbook 回退 route metadata 或重新拉起旧服。
- 交给下一 subagent 的上下文：
  - 从当前未提交 dry-run/report/envelope/actor header 改动继续，先复核 diff 是否只服务三进程 rollout 联调。
  - 明确真实 `-ExecuteSteps` 仍待主 agent 启动服务后验收；subagent 只能准备脚本、CLI、测试和 runbook。
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
- 派发说明：待派发时填写单个子任务边界；subagent 不直接 commit/push。
- 已完成上下文：待填写
- 验收计划：
  - 启动必要服务。
  - 用 mock-client 覆盖 redirect/reconnect 场景。
  - 如 `MYSERVER_CLIENT_ROOT` 可用，再检查 mybevy 侧配置和协议适配。
- 验收说明：待填写
- 运行服务/依赖：待填写
- 测试命令：待填写
- 阻塞/回退：待填写
- 交给下一 subagent 的上下文：待填写
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
- 派发说明：待派发时填写单个子任务边界；subagent 不直接 commit/push。
- 已完成上下文：第 1 项已覆盖模拟演练；真实 Redis/proxy route metadata 缺失恢复仍未执行。
- 验收计划：
  - 构造真实 Redis/proxy route metadata 缺失。
  - 运行 fault drill 或手动控制面调用。
  - 确认不会错误 retire old room。
- 验收说明：待填写
- 运行服务/依赖：待填写
- 测试命令：待填写
- 阻塞/回退：待填写
- 交给下一 subagent 的上下文：待填写
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
- 派发说明：待派发时填写单个子任务边界；subagent 不直接 commit/push。
- 已完成上下文：待填写
- 验收计划：
  - 启动服务并构造已排空状态。
  - 验证安全闸不满足时不会触发停服。
  - 验证安全闸满足时可请求 graceful shutdown。
- 验收说明：待填写
- 运行服务/依赖：待填写
- 测试命令：待填写
- 阻塞/回退：待填写
- 交给下一 subagent 的上下文：待填写
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
- 派发说明：待派发时填写单个子任务边界；subagent 不直接 commit/push。
- 已完成上下文：待填写
- 验收计划：
  - 增加至少一个真实或接近真实的 AI 状态 roundtrip 测试。
  - 验证导出、导入、继续 tick 后状态一致。
- 验收说明：待填写
- 运行服务/依赖：待填写
- 测试命令：待填写
- 阻塞/回退：待填写
- 交给下一 subagent 的上下文：待填写
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
- 派发说明：待派发时填写单个子任务边界；subagent 不直接 commit/push。
- 已完成上下文：待填写
- 验收计划：
  - 保持现有房间生命周期和 transfer 测试通过。
  - 增加并发房间操作或压力型单元测试。
  - 记录改造前后风险和行为边界。
- 验收说明：待填写
- 运行服务/依赖：待填写
- 测试命令：待填写
- 阻塞/回退：待填写
- 交给下一 subagent 的上下文：待填写
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
- 派发说明：待派发时填写单个子任务边界；subagent 不直接 commit/push。
- 已完成上下文：待填写
- 验收计划：
  - 增加 `check:proto` 或等价脚本。
  - 覆盖 mock-client 与 `packages/proto` 的消息号/字段校验。
  - 明确 chat proto 迁入共享包的路线。
- 验收说明：待填写
- 运行服务/依赖：待填写
- 测试命令：待填写
- 阻塞/回退：待填写
- 交给下一 subagent 的上下文：待填写
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
- 派发说明：待派发时填写单个子任务边界；subagent 不直接 commit/push。
- 已完成上下文：待填写
- 验收计划：
  - 新增 migration 目录和版本表。
  - 提供本地执行脚本。
  - 验证空库初始化和已有库增量升级路径。
- 验收说明：待填写
- 运行服务/依赖：待填写
- 测试命令：待填写
- 阻塞/回退：待填写
- 交给下一 subagent 的上下文：待填写
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
- 派发说明：待派发时填写单个子任务边界；subagent 不直接 commit/push。
- 已完成上下文：待填写
- 验收计划：
  - 先形成设计文档和最小状态持久化方案。
  - 覆盖撮合成功、失败、超时、取消和重启恢复测试。
- 验收说明：待填写
- 运行服务/依赖：待填写
- 测试命令：待填写
- 阻塞/回退：待填写
- 交给下一 subagent 的上下文：待填写
- 相关提交：待填写
