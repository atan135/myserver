# 批量启动与服务依赖收敛改造 Checklist

## 目标

将 MyServer 应用服务的启动契约改造为“允许无序批量启动、依赖异步发现、在有界时间内等待整体收敛、以 readiness 决定是否接流量”。`game-server`、`match-service`、`game-proxy` 和其他应用服务不得因为启动瞬间缺少另一个应用服务 endpoint 而直接退出或形成重启循环。

本清单同时修复启动失败后遗留 global-ID worker lease 和 Unix socket 的问题，并建立支持新旧实例重叠、滚动替换和独立恢复的实例级身份、注册与 socket 生命周期。基础设施依赖仍可使用健康门禁；应用服务之间不得依赖固定启动顺序。

本清单包含正式 Release 的构建推送、bundle 上传、生产服务器部署、接流量验收、稳定观察和必要时回滚。线上有状态操作只有在前置开发与隔离环境验收全部通过、发布参数与回滚边界明确且用户再次明确授权后才执行；不得把“脚本可用”或“隔离环境演练通过”视为已经完成生产上线。

## 已知问题与当前证据

- production Compose 当前由 `server-apply-release.sh` 批量启动 `game-server match-service ...`，但 `match-service` 又声明依赖 `game-server: service_started`，应用依赖与实际 discovery 收敛方向存在冲突。
- `game-server` 在 match endpoint 尚未注册时会记录 `match-service grpc endpoint not found`；当前代码已有后台 rediscovery，但启动期状态、readiness 和真正 fatal error 的边界不够清晰。
- `game-server::run` 在创建 listener、获取 worker lease 后仍有多个可能提前返回的初始化步骤；错误路径没有统一的异步资源清理出口，可能等待 lease TTL 才能重启。
- 固定 Unix socket 位于共享 volume。异常退出或强制停止后，残留 socket 会使后续实例出现 `address already in use`。
- 固定 service instance ID、worker lease 和 socket 名称不支持新旧 `game-server` 实例安全并存，限制滚动替换。
- migration-runner 等一次性运维容器必须使用 `--no-deps`，避免因环境文件变化连带重建 PostgreSQL、Redis 或 NATS。
- 本机 `local_help.txt` 已登记 WSL 原生发布工作区、生产服务器 SSH 入口和 `ops-status.sh`、`ops-health.sh`、`ops-disk-report.sh`、`ops-deploy.sh`、`ops-rollback.sh` 调用方式；该文件只作为本机运行参数来源，不得把其中的密码、私钥或其他凭据复制到仓库、日志和发布记录。

## 基础原则

- [x] 正式支持应用服务无序批量启动，不把固定启动顺序作为正确性前提。（验证：阶段 6 的 Compose 契约、阶段 7 随机顺序隔离演练和纠正生产 runner 均以应用批量更新及 readiness 收敛通过）
- [x] liveness 只表达进程是否可继续运行；readiness 表达当前实例是否满足接流量条件，两者不得混用。（验证：三服务独立 `/livez`/`/readyz` 契约通过；match lease 预占超过 120 秒时 live=200、ready=503，释放后同容器 ready=200）
- [x] 依赖暂缺、DNS/registry 短时失败和连接重建属于可恢复状态，不直接退出进程。（验证：阶段 3 discovery convergence、阶段 7 依赖迟到矩阵及纠正 match lease 实机恢复均无需进程重启）
- [x] 配置非法、身份/lease 所有权丢失和无法保证唯一性的状态才属于不可恢复错误。（验证：启动配置、registry identity、lease lost 和 socket owner 定向测试已通过，运行期所有权丢失仍保持 fatal 边界）
- [x] 超过启动收敛窗口时保持进程存活并维持 not-ready，由发布系统判定失败、报警或回滚，禁止依靠 restart loop 表达失败。（验证：纠正镜像预占 lease 超过 120 秒后同一 match 容器 running、RestartCount=0、live=200、ready=503）
- [x] 每个实例只注册、续租和清理自己的 registry、worker lease 与 socket，不删除其他活跃实例资源。（验证：阶段 4/5 owner 围栏、阶段 7 双实例/SIGKILL/退役演练及生产 registry 8 instance/8 heartbeat/6 lease 均通过）
- [x] 任何一次性 Compose runner 默认使用 `run --rm --no-deps`，不得隐式重建基础设施或业务依赖。（验证：部署契约 43 passed/5 Linux-only skip；纠正隔离 runner 的 migration initialize/preflight/apply/postflight 与生产 readiness one-shot 均实际使用 `--no-deps` 且无残留）
- [x] 正式发布必须使用 digest lock、校验后的 release bundle 和服务器受控运维脚本；禁止以直接执行单服务 `docker compose pull/up` 代替正式发布流程。（验证：`v0.1.0-e213dc981df5` 使用 schema v2 lock、校验 bundle 和 `ops-deploy.sh` 部署，14/14 digest 最终匹配）
- [x] 构建、推送、上传、部署、开放流量和回滚均属于有状态操作，执行前必须记录目标 release、操作者、发布窗口、当前 release、回滚 release、数据库兼容性和用户授权。（验证：阶段 8-10 记录两次 release 的交付/部署授权；纠正部署前明确目标、当前/回滚 release、零 migration 和生产基线并取得 release-specific 确认）
- [x] `local_help.txt` 及生产 secret 只在本机或服务器受控位置读取；checklist、Git、命令行参数、测试输出和日志不得记录密码、token 或私钥内容。（验证：本机参数只经环境变量/脱敏脚本传递；KCP 报告扫描 JWT、Bearer、token/ticket/password 和完整身份残留均为 0，`local_help.txt` 未提交）

## 阶段 1：启动契约与故障基线

- 开始时间：2026-08-07 18:25:12 +08:00
- 结束时间：2026-08-08 09:32:11 +08:00
- 开发总结：完成三服务当前启动/退出链路盘点，建立无 required 环的依赖分类、共享启动状态机、稳定错误码与安全观测字段，并用 JSON fixture、源码顺序契约和 Rust 行为测试固化四类改造前故障基线。
- 验证记录：Node 基线测试 5/5、global-id lease 行为测试 1/1、game-server socket 注入测试 1/1、match discovery panic 测试 1/1、game-proxy 零路由测试 1/1、service-registry startup contract 测试 4/4 通过；本轮 Rust 文件逐文件 rustfmt check 通过。Windows 不执行 `#[cfg(unix)]` 的真实双 socket 用例，当前由跨平台注入测试覆盖冲突分支，真实 Unix 执行保留后续 WSL/隔离环境验收。

- [x] 盘点 `game-server`、`match-service`、`game-proxy` 的启动步骤、registry 注册、dependency discovery、worker lease、listener、readiness 和 shutdown 顺序。（验证：`docs/后台与运维/应用服务启动契约与故障基线.md` 第 2 节逐服务记录当前控制流与清理缺口）
- [x] 明确必要依赖与可选依赖；必要依赖暂缺只阻止 ready，可选依赖暂缺进入 degraded，不阻止无关业务。（验证：启动契约第 3 节及 `tests/fixtures/startup-convergence-baseline.json` 固化无环依赖图，Node DAG 测试通过）
- [x] 定义统一状态机：`Starting -> WaitingDependencies -> Ready <-> Degraded -> ShuttingDown`。（验证：`packages/service-registry/src/startup_contract.rs` 定义合法转换，`cargo test --manifest-path packages/service-registry/Cargo.toml startup_contract` 4/4 通过）
- [x] 定义稳定错误码和观测字段，至少覆盖 dependency pending/timeout、registry unavailable、lease unavailable/lost、socket conflict 和 startup phase failure。（验证：`StartupErrorCode` 序列化测试和 secret-free 观测字段测试通过，`endpoint` 明确限制为逻辑名称）
- [x] 增加可重复故障 fixture，复现 match 尚未注册、worker lease 未过期、残留两个 Unix socket 和 proxy 找不到 endpoint 的现状。（验证：fixture 覆盖四场景；global-id、socket、proxy 行为测试通过，Unix 真实双 socket 用例已以 `#[cfg(unix)]` 固化）
- [x] 用测试确认当前 HEAD 中首次 match discovery 失败是否真的导致进程退出，并定位退出所对应的实际 `?` 或任务失败，禁止仅按相邻日志推断根因。（验证：`missing_initial_match_discovery_panics_before_client_initialization` 通过，确认退出点是 `MatchClientConfig::from_env` 显式 panic，发生在 listener/socket/lease 取得后）

