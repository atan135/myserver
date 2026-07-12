# myforge-agent 蓝图生成闭环 Checklist

## 目标

完成第一阶段 `admin-web -> admin-api -> apps/myforge-agent -> C:\project\myforge -> admin-api -> admin-web` 闭环。`admin-web` 能创建方圆灵构蓝图生成任务、查看 agent 在线状态、轮询任务状态并展示执行结果；`admin-api` 负责权限、审计、任务持久化、WebSocket 转发和结果验签；`apps/myforge-agent` 使用 Rust 实现，主动连接远程 `admin-api`，在 `MYFORGE_ROOT` 指向的外部 `myforge` 工作区执行受控 `codex exec` 命令并回传摘要。

本清单不实现 `game-server` 接入，不实现通用远程终端，不实现 PTY、文件浏览器、任意 shell 平台或跨机器资源发布流程。

## 基础原则

- [x] 以 `docs/协议与客户端/方圆灵构myforge-agent蓝图生成服务端调用设计.md` 为第一阶段事实源，发现实现与文档冲突时同步修正文档。（验证：阶段 1 收口实现契约，阶段 7.1 同步 nullable/权限语义，阶段 12 对照最终代码补当前落地状态并统一完整 API 路由）
- [x] `apps/myforge-agent` 必须使用 Rust 技术栈，独立维护 `apps/myforge-agent/Cargo.toml`，验证命令使用 `cargo test/check --manifest-path apps/myforge-agent/Cargo.toml`。（验证：独立 Rust 2024 crate 已落地；最终 `cargo test` 93/93 + 11/11，check/fmt/clippy 全部通过）
- [x] `admin-api` 只接收 `admin-web` 触发的 typed request，不暴露任意 shell 字符串给普通调用方。（验证：HTTP 仅接受 `fangyuan.blueprint.generate` typed body，schema/前端 payload 拒绝 command、args、cwd、profile、dryRun 和权限开关；真实联调只下发固定内部 `command.execute`）
- [x] `myforge-agent` 只允许在 `MYFORGE_ROOT` 下执行子进程，`artifactFile`、`rulesFile` 等路径必须是相对路径并通过越界校验。（验证：Codex cwd 固定为 canonical root，artifact/rules 双端校验前缀、后缀、绝对路径、反斜杠、`..`、设备名和 symlink/junction 越界；最高权限绕过 OS 沙箱的例外已显式警示）
- [x] P0 不接入 `game-server`，所有 NATS、资源发布、配置热更和玩法触发只保留后续设计入口。（验证：累计 `bbf67ab..f12e8fa` 共 62 个变更路径，不含 `apps/game-server`、NATS 接入、资源发布或玩法触发实现；主契约继续列为非目标）
- [x] 每个阶段完成后补充验证记录；涉及代码提交时按阶段独立提交，不混入无关工作区改动。（验证：12 个阶段均有开始/结束/总结/验证记录；从 `2196467` 到 `f12e8fa` 共 12 个聚焦提交，阶段 11 无代码改动未制造空提交）

## 阶段 1：契约和边界收口

- 开始时间：2026-07-11 22:05:08 +08:00
- 结束时间：2026-07-11 23:04:33 +08:00
- 开发总结：将 P0 草案收口为 Node/Rust 可共同实现的 typed task 契约，补齐 Ed25519/JCS 双向签名、严格 I-JSON、防重放、limits 协商、WebSocket FIFO/并发模型、状态机、取消 deadline、dry-run、artifact/audit、持久化、HTTP、权限和失败码，并明确所有非目标边界。
- 验证记录：PowerShell 独立解析 16 个 JSON 示例成功、72 个 Markdown fence 配对；`git diff --check` 通过；`git diff --name-only` 仅包含 `docs/协议与客户端/方圆灵构myforge-agent蓝图生成服务端调用设计.md`。

- [x] 复核设计文档中的 P0 闭环、非目标、WebSocket 消息、HTTP API、持久化字段、权限点和失败码。（验证：设计文档第 2、8、11、12、13、14 节分别位于第 29、471、958、1061、1228、1254 行，完整定义对应契约）
- [x] 明确第一阶段唯一业务任务为 `fangyuan.blueprint.generate`，不实现通用 `command.execute` 管理入口。（验证：设计文档第 19-25 行固定唯一 taskType，并禁止 command/args/cwd/profile HTTP 输入）
- [x] 明确 `projectId`、`agentId`、`requestId`、`artifactFile`、`consumerTargetFile`、`rulesFile`、`prompt` 的字段语义和必填性。（验证：设计文档第 70-76 行字段表逐项定义生成方、必填性、格式和路径语义）
- [x] 明确 `admin-web` 只通过轮询查询任务状态，P0 不新增浏览器 WebSocket 或 SSE。（验证：设计文档第 36 行和第 1250 行固定 HTTP 轮询及终态停止规则）
- [x] 明确 `game-server`、NATS、资源发布和 mybevy 写入都不进入本轮实现范围。（验证：设计文档第 43-46 行逐项列为 P0 非目标，第 1386 行起保留为后续扩展边界）
- [x] 验证项：形成实现契约说明，能解释 `admin-web`、`admin-api`、Rust `myforge-agent`、外部 `myforge` 工作区之间的职责边界。（验证：设计文档第 52-62 行职责矩阵；独立解析 16 个 JSON 示例成功且 `git diff --check` 通过）

## 阶段 2：admin-api 数据模型和配置

- 开始时间：2026-07-11 23:07:02 +08:00
- 结束时间：2026-07-11 23:46:13 +08:00
- 开发总结：新增 myforge agent/task 双表和双启动路径 DDL、严格 Ed25519/limits 配置解析、known-agent 启动同步及独立事务式 MyforgeStore；完整覆盖 agent connection CAS、任务状态/取消/幂等和生命周期审计。
- 验证记录：定向 `config.test.js + myforge-store.test.js` 44/44 通过；`npm test --workspace admin-api` 145/145 通过；`npx tsc -p apps/admin-api/tsconfig.json --noEmit`、`node --check apps/admin-api/src/myforge/myforge-store.js`、`git diff --check` 通过。按约定未启动 PostgreSQL，真实 DDL 执行留待联调阶段。

