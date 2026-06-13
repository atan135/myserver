# 项目未完成任务清单

更新时间：2026-06-13 21:32:57 +08:00

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

- 状态：已完成
- 开始时间：2026-06-12 18:43:54 +08:00
- 结束时间：2026-06-13 21:03:34 +08:00
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
  - 阶段性工具收口已提交：
    - `8bf8a5c docs: 完善 todolist 协作交接模板`
    - `8281202 test(rollout): 完善三进程演练报告准入`
  - 本次真实验收使用直连 old game-server TCP `7000` 创建 `movement_demo` retained empty room，避免 proxy 在 room 准备阶段产生旧 player route 干扰 `complete-if-drained`。
  - 临时验收文件和报告均保留在 `.tmp/`，不提交；`apps/chat-server/*` 仍为既有无关 modified，本项不触碰。
- 验收计划：
  - 启动 old game-server、new game-server、game-proxy 及必要依赖。
  - 执行三进程演练脚本的非 destructive 路径。
  - 用 mock-client 验证 room transfer 后同 `room_id` 进入新服。
  - 在主 agent 复核 diff 后，按 runbook 启动 auth-http、old/new game-server、game-proxy，以及 Redis/MySQL/NATS 等脚本实际需要的依赖。
  - 先运行 dry-run/report，确认 envelope、actor header、端口和 route metadata 参数正确，再执行 `-ExecuteSteps`。
  - execute 通过后检查 old freeze/export、new import/ownership confirm、proxy route upsert、old retire、complete-if-drained 的结果和日志。
- 验收说明：
  - 阶段性工具收口已通过：脚本 dry-run / execute 都会写 report，`-ExecuteSteps` 在控制面调用前拒绝空值、占位值、非法 `RoomId` / `RolloutEpoch` / `AdminActor`。
  - `rollout-transfer-cli.js` 的 parse error、validation failure、execute failure、fatal catch 均输出机器可读 JSON envelope。
  - `--proxy-admin-actor` 已由外层脚本传到底层 CLI，`ProxyAdminClient` 会发送 `X-Admin-Actor`。
  - 已运行并通过：
    - `node --test --experimental-test-isolation=none --test-concurrency=1 tests/rollout-transfer-cli.test.mjs tests/room-transfer-orchestrator.test.mjs`
    - `powershell -NoProfile -ExecutionPolicy Bypass -File scripts/rollout-three-process-drill.ps1 -SkipPortProbe -ReportPath .tmp\rollout-three-process-drill-report-main-check.json`
    - `powershell -NoProfile -ExecutionPolicy Bypass -File scripts/rollout-three-process-drill.ps1 -ExecuteSteps -SkipPortProbe -RoomId '<ROOM_ID>' -RolloutEpoch rollout-test -ReportPath .tmp\rollout-three-process-drill-report-main-fail-check.json`，预期失败且未调用服务，report 只记录 `preflight-gate failed`。
  - 真实服务验收已完成：
    - 运行环境：Redis Windows service、NATS、本地 `auth-http`、`game-server-old`、`game-server-new`、`game-proxy`。
    - 房间准备：`.tmp\prepare-rollout-room.mjs` 直连 old `127.0.0.1:7000`，以 `movement_demo` 创建并 leave `rollout-room-20260613210106`；脚本按消息类型等待 `ROOM_JOIN_RES`，确认可处理 `ROOM_FRAME_RATE_PUSH` 先到达。
    - 准备后 old drain status：`ownedRoomCount=1`、`transferableEmptyRoomCount=1`，目标 room `onlineMemberCount=0`。
    - 准备后 proxy `/room-routes` 和 `/player-routes` 均为空，确认没有旧 proxy route 污染。
    - dry-run 报告：`.tmp\rollout-three-process-drill-report-dryrun-real.json`，计划包含 `old_freeze -> old_export -> new_import -> new_confirm_ownership -> proxy_route_upsert -> old_retire`，shutdown safety gate 保持 skipped。
    - execute 报告：`.tmp\rollout-three-process-drill-report-execute.json`，`ok=true`、`mode=execute`、`transfer.ok=true`、`transfer.summary.stage=complete`。
    - execute completed stages：`old_freeze`、`old_export`、`new_import`、`new_confirm_ownership`、`proxy_route_upsert`、`old_retire`。
    - `proxy-complete-if-drained=ok`，drain evaluation 为 `Drained`，`blocked_room_count=0`、`blocked_player_count=0`，end summary 清理当前 rollout room route 1 条。
    - shutdown safety gate 未执行：`allowShutdownRequest=false`、`shutdownRequestCanRun=false`、`shutdown-safety-gate=skipped`。
    - 事后 proxy `/status`：`active_upstream=game-server-new`、`rollout_session=null`、`room_route_count=0`、`player_route_count=0`。
    - 事后 old drain status：目标 room 为 `RetiredOnOld`，`ownedRoomCount=0`、`migratingRoomCount=0`、`retiredRoomCount=1`、`connectionCount=0`。
  - 未覆盖项：
    - 本项只覆盖空房迁移控制面第一阶段，不覆盖 `ServerRedirectPush` 后真实客户端重连；该项继续由第 3 项跟进。
    - 本项没有传 `-AllowShutdownRequest`，不会自动停止 old server；部署平台自动停旧进程继续由第 5 项跟进。