## 阶段 2：Liveness、Readiness 与依赖状态模型

- 开始时间：2026-08-08 09:34:07 +08:00
- 结束时间：2026-08-08 10:15:00 +08:00
- 开发总结：在 `service-registry` 中建立共享健康状态机，并接入 `game-server`、`game-proxy`、`match-service` 的本地资源、registry 心跳和服务依赖状态；新增独立 `/livez`、`/readyz`、有界收敛/稳定/stale 窗口、结构化安全诊断，以及运行期丢失和恢复路径。production Compose、密钥初始化脚本、示例环境和运维文档同步了默认窗口及 match-service 健康端口。
- 验证记录：`cargo test --manifest-path packages/service-registry/Cargo.toml`（30 个单元测试 + 11 个集成测试）、`cargo test --manifest-path apps/game-proxy/Cargo.toml`（161 个）、`cargo test --manifest-path apps/match-service/Cargo.toml`、注入非敏感测试公钥映射后的 `cargo test --manifest-path apps/game-server/Cargo.toml`（515 个）及 `node --test tests/registry/startup-convergence-baseline.test.mjs`（8 个）均通过；全部本阶段 Rust 改动文件通过 Rust 2024 `rustfmt --check`，`git diff --check` 通过。未启动外部依赖、服务、Docker 或线上操作。

- [x] 为应用服务建立可共享的 dependency state，记录每个依赖的 `Pending / Ready / Degraded / Failed`、最近成功时间、最近错误类型和重试次数。（验证：`packages/service-registry/src/health.rs` 定义 `HealthState`、`DependencySnapshot` 与四态状态机，service-registry 全量测试通过）
- [x] 提供独立 liveness 与 readiness 语义；进程/runtime 正常时 liveness 为成功，只有全部 required dependency 收敛且本地资源可用时 readiness 才成功。（验证：`packages/service-registry/src/readiness.rs` 分离 `/livez` 与 `/readyz`；三个目标服务登记 listener、lease、store、registry 和服务依赖状态，相关全量测试通过）
- [x] readiness 输出结构化未就绪原因，不包含 URL userinfo、token、密码、内部凭据或可复用连接信息。（验证：健康快照仅序列化服务、实例、状态、结构化错误码和时间/计数；`serialized_snapshot_contains_no_connection_or_credential_fields`、`readyz_serializes_only_structured_safe_errors` 通过）
- [x] 引入可配置启动收敛窗口、ready 稳定窗口和 dependency stale 窗口，并给 production 设置有界默认值。（验证：`HealthConfig::try_from_env` 严格校验 120/10/60 秒默认值与上限/窗口关系；`deploy/docker/compose.production.yml`、三个 `.env.example` 和生产密钥初始化脚本已配置）
- [x] 启动收敛窗口超时后保持进程存活和 not-ready，输出一次状态转换与指标，不持续刷相同错误日志。（验证：`HealthState::recompute` 仅首次超时递增 `startup_convergence_timeout_total` 并记录状态转换；超时保持 live/not-ready，`startup_timeout_is_counted_once_and_recovery_still_works` 通过）
- [x] 运行期依赖丢失时从 Ready 转为 Degraded/not-ready，冻结依赖该服务的操作并继续恢复；恢复后经过稳定窗口再回 Ready。（验证：registry 心跳 observer、match rediscovery、proxy route watch 和 match internal 请求反馈均更新健康状态；不可用 proxy 路由被冻结，required 依赖恢复需稳定窗口，`runtime_dependency_loss_is_not_reclassified_as_startup_timeout`、`stale_required_dependency_drops_readiness_until_stable_recovery` 及服务全量测试通过）

## 阶段 3：Registry Discovery 异步收敛

- 开始时间：2026-08-08 10:16:54 +08:00
- 结束时间：2026-08-08 10:56:05 +08:00
- 开发总结：新增共享 discovery convergence runner（立即首轮、250ms 到 10s 有界指数退避、20% jitter、5s 单次超时）和 unhealthy-first registry publication；`game-server`、`game-proxy` 的首次发现与运行期恢复统一进入同一状态机，`match-service` 在传输失败后主动失效 endpoint cache。三个服务只在共享 readiness 稳定后发布 healthy，依赖丢失或 registry 故障时重新发布 unhealthy。
- 验证记录：`service-registry` 40 个单元测试 + 11 个集成测试、`game-proxy` 164 个测试、`match-service` 50 个测试、注入非敏感测试公钥映射后的 `game-server` 515 个测试、Node 启动契约基线 8 个测试均通过；本阶段 Rust 文件全部通过 Rust 2024 `rustfmt --check`，`git diff --check` 通过。曾误聚合运行 `tests/registry` 全目录，54 项中 39 通过、15 项因共享 Redis mock/端口与既有 e2e 启动逻辑冲突失败；该命令不作为验收证据，两个遗留 `node src/server.js` 测试子进程已按精确 PID 清理并确认无残留。未启动 Docker、Compose、远端或线上操作。

- [x] 将初次 discovery 与后续 rediscovery 统一到同一状态机和重试实现，避免启动路径与运行期路径语义漂移。（验证：`packages/service-registry/src/convergence.rs` 提供 `spawn_convergence`；`game-server` match 和 `game-proxy` upstream 均以同一 runner 承载首次及后续尝试）
- [x] 对 registry client 创建失败、endpoint 未找到、连接失败和 endpoint 变化使用有上限的指数退避与 jitter。（验证：`ConvergenceConfig` 默认 250ms 初始、10s 上限、20% jitter、5s timeout；lazy client、发现错误与连接错误统一返回结构化 `ConvergenceAttempt::Retry`，边界与 timeout 单测通过）
- [x] `game-server` 在 match endpoint 暂缺时保持运行并持续发现，不生成不可恢复启动错误。（验证：移除 `require_initial_match_discovery` panic，server 以空 client 启动并标记 Pending；`rediscovery_connects_empty_client_after_endpoint_arrives` 和 game-server 全量测试通过）
- [x] `match-service` 与 `game-server` 可以同时启动，任意一方先注册都能最终建立连接并进入 Ready。（验证：match-service 的 `game-server.internal` 保持 optional；unhealthy-first 发布允许 match-service 先 Ready，随后 game-server 收敛并健康发布，迟到 endpoint 与 cache 切换测试通过）
- [x] `game-proxy` 在没有健康 `proxy-local` endpoint 时保持存活和 not-ready；endpoint 出现、迁移或消失时能够增量更新路由。（验证：前端 listener 先 bind，再启动后台 convergence；失败/空快照冻结旧路由，权威快照增量加入新路由，零路由恢复、迁移、消失测试及 164 个全量测试通过）
- [x] registry instance 只在本地 listener、lease 和 required dependency 满足契约后标记 healthy；启动中的实例不得提前对 proxy 宣称可接流量。（验证：`packages/service-registry/src/publication.rs` 首次强制 `Register(false)`，仅 `HealthState.snapshot().ready` 后发布 healthy，依赖或 registry 丢失后重新 unhealthy-first；发布状态机测试通过）
- [x] 为 endpoint 暂缺、迟到、切换、重复、TTL 过期、registry 短时不可用和恢复增加定向测试。（验证：新增空 client 迟到建连、proxy 零路由恢复、重复 endpoint 确定性、权威快照切换/消失、match cache 主动失效测试；既有 registry heartbeat TTL 过滤、cache TTL 和 outage recovery 测试契约继续保留，四包全量测试通过）

