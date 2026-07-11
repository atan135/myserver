# 共享帧同步调试验证 Checklist

## 目标

统一承接共享帧同步的调试、自动化验证、真实联调、客户端验收、失败归档和最终验收记录。`summary/游戏服共享帧同步接入_checklist.md` 与 `summary/mybevy共享帧同步接入_checklist.md` 只保留功能接入和模块实现记录；后续涉及 `game-server`、`tools/lockstep-client`、外部 `mybevy`、单/双客户端、重连、观战、HUD、日志、截图、文档核对和最终验收的工作，统一在本清单推进和关闭。

本清单不重新拆解 P0/P1/P2 的 `sim-core` 规则开发，也不替代 `summary/帧同步流量优化_checklist.md` 的带宽治理专项。需要启动 Redis、PostgreSQL、Core NATS、`auth-http`、`game-server`、`game-proxy` 或外部 `mybevy` 客户端前，必须先列出依赖、命令、端口、外部仓库路径和影响范围，并等待用户确认。

## 基础原则

- [x] 调试验证以服务端权威 `stateHash`、客户端 local hash、事件序列、实体 fixed 坐标和恢复 snapshot 为主要判据。（验证：阶段 3-7 的 online 报告逐帧保存权威/本地 hash、fixed world、events 和恢复 snapshot；阶段 9 failure triage 保留首 mismatch 双方证据）
- [x] 自动化优先：凡是 hash、事件、坐标、输入 payload、快照恢复、构建回归和文档字段一致性能用脚本验证的，不依赖人工目测。（验证：阶段 2-9 由 Rust 测试、wrapper SelfTest、headless JSONL、online 对账、构建/proto 和结构化报告完成；阶段 10 独立契约断言通过）
- [x] 人工验收只保留在视觉表现、HUD 可读性、技能/伤害/Buff 表现观感和真实客户端操作体验上。（验证：阶段 5 仅对两张 GUI 截图执行人工目检，其余 hash/world/input/event/recovery 判据均自动化；正式文档明确该边界）
- [x] 任何真实多服务联调都必须使用临时 room、临时 ticket、可清理的 Redis key 和明确日志目录。（验证：阶段 3、5、6、7、9 的 wrapper run 均使用唯一 run/room、临时 character-bound ticket、精确 key prefix 和 `logs/lockstep-online/<run-id>/`）
- [x] 调试脚本失败时必须保留最小复现输入、房间号、frame、server hash、client hash、事件差异、实体差异和相关日志路径。（验证：阶段 9 fresh fixture `stage9-main-review-20260711-01` 在 frame 3 保存双方 hash、输入/实体/事件 diff、room 和 artifact 路径，service 日志可归档）
- [x] 不把外部客户端路径写死到 MyServer 代码或脚本；访问外部客户端统一通过 `MYSERVER_CLIENT_ROOT` 或用户确认的实际路径。（验证：阶段 8 确认 wrapper 支持 `-ClientRoot` 与 Process→User 环境变量回退，正式脚本无本机 mybevy 绝对路径；非相邻 Cargo path 限制已写入文档）
- [x] 完成本清单前，不删除或替换 `robot_sync_room`、`movement_demo`、`combat_demo`、`arena.robot_sync` 等旧联调入口。（验证：阶段 8 的 factory/scene 回归和 `robot_sync` 95/95 测试确认全部旧入口仍保留）

## 阶段 1：接管范围和旧清单收口

- 开始时间：2026-07-09 17:28:35 +08:00
- 结束时间：2026-07-09 17:28:35 +08:00
- 开发总结：新增统一调试验证清单，接管游戏服和 mybevy 接入清单中的真实联调、自动化验收、单/双客户端验收、重连/观战验证、构建回归、文档核对和最终验收记录。原接入清单保留功能实现和已完成阶段证据，后续调试验证不再在两个接入清单中重复拆分。
- 验证记录：只更新 `summary/` 下 checklist 文档；未启动 Redis、PostgreSQL、Core NATS、MyServer 服务、`tools/lockstep-client` online 或外部 `mybevy` 客户端。

- [x] 明确本清单接管 `summary/游戏服共享帧同步接入_checklist.md` 的 online 对账、端到端联调和最终验收项。
- [x] 明确本清单接管 `summary/mybevy共享帧同步接入_checklist.md` 的单客户端、双客户端、重连/观战、构建回归、接入文档和最终验收项。
- [x] 保留 `summary/共享帧同步后续接入与联调_checklist.md` 中已完成的 MyServer 侧 online 对账证据，作为后续脚本化复验的基线。
- [x] 保留 `summary/帧同步流量优化_checklist.md` 为独立带宽治理专项，不混入本轮功能验收。
- [x] 验证项：游戏服和 mybevy 接入清单均记录后续调试验证由本清单统一完成。

## 阶段 2：离线基线和静态契约复验

- 开始时间：2026-07-10 14:07:48 +08:00
- 结束时间：2026-07-10 14:31:09 +08:00
- 开发总结：完成共享确定性核心、服务端适配层和 lockstep-client 的离线基线复验；补齐 lockstep-client 对 config/schema 元数据、eventCount、eventSummaries、debugState 和 observer 诊断字段的显式建模与漂移校验，并同步正式外部客户端接入契约。移动与近战 offline hash 已固定，online dry-run 能稳定生成 packet plan 且不启动网络。
- 验证记录：`sim-core` 98/98、`game-server lockstep_sim` 40/40、`lockstep-client` 27/27，`cargo fmt --manifest-path tools/lockstep-client/Cargo.toml -- --check` 与 `git diff --check` 通过；game-server 保留既有 build script 1 条 deprecated warning 和测试目标 53 条 unused/dead-code/lifetime warning。阶段 2 仅使用本机 Rust/Cargo，未启动 Redis、PostgreSQL、Core NATS、auth-http、game-server、game-proxy 或 mybevy，也未执行真实网络连接。