- 运行服务/依赖：
  - 已使用 Redis、Core NATS、auth-http、old/new game-server、game-proxy。
  - `MYSQL_ENABLED=false`，本次登录和控制面验收未依赖 MySQL。
  - 关键端口：auth `3000`、old game/admin `7000/7500`、new game/admin `7001/7501`、proxy admin `7101`、proxy TCP fallback `14000`、Redis `6379`、NATS `4222`。
- 测试命令：
  - `node --test --experimental-test-isolation=none --test-concurrency=1 tests/rollout-transfer-cli.test.mjs tests/room-transfer-orchestrator.test.mjs`
  - `powershell -NoProfile -ExecutionPolicy Bypass -File scripts/rollout-three-process-drill.ps1 -SkipPortProbe -ReportPath .tmp\rollout-three-process-drill-report-main-check.json`
  - `powershell -NoProfile -ExecutionPolicy Bypass -File scripts/rollout-three-process-drill.ps1 -ExecuteSteps -SkipPortProbe -RoomId '<ROOM_ID>' -RolloutEpoch rollout-test -ReportPath .tmp\rollout-three-process-drill-report-main-fail-check.json`
  - `node .tmp\prepare-rollout-room.mjs --room-id rollout-room-20260613210106 --guest-id rollout-room-20260613210106-guest --http-base-url http://127.0.0.1:3000 --host 127.0.0.1 --port 7000 --policy-id movement_demo --timeout-ms 10000`
  - `powershell -NoProfile -ExecutionPolicy Bypass -File scripts\rollout-three-process-drill.ps1 -RoomId rollout-room-20260613210106 -RolloutEpoch rollout-20260613210231 -OldServerId game-server-old -NewServerId game-server-new -OldAdminPort 7500 -NewAdminPort 7501 -ProxyAdminUrl http://127.0.0.1:7101 -ProxyAdminToken <set> -OldAdminToken <set> -NewAdminToken <set> -AuthBaseUrl http://127.0.0.1:3000 -ServiceToken <set> -AdminActor rollout-three-process-drill -ReportPath .tmp\rollout-three-process-drill-report-dryrun-real.json`
  - `powershell -NoProfile -ExecutionPolicy Bypass -File scripts\rollout-three-process-drill.ps1 -ExecuteSteps -RoomId rollout-room-20260613210106 -RolloutEpoch rollout-20260613210231 -OldServerId game-server-old -NewServerId game-server-new -OldAdminPort 7500 -NewAdminPort 7501 -ProxyAdminUrl http://127.0.0.1:7101 -ProxyAdminToken <set> -OldAdminToken <set> -NewAdminToken <set> -AuthBaseUrl http://127.0.0.1:3000 -ServiceToken <set> -AdminActor rollout-three-process-drill -ReportPath .tmp\rollout-three-process-drill-report-execute.json`
  - `Invoke-RestMethod http://127.0.0.1:7101/status -Headers @{ Authorization = 'Bearer <proxy-admin-token>' }`
  - `Invoke-RestMethod http://127.0.0.1:7101/rollout -Headers @{ Authorization = 'Bearer <proxy-admin-token>' }`
  - `Invoke-RestMethod http://127.0.0.1:3000/api/v1/internal/game-server/rollout-drain-status -Headers @{ 'X-Service-Token' = '<service-token>' }`