## 阶段 4：启动事务与资源回收

- 开始时间：2026-08-08 10:57:30 +08:00
- 结束时间：2026-08-08 11:39:04 +08:00
- 开发总结：`game-server` 新增启动事务和资源 owner，将配置/外部存储、worker lease、network/local listener、readiness/publication/background task、active run 与统一 cleanup 收敛到一个错误出口。worker lease 支持严格有界、可取消重试；socket 只有在取得 lease 后逐个 bind 并立即登记所有权；正常信号、listener fatal、lease lost 和 bootstrap 错误均执行五步幂等清理，保留原始错误并聚合清理失败。
- 验证记录：`game-server` 全量 527 个测试通过，`startup::tests` 10 个、local socket 定向测试、Node 启动契约基线 9 个及 `service-registry` 40 个单元 + 11 个集成测试通过；本阶段 Rust 文件通过 Rust 2024 `rustfmt --check`，`git diff --check` 通过。两轮主审修复了部分 socket bind 绕过统一 rollback、后台 task panic 静默丢失，以及 database/readiness/shutdown 仅有源码断言而缺少行为故障注入的问题。未启动服务、外部依赖、Docker、Compose 或线上操作。

- [x] 将 `game-server` 启动拆分为 bootstrap、active run 和 shutdown/rollback 三段，所有 bootstrap 失败统一进入资源回收出口。（验证：`apps/game-server/src/server.rs` 将 bootstrap/active 包在单一 `run_result`，生产使用 `startup::run_then_cleanup` 无条件执行 `run_cleanup`；database/readiness/active error 行为测试均执行完整五步清理）
- [x] 在创建或接管 Unix socket 前先取得实例身份与 worker lease，禁止未取得所有权的实例覆盖活跃 socket。（验证：`WorkerLease::acquire_redis` 位于所有 listener bind 之前；`StartupOwnership` 拒绝 lease 前的 listener/socket claim；两个 local socket 逐个 bind 后立即登记 owner，Node 顺序契约和 ownership 单测通过）
- [x] 任意初始化步骤失败时停止 renewal/background task、按 token compare-and-delete 释放自己的 worker lease、注销自己的 registry instance并释放 listener。（验证：`GameServerResources` 记录 task、socket、registry、lease 和五类 store；cleanup 停止任务、移除 owned path、注销配置实例、调用 `WorkerLease::release_redis` compare-and-delete 并关闭 store，各 Redis/DB 操作有 5s 上限）
- [x] 正常停止、SIGTERM 和应用内部 fatal shutdown 都执行同一幂等清理流程；单项清理失败不跳过后续清理。（验证：Ctrl-C/SIGTERM、Notify、listener fatal channel、accept error 与 lease loss 均收敛为 run result；`run_cleanup` 固定执行到 `CloseStores`，task panic 和 lease release failure 测试证明错误聚合且后续步骤继续）
- [x] 为 worker lease 获取增加有界等待与可观测重试，允许前一实例正常退出或 TTL 到期，期间保持启动状态而非快速重启。（验证：`LeaseWaitConfig` 严格校验默认 120s、250ms 到 5s 退避及上限；`wait_for_worker_lease` 以总 deadline 覆盖单次 attempt、支持信号取消并只输出结构化非敏感重试字段，重试成功/timeout/cancel 测试通过）
- [x] 对配置错误、数据库初始化失败、readiness bind 失败、match pending、lease release 失败和 shutdown 中断增加故障注入测试。（验证：`startup::tests` 10 个用例覆盖非法配置、DB/readiness/active 原错误优先、lease 重试/超时/取消、match pending 可恢复、lease cleanup failure 继续关闭 store；server task panic 和 local socket owner 测试补充真实资源路径）

## 阶段 5：实例级 Unix Socket 与滚动替换

- 开始时间：2026-08-08 11:40:44 +08:00
- 结束时间：2026-08-08 12:56:06 +08:00
- 开发总结：完成 game-server 实例级 socket、lease/registry 身份围栏与受控 stale reclaim；drain 会停止新玩家和 proxy-local 接入并将实例降为 unhealthy，显式鉴权 shutdown 请求以三态应答驱动立即退出或有界等待，超时后保护现有会话并允许重试。生产 Compose、环境示例、密钥初始化、协议生成物、兼容基线和滚动替换文档同步更新。
- 验证记录：game-server 536 个测试、game-proxy 164 个测试、match-service 全量测试、global-id Redis feature 10 个测试、Node 定向契约 31 个测试及 proto 六文件 28 个测试通过；shutdown 定向组 8/8、鉴权 blocker handler、drain monitor 和 socket/config 定向测试通过，`git diff --check` 无空白错误。Windows 无法执行 `#[cfg(unix)]` 的真实 Unix socket、SIGKILL、symlink 和 inode replacement 用例，相关 fixture/测试已纳入代码，实际执行保留阶段 7 的授权 WSL/隔离环境验收。

- [x] 将固定 socket 改为包含稳定 instance ID 的实例级路径，例如 `/run/myserver/game-server-<instance-id>.sock`，并限制 basename、目录和长度。（验证：`apps/game-server/src/config.rs` 从 `GAME_SOCKET_ROOT`、`GAME_SOCKET_BASENAME`、`SERVICE_INSTANCE_ID` 派生实例路径并严格校验 ASCII、组件和 Unix path 长度，config/socket 定向测试通过）
- [x] registry endpoint metadata 发布当前实例的精确 socket；proxy 不再依赖单个全局固定路径。（验证：`apps/game-server/src/server.rs` 将当前实例 listener 精确路径写入 `proxy-local.endpoint.socket`，game-proxy 仅消费健康 endpoint metadata；game-proxy 164 个测试通过）
- [x] socket 创建支持受控 stale reclaim：只处理当前实例路径，设置有界 spin/timeout，并验证目标类型与受控目录。（验证：`apps/game-server/src/local_socket.rs` 仅在持有 lease 后检查当前实例目标，只回收 `ConnectionRefused/NotFound` 的 socket，拒绝活跃 socket、symlink、普通文件、目录、超时与未知错误；定向测试通过）
- [x] listener drop 和显式 shutdown 均回收当前实例 socket；SIGKILL fixture 验证后继同实例可安全接管残留路径。（验证：cleanup 以捕获的 `dev+ino` 和 `WorkerLease::owns_redis` 双重围栏删除 owned socket；真实 Unix SIGKILL/替换 inode fixture 已纳入 `#[cfg(unix)]` 测试，Windows 执行留待阶段 7）
- [x] 新旧 game-server 实例可同时运行、分别持有 lease/socket/registry identity，proxy 可在 readiness 通过后加入新实例。（验证：production 配置支持独立 `GAME_SERVER_INSTANCE_ID`/`GAME_SERVER_WORKER_ID`，实例路径、lease 和 registry metadata 均按 identity 隔离，publication 仍由共享 readiness 门禁控制）
- [x] 实现旧实例 drain、停止接新连接、等待有界会话迁移/退出、注销 registry、释放 lease/socket 的滚动替换闭环。（验证：biased watch 停止 TCP/proxy-local 新 accept 并降级 required listener；鉴权 `RequestServerShutdownReq` 返回 `shutdown_armed` 三态，默认 300 秒等待、超时保护和显式重试测试通过，cleanup 统一注销 registry 并释放 lease/socket）
- [x] 验证第二个未获 lease 的实例不能覆盖活跃 socket，任何实例不能清理其他实例路径。（验证：socket 创建要求先取得 worker lease，stale reclaim 仅限精确当前实例路径；清理前执行 Redis compare-only ownership 和 inode identity 校验，lease/socket owner 定向测试通过）