- [x] 运行 `cargo test --manifest-path packages/sim-core/Cargo.toml`，确认共享确定性核心测试通过。（验证：主 agent 复跑 98 passed、0 failed，doc-test 0）
- [x] 运行 `cargo test --manifest-path apps/game-server/Cargo.toml lockstep_sim`，确认服务端适配层和 `lockstep_sim_demo` 回归通过。（验证：主 agent 复跑 40 passed、0 failed、369 filtered；覆盖 policy、snapshot、frame envelope、observer/reconnect 和旧 demo 保护）
- [x] 运行 `cargo test --manifest-path tools/lockstep-client/Cargo.toml`，确认 offline / online parser / mismatch 诊断测试通过。（验证：主 agent 复跑 27 passed、0 failed；`tools/lockstep-client/src/online.rs:2537` 覆盖完整服务端字段解析，`:2620` 覆盖 config/schema/eventCount mismatch 拒绝）
- [x] 运行 `cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode offline --scenario move_straight`，记录 final frame 和 final hash。（验证：final frame `5`，final hash `ad9a151d0953d437`）
- [x] 运行 `cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode offline --scenario lockstep_demo_melee`，记录 final frame 和 final hash。（验证：final frame `1`，final hash `959839fddfc8c0dc`）
- [x] 运行 online dry-run 的移动和近战场景，确认 `sim_input` packet plan、snapshot/hash/events/inputSources 消费入口仍可用。（验证：`move_straight` 生成 5 个 packet、`lockstep_demo_melee` 生成 1 个 packet；两者均列出 `initialSnapshot` 与 hash/events/eventSummaries/inputSources 消费入口并输出 `network: not started; dry-run only`）
- [x] 核对 `lockstep_sim_demo` policy ID、`sim_input` JSON 字段、`initialSnapshot`、`lastFrame`、`observerFrame`、`stateHash`、`eventSummaries` 与协议文档一致。（验证：policy 常量见 `apps/game-server/src/gameroom/lockstep_sim_demo/mod.rs:17`，严格 `sim_input` schema 见 `apps/game-server/src/core/system/lockstep_sim/mod.rs:939`；客户端 DTO/校验见 `tools/lockstep-client/src/online.rs:687`、`:762`、`:794`、`:867`、`:1049`、`:1152`，正式字段契约见 `docs/协议与客户端/外部客户端接入说明.md:174`、`:176`、`:177`、`:180`）
- [x] 验证项：输出一份离线基线记录，包含命令、结果、hash、既有 warning 和未执行真实服务的原因。（验证：本阶段验证记录包含 3 组测试数量、2 个 final frame/hash、既有 warning；真实服务未执行，因为阶段 2 限定为本地离线测试和 online dry-run，真实联调需阶段 3 单独确认）

## 阶段 3：MyServer online 对账自动化

- 开始时间：2026-07-10 14:34:15 +08:00
- 结束时间：2026-07-10 19:12:52 +08:00
- 开发总结：完成 MyServer 侧共享帧 online 一键编排、临时 ticket/key ownership、结构化成功/失败证据和精确 cleanup 闭环；真实联调期间修复了长期子进程继承输出句柄导致的启动阻塞、Windows PowerShell 5.1 PID JSON 顶层数组读取、observer 离房错误重置 `InGame` 阶段三个问题。最终成功报告可直接归档移动/近战 hash、最终帧事件与摘要、observer recovery 帧/hash，以及 ticket、registry、进程、端口和环境清理结果。
- 验证记录：主 agent 复跑 lifecycle 6/6、`game-server lockstep_sim` 40/40、`lockstep-client` 33/33、PowerShell 5.1 SelfTest 15/15、Node helper 5/5、精确 rustfmt、PowerShell AST、`git diff --check` 和三阶段 dry-run均通过。最终真实 run `online-20260710-1904-codex05` 的 `report.json` 为 passed：move frame 5/hash `110e5725a9b3515d`，melee frame 1/hash `33c409b8b0be5d12` 且实际事件/摘要均为 2，observer current/snapshot/initial/last/observerLast 均为 5、hash `110e5725a9b3515d`。cleanup overall/Redis/registry/process/environment 均通过，4 个 ticket key 与 2 个 registry key compare-delete 后独立 `EXISTS=0`、`PTTL=-2`；`4222/7000/7500` 空闲，Redis 仍为 PID `4904`，PID 文件不存在，日志确认 `db_enabled=false` 且无 PostgreSQL 连接。未启动已排除范围的 match-service，因此保留 1 条 `9002` 连接失败和 1 条重发现 warning，不影响三场景结果；各次失败 run 和最终 `final-run.*` 日志均保留在 `logs/lockstep-online/<run-id>/`。