- [x] 新增 `myforge_agents` 和 `myforge_task_runs` 持久化结构，字段覆盖在线状态、capabilities、任务状态、prompt、command preview、stdout/stderr 摘要、artifact、audit、错误码和时间戳。（验证：`db/init.sql:131`、`:174` 定义双表、CHECK 和索引；`apps/admin-api/src/db-client.js` 同步启动 DDL）
- [x] 在 `admin-api` 配置中增加 `MYFORGE_ENABLED`、server key 路径、agent 公钥映射、TTL、超时和输出大小限制。（验证：`apps/admin-api/src/config.js:225` 构建 server 配置，`.env.example` 列出 P0 server 变量）
- [x] 增加配置解析和默认值校验，缺失 key、非法 JSON、公钥不可读、超时范围非法时给出明确错误。（验证：`apps/admin-api/src/config.js:39-266` 实现 strictBoolean、整数范围、Ed25519 配对/fingerprint 和 agent map 校验；定向配置测试通过）
- [x] 扩展 `AdminStore` 或独立 store，提供 agent upsert、agent offline、task create、task started、task result、task query、task list、task cancel 状态写入方法。（验证：`myforge-store.js:252` 起提供 agent 初始化/register/heartbeat/offline；`:452` 起提供 create/dispatch/start/result/error/fail/query/list/count/cancel，并以 connection CAS 防止旧连接覆盖）
- [x] 所有任务创建、下发、开始、完成、失败和取消路径都写入 `admin_audit_logs`。（验证：`myforge-store.js:198` 统一生命周期审计，create/dispatch/started/result/error/fail/cancel 均在同一事务调用；审计脱敏测试通过）
- [x] 验证项：运行 `npm test --workspace admin-api` 或覆盖新增 store/config 的定向 Node 测试；无法运行时记录阻塞原因。（验证：定向 44/44、完整 admin-api 145/145、TypeScript/语法/diff 检查通过；未启动 PostgreSQL 的风险已记录）

## 阶段 3：admin-api WebSocket agent 通道

- 开始时间：2026-07-11 23:48:21 +08:00
- 结束时间：2026-07-12 01:05:53 +08:00
- 开发总结：在 `admin-api` 接入 Fastify WebSocket agent 通道，落地严格 UTF-8/I-JSON、canonical JCS、Ed25519 双向签名、进程级有界 replay cache、challenge/register/heartbeat/started/result/error 状态机、连接替换与 connection CAS、单连接 FIFO/operation mutex、消息过期与取消 deadline、终态 semantic digest 幂等、安全审计和先关 agent socket 的幂等停服顺序；未启动真实服务或外部依赖。
- 验证记录：2026-07-12 主 agent 运行定向 shutdown/protocol/schema/WebSocket 测试，47 passed；运行 `npm test --workspace admin-api`，193 passed；运行 `npx tsc -p apps/admin-api/tsconfig.json --noEmit` 和 `git diff --check` 均通过。测试使用 fake socket/store、可暂停 writer/handler 和 Fastify inject，未监听真实端口；真实 WSS/TLS 与 PostgreSQL 联调留待后续阶段。

- [x] 在 `admin-api` 增加 `/api/v1/myforge/ws` WebSocket 接入，支持 `agentId` 和 `projectId` 查询参数。（验证：`myforge-websocket.js:183-217` 严格校验 upgrade/subprotocol/query/known agent，`:719-740` 注册 Fastify WebSocket route；Fastify inject fallback 测试通过）
- [x] 实现 challenge、agent 签名校验、server command 签名和 result 验签流程。（验证：`myforge-connection.js:92-109` 生成签名 challenge，`myforge-websocket.js:240-248` 验签并强制 canonical frame，`:621-703` 签名并复验 execute/cancel；固定 Ed25519/JCS 向量测试通过）
- [x] 维护内存在线 agent 连接表，支持心跳、断线状态更新、重复连接处理和服务关闭时连接清理。（验证：`myforge-websocket.js:148-150` 定义连接表，`:331-382` 注册替换与心跳，`:557-571` connection-CAS 断线清理，`:711-715` 关闭全部连接；停服顺序与重复连接测试通过）
- [x] 支持 `agent.hello`、`agent.register`、`command.started`、`command.result` 消息解析和 schema 校验。（验证：`schemas.js:272-447` 定义 hello/register/started/result 严格 schema 与状态映射，`myforge-websocket.js:302-325` 串行分派；同时覆盖 heartbeat、command.error 和 protocol.error）
- [x] 对非法签名、过期 timestamp、未知 agent、公钥不匹配、重复 request result 等失败路径返回结构化错误并记录安全审计。（验证：`myforge-websocket.js:522-554` 生成脱敏签名 protocol.error 并审计后关闭，`:158-180` 限定审计字段；测试覆盖错误公钥签名、过期、replay、unknown agent、non-canonical frame、重复结果幂等与冲突）
- [x] 验证项：为 WebSocket 握手、注册、签名失败、断线、结果验签和重复消息增加单元测试或轻量集成测试。（验证：定向 47/47、完整 admin-api 193/193、TypeScript 与 diff 检查通过；另覆盖双 socket 并行、execute/cancel wire 顺序、writer 到期、replay 容量和幂等停服）

## 阶段 4：admin-api 任务编排和 HTTP API

- 开始时间：2026-07-12 01:08:42 +08:00
- 结束时间：2026-07-12 02:54:16 +08:00
- 开发总结：新增 myforge agent/任务查询、typed 蓝图任务创建和幂等取消 API；以单连接 operation mutex、delivery reservation、FIFO claim 和 watchdog 串联 queued/dispatched/running/terminal 状态，补齐权限、严格输入校验、响应脱敏、断线/写失败分类，以及可排空 watchdog、WebSocket 入站后台工作和连接操作的停服闭环。
- 验证记录：主 agent 定向运行权限、store、typed input、orchestrator、controller、protocol/schema、WebSocket 测试 100/100 通过；完整 `npm test --workspace admin-api` 226/226 通过（首次受 300 秒工具上限终止且无测试失败，调高上限后 210 秒完成）；`npx tsc -p apps/admin-api/tsconfig.json --noEmit`、5 个相关 JS `node --check`、`git diff --check` 通过。未启动 PostgreSQL 或真实服务，真实 DDL/连接联调留待阶段 10。