## 阶段 6：Compose 与发布编排改造

- 开始时间：2026-08-08 12:57:43 +08:00
- 结束时间：2026-08-08 13:43:28 +08:00
- 开发总结：production Compose 移除应用层启动顺序，release runner 以一次 `up -d` 批量启动九个应用，并通过 bundle 内共享 probe 连续等待 registry TTL 与 Ready 稳定窗口。发布、回滚、restart 和 replace 复用同一收敛函数；回滚保持前向数据库 schema、独立校验 target/source bundle，并以显式数据库兼容授权防止不安全自动回滚。所有 Compose one-shot runner 收敛为 `run --rm --no-deps`，文档与本机命令示例同步更新。
- 验证记录：部署静态契约 18/18、startup baseline 11/11、discovery scan 19/19 通过；Windows Git Bash `bash -n scripts/docker/*.sh deploy/docker/scripts/*.sh`、`node --check scripts/docker/release-readiness-probe.mjs` 和 `git diff --check` 通过。主审第 1 轮修复了 restart/replace 在无效收敛窗口下先变更容器，以及 rollback readiness source 未重新校验完整 bundle 的问题。未运行 Docker、Compose、服务或线上操作，真实 CLI/runtime 联调保留阶段 7。

- [x] 移除 `game-server`、`match-service`、`game-proxy` 之间用于表达固定应用启动顺序的 `depends_on`；只保留数据库、Redis、NATS 等基础设施健康门禁。（验证：`deploy/docker/compose.production.yml` 删除 game-server/game-proxy/auth-http 等应用依赖，只保留 PostgreSQL、Redis、NATS 与 socket 初始化门禁；静态测试逐服务解析依赖并通过）
- [x] `server-apply-release.sh` 支持应用服务单次批量 `up -d`，随后统一等待 required readiness、registry TTL 和稳定窗口。（验证：runner 单条命令启动九个应用，随后调用 `wait_for_release_readiness`；共享 helper 默认 180 秒总窗口、30 秒 registry TTL、10 秒 Ready 稳定窗口）
- [x] 发布成功以全部 required service ready 且连续稳定为准，不以容器 running、启动顺序或一次 discovery 成功为准。（验证：`release-readiness-probe.mjs` 并发校验 required Node health 与四个 Rust `/readyz` 的 service/instance/payload，helper 仅在全体连续成功 40 秒后返回）
- [x] 发布超时时输出未收敛服务、dependency state、错误码和实例 ID，并执行已有版本回滚；不得通过人工删除未知 registry key/socket 继续发布。（验证：probe 仅输出白名单安全字段，超时记录 `READINESS_CONVERGENCE_TIMEOUT` 并单次调用上一 release；脚本和文档均无删除 registry/socket 绕过动作）
- [x] migration preflight/apply/postflight 及其他一次性 runner 统一使用 `run --rm --no-deps`，增加静态测试阻止回归。（验证：`tests/deploy/release-orchestration-contract.test.mjs` 扫描 `scripts/docker/*.sh` 的 Compose run 命令并要求连续 `run --rm --no-deps`，部署测试 18/18 通过）
- [x] 发布、回滚和单服务替换复用同一 readiness 收敛函数，不维护三套不同启动顺序。（验证：release runner、`ops-restart.sh`、新增 `ops-replace.sh` 和 `ops-rollback.sh` 均复用 `readiness-convergence.sh`；restart/replace 在有状态命令前校验窗口，rollback 独立校验 target/source identity 与完整 SHA256SUMS）
- [x] 更新 Compose/发布脚本测试，断言批量启动、无应用层顺序依赖、runner `--no-deps` 和有界收敛门禁。（验证：新增 `release-orchestration-contract.test.mjs` 覆盖依赖解析、单批启动、调用顺序、TTL/稳定窗口、安全诊断、回滚围栏与 instance ID 脱敏提取，相关部署测试 18/18 通过）

## 阶段 7：集成故障演练与服务端验收

- 开始时间：2026-08-08 14:39:40 +08:00
- 结束时间：2026-08-09 00:47:05 +08:00
- 开发总结：在 WSL 原生工作区以独立 `myserver-phase7-b54acdd-b` Compose project 完成无序批量启动、依赖迟到/超时、实例级资源故障、双实例滚动替换、安全退役、批量发布失败及自动/手工回滚演练。运行期发现并修复 drain 后 admin endpoint 健康投影阻断精确停服的问题，补齐操作锁、恢复 journal 与 shutdown-only live registry discovery；最终显式 migration postflight、服务端集成测试和精确清理均通过，未访问或修改生产环境。
- 验证记录：Linux 运维/故障 fixture 34/34，game-server 536、game-proxy 164、match-service 与 global-id Redis feature 测试通过；Windows 部署/数据库/启动组合 36 passed、1 个需显式本机 PostgreSQL 的既有 drill skipped，admin-api 定向 56/56、registry 31/31、启动基线 12/12，完整服务端集成 19/19。五库 production preflight/apply/postflight 与 drift 检查通过；隔离环境最终清除全部 project 容器、网络、volume、镜像及临时 secret/release 数据。

- [x] 在隔离 Compose 环境随机化应用服务启动延迟和顺序，多轮验证最终全部 Ready 且无 restart loop。（验证：`myserver-phase7-b54acdd-b` 中九个应用经多轮批量启动最终连续 Ready；除故障演练显式触发的旧 game-server RestartCount=1 外无意外重启，候选与三项基础设施 RestartCount 均为 0）
- [x] 分别延迟 match、game、proxy、registry、Redis 和 NATS，验证依赖在收敛窗口内出现时自动恢复。（验证：隔离延迟矩阵逐项恢复后无需按应用顺序启动；game-server 日志记录 match endpoint 从 missing 到 discovered 并在 27.6 秒转 Ready，proxy 随 registry 恢复收敛，Redis/NATS 恢复后依赖服务自动 Ready）
- [x] 让依赖超过收敛窗口，验证服务保持存活/not-ready、发布门禁失败且诊断信息完整；依赖恢复后无需重启即可 Ready。（验证：synthetic failed bundle 将 match Redis/registry 保持不可达 77 秒，诊断明确输出 match `READINESS_UNREACHABLE`、game/proxy `DEPENDENCY_PENDING` 和 `READINESS_CONVERGENCE_TIMEOUT`；恢复 previous bundle 后 readiness 连续稳定 7 秒并自动回滚收敛）
- [x] 演练 SIGTERM、SIGKILL、OOM/强制停止、stale lease、残留 socket 和 registry TTL 过期，确认实例只回收自己的资源。（验证：Linux 34/34 覆盖真实 flock、SIGKILL journal 恢复及 timeout/nonzero/OOM/identity drift 退役恢复；Compose 故障矩阵覆盖 SIGTERM/强停、stale lease/socket/TTL，旧 worker 5 lease/socket/registry 清除时候选 worker 7 的 lease、双 socket 和 heartbeat 均保留）
- [x] 执行新旧 game-server 滚动替换，验证 proxy 在新实例 Ready 前不路由，切换后旧实例 drain 并完整清理。（验证：`game-server-1` 与候选 `game-server-2` 独立持有 worker 5/7；双人审批 drain 后新会话拒绝、既有 reconnect 通过，候选 Ready 后 proxy endpoint_count 连续为 1；精确 shutdown 返回 `ok=true/shutdown_armed=true` 且三类 blocker=0，旧实例 exit 0、非 OOM、policy=no、journal/lease/socket/registry 全部清除）
- [x] 执行批量发布失败和回滚，确认 PostgreSQL/Redis/NATS 容器 ID、RestartCount 和 volume 不因 runner 或环境文件变化被意外重建。（验证：checksummed synthetic bundle 的 required match/registry 故障触发发布超时并自动回滚，`ops-rollback` 手工回滚也连续稳定 7 秒；PostgreSQL `9c147c...`、Redis `afa42c...`、NATS `eac072...` 的完整容器 ID、RestartCount=0 与三个 data volume 名在前后完全一致）
- [x] 运行相关 Rust/Node 测试、部署静态测试、Compose config、migration preflight/postflight 和完整服务端集成测试。（验证：Rust game-server 536、game-proxy 164 及相关服务测试通过；部署/数据库/启动组合 36 passed/0 failed/1 gated skip，Compose config 与 bundle SHA256 通过，五库 production postflight `ok=true`/drift clean，`npm run test:integration` 19/19）
- [x] 更新服务发现、健康检查、发布、回滚、故障恢复和滚动替换文档，并记录生产启用与回滚条件。（验证：提交 `565dc04`、`ee06075`、`74812e3` 同步更新 OPS、正式 Release 上线说明、空房接管灰度规范及安全退役/控制口发现边界；文档明确 registry-only、前向 migration 兼容授权、break-glass 双人审批和生产启用/回滚门禁）