- [x] 封装或确认可复用的 MyServer online 调试脚本，能启动最小 dev-stack、准备临时 ticket、运行对账并清理资源。（验证：`scripts/online-lockstep-reconcile.ps1:513` 使用 launcher-only wait，`:576` 统一读取 PID ownership；最终 run `online-20260710-1904-codex05` 一次命令完成 NATS/game-server 启停、双 ticket provision、三场景和精确 cleanup，报告 status=passed）
- [x] 自动化移动场景：`tools/lockstep-client --mode online --scenario move_straight` 能完成服务端 hash 与本地 replay hash 对账。（验证：最终报告 move stage passed/exitCode=processExitCode=0，room `lockstep-online-20260710-1904-codex05-move`，frame 5，已对账 hash `110e5725a9b3515d`，finalEventCount=0）
- [x] 自动化近战场景：`tools/lockstep-client --mode online --scenario lockstep_demo_melee` 能完成事件和 hash 对账。（验证：最终报告 melee stage passed，frame 1/hash `33c409b8b0be5d12`；实际 `skill_cast` 1000->9000 skill 1/value 1/seq 0 与 `damage_applied` value 14/seq 1 均与本地 replay 相等，`skillCast`/`damage` 摘要各 1 条）
- [x] 自动化 observer recovery probe，确认 `RoomJoinAsObserverRes.snapshot.game_state` 可恢复并对齐 `observerFrame.lastFrame`。（验证：`apps/game-server/src/core/runtime/room_manager/tests/lifecycle.rs:142` 锁定 observer 离房不重置对局；最终报告 observer stage passed，current/snapshot/initialSnapshot/last/observerLast 全为 5，observer/final hash 均为 `110e5725a9b3515d`）
- [x] 记录联调依赖：Redis、Core NATS、`game-server`、必要 ticket key、端口、日志目录和停止命令。（验证：`tools/lockstep-client/README.md:218` 的 dependency matrix 记录 Redis `6379`、NATS `4222`、game-server `7000/7500` 与 PostgreSQL 禁用边界；`:265` 记录 report/日志和 exact cleanup，脚本 plan 输出三场景命令、registry planned ownership 与 `DB_ENABLED=false`）
- [x] 脚本失败时输出 room id、ticket 来源、endpoint、失败 stage、frame、server hash、client hash、事件差异和日志路径。（验证：`scripts/online-lockstep-reconcile.ps1:322` 定义 v1 report，`:783` 解析 mismatch 诊断，`:817` 校验成功证据，`:1432` 写入 failure 上下文；`online-20260710-1814-codex03` 将 observer cleanup 的 `room_end/ROOM_NOT_IN_GAME` 与日志路径完整归档）
- [x] 联调结束后清理临时 Redis key、停止由脚本启动的服务并确认端口不再监听。（验证：最终报告 cleanup overall/Redis/registry/process/environment=true、PID file=`matched-owned-pids`；4 个 ticket key 与 instance/heartbeat 两键均 deleted，主 agent 独立复核 6 键 `EXISTS=0/PTTL=-2`，仅 Redis `6379/PID 4904` 保留）
- [x] 验证项：一条命令或一组明确命令能复跑移动、近战和 observer recovery online 对账。（验证：`powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\online-lockstep-reconcile.ps1 -Execute -StartDevStack -ProvisionDevTickets -RunId <run-id> -RedisKeyPrefix 'lockstep-online:<run-id>:'`；最终 run 三 stage 全部 passed，secret 仅由 wrapper 进程环境提供）

## 阶段 4：mybevy headless telemetry 验收入口

- 开始时间：2026-07-11 10:15:12 +08:00
- 结束时间：2026-07-11 10:45:25 +08:00
- 开发总结：在外部 `mybevy` 增加独立 `lockstep-sim-headless` 开发/测试 CLI，默认 GUI 与 MyServer 认证链路保持不变；新增版本化 JSONL telemetry、固定移动/面向/停止/技能+DoT fixture、fixture authority 与 local replay 双轨 hash 对账、快照恢复重放、mismatch 故障注入及无服务端连接失败码。离线 authority hash 通过 `source=offline_fixture_authority` 和 `serverConnected=false` 明确标识，不冒充真实在线服务证据。
- 验证记录：主 agent 复跑 `cargo fmt --check`、`cargo check --lib`、`cargo check --bin lockstep-sim-headless`、`cargo test lockstep_sim_headless --lib`（7 passed）和 `cargo test lockstep_sim --lib`（88 passed）均通过，仅保留既有 `checkbox` dead_code warning；`git diff --check` 通过。实际 CLI 成功路径输出 9 条 schema v1 JSONL，最终 frame 6 fixture authority/local hash 均为 `a3fcee835b1b8459`、mismatch=false、recovery=verified；mismatch 注入在 frame 3 以 exit 2、`HEADLESS_HASH_MISMATCH/frame_compare` 结束；连接 `127.0.0.1:1` 以 exit 5、`HEADLESS_CONNECT_FAILED/connect` 结束。本阶段未启动 Redis、NATS、game-server、auth-http、game-proxy 或 GUI。

- [x] 确认 `MYSERVER_CLIENT_ROOT` 指向外部 `mybevy` 仓库，且用户允许运行或修改该仓库。（验证：用户级环境变量为 `C:\project\mybevy`，`git rev-parse --show-toplevel` 返回 `C:/project/mybevy`，`project/Cargo.toml` 和 lockstep 模块存在；用户已在本轮明确确认执行方案和运行/修改范围）
- [x] 在 mybevy 中提供 headless、bot 或测试模式入口，能进入 `arena.lockstep_sim` 并输出结构化 telemetry。（验证：`project/src/bin/lockstep-sim-headless.rs` 提供独立 CLI，`project/src/lockstep_sim_headless.rs:27` 固定场景 `arena.lockstep_sim`，`:408` 运行离线 fixture；实际成功命令 exit 0 并输出 9 条记录）
- [x] telemetry 至少包含 room、policy、player、frame、server hash、local hash、mismatch、实体 fixed 坐标、事件摘要和 replay recovery 状态。（验证：`project/src/lockstep_sim_headless.rs:128` 定义完整 `TelemetryRecord`，实体 fixed 坐标、事件摘要和恢复结构分别由 `EntityTelemetry`、`EventSummaryTelemetry`、`ReplayRecoveryTelemetry` 承载；实际最终记录包含 frame 6、双 hash、mismatch=false 和 recovery=verified）
- [x] telemetry 输出格式稳定为 JSONL、JSON 文件或可脚本解析的日志行。（验证：`project/src/lockstep_sim_headless.rs:379` 逐记录序列化 JSONL，schema=`mybevy.lockstep.telemetry`、schemaVersion=1；稳定性测试两次运行字节级相等且以换行结尾）
- [x] 支持固定输入脚本：移动、停止、面向、基础技能、Buff / Dot fixture 或等价场景。（验证：`project/src/lockstep_sim_headless.rs:829` 固定 frame 1 move、2 face、3 stop、4 cast skill，fixture 技能施加 DoT；实际 telemetry 事件包含 `skill_cast`、`buff_applied`、`buff_tick`、`damage_applied`）
- [x] 支持在无服务端或连接失败时输出明确错误码和失败阶段。（验证：`project/src/lockstep_sim_headless.rs:692` 的 connect probe 不发送应用数据；实际连接 `127.0.0.1:1` 返回 exit 5、`HEADLESS_CONNECT_FAILED`、failureStage=`connect`）
- [x] 验证项：无需人工看画面即可判断客户端 replay 是否与服务端 hash 对齐。（验证：离线 fixture 将 authority hash 明确标为 `offline_fixture_authority` 且 `serverConnected=false`；成功路径 exit 0/hash matched，注入差异在 frame 3 返回 exit 2 和稳定 mismatch 记录，`cargo test lockstep_sim_headless --lib` 7/7 通过）