- [x] 新增 `GET /api/v1/myforge/agents`，返回在线状态、最近心跳、capabilities 和 `forgeRoot` 摘要。（验证：`myforge.controller.ts:56-59` 注册权限路由，`myforge-orchestrator.js:40-55,225-233` 生成脱敏 agent 投影）
- [x] 新增 `GET /api/v1/myforge/tasks` 和 `GET /api/v1/myforge/tasks/:requestId`，支持后台列表和详情展示所需字段。（验证：`myforge.controller.ts:62-71` 注册列表/详情路由，`myforge-orchestrator.js:65-120,236-257` 返回稳定分页列表与脱敏详情）
- [x] 新增 `POST /api/v1/myforge/tasks/fangyuan-blueprint`，按 typed request 生成受控 `codex exec` 提示词和 command preview。（验证：`myforge.controller.ts:74-78` 返回 202，`myforge-task-input.js:141-208` 固定渲染安全模板和非执行 preview，`myforge-orchestrator.js:260-279` 先持久化再尽力调度）
- [x] 新增 `POST /api/v1/myforge/tasks/:requestId/cancel`，支持取消 queued / dispatched / running 状态并通知在线 agent。（验证：`myforge.controller.ts:81-85` 注册取消路由，`myforge-orchestrator.js:282-488` 实现 queued 直接终态、active 固定 deadline、重复幂等和 delivery 失败关连接；取消竞态测试通过）
- [x] 接口使用 `@Permissions()` 权限点：`myforge.agent.read`、`myforge.task.read`、`myforge.task.create`、`myforge.task.cancel`。（验证：`roles.decorator.ts:25-28` 声明权限，`myforge.controller.ts:57-83` 逐路由绑定，`roles.guard.test.js:84-101,124-136` 验证仅 admin/super_admin 放行）
- [x] 校验 `artifactFile`、`rulesFile`、`consumerTargetFile`、primitive limit、bounds、requirements 和 theme，拒绝绝对路径、`../`、Windows drive 和反斜杠提交格式。（验证：`myforge-task-input.js:58-138,180-209` 执行 exact-object、UTF-8、路径前后缀、POSIX 规范化和 typed prompt 上限校验；非法输入测试通过）
- [x] agent 离线、任务下发超时、签名失败、命令过期、结果超大、审核失败等状态都能落库并返回给 `admin-web`。（验证：`myforge-store.js:377-462,943-1040` 持久化断线与四类 watchdog 终态，`myforge-websocket.js:439-535` 校验结果大小/协议并回调调度，`myforge-orchestrator.js:88-120` 在详情暴露 artifact/audit/error；投影与失败状态测试通过）
- [x] 验证项：补充 controller/service 测试，覆盖成功创建、离线失败、权限拒绝、路径非法、轮询任务详情和取消任务。（验证：主 agent 定向 100/100、完整 admin-api 226/226、TypeScript/语法/diff 检查通过；另覆盖 FIFO、claim/close 竞态、取消 pre-wire 失败和 shutdown quiescence）

## 阶段 5：Rust myforge-agent 工程骨架

- 开始时间：2026-07-12 02:56:44 +08:00
- 结束时间：2026-07-12 03:30:00 +08:00
- 开发总结：新增独立 Rust 2024 myforge-agent crate，落地严格环境配置、Ed25519 key 校验、外部 workspace preflight、最小环境/canonical cwd Codex capability probe、平台/hostname/fangyuan/audit 能力、安全配置摘要和可测试 CLI connector seam；阶段 6 前默认 connect intent 明确返回未实现错误，不伪报已连接。
- 验证记录：主 agent 运行 `cargo test --manifest-path apps/myforge-agent/Cargo.toml` 28/28 通过，`cargo fmt --manifest-path apps/myforge-agent/Cargo.toml -- --check`、`cargo check --manifest-path apps/myforge-agent/Cargo.toml`、`cargo clippy --manifest-path apps/myforge-agent/Cargo.toml --all-targets -- -D warnings` 和未跟踪文件尾随空白扫描通过。测试使用注入 env/probe/connector，不依赖真实 Codex、WebSocket 或外部服务。

- [x] 新增 `apps/myforge-agent/Cargo.toml`、`src/main.rs` 和基础模块结构，项目使用 Rust 2024 edition。（验证：`Cargo.toml:1-4` 定义独立 crate/edition，`src/lib.rs` 导出 app/config/error/keys/logging/preflight 模块，cargo check 通过）
- [x] 实现配置加载：`ADMIN_API_WS_URL`、`MYFORGE_AGENT_ID`、`MYFORGE_PROJECT_ID`、agent 私钥、公钥、server 公钥、`MYFORGE_ROOT`、shell、超时和日志配置。（验证：`config.rs:90-180` 组装 AgentConfig，`:311-520` 实现 strict bool、十进制范围/不变量、legacy shell 和日志解析；`keys.rs:33-98` 有界读取并校验 PKCS#8/SPKI Ed25519 及 key pair）
- [x] 使用 `tracing` 和仓库现有日志环境变量风格输出结构化日志，避免打印私钥、完整签名和敏感配置。（验证：`logging.rs:12-55` 使用 EnvFilter 与 console/file layer；`config.rs:105-120`、`keys.rs:21-29`、`preflight.rs:106-117` 提供脱敏 Debug，安全摘要测试通过）
- [x] 实现启动前 preflight：校验 `MYFORGE_ROOT` 存在、可访问、不是 MyServer 仓库根目录、不是 `apps/myforge-agent` 目录。（验证：`preflight.rs:174-236` canonicalize 并检查 readable directory 与双向路径重叠，测试同时拒绝 agent 目录、MyServer 根和其父目录）
- [x] 实现平台信息、hostname、shell、Codex 可用性和 fangyuan capability 探测。（验证：`preflight.rs:120-171,262-410` 生成固定 capabilities，Codex `--version` 使用 canonical root cwd、3 秒上限和无 MYFORGE 变量的最小环境；dry-run/非 dry-run 测试通过）
- [x] 增加最小 CLI：启动连接、打印配置摘要、`--check` 只做 preflight 不连接远程。（验证：`app.rs:8-55,57-123` 区分 Check/Connect 并注入 Connector，`:250-260` 证明 check 不调用连接；`main.rs:9-45` 串联 config/logging/preflight/dispatch，阶段 6 前 connect 明确返回 MYFORGE_CONNECT_NOT_IMPLEMENTED）
- [x] 验证项：运行 `cargo test --manifest-path apps/myforge-agent/Cargo.toml` 和 `cargo check --manifest-path apps/myforge-agent/Cargo.toml`。（验证：主 agent cargo test 28/28、cargo check、fmt check、clippy -D warnings 与空白扫描全部通过）

## 阶段 6：Rust myforge-agent 签名和 WebSocket 客户端