- 阻塞/回退：
  - 无当前阻塞。
  - 如复现时 execute 中途失败，保留 `.tmp\rollout-three-process-drill-report-execute.json` 和 `.tmp\rollout-drill-logs/`，优先确认 old room 是否仍未 retire、proxy route 是否仍指向 old，再按 runbook 回退 route metadata 或重新准备空房。
- 交给下一 subagent 的上下文：
  - 第 2 项真实 old/new/proxy/auth 空房迁移控制面已完成，不要重新打开。
  - 第 3 项继续验证 redirect 后真实客户端重连；可复用已启动服务，但注意 proxy 当前 active upstream 已是 `game-server-new`，old server 仍处于 drain mode，若要构造 redirect 场景需先按 runbook 恢复或重新准备 old/new 状态。
  - 第 5 项才处理 `-AllowShutdownRequest` / 自动停旧服，本项验收明确未执行停服请求。
- 相关提交：本次提交 `docs: 记录真实三进程 rollout 验收结果`

### 3. redirect 后真实客户端重连验证

- 状态：已完成
- 开始时间：2026-06-13 21:05:00 +08:00
- 结束时间：2026-06-13 21:32:57 +08:00
- 优先级：P0
- 范围：
  - `tools/mock-client`
  - 外部客户端 `mybevy` 的接入验证说明
  - `apps/game-server`
  - `apps/game-proxy`
- 目标：
  - 验证 `ServerRedirectPush` 下发后，客户端断开旧连接并通过 proxy 重连到新服。
  - 明确 mock-client 与 mybevy 的适配边界。
- 派发说明：
  - 本项原计划交给新的 subagent 做只读复核和局部实现检查；subagent 因上游 stream disconnect 未返回结果，主 agent 继续本地复核、修正、验收和提交。
  - subagent 不直接 commit/push，不触碰 `apps/chat-server/*` 既有无关改动。
- 已完成上下文：
  - 新增 mock-client 场景 `server-redirect-transfer-reconnect`，覆盖登录、进房、old admin 触发 `ServerRedirectPush`、old/new room transfer、proxy room route upsert、proxy player route upsert、按 redirect target 重新连接 proxy、重新 `AuthReq` 并优先 `RoomReconnectReq`。
  - `joinRoomExpectSuccess` 现在会传递 `--policy-id`，并按消息类型等待 `ROOM_JOIN_RES` 和 `ROOM_STATE_PUSH`，可处理 `ROOM_FRAME_RATE_PUSH` 先到达的包顺序。
  - 抽取 redirect reconnect helper，保留既有 `server-redirect-reconnect` 场景“等待外部控制面触发 push”的语义；主动触发只放在新的三进程 transfer reconnect 场景。
  - `ProxyAdminClient` 新增 `upsertPlayerRoute`，用于 transfer 后把玩家路由绑定到新 owner，避免 proxy reconnect 因找不到离线 player route 返回 `PLAYER_NOT_OFFLINE`。
  - 早期失败观察已转化为修正：
    - 只等待 redirect 的旧场景不会自行触发 push，无法独立完成端到端验证。
    - 新场景初版缺少主动 trigger，无法收到 `ServerRedirectPush`。
    - 仅迁移 room route 不足以让 proxy 将 `RoomReconnectReq` 路由到新服，需要同步写 player route。
- 验收计划：
  - 启动必要服务。
  - 用 mock-client 覆盖 redirect/reconnect 场景。
  - 如 `MYSERVER_CLIENT_ROOT` 可用，再检查 mybevy 侧配置和协议适配。