## 阶段 5：单客户端调试验收

- 开始时间：2026-07-11 10:47:39 +08:00
- 结束时间：2026-07-11 14:17:29 +08:00
- 开发总结：完成 mybevy 单客户端真实在线与 GUI smoke 验收入口：headless 复用既有 Network/MyServer/Authority/replay 链路按 `lockstep_sim_demo` 的 input lead 发送移动、停止和技能输入，GUI smoke 以显式环境开关运行并生成在线/离线结构化报告与 1280x720 截图；在线场景对齐服务端 hash 并覆盖移动、技能、命中和伤害，离线视觉 fixture 补齐 Buff、DoT 和死亡观感。MyServer wrapper 统一编排临时 ticket、loopback endpoint、进程/Redis ownership、报告归档和精确清理，默认 guest 登录与普通 GUI 行为保持不变。
- 验证记录：主 agent 复核真实 run `stage5-mybevy-gui-20260711-08` 的 `report.json` 为 passed：room `lockstep-stage5-mybevy-gui-20260711-08-mybevy-gui`，frame 16，server/local hash 均为 `b7a8f393f256c5cd`，玩家 fixed x `0 -> 600`，目标 hp `150 -> 136`，事件为 `skill_cast`、`damage_applied`；离线 fixture frame 104 覆盖 `buff_applied`、`buff_tick`、DoT damage、`buff_expired` 和死亡，目标 hp=0/alive=false。两张 1280x720 截图经主 agent 目检 HUD、实体和战斗标记可读，SHA-256 不同。cleanup 的 Redis/registry/process/environment 均通过，临时 ticket/version 和 registry keys 已删除，脚本启动的 NATS/game-server 已停止，`4222/7000/7500` 不再监听。回归：`cargo fmt --all -- --check`、`cargo test lockstep_sim --lib`（100/100）、`cargo test robot_sync --lib`（95/95）、`cargo test authority --lib`（19/19）、`cargo test myserver --lib`（77/77）、`cargo check --lib --bin project --bin lockstep-sim-headless`、PowerShell SelfTest（21 项）、`npm run check:proto` 和两仓 `git diff --check` 均通过，仅保留既有 `checkbox` dead-code warning。

- [x] 启动 MyServer 依赖后，单个 mybevy 客户端能进入 `arena.lockstep_sim`。（验证：wrapper run `stage5-mybevy-gui-20260711-08` 启动 Core NATS/game-server 并 provision 临时 ticket，在线 GUI 报告 source=`myserver_authority`、policy=`lockstep_sim_demo`、status=`captured_with_fixture_gaps`，stage exitCode/processExitCode 均为 0）
- [x] 单客户端移动输入能发送 `sim_input`，服务端下发 frame 后客户端 local hash 与 server hash 对齐。（验证：`online_headless.rs` 和 `visual_smoke.rs` 通过既有 `build_sim_input_envelope`/`AuthorityCommand::SendInput` 发出 lead=2 的输入；真实 run 玩家 fixed x 从 0 到 600，frame 16 的 local/server hash 均为 `b7a8f393f256c5cd`、mismatch=false）
- [x] 单客户端基础技能能触发 `SkillCast`、命中、伤害或 Buff 事件，并在 telemetry 中稳定输出。（验证：在线报告 eventKinds=`skill_cast,damage_applied`、combatVisualKinds=`skill_cast,hit,damage_number`，目标 hp 从 150 降到 136；离线 fixture 报告稳定输出 `buff_applied`、`buff_tick`、`damage_applied`、`entity_died`、`buff_expired`）
- [x] 客户端可视化位置来自 `SimWorld` fixed 坐标，不由 Bevy render delta 提前推进权威状态。（验证：在线与离线视觉报告均记录 `authoritativePositionSource=LockstepSimReplayState.world/SimWorld`、`renderDeltaWritesAuthority=false`，在线 `playerRawPositionMatchesSimWorld=true`；`cargo test lockstep_sim --lib` 100/100 通过）
- [x] HUD 或日志能展示 room、policy、frame、local hash、server hash、mismatch 和事件数量。（验证：在线 `mybevy-online-report.json` 的 HUD 包含 room、policy、player、frame、entities/events、local/server hash、mismatch、recovery 和 first_mismatch；主 agent 目检截图确认 HUD 全部位于面板内且可读）
- [x] 人工 smoke 检查移动、技能、伤害数字、Buff / Dot、死亡状态和 HUD 可读性。（验证：主 agent 目检 `mybevy-online.png` 与 `mybevy-offline-fixture.png`；在线覆盖移动、技能/命中/伤害标记，离线覆盖 Buff/DoT/死亡标记与死亡姿态，报告 combinedAcceptanceComplete=true）
- [x] 验证项：记录单客户端命令、room id、输入脚本、hash 结果、事件结果、截图或人工 smoke 结论。（验证：完整命令编排、room、frame/hash、输入与事件、两份子报告、两张截图、stdout/stderr 和 cleanup 证据统一归档于 `logs/lockstep-online/stage5-mybevy-gui-20260711-08/`；`report.json` schema=`myserver.lockstep-online-reconcile.report.v1`、status=passed）

## 阶段 6：双客户端一致性验收

