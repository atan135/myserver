# old/new/proxy 三进程 rollout 演练入口

## 文档定位

本文说明 `scripts/rollout-three-process-drill.ps1` 的使用方式。该脚本是第一阶段可重复演练入口，用来把已经落地的控制面工具串起来：

```text
preflight
-> proxy /rollout/start
-> old game-server drain on
-> 选择可迁移空房
-> old freeze/export
-> new import/confirm
-> proxy /room-route/upsert
-> old retire
-> rollout drain status
-> proxy /rollout/complete-if-drained
-> 可选 request-server-shutdown
```

脚本默认是 dry-run，只做本地工具检查、端口探测、命令输出、`rollout-transfer-cli --dry-run` 机器可读计划校验和执行报告归档，不会启动服务，不会修改正在运行的服务状态，也不会请求旧服停服。2026-06-13 已在真实 old/new/proxy/auth 环境中人工执行一轮 `movement_demo` 空房迁移控制面并通过；后续复现仍需要主 agent 或人工先确认依赖和服务状态。

## 前置条件

运行 `-ExecuteSteps` 前，主 agent 或人工需要先确认并启动依赖和服务：

- Redis / MySQL / NATS 等依赖按当前环境要求启动。
- `auth-http` 已运行，且内部 game-server admin client 指向 old game-server。
- old game-server 已运行，例如 `game-server-old`，玩家端口默认 `7000`，admin 端口默认 `7500`。
- new game-server 已运行，例如 `game-server-new`，玩家端口默认 `7001`，admin 端口默认 `7501`。
- `game-proxy` 已运行，admin 默认 `http://127.0.0.1:7101`，并能发现 old/new 两个 upstream。
- proxy admin token、old/new game admin token、auth-http 内部 `X-Service-Token` 按实际环境配置。
- proxy admin 写接口会记录 `X-Admin-Actor`；如果 `PROXY_ADMIN_AUDIT_REQUIRE_ACTOR=true`，脚本和底层 transfer CLI 必须提供合法 actor。默认 actor 是 `rollout-three-process-drill`。

脚本不会调用 `scripts/dev-stack.ps1`，也不会自动启动任何真实服务。需要启动时应先由主 agent 或用户确认。

## 可迁移房间要求

当前第一阶段只支持空房或全员离线房间的 transfer。有人在线的 room 会在 freeze 阶段被 old game-server 拒绝，错误通常是 `ROOM_TRANSFER_HAS_ONLINE_MEMBERS`。

建议选择方式：

1. 先开启 drain，阻止旧服继续创建新房。
2. 查询旧服 drain status，优先从 `transferableEmptyRoomSamples` 中选择 `onlineMemberCount == 0` 且仍由 old 持有的 room。
3. 如果没有可迁移空房，先按当前测试环境准备一个支持 transfer 的 room，让所有成员离开或断线，确认 status 里出现可接管空房后再执行 transfer。

不要把 `apps/simple-client` 当正式客户端准备工具；正式客户端在外部 `mybevy` 仓库，本仓库侧演练使用 `tools/mock-client`。

## Dry-run

默认模式只输出预检结果和计划命令：

```powershell
powershell -ExecutionPolicy Bypass -File scripts/rollout-three-process-drill.ps1
```

指定参数但仍不执行写操作：

```powershell
powershell -ExecutionPolicy Bypass -File scripts/rollout-three-process-drill.ps1 `
  -RolloutEpoch rollout-20260612-a `
  -RoomId room-empty-001 `
  -OldServerId game-server-old `
  -NewServerId game-server-new
```

默认 dry-run 允许 `RoomId` / `RolloutEpoch` 为空，此时脚本会在展示命令和计划里使用 `<ROOM_ID>` / `<ROLLOUT_EPOCH>` 占位，便于先检查工具、端口和步骤顺序。占位只允许 dry-run；`-ExecuteSteps` 和底层 CLI 执行模式都会拒绝占位值。需要把参数完整性作为准入条件时，直接调用底层 CLI：

```powershell
node tools/mock-client/src/rollout-transfer-cli.js --dry-run `
  --rollout-epoch rollout-20260612-a `
  --room-id room-empty-001 `
  --old-server-id game-server-old `
  --new-server-id game-server-new `
  --old-admin-port 7500 `
  --new-admin-port 7501 `
  --proxy-admin-url http://127.0.0.1:7101 `
  --proxy-admin-actor rollout-three-process-drill