## 阶段 8：正式发布准入与生产只读基线

- 开始时间：2026-08-09 07:33:54 +08:00
- 结束时间：2026-08-09 08:47:57 +08:00
- 开发总结：已完成目标源码、Windows/WSL 发布工作区和生产只读基线核验；目标源码 revision 为 `c09447f703cfc028881bb6d75f4f0a2dd729ca8f`，release ID 为 `v0.1.0-c09447f703cf`，部署前生产及直接回滚候选为 `v0.1.0-fef5c9b49d36`。2026-08-09 先取得 Phase 9 构建、推送和 bundle 上传授权并完成交付，08:47:57 再取得 Phase 10 最终部署授权；发布前技术准入和授权门禁结论为 GO，后续实际部署结果单独记录于阶段 10。
- 验证记录：Windows 与 WSL 原生 ext4 checkout 均为干净 `main@c09447f`；生产只读 `ops-status/health/disk-report`、bundle SHA256、容器 inspect、schema v2 lock、ACR manifest、DNS/TLS、Docker data-root、监听端口、UFW 与 Redis registry TTL 检查通过。13 个生产容器 RestartCount 全为 0，14/14 Compose 镜像匹配 lock；根盘使用 46%，`/data` 为 59 GB 且使用 1%，Docker root 为 `/data/myserver/docker`。

- [x] 确认阶段 1 至阶段 7 全部完成，相关代码、配置、协议、迁移和文档已在 Windows 工作区完成审核与提交；未完成项不得以生产环境验证代替。（验证：阶段 1-7 全部有逐项证明与结束记录；Windows `main@c09447f` 工作树干净，阶段 7 隔离验收和完整服务端集成均在生产只读检查前完成）
- [x] 明确本次目标 Git commit、release ID、发布操作者、发布窗口、预期公网入口、当前生产 release 和候选回滚 release，并取得执行构建、推送、上传及生产部署的明确授权。（验证：目标 `c09447f703c...`、`v0.1.0-c09447f703cf`、操作者 `gameops`、公网 API/后台/聊天域名及 `4000/UDP` 已核对；部署前与直接回滚候选为 `v0.1.0-fef5c9b49d36`；Phase 9 交付授权和 2026-08-09 08:47:57 +08:00 Phase 10 最终部署授权均已取得）
- [x] 按 `local_help.txt` 的 `MYSERVER_WSL_DISTRO` 与 `MYSERVER_WSL_PROJECT_ROOT` 定位 WSL 原生工作区，确认其工作树干净、当前分支正确，且 `HEAD` 与准备发布的 Windows Git commit 完全一致；禁止从 `/mnt/h/project/MyServer` 构建或发布。（验证：WSL checkout filesystem 为原生 ext2/ext3，`main@c09447f703cfc028881bb6d75f4f0a2dd729ca8f` 且 worktree clean，与 Windows HEAD 完全一致）
- [x] 审阅本次五库 migration、配置和运行期资产差异；记录 migration 可逆性、数据库前后兼容窗口、备份 artifact ID/checksum 以及允许回滚的最旧 release。不可逆或 contract migration 不得声明可自动回滚 schema。（验证：`fef5c9b49d36..c09447f` 在 `db/migrations` 与 `db/config` 零差异，五库 schema 不变、无需 schema 回滚，backup artifact ID/checksum 记为 N/A；前后应用共享当前 schema，允许直接回滚到 `v0.1.0-fef5c9b49d36`，更早 release 未纳入本次兼容承诺）
- [x] 使用 `local_help.txt` 登记的只读 SSH 入口执行 `ops-status.sh`、`ops-health.sh` 和 `ops-disk-report.sh`，记录发布前容器状态、运行镜像、RestartCount、基础设施容器 ID、磁盘余量、当前 release 及异常告警；此阶段不得重启、部署或修改服务器。（验证：三个脚本通过；当前 `v0.1.0-fef5c9b49d36` bundle SHA256 校验通过，13 容器 running 且 RestartCount=0，PostgreSQL/Redis/NATS 完整 ID 与 volume 已记录，根盘 46%、`/data` 1%，无异常告警或状态修改）
- [x] 核验生产 DNS、TLS/Caddy 域名、ACR 只读拉取能力、Docker data-root、端口与防火墙策略、Redis registry 和备份可恢复性满足《正式 Release 上线说明》的准入条件。（验证：API/后台/聊天 DNS 均匹配登记服务器且 TLS 有效期大于 7 天，HTTP 308、聊天 HTTP/1.1 426 符合边缘契约；ACR digest manifest 可读，14/14 运行/runner 镜像匹配 schema v2 lock；Docker root=`/data/myserver/docker`，`/data` 59 GB，UFW 放行 80/TCP、443/TCP、4000/UDP；registry 8 instance/8 heartbeat 全部 healthy 且 TTL>0、6 worker lease TTL>0，无 legacy/direct fallback；本次无 migration，备份证据 N/A）
- [x] 形成 go/no-go 记录；任一准入项失败时停止发布，不通过人工删 registry key、socket、volume、迁移历史或 secret 绕过门禁。（验证：Phase 9 交付前记录 GO for delivery / NO-GO for deploy；取得 Phase 10 最终授权且部署前基线复核无漂移后更新为 GO for deploy；未通过删除 registry key、socket、volume、迁移历史或 secret 绕过门禁，实际部署失败另记阶段 10）

## 阶段 9：正式镜像、Release Lock 与 Bundle 交付

- 开始时间：2026-08-09 07:46:59 +08:00
- 结束时间：2026-08-09 08:11:37 +08:00
- 开发总结：从干净的 WSL 原生工作区发布 `v0.1.0-c09447f703cf`，推送 11 个 `linux/amd64` 应用镜像及 schema v2 digest lock；release lock 提交 `1670cdea18ba` 仅修改 `deploy/docker/images.lock.json`，Windows、WSL 与 `origin/main` 已统一。随后从受控本地与生产配置读取参数，上传并校验 bundle，事务安装 10 个运维脚本和 release runner；未执行部署、Compose、migration、容器重启或 `current` 切换。
- 验证记录：严格 lock 断言通过（目标 revision `c09447f703cf...`、11 应用、3 基础设施、`linux/amd64`、全部 immutable digest/reference）；registry 查询确认 11/11 SBOM、11/11 provenance 非空且 revision 11/11 匹配。服务器 bundle `SHA256SUMS`、`RELEASE`、Compose、migration、CSV、scene、lock、runner 和 10/10 ops 脚本一致；安装前缺少 `/data/myserver/run`，按运维安装所需以部署用户所有、`0700` 权限一次性补建后事务恢复成功，pending journal 已清理。上传前后 `current=v0.1.0-fef5c9b49d36`、13 个容器状态指纹及 secrets 元数据指纹均未变化。