- 开始时间：2026-07-11 14:19:49 +08:00
- 结束时间：2026-07-11 15:08:51 +08:00
- 开发总结：完成双 mybevy 客户端同房一致性验收。headless 在同一进程内运行两个彼此独立的 Bevy App、MyServer 连接和 authority/replay 栈：主动端先入房，双方 ready 后仅主动端 start 并发送 move/cast/stop，被动端只消费权威帧；双方按共同 authority frame 逐帧比较服务端/local hash、完整 fixed world、hp/alive/buff、输入 source/seq/command 和确定性事件。wrapper 新增 `dual-client` 模式、双 ticket 编排、聚合报告及首 mismatch diagnostics，默认单客户端、GUI、guest 登录和旧入口不变。
- 验证记录：真实 run `stage6-mybevy-dual-20260711-02` 的 `report.json` 为 passed，room `lockstep-stage6-mybevy-dual-20260711-02-mybevy-dual`；共同 frame 1-6 共 6 帧全部 matched，final frame/hash=`6/00e13c690256efc9`，firstMismatchFrame=null。主动角色 `chr_b30e2a52fe0e0a847acb`/entity 1000 fixed x `0 -> 600`，被动角色 `chr_31a4dc452fc7e3f24176`/entity 1001 fixed x `2000 -> 2000`、local input acknowledgements=0；frame 2 双端一致回放 skill_cast/damage_applied，frame 4 一致回放 stop seq 2，训练目标最终 hp=136/alive=true。cleanup 的 Redis/registry/process/environment 均通过；主 agent 独立确认 4 个 ticket/version key 和 2 个 registry key 均 `EXISTS=0/PTTL=-2`、PID 文件不存在、相关环境变量为空、`4222/7000/7500` 无监听，仅保留原 Redis `6379/PID 4904`。主 agent 复跑 `cargo fmt --all -- --check`、dual 5/5、deferred client 1/1、`cargo test lockstep_sim --lib` 108/108、三目标 cargo check、wrapper SelfTest 23/23、`npm run check:proto` 和两仓 diff check 均通过；worker 另跑 authority 20/20、myserver 77/77、robot_sync 95/95，均通过，仅保留既有 `checkbox` dead-code warning。

- [x] 两个 mybevy 客户端进入同一 `lockstep_sim_demo` room，并完成 ready / start。（验证：run02 使用两个不同 character-bound ticket 和独立连接进入同一 room；`online_headless.rs` 等主动端 ready 后再激活被动端，双方 ready 后仅主动端发送 `StartRoom`，两端均观察到 in_game 后才发输入）
- [x] 双客户端在同一服务端 frame 上的 local hash 一致。（验证：聚合报告逐帧列出 frame 1-6 的 activeLocalHash/passiveLocalHash，6 个共同 frame 全部相等，final local hash 均为 `00e13c690256efc9`）
- [x] 双客户端实体 fixed 坐标、hp、alive 状态和事件序列一致。（验证：每个共同 frame 对完整 `SimWorld` 和事件 Vec 做等值比较；run02 每帧 matched，最终双方均记录 entity 1000 x=600/hp=100/alive=true、entity 1001 x=2000/hp=100/alive=true、target hp=136/alive=true，frame 2 事件顺序为 skill_cast seq 0、damage_applied seq 1）
- [x] 服务端 hash 与两个客户端 local hash 一致。（验证：每帧同时要求两个 `serverHash.source=my_server_authority` 且 server/active/passive 三值相等；run02 final 三值均为 `00e13c690256efc9`、mismatch=false）
- [x] 支持一端输入移动、另一端只 replay；两端都不能用 render tick 提前改变权威状态。（验证：唯一 input source 为主动角色，seq 1 包含 move/cast、seq 2 为 stop；主动 entity x `0 -> 600`，被动 entity x 保持 2000 且 local input acknowledgements=0；两端状态均来自 authority replay 的 `SimWorld`，headless 未启用 render tick 状态推进）
- [x] 双客户端日志能按 room、player、frame 和 seq 关联排查。（验证：JSONL 每条记录包含统一 room/policy、`clientRole=active_input|passive_replay`、实际 player、frame，以及每个 input 的 characterId/entityId/sequence/command 和每个 event 的 sequence；stdout/stderr 路径写入 stage report）
- [x] 验证项：输出双客户端 telemetry 对账报告，明确首个 mismatch frame 或确认全程 matched。（验证：`logs/lockstep-online/stage6-mybevy-dual-20260711-02/report.json` 的 `dualReconciliation` 保存 comparedFrames、实体/输入/事件明细、matched=true、firstMismatchFrame=null；失败实现按 hash/entity/event/input 分类输出首个 frame 和双方 diff）

## 阶段 7：重连和观战恢复验收

- 开始时间：2026-07-11 15:11:17 +08:00
- 结束时间：2026-07-11 16:22:03 +08:00
- 开发总结：完成 mybevy 主客户端断线重连和 observer 恢复验收。新增显式 `ReconnectWithTicket`，仅在旧连接已关闭时设置 transport recovery 计划并复用既有鉴权、`RoomReconnectReq`、Authority snapshot/recent+waiting frames 与 replay；接入 `RoomJoinAsObserverReq/Res`，observer 复用同一 snapshot parser 和 replay 核心且不获得控制绑定。headless 新增 `online-reconnect-observer` 场景，按恢复 snapshot+1 检查连续帧、三方 hash/world/input/event、重复应用计数和 observer 输入边界；wrapper 增加对应编排、报告和 mismatch diagnostics。默认 guest、自动重连、GUI、single/dual 和 robot_sync 语义保持不变。
- 验证记录：真实 run `stage7-mybevy-recovery-20260711-02` 的 report status=passed，room `lockstep-stage7-mybevy-recovery-20260711-02-mybevy-recovery`。主客户端断前 frame/hash=`4/ae01417d562e8fcd`，断前 input frame 2 为 move+cast_skill、事件为 skill_cast+damage_applied；disconnect generation=2，`RoomReconnectRes` snapshot frame/hash=`4/ae01417d562e8fcd`，recovery generation=3。恢复后 frame 5-8 严格连续、ignored duplicate/old=0，frame 6 的 stop seq 2 恰好应用一次，最终 frame/hash=`8/7fa61f45d93db1b5`。observer snapshot frame=4/generation=1，frame 5-8 与主客户端逐帧 hash/world/input/event 相等，local input ack=0、无 control binding。主 agent 复跑 fmt、lockstep_sim 114/114、authority 21/21、myserver 81/81、三目标 cargo check、wrapper SelfTest 27 项、`npm run check:proto` 和两仓 diff-check 均通过；worker 另跑 robot_sync 95/95。主 agent 独立确认 4 个 ticket/version key 与 2 个 registry key 均 `EXISTS=0/PTTL=-2`，环境为空、PID 文件不存在、`4222/7000/7500` 无监听、仅原 Redis `6379/PID 4904` 保留；日志 JWT/ticket JSON/环境赋值形态扫描为 0。仅保留既有 `checkbox` dead-code warning。