- 开始时间：2026-07-12 03:31:50 +08:00
- 结束时间：2026-07-12 04:44:10 +08:00
- 开发总结：实现严格 UTF-8/I-JSON 与 RFC 8785 JCS、Ed25519 双向签名、TTL/identity/limits/schema 校验、进程级 replay 和 requestId 幂等；接入带固定子协议的 ws/wss 客户端、64 容量双向 FIFO、单 writer/dispatcher、共享绝对写截止时间、心跳、重连、退出信号与连接级任务取消，并为阶段 7 保留可注入 command handler seam。真实 Codex、started/result 成功链路、artifact/audit 和 cancelled result 留待后续阶段。
- 验证记录：主 agent 运行 `cargo test --manifest-path apps/myforge-agent/Cargo.toml`，47 个库测试和 8 个真实随机端口 loopback WebSocket 测试共 55/55 通过；`cargo fmt --manifest-path apps/myforge-agent/Cargo.toml -- --check`、`cargo check --manifest-path apps/myforge-agent/Cargo.toml`、`cargo clippy --manifest-path apps/myforge-agent/Cargo.toml --all-targets -- -D warnings`、`git diff --check` 和新增文件尾随空白检查通过。测试未启动真实 admin-api、外部网络或 Codex。

- [x] 实现 agent 侧签名、server 签名校验、timestamp / TTL 校验和 requestId 去重。（验证：`protocol.rs:584-681` 实现签名、验签和 semantic digest，`schemas.rs:784` 校验时间窗口，`state.rs:22-333` 实现进程级 replay/request registry；固定 Node signingBytes/signature 向量测试通过）
- [x] 实现连接 `ADMIN_API_WS_URL`，发送 `agent.hello` 和 `agent.register`。（验证：`runtime.rs:136-242` 使用结构化 query、固定 `myserver.myforge.v1` 子协议和本地 parser cap 建连，`:414-487` 验证 challenge 后按 FIFO 发送 hello/register；loopback 握手测试通过）
- [x] 实现心跳、断线重连、指数退避和退出信号处理。（验证：`runtime.rs:136-344` 实现重连与连接任务收敛，`:1423-1472` 发送签名 heartbeat，`app.rs:66-111` 监听 Unix/Windows 退出信号；重连、未完成握手 shutdown 和 active request 先取消测试通过）
- [x] 实现 `command.execute`、`command.cancel` 和错误消息处理。（验证：`runtime.rs:493-545,672-932` 串行分派 execute/cancel、异步 command worker、取消 token、command/protocol error；`command.rs:5-58` 提供阶段 7 handler seam，cancel loopback 测试通过）
- [x] 对 server 签名错误、消息 schema 错误、command 过期、未知 profile、重复 requestId 等情况返回失败结果。（验证：`runtime.rs:550-667,672-818,880-917` 按错误类型生成签名 protocol.error/command.error 并关闭 fatal 连接；loopback 覆盖错误签名、过期、unknown field、缺失 nullable 字段、identity/state、未知 profile和重复 execute）
- [x] 验证项：使用本地 mock WebSocket server 或 Rust 单元测试覆盖握手、注册、签名失败、重连和消息解析。（验证：主 agent 独立运行 55/55 测试通过；另覆盖 trusted challenge 拒绝上下文、noncanonical frame、64 帧 backpressure、writer late-write 禁止、WSS 编译和完整 shutdown 收敛）

## 阶段 7：Rust myforge-agent 受控执行器

- 开始时间：2026-07-12 04:46:45 +08:00
- 结束时间：2026-07-12 08:22:10 +08:00
- 开发总结：完成 Rust 受控执行器、进程组终止、有界输出、artifact 与受信 auditor 摘要，并收口 started/cancel、terminal writer 和有界 backpressure 竞态；`dry_run` 超时按冻结 schema fail closed，由 server watchdog 落 `MYFORGE_COMMAND_TIMEOUT`。
- 验证记录：主 agent 独立通过 Rust 99/99、admin-api 231/231、冻结 schema 5/5、myforge store/WebSocket 59/59、fmt/check/clippy `-D warnings`、`aarch64-linux-android` lib check 和 `git diff --check`；未启动服务或真实 Codex，真实联调留待阶段 10/11。

- [x] 实现 `codex_exec` profile，只允许执行由 `admin-api` 下发的受控 `codex exec "<prompt>"` 命令。（验证：`execution.rs:140` 实现唯一 handler，`:775` 固定 `exec --sandbox workspace-write --ephemeral --color never <renderedPrompt>` 直接参数）
- [x] 子进程 cwd 固定为 `MYFORGE_ROOT`，不允许 command 覆盖到任意目录。（验证：`execution.rs:1008` 仅使用 preflight canonical root 作为 `current_dir`，wire schema 无 cwd/command/args/shell 字段）
- [x] 对 `artifactFile`、`rulesFile`、`consumerTargetFile` 做相对路径和越界校验，拒绝绝对路径、`..`、drive prefix 和反斜杠。（验证：`execution.rs:637-771` 实现三路径语法、Windows 设备名、canonical parent/root 和 symlink/junction 边界校验；路径单测通过）
- [x] 捕获 stdout、stderr、exit code、startedAt、completedAt，并按 `maxOutputBytes` 截断。（验证：`execution.rs:1208-1332` 并行排空双管道，无效 UTF-8 替换后按字节计数与截断；result schema 和超大 frame fallback 测试通过）
- [x] 实现 timeout 后终止子进程并返回 `MYFORGE_COMMAND_TIMEOUT`。（验证：`execution.rs:58-81,1026-1143` 用单一 monotonic deadline 覆盖进程/管道/artifact/audit，timeout/cancel/drop 显式 `start_kill` 整个进程组；Windows timeout/cancel/drop 真实进程树测试通过）
- [x] 执行结束后读取 artifact 摘要：exists、sha256、bytes、modifiedAt。（验证：`execution.rs:798-879` 在 cancel/deadline 约束下流式计算 SHA-256，复核长度、mtime 和最终 canonical path；artifact hash 测试通过）
- [x] 如 `myforge` 工作区存在审核命令或脚本，接入可选审核并返回 `audit` 摘要；没有审核器时返回明确的 skipped / unavailable 状态。（验证：`preflight.rs:109-139,311-411` 固定 auditor canonical path/hash/bytes/mtime 且 Debug 脱敏，`execution.rs:1356-1566` 在统一 deadline 内复核身份并解析严格 JSON；篡改和链接替换测试证明不启动 auditor）
- [x] 验证项：Rust 单元测试覆盖路径校验、输出截断、timeout、artifact 摘要和失败码；本地 dry-run 可以用无副作用命令替代真实 Codex。（验证：主 agent `cargo test` 99/99；`execution.rs:2308` 证明 dry-run 只读且不启动进程；`runtime.rs:2985-3679` 覆盖 start-send、capacity、cancel/result 和 started/cancel 确定性竞态）