- [x] 在已获授权且干净的 WSL 原生工作区运行 `scripts/docker/publish-release.sh`，构建并推送正式 `linux/amd64` 应用镜像，生成并校验 schema v2 `deploy/docker/images.lock.json`；禁止使用 `-docker-test-` 镜像或可变 tag 作为生产依据。（验证：发布脚本返回 `release_id=v0.1.0-c09447f703cf`、`revision=c09447f703cfc028881bb6d75f4f0a2dd729ca8f`，11 个正式应用镜像全部 build/push 成功并生成 lock）
- [x] 核对 `images.lock.json` 的 release ID、Git revision、11 个应用镜像、3 个基础设施镜像、平台和全部 digest，并保存 SBOM/provenance 与镜像推送证据。（验证：额外 Node 严格断言精确 11+3 集合、schema v2、`linux/amd64`、dirty=false 及全部 digest/reference；构建日志含 11 次 SBOM 和 22 条 attestation export 事件，registry `imagetools inspect` 确认 SBOM/provenance/revision 均 11/11）
- [x] 确认发布脚本只提交并推送预期的 `images.lock.json` 发布记录；随后将 Windows 工作区 fast-forward 到该 release 提交，确认 Windows 与 WSL 不分叉后再继续。（验证：`1670cdea18ba` 父提交精确为目标源码 commit，`git diff-tree` 唯一文件为 `deploy/docker/images.lock.json`；Windows fast-forward 后与 WSL、`origin/main` 同 HEAD、均 clean、divergence=0/0）
- [x] 从 `local_help.txt` 读取服务器 host、port、user 和 WSL 原生 SSH identity，并从经确认的生产配置取得 Caddy/API/后台/聊天/游戏域名与证书邮箱参数；通过 `scripts/docker/upload-release-bundle.sh` 创建、上传并在服务器校验 bundle，不得把 SSH 私钥、密码或服务器 secret 写入命令输出、文件或 Git。（验证：关闭 shell tracing，以 `WSLENV` 和捕获变量传递本地/远端参数且未回显值；上传脚本返回目标 release 路径并完成服务器 `SHA256SUMS` 校验）
- [x] 核对服务器 `/data/myserver/release/<release-id>/` 中的 `RELEASE`、`SHA256SUMS`、Compose、迁移、CSV、scene 和 `images.lock.json` 完整一致，并确认 `/data/myserver/apply-release.sh` 来自本次已验证 runner。（验证：目标目录全量 checksum 通过，RELEASE 三字段匹配，所列资产存在，本地/远端 lock hash 相同，runner `cmp` 相同，10/10 ops 脚本逐文件 `cmp` 相同且安装 journal 已清理）
- [x] bundle 上传完成后只做只读核验，不提前更新 `current` 软链、不直接运行 Compose，也不覆盖 `/data/myserver/secrets/`。（验证：上传前后 `current` 均为 `v0.1.0-fef5c9b49d36`，13 个容器 ID/RestartCount/image/status 聚合指纹不变，secrets 文件元数据聚合指纹不变；未执行 runner 输出的部署命令）

## 阶段 10：生产部署、接流量与稳定观察

- 开始时间：2026-08-09 08:47:57 +08:00
- 纠正发布开始时间：2026-08-09 10:39:11 +08:00
- 纠正发布目标：修复应用 release 重建 PostgreSQL/Redis/NATS 以及 `match-service` worker lease 首次冲突触发 restart loop 的生产缺陷；完成新 release 后使用已授权的 disposable guest/角色执行真实 KCP smoke。
- 纠正版本隔离验收时间：2026-08-09 11:51:00 +08:00
- 纠正 Release 交付时间：2026-08-09 17:18:23 +08:00
- 纠正 Release：`v0.1.0-e213dc981df5`；源码 revision：`e213dc981df5f2bf1e678c90e00f82c9c68c9330`；lock commit：`8ee01dfe9b199d4e1e2151518db5078327eaa23f`
- 结束时间：2026-08-10 00:10:51 +08:00
- 开发总结：首次按最终授权调用受控 `ops-deploy.sh` 时，本地 SSH 包装约 5 秒后以 124 超时并丢失远端 stdout/退出码；远端 runner 继续约 293 秒后退出，停在“9 个业务应用为目标 digest、Caddy 与 `current` 仍为旧 release”的部分发布态，并意外重建 PostgreSQL/Redis/NATS，`match-service` 因 worker lease 冲突重启 9 次。取得独立重跑授权后，以完整长连接、原始退出码和脱敏日志保全重跑同一受控命令，runner exit 0，完成五库门禁、40 秒 readiness、postflight、Caddy 更新和 `current` 切换；未触发自动回滚，未手工 Compose、切软链或修复数据。随后以 `d693d40`、`e213dc9` 修复 match lease 等待和发布基础设施围栏，发布纠正 Release `v0.1.0-e213dc981df5`。纠正生产部署完成五库、readiness、Caddy 与 `current` 切换，PostgreSQL/Redis/NATS 身份、volume、RestartCount 全程不变，所有应用 RestartCount=0；真实生产 KCP guest/角色会话完成鉴权和 Pong，316 秒/6 轮稳定观察通过。首次发布失败历史保留为发布事故证据，最终成功结论以纠正 Release 为准。
- 验证记录：恢复日志保存于 WSL `/tmp/myserver-phase10-recovery.log`，127 行敏感信息扫描为 0，原始 exit 0。当前 13 个常驻容器 running，14/14 Compose 镜像引用匹配 schema v2 lock；五库 history 为 5/2/2/1/1、failed/pending=0、drift/关键表/postflight 通过。required readiness 8/8；registry 8 instance/8 heartbeat/6 worker lease，stale/invalid/orphan=0。独立稳定观察 329 秒/6 轮均为 8/8 ready、13 running、restart delta=0、关键异常日志 0；二次执行基础设施 ID/volume/RestartCount 不变，`match-service` RestartCount 保持 9。公网 API/后台 200、聊天 426，TLS 剩余 79/79/84 天，4000/UDP 双栈映射与 UFW 规则存在，根盘 47%、`/data` 1%。尚未执行真实带角色 ticket 的 KCP 玩家协议会话；现有 mock-client 自动建 guest/角色会写生产数据，需另行授权或提供受控 smoke 账号。
- 纠正版本隔离验收记录：Windows、WSL 原生工作区与 `origin/main` 均为干净 `e213dc981df5`。唯一 Compose project `myserver-corrective-e213dc9-drill` 中真实 `server-apply-release.sh` 最终 exit 0，三项基础设施门禁、五库 preflight/apply/postflight、40 秒 readiness 稳定窗口、Caddy 与隔离 `current` 切换全部通过；应用和 Caddy 实际使用 `up -d --no-deps`，migration initialize/preflight/apply/postflight one-shot 实际使用 `run --rm --no-deps`。PostgreSQL、Redis、NATS 的 container ID、volume name、RestartCount 在 runner 前后及 lease 专项后完全一致且 RestartCount 均为 0。预占 worker `777/6` 超过 120 秒后，纠正镜像的同一 match 容器保持 running、RestartCount=0、`/livez=200`、`/readyz=503`；owner 校验后释放测试 lease，11.086 秒内同容器恢复 `/readyz=200`、`ready=true`、required pending=0，未发生重启。恢复后总体 `state=degraded` 来自仍 Pending 的 optional `game-server.internal`，符合共享健康模型“optional degraded 不阻塞 readiness”的既有契约，不属于 lease 恢复残留。演练结束后精确 project 的容器、网络、volume、本地测试镜像及临时 bundle/secrets/ops-state/log 全部清零，WSL worktree 仍干净；未访问生产。
- 纠正 Release 交付记录：WSL DNS stub 首次在 Git 同步前短时解析失败，未产生构建、推送或工作区改动；仅重启本地 WSL `systemd-resolved` 后重跑同一已授权发布脚本，原始 exit 0。11 个 `linux/amd64` 应用镜像全部构建推送并生成 SBOM/provenance attestation，schema v2 lock 严格验证 11 个应用、3 个基础设施、目标 revision、平台、clean source 和全部 immutable digest/reference。lock commit `8ee01df` 的父提交精确为 `e213dc9` 且唯一修改 `deploy/docker/images.lock.json`；Windows、WSL 与 `origin/main` 已 fast-forward 到同一 commit、divergence=0/0。bundle 已上传到受控 release 目录并通过服务器 `SHA256SUMS`，10 个 ops 脚本与 runner 事务安装成功；`current` 仍为 `v0.1.0-c09447f703cf`，未运行 Compose、migration 或部署。发布前只读基线为 13 个容器 running，PostgreSQL/Redis/NATS healthy、RestartCount=0 且 container ID/volume 已记录，旧 match 历史 RestartCount=9，pending ops/retire journal 均不存在；目标与当前 release 在 `db/migrations`、`db/config` 零差异，backup artifact/checksum=N/A，直接回滚候选为当前 `v0.1.0-c09447f703cf`。纠正生产部署等待 release-specific 最终授权。
- 纠正生产验收记录：用户在展示目标 `v0.1.0-e213dc981df5`、当前/回滚 `v0.1.0-c09447f703cf`、零 migration、backup N/A 和基础设施基线后明确回复“确认部署”。受控 runner 完成 14 镜像 pull、三项基础设施 fail-closed gate、五库 preflight/apply/postflight、40 秒 readiness、Caddy 更新及 `current` 切换；本地 `tee` 包装器因变量转义错误未保留可靠的 SSH 原始退出码，该证据缺口不做隐瞒，未据此盲目重跑。runner 终态标记、目标 `current`、13 个常驻容器、五库 JSON 和后续独立核验共同确认部署成功。14/14 lock/Compose/容器 digest 匹配；PostgreSQL、Redis、NATS 的完整 container ID、volume 和 RestartCount=0 与发布前精确一致；全部新应用和 Caddy RestartCount=0。required readiness 8/8，registry 8 instance/8 heartbeat/6 worker lease，最小 TTL 20 秒，关键日志 0，migration one-shot 残留 0，pending ops/retire journal 均不存在。公网 API/后台 200、聊天 426、TLS 校验通过，服务器仅监听并双栈映射 `4000/UDP`、无 `4000/TCP` listener。mybevy Release 客户端以强制 KCP 在 3.5 秒内完成 guest login 201、角色列表 200、角色创建 201、角色选择 200、descriptor `Kcp:4000/UDP`、KCP connected、`AuthReq/AuthRes ok` 和 Pong，正常 exit 0；授权的一次性 guest/角色保留，脱敏指纹 `e1859fbc266f`，凭据扫描 0。最终稳定观察 316 秒/6 轮均为 13 running、8/8 ready、restart_nonzero=0、基础设施身份不变、关键日志 0。