```

该 CLI dry-run 只做参数和计划校验，不打开 game-server admin socket，不访问 proxy HTTP，也不会 retire / shutdown。输出 JSON 中的关键字段：

- `safety.callsControlPlane=false`
- `safety.requestsShutdown=false`
- `plan.plannedStages=old_freeze -> old_export -> new_import -> new_confirm_ownership -> proxy_route_upsert -> old_retire`
- `plan.endpoints` 展示 old/new game-server admin 和 game-proxy admin 目标
- `plan.endpoints.gameProxyAdmin.actor` 展示将用于 `X-Admin-Actor` 的操作人
- `plan.routeCas` 展示 route CAS 默认策略

脚本每次 dry-run 或 execute 都会写出执行报告，默认路径：

```text
.tmp/rollout-three-process-drill-report.json
```

也可用 `-ReportPath` 或 `MYSERVER_ROLLOUT_REPORT_PATH` 指定。报告不保存 token 明文，只保存 token 状态、endpoint、actor、stage 结果和 `rollout-transfer-cli` 的 JSON plan/result。主 agent 可直接检查：

- `ok=true`
- `mode=dry-run` 或 `mode=execute`
- `safety.startsServices=false`
- `transfer.ok=true`
- dry-run 时 `transfer.safety.callsControlPlane=false`
- execute 时 `transfer.mode=transfer-execute` 且 `transfer.summary.stage=complete`

## 执行步骤

确认服务已经运行后，使用 `-ExecuteSteps` 调用控制面：

```powershell
powershell -ExecutionPolicy Bypass -File scripts/rollout-three-process-drill.ps1 `
  -ExecuteSteps `
  -RolloutEpoch rollout-20260612-a `
  -RoomId room-empty-001 `
  -OldServerId game-server-old `
  -NewServerId game-server-new `
  -OldAdminPort 7500 `
  -NewAdminPort 7501 `
  -ProxyAdminUrl http://127.0.0.1:7101 `
  -AdminActor rollout-three-process-drill `
  -AuthBaseUrl http://127.0.0.1:3000
```

执行模式下，`rollout-transfer-cli` 不再只输出原始 orchestrator 结果，而是输出统一 JSON envelope：

- `ok`：transfer 是否成功。
- `mode=transfer-execute`。
- `safety.callsControlPlane=true`、`safety.requestsShutdown=false`。
- `validation`：与 dry-run 相同的参数校验结果。
- `summary.stage`：成功时应为 `complete`；失败时为失败阶段，例如 `old_freeze`、`new_import`、`proxy_route_upsert`、`old_retire`。
- `summary.completedStages`：成功完成的阶段列表。
- `summary.errorCode`：失败时用于定位服务端或控制面错误。
- `result`：底层 orchestrator 原始结果，包含 checksum、imported roomVersion 和 proxy route CAS 细节。

`request-server-shutdown` 默认不会执行。只有同时传入 `-ExecuteSteps` 和 `-AllowShutdownRequest` 才会调用 auth-http 的 `POST /api/v1/internal/game-server/shutdown-if-drained`，该接口仍由 game-server 自身校验 `drain_mode_enabled == true`、`connection_count == 0`、`owned_room_count == 0`、`migrating_room_count == 0` 后才会触发 graceful shutdown。

```powershell
powershell -ExecutionPolicy Bypass -File scripts/rollout-three-process-drill.ps1 `
  -ExecuteSteps `
  -AllowShutdownRequest `
  -RolloutEpoch rollout-20260612-a `
  -RoomId room-empty-001
```

## 常用环境变量

脚本参数可以直接传入，也可以用环境变量覆盖默认值：