- [x] 主客户端断线重连后使用服务端恢复快照重建 `SimWorld`。（验证：显式 reconnect 等旧 `connection_id` 清空后复用 transport recovery 计划；run02 断前 world/hash 位于 frame 4，`RoomReconnectRes.snapshot.game_state` 解析后 snapshot frame/hash 与断前相同，scene/replay generation 从 disconnect 2 更新为 recovery 3）
- [x] 重连后从恢复 snapshot frame 继续消费权威帧，不重复应用或跳过输入。（验证：`collect_recovery_stream` 要求首帧为 snapshot+1 并逐帧 +1；run02 从 snapshot frame 4 连续消费 5、6、7、8，ignoredDuplicateOrOldFrames=0，恢复后 stop seq 2 在 frame 6 只出现一次）
- [x] 重连后 local hash 能重新与 server hash 对齐。（验证：恢复流每帧强制 server hash 存在且与 local hash 相等；run02 frame 5-8 全 matched，最终 server/local hash 均为 `7fa61f45d93db1b5`）
- [x] 观战客户端不发送本地输入，但能从 `RoomJoinAsObserverRes.snapshot.game_state` 和后续 frame replay 到一致 hash。（验证：observer 通过新增 observer command/event 接收 frame 4 snapshot 与 recent/waiting frames，Authority/replay 复用正式恢复路径；run02 observer ack=0、hasControlBinding=false，frame 5-8 与 primary 的 hash、完整 world、inputs、events 全相等）
- [x] snapshot schema、config hash 或 sim schema mismatch 时，客户端给出稳定错误码并停止 replay。（验证：聚焦测试分别锁定 `HEADLESS_SNAPSHOT_SCHEMA_VERSION_MISMATCH/snapshot_schema_validation`、`HEADLESS_SNAPSHOT_CONFIG_HASH_MISMATCH/snapshot_config_validation`、`HEADLESS_SIM_SCHEMA_VERSION_MISMATCH/sim_schema_validation`，并断言 replay world、lastAppliedFrame、hash history 被清空；`cargo test lockstep_sim --lib` 114/114 通过）
- [x] 验证项：记录重连和观战的 room、frame、snapshot frame、hash、recovery generation、失败路径和日志。（验证：`logs/lockstep-online/stage7-mybevy-recovery-20260711-02/report.json` 保存 primary/observer 身份、断线前输入/事件、snapshot/response/waiting frame、generation、逐帧实体/输入/事件、连续性和 mismatch 字段，stdout/stderr 同目录归档；status=passed、failure=null）

## 阶段 8：构建、回归和旧入口保护

- 开始时间：2026-07-11 16:26:19 +08:00
- 结束时间：2026-07-11 16:37:02 +08:00
- 开发总结：完成 mybevy、共享 sim-core、game-server lockstep 适配层、lockstep-client、协议同步和 wrapper 的全量构建回归，无需代码修复。新增 `arena.lockstep_sim` 与旧 `arena.robot_sync` 保持独立注册/插件/路由，MyServer 的 `robot_sync_room`、`movement_demo`、`combat_demo`、`lockstep_sim_demo` factory/policy 均保留。外部仓库脚本定位使用 `-ClientRoot` 或 `MYSERVER_CLIENT_ROOT`（Process -> User 回退），正式脚本未写死本机 mybevy 绝对路径；Cargo 依赖使用相对路径并要求 MyServer/mybevy 保持当前相邻目录布局。
- 验证记录：主 agent 与独立 worker 均确认 mybevy fmt、lockstep_sim 114/114、robot_sync 95/95、authority 21/21、myserver 81/81、`cargo check --lib --bin project --bin lockstep-sim-headless` 全部通过；MyServer `npm run check:proto`、sim-core 98/98、game-server lockstep_sim 40/40、lockstep-client 33/33、lockstep-client fmt、wrapper SelfTest 27 项、PowerShell AST 和两仓 diff-check 全部通过。用户级 `MYSERVER_CLIENT_ROOT=C:\project\mybevy` 可解析且 git root 匹配，当前 Process scope 为空时 wrapper 能回退 User scope；mybevy Cargo.toml 以 `../../MyServer/packages/{authority-core,sim-core}` 相对依赖当前相邻仓库。两仓工作树保持干净，无 NATS/game-server/client/cargo 残留，`4222/7000/7500` 无监听，仅原 Redis `6379/PID 4904` 保留。warning 仅既有 mybevy `checkbox` dead_code、game-server build deprecated 和 test target 53 条 unused/dead-code/lifetime warning。