- [x] 部署前再次展示目标 release、当前 release、回滚 release、migration/备份结论和发布前基线，取得执行生产部署的最终明确授权。（验证：首次 release 展示 `c09447f/fef5c9b` 并取得“确认执行”；纠正 release 展示目标 `v0.1.0-e213dc981df5`、当前/回滚 `v0.1.0-c09447f703cf`、零 migration、backup N/A 和 13 容器基线后，用户明确回复“确认部署”）
- [x] 使用 `local_help.txt` 登记的 SSH 与确认参数调用 `/home/gameops/script/ops-deploy.sh --release-id <release-id> --confirm <release-id> --rollback-db-compatible`；该参数只能在已确认上一应用 release 兼容本次前向 migration 后使用；禁止绕过 `ops-deploy.sh` 直接执行单服务 `docker compose pull/up` 或手工替换 `current` 软链。（验证：首次 release 的恢复重跑原始 exit 0；纠正 release 仅调用同一受控命令，未直接执行生产 Compose、手工切软链、删除 lease/socket 或修改数据，最终 `current` 为目标 release）
- [x] 监督 runner 完成 bundle/Compose 校验、digest pull、基础设施健康门禁、migration preflight/apply、应用批量更新、required readiness postflight、Caddy 更新和 `current` 切换；任一步失败立即停止后续接流量动作并保留原始退出码与诊断输出。（验证：纠正 runner 输出包含 14 镜像 pull、三项 infra gate、五库 preflight/apply/postflight、`readiness_converged stable_seconds=40`、Caddy recreate 和目标 `current`；本地包装器未保留可靠 SSH 原始退出码的证据缺口已明确记录，随后以独立终态核验确认成功且未盲目重跑）
- [x] 验证 required service 在有界窗口内连续 Ready，registry endpoint/heartbeat 与实例 ID 正确，无 legacy direct fallback、restart loop、stale lease、socket conflict 或持续重复错误日志。（验证：纠正生产 316 秒/6 轮均 8/8 Ready、全服务 RestartCount=0、关键日志 0；registry 8 instance/8 heartbeat/6 lease 且最小 TTL 20 秒，正式入口仅 registry/KCP，无 legacy direct fallback）
- [x] 核对实际运行镜像 digest 与 `images.lock.json` 完全一致，PostgreSQL/Redis/NATS 容器 ID、RestartCount 和 volume 未被意外重建，migration history、drift 和关键表 postflight 全部通过。（验证：纠正生产 14/14 digest、13/13 常驻容器实际 image ID 匹配；三基础设施完整 ID、三个 volume 和 RestartCount=0 与发布前精确一致；五库 history/failed/pending/drift/关键表 postflight 全部通过）
- [x] 验证 Caddy 证书和 API、后台、聊天及游戏入口端到端请求；仅在 postflight 与 readiness 全部通过后维持或开放 `80/TCP`、`443/TCP` 和 `4000/UDP` 流量。（验证：API 200、后台 200、聊天 426、TLS 校验通过，服务器 `4000/UDP` 双栈监听且无 `4000/TCP` listener；mybevy Release 强制 KCP 完成 guest/角色/ticket、AuthReq/AuthRes ok 和 Pong，exit 0）
- [x] 在约定稳定观察窗口内持续执行只读 status、health、disk 和关键服务日志检查，记录错误率、重启、依赖状态、registry TTL、资源使用及玩家主链路结果；达到门槛后才宣布发布成功。（验证：纠正发布观察 316 秒/6 轮逐轮 13 running、8/8 ready、restart_nonzero=0、infra identity stable、critical logs=0；registry TTL、HTTP/TLS 与真实 KCP 主链路均通过）

## 阶段 11：生产回滚处置与发布留痕

- 开始时间：2026-08-10 00:10:51 +08:00
- 结束时间：2026-08-10 00:13:46 +08:00
- 开发总结：纠正 Release `v0.1.0-e213dc981df5` 的 digest、数据库、readiness、registry、基础设施、Caddy/TLS、公网 HTTP、真实 KCP 和稳定观察门禁全部通过，未满足任何回滚触发条件，因此明确判定“不需要回滚”。保留 `v0.1.0-c09447f703cf` 作为当前 schema 兼容的直接回滚候选，但未取得也未使用回滚授权，未调用 `ops-rollback.sh`。首次 release 的基础设施重建、match restart loop 和 SSH 包装超时，以及纠正部署本地日志包装器未保留可靠原始 SSH exit 的证据缺口均保留在阶段 10，不从发布历史中删除。
- 验证记录：目标 release、源码 revision、lock commit、schema v2 lock、bundle `SHA256SUMS` manifest hash `c437f99bb58788c80ddf552eec7a197d84dad7d3fe10a1a726c16140457c8ad3`、五库结果、backup N/A、发布前后三基础设施基线、14/14 digest、8/8 readiness、registry 8/8/6、316 秒观察和 KCP 脱敏报告均已记录。生产只公开 Caddy 80/443 TCP 与 game-proxy 4000/UDP；KCP smoke 产物位于 ignored `logs/prod-kcp-smoke-20260809-235727/`，凭据扫描为 0。Windows、WSL 与 `origin/main` 在归档前均为干净 `8ee01df`、divergence=0/0。