| 参数 | 环境变量 | 默认 |
|------|----------|------|
| `-RoomId` | `ROOM_ID` / `MYSERVER_ROLLOUT_ROOM_ID` | 空 |
| `-RolloutEpoch` | `ROLLOUT_EPOCH` / `MYSERVER_ROLLOUT_EPOCH` | 空 |
| `-OldServerId` | `MYSERVER_OLD_SERVER_ID` | `game-server-old` |
| `-NewServerId` | `MYSERVER_NEW_SERVER_ID` | `game-server-new` |
| `-OldAdminHost` | `MYSERVER_OLD_GAME_ADMIN_HOST` | `127.0.0.1` |
| `-OldAdminPort` | `MYSERVER_OLD_GAME_ADMIN_PORT` | `7500` |
| `-OldAdminToken` | `MYSERVER_OLD_GAME_ADMIN_TOKEN` / `GAME_ADMIN_TOKEN` | dev 默认 token |
| `-NewAdminHost` | `MYSERVER_NEW_GAME_ADMIN_HOST` | `127.0.0.1` |
| `-NewAdminPort` | `MYSERVER_NEW_GAME_ADMIN_PORT` | `7501` |
| `-NewAdminToken` | `MYSERVER_NEW_GAME_ADMIN_TOKEN` / `GAME_ADMIN_TOKEN` | dev 默认 token |
| `-ProxyAdminUrl` | `MYSERVER_PROXY_ADMIN_URL` | `http://127.0.0.1:7101` |
| `-ProxyAdminToken` | `PROXY_ADMIN_TOKEN` | dev 默认 token |
| `-AuthBaseUrl` | `MYSERVER_AUTH_BASE_URL` | `http://127.0.0.1:3000` |
| `-ServiceToken` | `MYSERVER_INTERNAL_API_TOKEN` / `INTERNAL_API_TOKEN` | 空 |
| `-TimeoutMs` | `MYSERVER_ROLLOUT_TIMEOUT_MS` | `5000` |
| `-AdminActor` | `MYSERVER_ADMIN_ACTOR` / `MYSERVER_PROXY_ADMIN_ACTOR` | `rollout-three-process-drill` |
| `-ReportPath` | `MYSERVER_ROLLOUT_REPORT_PATH` | `.tmp/rollout-three-process-drill-report.json` |

底层 `rollout-transfer-cli.js` 的 `--proxy-admin-actor` 默认读取 `MYSERVER_PROXY_ADMIN_ACTOR`，未设置时为 `rollout-transfer-cli`。外层脚本会显式把 `-AdminActor` 传给 CLI，因此通过脚本执行时统一使用脚本 actor。

## 真实服务验收检查

主 agent 启动真实服务后，建议按以下顺序验收：

1. 先执行 dry-run：带上真实 `-RolloutEpoch`、`-RoomId`、token、endpoint 和 `-SkipPortProbe` 或保留端口探测，确认 report 中 `mode=dry-run`、`transfer.ok=true`、`transfer.safety.callsControlPlane=false`。
2. 启动 old/new/proxy/auth 后执行 `-ExecuteSteps`，先不要加 `-AllowShutdownRequest`，确认 `transfer.ok=true`、`transfer.summary.stage=complete`、`proxy-complete-if-drained` stage 为 `ok`。
3. 检查 old drain status 输出中 `ownedRoomCount=0`、`migratingRoomCount=0`、`connectionCount=0`；如开启 proxy 真实 drain 校验，`complete-if-drained` 不应返回 blocker。
4. 需要演练停服安全闸时，再单独执行带 `-ExecuteSteps -AllowShutdownRequest` 的命令，或用 mock-client `request-server-shutdown` 场景调用；不要把 shutdown 作为默认执行路径。

最近一次人工验收记录见 `summary/todolist.md` 第 2 项。该次验收使用 Redis、Core NATS、auth-http、old/new game-server 和 game-proxy，跑通 old freeze/export、new import/confirm ownership、proxy route upsert、old retire 和 `complete-if-drained`；未传 `-AllowShutdownRequest`，因此没有请求自动停旧服。

## 仍未完成

这个入口只把当前已存在的控制面调用串成可重复步骤。它已有一轮真实服务人工验收，但不代表以下能力已经完成：

- 还没有把真实三进程联调纳入自动测试准入；当前只覆盖 dry-run / preflight / plan/result 归档级准入和一次人工验收记录。
- 外部 `mybevy` redirect/reconnect 适配仍未完成。
- route metadata 真实丢失后的恢复演练仍未完成。
- 部署平台自动停止旧进程仍未完成。
- 同连接迁移 / L7 relay 仍是后续目标态。