## 阶段 7.1：空工作区与 Codex 最高权限执行补充

- 开始时间：2026-07-12 11:09:59 +08:00
- 结束时间：2026-07-12 11:55:14 +08:00
- 开发总结：将蓝图任务扩展为显式无规则和空 artifact 目录可执行，保留路径与 typed task 边界；新增只能由 agent 本机开启并经签名 capability 暴露的 Codex 最高权限模式，dispatch 时冻结权限与 command preview 快照；同步 Node/Rust schema、数据库迁移、结果状态、审计和设计文档。
- 验证记录：worker 运行 MyForge Node 定向测试 98/98、Rust 库测试 93/93、WebSocket 测试 11/11，并通过 tsc、cargo check/fmt/clippy 与 diff 检查；admin-api 全量 TAP 打印 232/232、0 fail/0 cancelled 后外层在 300.7 秒触及工具上限。主 agent 独立复跑 Node 高风险定向测试 85/85、Rust 93/93 + 11/11、tsc、cargo fmt 和 `git diff --check`，全部通过；第 1 轮审核补齐 Rust required-nullable 严格反序列化和数据库历史回填/幂等 CHECK。

- [x] 将 `rulesFile` 改为显式可选字段；为 `null` 时允许无规则执行，提供相对路径时仍要求文件存在且通过 `MYFORGE_ROOT` 越界校验。（验证：`myforge-task-input.js:145-168` 渲染无规则 prompt；`schemas.rs:114-127` 强制 nullable 键必须出现，`execution.rs:645-694` 仅对非 null 规则做 canonical 边界校验）
- [x] 保持 `artifactFile` 为受控相对目标路径，但允许其父目录尚不存在，由 Codex 在执行期间创建；对已有路径、符号链接和执行后 artifact 继续做越界校验。（验证：`execution.rs:696-753` 逐级校验已有祖先并保留缺失后缀，执行后 `observe_artifact` 再 canonicalize；链接逃逸和缺失目录测试通过）
- [x] 无论 artifact 是否生成都回传 Codex stdout、stderr、exit code 和时间信息；Codex 成功但 artifact 缺失时返回可见的 `completed_with_errors / MYFORGE_TARGET_FILE_MISSING`。（验证：`execution.rs:318-456` 先构造带输出 result 再映射 artifact 缺失；Node/Rust schema 与缺失 artifact 测试通过）
- [x] 增加仅由 agent 本机严格布尔配置启用的 Codex 最高权限模式，等价调用原生 Codex `--dangerously-bypass-approvals-and-sandbox`，不得由 HTTP 或 WebSocket 消息远程切换。（验证：`config.rs:159-174` strict local env，`execution.rs:816-836` 精确 argv；Node/Rust unknown-field 测试拒绝远程 `dangerFullAccess`）
- [x] agent capabilities、配置摘要、command preview 和审计日志明确暴露 `danger_full_access`，并保留 `APPDATA`、`CODEX_HOME`、`HOME`、`USERPROFILE` 以复用同一 Windows 用户的本机 Codex 认证。（验证：`preflight.rs:60,201` 签名 capability；`myforge-orchestrator.js:537-547` dispatch 冻结精确 preview；`myforge-store.js:145-155` 审计快照；`execution.rs:37-55` 环境 allowlist）
- [x] 同步 Node/Rust schema、持久化字段、提示词模板、设计文档和失败码语义，保持 typed task、签名、防重放、timeout、取消与输出上限不变。（验证：`schemas.js:196-207`、`schemas.rs:1028-1069` 双端契约；`db/init.sql:221-266` nullable/历史回填/幂等 CHECK；设计文档第 5、6、8、11、14-16 节同步）
- [x] 验证项：Node/Rust 测试覆盖无规则、缺失 artifact 父目录、artifact 缺失仍回传输出、最高权限参数精确性、远程不可切换和 capability 协商。（验证：主 agent Node 85/85、Rust 93/93 + 11/11、tsc、fmt、diff 检查通过；worker MyForge Node 98/98、clippy/check 通过）

## 阶段 8：admin-web API 接入和权限入口

- 开始时间：2026-07-12 08:27:58 +08:00
- 结束时间：2026-07-12 08:39:58 +08:00
- 开发总结：完成 MyForge 前端权限/API 接入，增加按读取权限准入的最小入口和可复用错误归一化；第 1 轮审核修正了通用 404 被误报为任务不存在的问题，完整任务页面留待阶段 9。
- 验证记录：主 agent 运行 `npm run build --workspace admin-web` 通过（Vite 转换 2224 个模块，仅有既有大 chunk 警告）；Node 适配器/权限/错误映射断言与路由菜单静态断言通过；`git diff --check` 通过。

- [x] 在 `admin-web` 权限矩阵中增加 `myforge.agent.read`、`myforge.task.read`、`myforge.task.create`、`myforge.task.cancel`。（验证：`src/auth/permissions.js` 定义四项权限，viewer/operator 未获授权，admin/super_admin 全权限断言通过）
- [x] 增加 `admin-web/src/api` 的 myforge API 封装，覆盖 agents、tasks、task detail、create fangyuan task 和 cancel。（验证：`src/api/index.js` 的 5 个方法经 axios adapter 断言确认 HTTP 方法、路径编码、查询参数和 cancel body）
- [x] 增加路由和菜单入口，只有具备对应权限的管理员可见。（验证：`/myforge` route meta 与菜单共同使用 `MYFORGE_ENTRY_PERMISSIONS`，router guard/菜单条件静态断言通过）
- [x] 页面加载失败、无权限、agent 离线、任务不存在、接口超时等状态都有清晰提示。（验证：`src/views/MyForge.vue` 实际加载入口展示分区状态，`src/api/myforge-errors.js` 的 timeout/403/404/disabled/unreachable 与列表 generic 404 上下文断言通过）
- [x] 验证项：运行 `npm run build --workspace admin-web`，并手动核对菜单权限和 API 调用路径。（验证：生产构建通过，2224 个模块；主 agent 核对 5 个 API 路径及 route/menu 权限条件一致）

## 阶段 9：admin-web 任务创建、列表和详情页面