- [x] 运行 mybevy 项目约定的 `cargo test lockstep_sim --lib`。（验证：主 agent 与 worker 复跑均为 114 passed、0 failed）
- [x] 运行 mybevy 项目约定的 `cargo test robot_sync --lib`，确认旧 `arena.robot_sync` 回归未破坏。（验证：主 agent 与 worker 复跑均为 95 passed、0 failed，覆盖场景注册、MyServer join/ready/start、fixed replay、视觉与 HUD）
- [x] 运行 mybevy 项目约定的 `cargo test authority --lib` 和 `cargo test myserver --lib` 或当前等价命令。（验证：authority 21/21、myserver 81/81 通过，覆盖 reconnect/observer snapshot、正式认证和协议编码）
- [x] 运行 mybevy 项目约定的 `cargo check --lib` 或构建命令。（验证：`cargo check --lib --bin project --bin lockstep-sim-headless` 通过，默认 GUI、库和 headless 同时可编译）
- [x] 检查 `MYSERVER_CLIENT_ROOT`、相对 path dependency 和 Windows 本地路径，不写死本机绝对路径为唯一依赖。（验证：用户级 env 为 `C:\project\mybevy` 且 git root 匹配；wrapper 从 Process 回退 User 并支持 `-ClientRoot`；正式脚本无 `C:\project\mybevy` 字面量，Cargo 依赖为 `../../MyServer/...` 相对路径。当前无需补充信息，但非相邻仓库布局需同步调整 Cargo path）
- [x] 检查 MyServer 侧 `npm run check:proto`、相关 Rust check/test 或当前约定命令。（验证：check:proto 通过并实际检查外部 mybevy；sim-core 98/98、game-server lockstep_sim 40/40、lockstep-client 33/33、其 rustfmt 和 wrapper SelfTest 27 项均通过）
- [x] 验证项：构建和回归记录覆盖新增 `arena.lockstep_sim`、旧 `arena.robot_sync`、authority 层和 MyServer 协议层。（验证：scenes.csv/独立插件注册测试同时覆盖两个 arena；`factory_creates_lockstep_sim_demo_without_replacing_old_demos` 明确创建并核对 robot_sync_room、movement_demo、combat_demo、lockstep_sim_demo；完整数量与 warning 记录见本阶段验证记录）

## 阶段 9：失败诊断和日志归档

- 开始时间：2026-07-11 16:37:59 +08:00
- 结束时间：2026-07-11 17:41:49 +08:00
- 开发总结：完成共享帧失败诊断和日志归档闭环。mybevy 在 mismatch 失败记录中按 v1 schema 输出双方 hash、输入、实体和事件；在线侧缺少服务端快照时显式标记 `not_available`。MyServer wrapper 新增 artifact index、统一 triage、常见错误排查索引、敏感值保护和强制 Cargo offline 的 synthetic diagnostic fixture；run-owned game-server/NATS 等服务停止后将日志复制到当前 run 的 `owned-services/`，保留源路径、字节数和归档状态，缺失或复制失败会使 cleanup 失败。Execute、DryRun、默认 GUI 和既有成功报告保持兼容。
- 验证记录：主 agent 独立复跑 mybevy `cargo fmt --all -- --check`、`cargo test lockstep_sim_headless --lib`（11/11）、三目标 `cargo check`，wrapper SelfTest（34/34）、PowerShell AST、`npm run check:proto` 和两仓 `git diff --check`，全部通过，仅保留既有 `checkbox` dead-code warning。fresh fixture `stage9-main-review-20260711-01` 由 `cargo run --offline` 执行，wrapper exit 0 且报告按预期 status=failed/verified=true/networkUsed=false：首 mismatch frame 3，server/client hash=`50d922fa7121f8af`/`1d5dbc2d9d4361cc`，实体 x=`600/601`，input/entity/event diff 均 complete，适用 artifact missing=0。真实 online smoke `stage9-main-online-20260711-01` status=passed，frame/hash=`6/d2a4adc2ef03d53d`；game-server/NATS 的 4 份日志全部归档到 run 目录，源/归档 SHA-256 一致，artifact missing=0。cleanup 的 Redis/registry/process/environment 均通过，临时 key 扫描为 0，PID 文件不存在，`4222/7000/7500` 无监听，仅原 Redis `6379/PID 4904` 保留；9 份归档文件 JWT/ticket 明文形态扫描为 0。

- [x] 定义统一的调试输出目录，保存 MyServer 日志、lockstep-client 报告、mybevy telemetry、截图和命令输出。（验证：`scripts/online-lockstep-reconcile.ps1:683` 生成 artifact index，`:1310` 在 owned PID 停止后归档服务日志；真实 run `stage9-main-online-20260711-01/owned-services/` 保存 game-server/NATS stdout/stderr 共 4 份，索引均 present 且源/副本 SHA-256 一致，客户端 JSONL、命令和 run report 位于同一 run 目录）
- [x] mismatch 时保留首个不一致 frame、server hash、client hash、输入列表、实体差异和事件差异。（验证：`mybevy/project/src/lockstep_sim_headless.rs:172` 挂载 v1 comparison，`:2171` 输出完整 offline 双方快照，wrapper `scripts/online-lockstep-reconcile.ps1:1618` 解析首个 mismatch；fresh fixture frame 3 保存双方 hash、输入、事件和实体 x=`600/601`，三类 diff status 均为 complete）
- [x] 非 hash 失败时记录连接、登录、join、ready、start、reconnect、observer、snapshot parse 或 payload validation 阶段。（验证：`scripts/online-lockstep-reconcile.ps1:803` 将原始 error code/stage 归一到 connect、authentication、room_join、room_ready、room_start、room_reconnect、observer_recovery、snapshot_validation/restore 和 payload_validation；SelfTest 的 failure-stage-classification 覆盖上述类别，34/34 通过）
- [x] 为常见错误码建立排查索引，例如 ticket 失败、policy 不匹配、payload 字段不兼容、config hash mismatch、schema mismatch。（验证：`scripts/online-lockstep-reconcile.ps1:779` 定义 versioned diagnostic index，包含 ticket-auth、policy-mismatch、payload-schema、config-hash、snapshot-schema、sim-schema、hash-mismatch 和 cleanup 等入口；SelfTest 的 diagnostic-index-lookup 通过，fresh report 将 `HEADLESS_HASH_MISMATCH` 关联到 hash-mismatch 建议）
- [x] 验证项：任意失败报告能让后续开发者不重跑全流程也能定位首个可疑 frame 或失败阶段。（验证：强制 offline 的 `stage9-main-review-20260711-01/report.json` 同时包含 artifact、triage、diagnostic index、原始 JSONL 路径、首 mismatch frame/hash 和完整三类 diff；真实 online run 证明服务日志可归档，适用 artifact missing=0，三组互斥参数负例均在创建 artifact/启动服务前稳定拒绝）

## 阶段 10：文档同步和清单归档