- 验收说明：
  - 代码复核范围：`tools/mock-client/src/args.js`、`constants.js`、`index.js`、`rollout-transfer.js`、`scenarios/index.js`、`scenarios/room.js`，以及 `tests/server-redirect-reconnect.test.mjs`、`tests/room-transfer-orchestrator.test.mjs`。
  - 单测已通过：
    - `node --test --experimental-test-isolation=none --test-concurrency=1 tests\server-redirect-reconnect.test.mjs`
    - `node --test --experimental-test-isolation=none --test-concurrency=1 tests\rollout-transfer-cli.test.mjs tests\room-transfer-orchestrator.test.mjs`
  - 真实服务验收已通过：
    - room：`redirect-room-20260613212549`
    - epoch：`redirect-20260613212549`
    - log：`.tmp\server-redirect-transfer-reconnect.log`
    - `ServerRedirectPush` 下发成功：`deliveredCount=1`、`failedCount=0`、`onlineMemberCount=1`。
    - transfer completed stages：`old_freeze`、`old_export`、`new_import`、`new_confirm_ownership`、`proxy_route_upsert`、`old_retire`。
    - transfer 后写入 proxy player route：`player-aaec2642-452a-4c58-955b-99fb6218411f` -> `redirect-room-20260613212549`，preferred server 为 `game-server-new`。
    - 重连经过 proxy target `127.0.0.1:14000`，重新鉴权后 `RoomReconnectRes.ok=true`、`finalMode=reconnect`、`finalRoomId=redirect-room-20260613212549`。
    - 重连期间先收到 `ROOM_FRAME_RATE_PUSH` 再收到 `ROOM_RECONNECT_RES`，新实现通过 `readUntil` 正确处理。
  - 未覆盖项：
    - 本项只验证 mock-client；外部 `mybevy` 尚未做真实联调，仍需按 `MYSERVER_CLIENT_ROOT` 和客户端仓库当前实现另行验证。
    - 本项不处理部署平台自动停旧进程，继续由第 5 项跟进。
- 运行服务/依赖：
  - 已复用第 2 项启动的 Redis Windows service、NATS、auth-http、old/new game-server、game-proxy。
  - 关键端口：auth `3000`、NATS `4222`、old game/admin `7000/7500`、new game/admin `7001/7501`、proxy admin `7101`、proxy TCP fallback `14000`。
  - 真实验收后临时服务仍在运行；`.tmp/` 日志和报告不提交。
- 测试命令：
  - `node --test --experimental-test-isolation=none --test-concurrency=1 tests\server-redirect-reconnect.test.mjs`
  - `node --test --experimental-test-isolation=none --test-concurrency=1 tests\rollout-transfer-cli.test.mjs tests\room-transfer-orchestrator.test.mjs`
  - `node tools\mock-client\src\index.js --scenario server-redirect-transfer-reconnect --http-base-url http://127.0.0.1:3000 --no-service-discovery --host 127.0.0.1 --port 14000 --room-id redirect-room-20260613212549 --guest-id redirect-room-20260613212549-guest --policy-id movement_demo --rollout-epoch redirect-20260613212549 --old-server-id game-server-old --new-server-id game-server-new --old-admin-host 127.0.0.1 --old-admin-port 7500 --old-admin-token <set> --new-admin-host 127.0.0.1 --new-admin-port 7501 --new-admin-token <set> --proxy-admin-url http://127.0.0.1:7101 --proxy-admin-token <set> --proxy-admin-actor task3-redirect-reconnect --redirect-target-host 127.0.0.1 --redirect-target-port 14000 --redirect-target-server-id game-proxy --redirect-transport tcp --redirect-retry-after-ms 0 --timeout-ms 15000`
- 阻塞/回退：
  - 无当前阻塞。
  - 若复现时 reconnect 返回 `PLAYER_NOT_OFFLINE` 或 room not found，优先检查 proxy `/player-routes` 是否存在当前 player -> room -> preferred server 绑定，以及 room route 是否指向 `game-server-new`。
  - 若 push 未收到，先确认 old room 仍在线且 old admin trigger 返回 `deliveredCount > 0`。
- 交给下一 subagent 的上下文：
  - 第 3 项 mock-client redirect -> transfer -> proxy reconnect 已通过并准备提交，不要重新打开。
  - 第 4 项继续处理真实 route metadata 缺失后的恢复演练；可参考第 1 项模拟 drill，但需要在真实 Redis/proxy route store 场景下验证不会误 retire old room，并明确恢复路径。
  - 注意 `apps/chat-server/*` 仍为既有无关 modified；下一项不要触碰或暂存这些文件。
- 相关提交：待提交

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