- 开始时间：2026-07-12 08:41:31 +08:00
- 结束时间：2026-07-12 12:19:39 +08:00
- 开发总结：完成 MyForge 任务创建、列表、详情、轮询、取消和完整状态展示；补充显式无规则开关与 `rulesFile:null` 契约，展示 agent/task 的 `dangerFullAccess` 三态及整机权限风险，artifact 缺失时自动展开并保留 Codex 输出；移动端导航、表格和创建弹窗可操作。
- 验证记录：worker 与主 agent 分别运行 `npm run test:myforge --workspace admin-web` 15/15 和生产构建，均通过（2227 个模块，仅有既有大 chunk 警告）；Edge + 临时 playwright-core 在 1440×900、390×844 完成登录态、Agent 权限、任务三态、5 秒轮询、无规则创建 payload、详情输出、artifact missing、空态和 503 错误态验收。两视口无页面横向溢出、header/body 重叠或按钮裁切，pageerror=0；唯一 console error 来自预期 mock 503。截图保存在系统临时目录，Vite/Edge 已清理，`git diff --check -- apps/admin-web` 通过。

- [x] 实现 agent 在线状态列表，显示 agentId、projectId、状态、hostname、platform、forgeRoot、capabilities 和 lastSeenAt。（验证：`src/views/MyForge.vue` 的 Agent tab 展示身份、在线标签、hostname/platform/version、`forgeRootSummary`、capabilities、lastSeenAt 与在线/离线统计）
- [x] 实现方圆灵构蓝图任务创建表单，包含 agent、theme、primitiveLimit、bounds、requirements、artifactFile、consumerTargetFile、rulesFile。（验证：`MyForge.vue` 创建对话框覆盖全部字段，projectId 从 agent 派生，离线 agent 明示 queued 语义，exact body 单测通过）
- [x] 表单对路径、数量、必填字段和超长文本做前端校验，错误文案与后端校验保持一致。（验证：`src/myforge/task-utils.js` 实现 UTF-8、路径、设备名、整数、requirements 与 16 KiB 校验；12/12 单测及前后端 20 组接受/拒绝与 renderedPrompt 对照通过）
- [x] 实现任务列表，显示 requestId、taskType、agent、状态、创建人、创建时间、开始时间、完成时间和耗时。（验证：`MyForge.vue` 任务 tab 提供筛选、分页、字段列和活动态刷新，query sequence + revision 阻止陈旧响应覆盖）
- [x] 实现任务详情，展示 prompt 参数、command preview、stdout / stderr 摘要、exit code、artifact、audit、错误码和错误信息。（验证：`src/views/MyForgeTaskDetail.vue` 分区展示完整详情投影、路径/命令复制、输出 bytes/truncated、JSON artifact/audit 和错误提示）
- [x] 详情页通过轮询刷新 running / dispatched / queued 任务，任务完成、失败或取消后停止轮询。（验证：详情使用单一 2.5 秒 `setTimeout` 链，active status 才调度；generation/activeLoads 防重入并在终态、404、route 变化和 unmount 清理）
- [x] UI 支持空状态、加载状态、错误状态、长 stdout/stderr 折叠、移动端基本可用和按钮防重复提交。（验证：表格 empty/loading/error+重试、输出 collapse/max-height/长行换行、移动断点与横向滚动已实现；create/cancel attempt guard 和取消确认 token 防重复提交）
- [x] 适配可选 `rulesFile` 和 `danger_full_access` capability 展示，表单允许显式无规则执行并清晰标识 agent 的整机权限风险。（验证：`task-utils.js:183-205,288-292` 精确生成 nullable request 与权限三态；`MyForge.vue:254-265,363-378,504-523` 展示 agent 风险并提供规则开关；详情展示任务权限且缺失 artifact 时保留输出）
- [x] 验证项：运行 `npm run build --workspace admin-web`，并在本地后台手动验证创建表单、列表轮询和详情展示。（验证：主 agent 15/15 与生产构建通过；Edge 1440×900、390×844 真实 viewport 验收通过，`rulesFile:null` 且 payload 不含 `dangerFullAccess`，截图与页面 metrics 已人工复核）

## 阶段 10：admin-api 与 Rust agent 联调

- 开始时间：2026-07-12 12:22:12 +08:00
- 结束时间：2026-07-12 12:53:29 +08:00
- 开发总结：完成独立 PostgreSQL 数据库、临时 NATS、既有 Redis、admin-api 与 Rust agent 的真实联调，覆盖空工作区、无规则、artifact 成功/缺失、双流截断、超时、运行中取消、离线排队、审核跳过/失败和完成态拒绝取消。新增最小 `.env.example`，明确原生 Codex 路径、dry-run、仅本机可开启的最高权限及资源限制；临时密钥、数据库、Redis key、fixture、日志和进程均已清理。
- 验证记录：admin-api `232/232`、Rust `93/93 + 11/11`，TypeScript、`cargo fmt --check`、`cargo check`、`cargo clippy --all-targets -- -D warnings` 全部通过。联调使用 admin-api `127.0.0.1:3101`、agent 出站 WebSocket、NATS `127.0.0.1:4322`、Redis `6379`、PostgreSQL `5432`；清理后 `3101/4322` 无监听，`C:\project\myforge` 工作树干净，主仓库只留下待提交的 `.env.example` 和本 checklist 记录。