- [x] 为部署失败、readiness 超时、错误率超标、依赖无法收敛、迁移异常和基础设施异常分别记录停止发布、隔离流量、继续观察或回滚的判定结果；未触发回滚时明确记录“不需要回滚”及依据。（验证：纠正发布无 runner failure marker、8/8 readiness、错误日志 0、五库无 failed/pending/drift、三基础设施零变化，316 秒稳定且 KCP 通过，判定不需要回滚；首次发布异常已单独保留）
- [x] 仅在目标旧 release 与当前数据库 schema 明确兼容且再次取得回滚授权后，调用 `/home/gameops/script/ops-rollback.sh --release-id <previous-release-id> --confirm <previous-release-id> --db-compatible`；数据库不兼容时禁止添加 `--db-compatible` 并停止自动回滚。（验证：候选 `v0.1.0-c09447f703cf` 与目标零 migration、schema 兼容，但因未触发回滚且未取得回滚授权，未调用 `ops-rollback.sh` 或添加 `--db-compatible`）
- [x] 回滚后重新验证 digest、required readiness、registry、Caddy/公网入口、数据库 postflight、基础设施容器 ID/RestartCount 和稳定观察窗口，确认未删除 volume、secret、registry 活跃实例或其他实例 socket。（验证：本次未触发回滚，因此回滚后验证不适用；前向纠正发布已完成同等完整终态验证，旧 release 仍保留为可用候选）
- [x] 若无法安全回滚，保持故障服务不接流量，保存现场与日志，按数据库恢复和生产事故流程升级处理；禁止手工修改 `_sqlx_migrations` 或执行未审阅的数据修复。（验证：未进入无法安全回滚分支；全过程未手工修改 `_sqlx_migrations`、数据库数据、secret、registry key、volume 或 socket）
- [x] 保存 release ID、代码与 lock commit、`images.lock.json`、bundle SHA256、迁移 JSON、备份证据、发布/回滚命令结果、生产前后基线、实际开放端口、操作者、授权记录和观察结论；敏感值必须脱敏或只保存 secret 引用。（验证：阶段 9-11 与本清单记录目标三元组、bundle manifest hash、五库 JSON 结论、backup N/A、基础设施 ID/volume/restart 基线、端口、授权、no-rollback 和观察结果；KCP 报告凭据扫描 0）
- [x] 线上发布成功或回滚稳定后更新发布、回滚和故障记录；全部 checklist 完成后按仓库约定将本文件归档到对应 `docs/<领域>/checklists/` 目录。（验证：生产纠正发布与 no-rollback 留痕已完成，本文件已从 `summary/` 归档到 `docs/后台与运维/checklists/批量启动与服务依赖收敛改造_checklist.md`）

## 最终完成定义

以下项目作为整体完成标准，不要求每个开发阶段都执行，由所有阶段完成后统一验收。

- 开始时间：2026-08-10 00:13:46 +08:00
- 结束时间：2026-08-10 00:13:46 +08:00
- 验收总结：阶段 1-11 全部开发、测试、隔离演练、正式交付、生产纠正部署、公网 KCP 验收与回滚判定完成。最终生产为 `v0.1.0-e213dc981df5`，14/14 digest 锁定，13 个常驻容器运行且 RestartCount=0，三基础设施未被纠正 runner 重建，required readiness 与 registry 正常，真实角色 ticket 的 KCP 鉴权/Pong 和 316 秒稳定窗口通过；不需要回滚，兼容候选为 `v0.1.0-c09447f703cf`。首次 release 的失败与纠正部署日志包装证据缺口均作为留痕保留。

- [x] 所有应用服务支持无序批量启动，并在依赖迟到时通过 discovery/readiness 自动收敛。（验证：阶段 3/6/7 代码、测试与随机顺序 Compose 演练通过，纠正生产批量更新最终 8/8 Ready）
- [x] 依赖暂缺或超过收敛窗口不会引发进程退出、restart loop、stale lease 或 socket 冲突。（验证：纠正 match lease 预占超过 120 秒仍同容器 live/not-ready、restart=0，释放后 11.086 秒恢复 Ready；生产关键日志 0）
- [x] liveness、readiness、degraded 和 fatal 状态边界明确，发布系统只向 Ready 实例导流。（验证：共享 HealthState、独立探针、optional degraded 契约和 fatal identity/ownership 测试通过，生产 readiness 8/8）
- [x] worker lease、registry identity 和 Unix socket 均为实例级所有权，正常退出和异常恢复不会影响其他实例。（验证：阶段 4/5/7 所有权与双实例故障演练通过；生产 8 instance/8 heartbeat/6 lease、无 stale/socket conflict）
- [x] 新旧 game-server 可重叠运行并完成 Ready、切流、drain、注销和资源清理。（验证：阶段 5 协议/控制面实现及阶段 7 双实例滚动替换、drain、shutdown 与资源清理实机演练通过）
- [x] Compose/发布/回滚不依赖固定应用服务启动顺序，所有一次性 runner 不连带重建依赖。（验证：部署套件 43 passed/5 Linux-only skip、启动收敛 13/13；纠正隔离与生产 runner 均保持三基础设施身份且 one-shot `--no-deps`）
- [x] 随机启动顺序、依赖迟到、超时恢复、SIGKILL、滚动替换和回滚故障矩阵全部通过且有可追溯证据。（验证：阶段 7 WSL 隔离矩阵、Linux fixture 34/34、完整服务端集成 19/19 与纠正 lease 专项证据通过）
- [x] 正式镜像和 bundle 对应同一已提交 Git revision，生产运行镜像全部由 schema v2 `images.lock.json` 的 digest 锁定，Windows 与 WSL 工作区最终不存在 release 分叉。（验证：源码 `e213dc9`、lock `8ee01df`、release `v0.1.0-e213dc981df5` 对齐，14/14 digest 匹配；归档前 Windows/WSL/origin 均为干净 `8ee01df`、0/0）
- [x] 目标 release 已通过受控 `ops-deploy.sh` 实际部署到生产，migration、required readiness、registry、Caddy/TLS 和公网玩家主链路均通过，且完成约定的稳定观察窗口。（验证：五库门禁、8/8 readiness、registry 8/8/6、API/admin/chat/TLS、真实 KCP Auth/Pong 与 316 秒/6 轮观察通过）
- [x] PostgreSQL、Redis、NATS 的容器与 volume 未被发布 runner 意外重建，生产发布前后基线、迁移和备份证据、授权、操作者、发布结果及回滚判定均已留痕。（验证：纠正发布前三基础设施完整 ID、volume 和 RestartCount=0 与发布后精确一致；零 migration、backup N/A、授权及 no-rollback 已记录）
- [x] 若生产回滚被触发，已在数据库兼容边界内通过 `ops-rollback.sh` 恢复并重新完成稳定验收；若未触发，已记录不回滚依据和仍可用的回滚 release。（验证：未触发回滚；不回滚依据为全部门禁及稳定观察通过，兼容回滚候选 `v0.1.0-c09447f703cf` 保留）