- 开始时间：2026-07-11 17:44:39 +08:00
- 结束时间：2026-07-11 18:15:25 +08:00
- 开发总结：完成共享帧调试入口、证据分层、复跑命令、依赖/ownership、精确 cleanup、failure triage 和自动化/人工 smoke 边界的正式文档同步，并将本清单归档到游戏服领域 checklists。文档审核修正了 headless fixture 不产生死亡事件的证据边界；dry-run 验证发现并由独立 worker 修复 visual artifact 适用性，现仅 execute visual-smoke 要求四个视觉文件 present。外部路径继续使用 `-ClientRoot` / `MYSERVER_CLIENT_ROOT`，并明确相邻 Cargo path dependency 限制。
- 验证记录：主 agent 独立复跑 wrapper SelfTest 34/34、PowerShell AST、`npm run check:proto` 和 `git diff --check`；mybevy/lockstep-client plan 分别展开 4/3 个阶段且 writesArtifacts/network=false。fresh dry-run `stage10-main-dry-20260711-01` 四阶段全部 passed、networkUsed=false、artifact missing=0、四个 visual artifact 均 not-applicable，未启动 owned service 或创建 Redis key。13 个关键契约字段、两条正式归档链接、四种 mode switch、两个 Cargo path 和 ticket 赋值扫描均通过；归档文件与本地 summary 最终逐字一致。

- [x] 更新外部客户端接入说明，记录 `arena.lockstep_sim` 调试入口、headless/telemetry 用法和真实联调前置条件。（验证：`docs/协议与客户端/外部客户端接入说明.md:218`-`:316` 区分 offline JSONL、online headless、GUI smoke 与 DiagnosticFixture，并记录 plan/dry-run/execute、端口、ticket 和路径前置条件）
- [x] 更新共享帧同步设计文档，引用本调试验证清单作为统一验收入口。（验证：`docs/游戏服与接入层/共享帧同步移动战斗核心设计.md:73` 更新实现快照，`:1233` 定义当前自动化/人工验收边界，`:1246` 链接正式归档 checklist）
- [x] 更新 `tools/lockstep-client` README 或脚本说明，记录一键 online 对账命令、依赖、清理方式和失败采集路径。（验证：`tools/lockstep-client/README.md:140` 记录 wrapper 模式与 plan，`:230` 记录 mybevy/DiagnosticFixture，`:362` 记录 artifact、triage、owned service 日志和 cleanup）
- [x] 文档中明确哪些项已自动化、哪些项仍需人工 smoke。（验证：外部接入说明 `5.8`、设计文档 `16.8` 和 README `Automated coverage and manual smoke` 均明确 hash/world/input/event/recovery 自动化，以及 HUD、技能/命中、伤害、Buff/DoT、死亡表现人工目检）
- [x] 若本 checklist 完成，按项目约定将完成后的 `summary/` checklist 归档到 `docs/<领域>/checklists/`。（验证：正式归档为 `docs/游戏服与接入层/checklists/共享帧同步调试验证_checklist.md`，最终内容与本地 `summary/共享帧同步调试验证_checklist.md` 逐字一致）
- [x] 验证项：文档里的场景 ID、policy ID、payload 字段、命令和路径与代码及脚本一致。（验证：主 agent 核对 `arena.lockstep_sim`、`lockstep_sim_demo`、`sim_input`、initial/last/observer snapshot、hash/event/input 字段、wrapper ValidateSet、5 个报告 schema 和 Cargo path；13 项契约断言、2 个链接、proto 与 dry-run 均通过）

## 最终完成定义

以下项目作为整体完成标准，不要求每个开发阶段都执行，由所有相关阶段完成后统一验收。

- 开始时间：2026-07-11 18:15:25 +08:00
- 结束时间：2026-07-11 18:15:25 +08:00
- 验收总结：共享帧同步调试验证已完整收口。MyServer 的移动、近战、observer recovery 与 mybevy 的单/双客户端、重连/观战、GUI smoke 均有可复跑命令和结构化证据；失败报告可定位首 mismatch 或非 hash 阶段并归档 run-owned 日志。构建、协议和旧入口回归通过，正式文档与归档清单已同步。最终保留边界是当前证据针对 loopback direct local 调试拓扑，不代表生产入口、跨实例迁移、完整 CSV 技能/Buff 映射或产品化预测/回滚已经完成。

- [x] MyServer 侧可自动复跑移动、近战和 observer recovery online 对账，并输出可归档报告。（验证：阶段 3 最终 run `online-20260710-1904-codex05` 三阶段 passed；阶段 9 wrapper 增加 artifact/triage/owned-service 归档并由真实 run 再验）
- [x] mybevy 侧具备可脚本解析的 `arena.lockstep_sim` telemetry，能验证 local hash、server hash、实体 fixed 坐标和事件序列。（验证：阶段 4 定义 `mybevy.lockstep.telemetry` v1 JSONL；阶段 5-7 online 报告和阶段 9 mismatch comparison 覆盖双方 hash、world、inputs、events）
- [x] 单客户端、双客户端、重连和观战恢复均有明确命令、依赖、结果和日志记录。（验证：阶段 5 run `stage5-mybevy-gui-20260711-08`、阶段 6 `stage6-mybevy-dual-20260711-02`、阶段 7 `stage7-mybevy-recovery-20260711-02` 均 passed，命令与路径已写入 README）
- [x] 至少一次人工 smoke 验收确认移动、技能、伤害/Buff 表现和 HUD 可读性符合调试场景要求。（验证：阶段 5 主 agent 目检 1280x720 在线/离线截图，覆盖移动、HUD、技能/命中/伤害、Buff/DoT 与死亡姿态）
- [x] `arena.robot_sync`、`robot_sync_room`、`movement_demo`、`combat_demo` 等旧入口没有被本轮调试验证改坏。（验证：阶段 8 `robot_sync` 95/95、factory 旧 demo 保护测试、authority/myserver/proto 与全量构建回归均通过）
- [x] 相关文档和旧接入 checklist 均指向本清单作为共享帧同步调试验证的统一完成入口。（验证：三份正式文档指向 `docs/游戏服与接入层/checklists/共享帧同步调试验证_checklist.md`；游戏服/mybevy 旧接入 checklist 已多处指向本统一清单，正式归档与本地 summary 最终一致）