- [x] 准备本地 key pair、agent 公钥配置、`MYFORGE_ROOT=C:\project\myforge` 和最小 `.env.example`。（验证：联调使用临时 Ed25519 三钥及 known-agent 映射；`apps/myforge-agent/.env.example` 覆盖全部必需 key 路径、root、Codex、audit、limits 和日志变量，敏感值扫描为空）
- [x] 启动 `admin-api` 和 Rust `myforge-agent`，确认 agent 注册后 `admin-web` 可看到在线状态。（验证：真实 `GET http://127.0.0.1:3101/api/v1/myforge/agents` 返回唯一 agent `status=online`、`codexExec=true`、`dangerFullAccess=true`；阶段 9 浏览器验收已确认 admin-web 使用同一响应展示在线与权限状态，阶段 11 再做真实页面复验）
- [x] 通过 `admin-api` 创建一条 `fangyuan.blueprint.generate` 任务，确认 WebSocket 下发、agent started、agent result 和 task 状态流转。（验证：requestId `37defedd-1c33-463a-a9a4-b6d8645cbe59` 经 queued/dispatched/running/completed 流转并保存 artifact 摘要，数据库保留完整 lifecycle 审计直至测试清理）
- [x] 在不调用真实 Codex 的 dry-run 模式下验证闭环，避免初期联调依赖外部 AI 执行时间。（验证：requestId `03b653a3-4755-4d6f-84dc-52db92230a1d` 在空工作区、`rulesFile:null` 下 dry-run completed，未创建 rules/artifacts 或启动 Codex）
- [x] 验证 stdout / stderr 截断、artifact 缺失、artifact 存在、审核 skipped / failed、agent 离线和 command timeout。（验证：`46d0a90d-d0e3-47b7-951b-072a0b48042c` 双流截断；`9a5900ce-c75e-4deb-9249-482b08dcf37c` artifact 缺失但保留输出；`37defedd-1c33-463a-a9a4-b6d8645cbe59` artifact 存在；`0b84fc3d-9cea-46a0-9d61-c006afa543ef` audit skipped；`f594fb13-3390-4527-99bc-01825ad060be` audit failed；`81e473a6-72e6-44e0-9915-4870e164bf4a` agent offline；`bf280430-7d2f-4ae0-b2c3-2fda35eac516` command timeout 且进程树 `2 -> 0`）
- [x] 验证取消任务：queued 可取消，running 能通知 agent 并尽量终止子进程，已完成任务不可取消。（验证：离线 queued requestId `81e473a6-72e6-44e0-9915-4870e164bf4a` 成功取消；running requestId `4694a03e-b475-49d1-a084-d2bd57ed9e12` 变为 cancelled 且进程树 `2 -> 0`；完成态取消返回 `409/MYFORGE_TASK_NOT_CANCELLABLE`）
- [x] 验证项：记录联调命令、端口、env、requestId、任务状态流转、artifact 摘要和错误路径结果。（验证：本阶段开发总结与验证记录已记录端口、脱敏配置、九个完整 requestId、状态/摘要/错误码、测试结果和精确清理结果）

## 阶段 11：admin-web 到 agent 端到端验收

- 开始时间：2026-07-12 12:56:34 +08:00
- 结束时间：2026-07-12 13:42:06 +08:00
- 开发总结：完成 admin-web、admin-api 与 Rust agent 的真实 Edge 端到端验收。dry-run 和本机认证的原生 Codex 均从页面创建并闭环；真实执行在无 `rules/`、无 `artifacts/` 的 `C:\project\myforge` 中使用最高权限创建 artifact，并完整回传 stdout/stderr。非法路径、错误 agent 签名、错误 server 签名和缺失规则错误态均 live 验证通过，无业务代码改动。
- 验证记录：dry-run requestId `b49b23ba-a8a5-4d8a-b6b3-c88a25334c24`，真实 Codex requestId `0168c545-a17b-4617-b9c0-fc236de54d9d`，错误详情 requestId `a4233e4a-f439-4de3-b31f-600b4e13d198`。Edge `1440x900`/`390x844` 共保留 10 张截图，`pageerror=0`，仅有 favicon 404；admin-api `232/232`、admin-web `15/15`、Vite build、Rust `93/93 + 11/11` 全部通过。主 agent 复核截图、两个仓库 clean、`3001/3002/4222` 空闲、全部 owned PID 停止、隔离数据库已删除、Redis 本轮 key 为 0，外部工作区恢复无 rules/artifacts。

- [x] 在用户确认后启动必要服务：PostgreSQL、Redis 如当前 admin-api 需要、`admin-api`、`admin-web`、Rust `myforge-agent`。（验证：用户明确确认启动；隔离数据库 `myserver_s11_c15313`、唯一 Redis prefix、run-owned NATS `4222`、admin-api `3001`、admin-web `3002` 和 agent 均启动 healthy，验收后精确清理）
- [x] 使用 `admin-web` 创建任务，确认 `admin-api` 落库并下发到在线 agent。（验证：真实 Edge 登录 `/myforge` 后创建 dry-run 与真实 Codex 任务；数据库 lifecycle audit 记录 queued/dispatched/running/terminal，Agent 页面显示 online、`codexExec=true`、整机最高权限）
- [x] `myforge-agent` 在 `MYFORGE_ROOT` 下执行命令，回传 started 和 result。（验证：真实 requestId `0168c545-a17b-4617-b9c0-fc236de54d9d` 在 `C:\project\myforge` 执行约 138.3s，exitCode 0，回传 stdout 203B/stderr 156061B，并从无目录状态创建 369B artifact）
- [x] `admin-web` 能通过轮询看到 queued / dispatched / running / completed 或 failed 的状态变化。（验证：真实 Edge 捕获 dry-run 与真实 Codex 的 dispatched -> running -> completed；缺失 rules 任务 requestId `a4233e4a-f439-4de3-b31f-600b4e13d198` 显示 failed，数据库审计补齐 queued 状态）
- [x] `admin-web` 任务详情能展示 stdout / stderr 摘要、artifact 摘要、audit 摘要、错误码和完成时间。（验证：桌面/移动截图显示真实任务 stdout、152.4KiB stderr、artifact bytes/hash、`unavailable/auditor_not_configured` 和时间线；失败详情显示 `MYFORGE_RULES_FILE_MISSING`、空输出、空 Artifact/Audit 与完成时间）
- [x] 验证安全边界：非法路径不能下发执行，agent 签名错误不能注册，server 签名错误 agent 拒绝执行。（验证：`../escape.ron` 在 UI 阻断且 API 返回 `400/MYFORGE_TARGET_PATH_INVALID`，任务/子进程不变；bad-agent key 触发 critical `MYFORGE_AGENT_SIGNATURE_INVALID` 且未注册；错误 server 公钥触发 `MYFORGE_SERVER_SIGNATURE_INVALID`、`registered=false`，既有任务结果未改变）
- [x] 验证项：完成一次 dry-run 端到端闭环和一次真实 `codex exec` 或用户确认的等价命令闭环，并记录 requestId 与结果。（验证：dry-run `b49b23ba-a8a5-4d8a-b6b3-c88a25334c24` completed；真实 Codex `0168c545-a17b-4617-b9c0-fc236de54d9d` completed。Codex PID 175796 路径与 `Get-CodexNativeExe` 一致，参数包含 `--dangerously-bypass-approvals-and-sandbox`/`--ephemeral`，不含 `--sandbox`/`workspace-write`）

## 阶段 12：文档、示例配置和最终清理

