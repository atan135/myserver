# rollout 故障演练入口

本文说明 `tools/mock-client/src/rollout-fault-drill-cli.js` 的使用方式。该入口用于推进 room rollout 9.4 故障演练，目标是提供可重复、默认安全、可归档 JSON 的脚本级演练。

当前定位:

- 默认 `dry-run`，只打印将要执行的故障演练计划，不访问服务，不调用写接口。
- `--simulate` 使用纯内存 mock client 验证编排停止点，不依赖 old/new/proxy/auth-http。
- 只有显式 `--execute` 才调用已有控制面接口。
- 不启动任何服务，不运行三进程联调脚本，不请求旧服停服。
- 这不是 mybevy 适配证明，也不是 old/new/proxy 三进程真实故障联调准入。

## 覆盖的故障

| drill | 目标阶段 | 行为 | 安全边界 |
|------|----------|------|----------|
| `import-failure` | `new_import` | old freeze/export 后，在 new import 前篡改 transfer payload，预期 import 或 checksum 校验失败。 | 停在 `new_import`；不 confirm ownership，不 upsert proxy route，不 retire old room。 |
| `route-upsert-failure` | `proxy_route_upsert` | import 与 ownership confirm 成功后，使用错误 `expected_room_version` 触发 proxy route CAS 失败。 | 停在 `proxy_route_upsert`；不 retire old room。 |
| `redirect-no-reconnect` | `redirect_no_reconnect` | 只触发或计划 `ServerRedirectPush`，明确不运行 mock-client reconnect 场景。 | 只验证 push/操作步骤；不声称 mybevy 已适配，不执行 reconnect。 |

## 默认 dry-run

```bash
node tools/mock-client/src/rollout-fault-drill-cli.js
```

输出 JSON 中 `mode` 为 `dry-run`，`safety.callsControlPlane=false`，不会访问服务。

指定单个故障:

```bash
node tools/mock-client/src/rollout-fault-drill-cli.js --drill import-failure
```

## 纯模拟验证

```bash
node tools/mock-client/src/rollout-fault-drill-cli.js --simulate
```

该模式会运行内存 mock 版 orchestrator，并校验结果是否满足:

- `ok=false`
- `expectedFailure=true`
- `stage` 停在预期故障阶段
- 后续破坏性阶段未完成

## 执行模式

仅在 old/new game-server admin 和 game-proxy admin 已由人工或主 agent 启动并确认配置后使用:

```bash
node tools/mock-client/src/rollout-fault-drill-cli.js ^
  --execute ^
  --drill import-failure ^
  --rollout-epoch rollout-20260612-fault ^
  --room-id room-empty-001 ^
  --old-server-id game-server-old ^
  --new-server-id game-server-new ^
  --old-admin-host 127.0.0.1 --old-admin-port 7500 --old-admin-token "<old-admin-token>" ^
  --new-admin-host 127.0.0.1 --new-admin-port 7501 --new-admin-token "<new-admin-token>" ^
  --proxy-admin-url http://127.0.0.1:7101 --proxy-admin-token "<proxy-admin-token>"
```

`redirect-no-reconnect` 执行模式需要 redirect target:

```bash
node tools/mock-client/src/rollout-fault-drill-cli.js ^
  --execute ^
  --drill redirect-no-reconnect ^
  --rollout-epoch rollout-20260612-fault ^
  --room-id rollout-redirect-room ^
  --old-admin-host 127.0.0.1 --old-admin-port 7500 --old-admin-token "<old-admin-token>" ^
  --redirect-target-host 127.0.0.1 ^
  --redirect-target-port 4000 ^
  --redirect-target-server-id game-server-new
```

## 结果归档

```bash
node tools/mock-client/src/rollout-fault-drill-cli.js --simulate --archive-dir artifacts/rollout
```

或指定文件:

```bash
node tools/mock-client/src/rollout-fault-drill-cli.js --simulate --archive-file artifacts/rollout/fault-drill.json
```

归档内容是完整 JSON report，便于后续 CI 或人工复核。

## 未覆盖范围

- 真实 old/new/proxy 三进程自动化故障联调。
- mybevy 客户端 redirect/reconnect 适配。
- 部署平台自动停旧进程。
- 同连接迁移 / L7 relay。
- `route metadata` 真实丢失后的端到端恢复演练。