- 开始时间：2026-07-12 13:43:13 +08:00
- 结束时间：2026-07-12 14:05:21 +08:00
- 开发总结：完成 MyForge P0 正式文档和运行示例收口。主契约补充当前落地状态、空工作区、Windows 同用户认证和原生 Codex 路径说明，并统一完整 API 路由；管理后台设计补充页面、权限、表结构、HTTP/WS 工作流、配置、错误可见性和最高权限风险；agent `.env.example` 补充 `Get-CodexNativeExe` 校验、`--check` 与正式启动命令。未修改业务代码或 game-server。
- 验证记录：admin-api `232/232`（首次 120s 外层超时后以 600s 成功复跑）、admin-web `15/15`、Vite build、admin-api TypeScript、Rust `93/93 + 11/11`、`cargo check`、`cargo fmt --check`、`cargo clippy --all-targets -- -D warnings` 和 `git diff --check` 全部通过。主 agent 复核变更仅 3 个允许文件，路由/权限/配置/表/失败码与代码一致，新增内容敏感值扫描为空。

- [x] 更新设计文档，记录最终落地的接口、表结构、配置名、失败码和 P0 限制。（验证：`docs/协议与客户端/方圆灵构myforge-agent蓝图生成服务端调用设计.md` 新增当前落地状态并保留两表、配置范围、错误码和非目标契约，5 个 HTTP 小节统一为实际 `/api/v1/myforge/...` 路由）
- [x] 为 `apps/myforge-agent` 增加 `.env.example` 或 README 片段，说明 key、`MYFORGE_ROOT`、启动命令、`--check`、dry-run 和安全边界。（验证：`apps/myforge-agent/.env.example` 覆盖三项 key、root、dry-run、audit/limits、最高权限风险、同 Windows 用户认证、原生 Codex 路径校验以及 `cargo run ... -- --check`/正式启动命令）
- [x] 更新 `admin-web` / `admin-api` 相关文档或管理后台说明，记录入口、权限和使用方式。（验证：`docs/后台与运维/管理后台设计.md` 新增 `/myforge` 与详情路由、四权限矩阵、两表、5 个 HTTP API、agent WebSocket、创建/轮询/取消/错误展示和本机权限三态说明）
- [x] 确认不提交私钥、真实 token、`.env`、本地绝对敏感配置、日志和生成产物。（验证：新增行扫描未发现 private key、Bearer/token/password/secret 实值、UUID、`C:\Users`、日志、截图、artifact 或生成目录；仅 `.env.example` 占位路径进入 diff）
- [x] 复跑最终检查：`npm test --workspace admin-api`、`npm run build --workspace admin-web`、`cargo test --manifest-path apps/myforge-agent/Cargo.toml`、`cargo check --manifest-path apps/myforge-agent/Cargo.toml`。（验证：admin-api 232/232、Vite build、Rust 93/93 + 11/11 与 cargo check 全部通过；另通过 admin-web 15/15、tsc、fmt 和 clippy）
- [x] 如任何检查不能运行，记录原因、替代验证和剩余风险。（验证：所有要求命令最终均可运行并通过；admin-api 首轮仅因工具 120s 时限中断，确认无残留后以 600s 复跑通过，不保留验证缺口）
- [x] 验证项：最终 diff 只包含本功能相关代码、文档和示例配置，不包含 `game-server` 接入实现。（验证：阶段 12 `git diff --name-only` 精确为 agent `.env.example`、MyForge 主契约和管理后台设计；未修改 game-server、其他业务模块或生成文件）

## 最终完成定义

以下项目作为整体完成标准，不要求每个开发阶段都执行，由所有相关阶段完成后统一验收。

- 开始时间：2026-07-12 14:07:21 +08:00
- 结束时间：2026-07-12 14:08:13 +08:00
- 验收总结：MyForge P0 蓝图生成闭环全部完成。admin-web、admin-api、Rust agent、PostgreSQL、Ed25519 WebSocket、受控 Codex 执行、artifact/audit、取消与错误可见性均通过自动化和真实 Edge 联调；无 rules/artifacts 的外部工作区使用本机认证原生 Codex 和本机最高权限成功创建 artifact 并回传完整输出。累计 12 个功能提交未接入 game-server/NATS/资源发布/通用终端，最终服务、临时数据库、Redis key、密钥和外部产物均已清理。

- [x] `admin-web` 可以创建方圆灵构蓝图生成任务并展示 agent、任务、artifact、audit 和错误信息。（验证：阶段 11 真实 Edge 桌面/移动端从 `/myforge` 创建任务，Agent/任务/详情展示在线、权限、输出、369B artifact、audit、错误码和时间线，`pageerror=0`）
- [x] `admin-api` 可以完成权限校验、审计、任务持久化、WebSocket 下发、结果验签和查询。（验证：真实隔离 PostgreSQL 保存 queued/dispatched/running/terminal 与 lifecycle audit；错误签名 live 拒绝，最终 admin-api 232/232 和 tsc 通过）
- [x] Rust `apps/myforge-agent` 可以主动连接 `admin-api`，在 `MYFORGE_ROOT` 下执行受控命令并回传结果。（验证：真实 agent 注册 online 后在 `C:\project\myforge` 启动原生 Codex PID 175796，exitCode 0，回传 stdout 203B、stderr 156061B、artifact hash 和完成时间）
- [x] dry-run 端到端闭环通过，至少一次真实或用户确认的等价命令闭环通过。（验证：dry-run requestId `b49b23ba-a8a5-4d8a-b6b3-c88a25334c24` completed；真实 Codex requestId `0168c545-a17b-4617-b9c0-fc236de54d9d` completed，并确认最高权限参数且无 workspace-write）
- [x] 非法路径、签名失败、agent 离线、超时、输出过大、artifact 缺失和审核失败都有可见错误状态。（验证：阶段 10/11 live 覆盖 400 非法路径、双向签名失败、offline queued、timeout、双流截断、artifact missing 保留输出、audit failed；UI 错误详情和自动化测试覆盖结构化错误/截断标记）
- [x] P0 未实现 `game-server` 接入、NATS 通知、资源发布或通用远程终端能力。（验证：累计 12 个提交路径扫描无上述实现，HTTP 仅提供 typed task，command/args/cwd/profile 和远程权限切换均被拒绝）
- [x] 最终验证命令和手动验收记录已写入对应阶段验证记录。（验证：阶段 9-12 记录前端构建/测试、admin-api 232/232、Rust 93/93 + 11/11、check/fmt/clippy、真实 Edge 双视口、dry-run/真实 Codex requestId 与清理结果）
