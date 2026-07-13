# 方圆灵构 myforge-agent 蓝图生成服务端调用设计

## 1. 文档定位与约定

本文是 MyServer P0 方圆灵构蓝图生成闭环的实现契约，约束以下链路：

~~~text
admin-web
  -> admin-api HTTP
  -> admin-api WebSocket
  -> apps/myforge-agent
  -> MYFORGE_ROOT 指向的外部 myforge 工作区
  -> admin-api
  -> admin-web 轮询
~~~

本文中的“必须”“不得”和“只允许”是 P0 的强制要求。后续实现若与本文冲突，应先依据实际代码和安全边界修正文档，再扩展协议，不得在 Node.js 与 Rust 两端各自解释。

P0 只定义一个业务任务：

~~~text
fangyuan.blueprint.generate
~~~

WebSocket 内部消息名 command.execute 只是该 typed task 的传输封装，不表示存在通用命令管理接口。P0 不提供 command.execute HTTP API，不接受调用方提交 command、args、cwd、profile 或任意 shell 字符串。

外部 myforge 是独立 Git 工作区，不是 MyServer 或 mybevy 的子目录。本机示例路径可以是 C:\project\myforge，但 MyServer 业务代码和数据库不得依赖该绝对路径。规则的上游参考可以来自 mybevy；任务提供非 null `rulesFile` 时，P0 执行只读取 myforge 内维护的规则副本。

### 1.1 当前落地状态

截至 2026-07，本文 P0 链路已按下列边界落地，后文继续作为实现契约和扩展约束：

| 范围 | 已落地行为 |
|------|------------|
| 控制面 | `admin-api` 提供 `/api/v1/myforge/*` 管理员 HTTP API 和 `/api/v1/myforge/ws` agent WebSocket；`admin-web` 在 `/myforge` 提供 Agent、任务创建、列表、详情和取消操作。 |
| 持久化 | PostgreSQL 使用 `myforge_agents` 保存配置同步后的 agent 身份、连接、capability 和协商限制，使用 `myforge_task_runs` 保存 typed task、权限快照、输出摘要、artifact/audit 摘要、错误和生命周期时间。 |
| 空工作区 | `rulesFile` 是必须出现但可为 null 的键；显式 null 表示无规则执行。`artifactFile` 仍必填并限制在 `artifacts/fangyuan/*.ron`，其父目录可不存在并由 Codex 创建。 |
| 执行结果 | Codex 退出 0 但未生成 artifact 时任务为 `completed_with_errors` / `MYFORGE_TARGET_FILE_MISSING`，仍保存 stdout、stderr、exitCode 和时间信息。 |
| 本机权限 | 默认使用 `--sandbox workspace-write`；只有 agent 本机 `MYFORGE_CODEX_DANGEROUS_FULL_ACCESS=true` 才改用 `--dangerously-bypass-approvals-and-sandbox`，HTTP、WebSocket 和管理页面都不能远程切换。 |
| 已验证链路 | 已完成 dry-run 和真实本机认证 Codex 的端到端闭环，包含无 `rules/`、无 `artifacts/` 的工作区；一次性 requestId 和逐项证据保留在对应 checklist。 |

真实 Codex 部署在 Windows 时，agent 必须与完成 Codex 登录认证的用户相同，并把 PowerShell 7 `Get-CodexNativeExe` 返回的原生 executable 路径配置为 `MYFORGE_CODEX_BIN`。不得把 `codex` / `codex-admin` function 或 `.ps1` wrapper 当成可执行文件路径。子进程只继承固定 allowlist 中当前确实存在的环境变量；`APPDATA`、`CODEX_HOME`、`HOME`、`USERPROFILE` 用于复用该用户认证，不为缺失变量伪造值。

## 2. P0 目标与非目标

### 2.1 P0 完成定义

P0 必须完成：

- admin-web 可以查看 agent、创建方圆灵构任务、查看任务列表和详情、取消可取消任务。
- admin-web 只通过 HTTP 创建任务和轮询状态，不新增浏览器 WebSocket 或 SSE。
- admin-api 完成管理员 JWT 鉴权、权限校验、typed request 校验、受控提示词生成、任务持久化、审计、WebSocket 下发和 agent 结果验签。
- Rust apps/myforge-agent 主动连接 admin-api，在 MYFORGE_ROOT 内执行固定 codex_exec profile，回传有限的输出、artifact 和 audit 摘要。
- 外部 myforge 提供 Codex 上下文；规则副本和审核器可选，artifact 父目录可以不存在并由 Codex 创建。

### 2.2 P0 明确不实现

- 不接入 game-server。
- 不发布 NATS 事件，也不消费 NATS。
- 不实现资源发布、配置热更、对象存储上传或跨机器文件复制。
- 不向 mybevy 写文件；consumerTargetFile 只保存为未来交付位置的元数据。
- 不实现通用 command.execute 管理入口、远程 shell、PTY、终端会话、文件浏览器或任意文件写入 API。
- 不让玩家客户端、auth-http 或普通内部服务直接触发 agent。
- 不让浏览器直连 myforge-agent。
- 不自动提交 myforge 或 mybevy Git 变更。

## 3. 组件职责边界

| 组件 | P0 负责 | P0 不负责 |
|------|---------|-----------|
| admin-web | 管理员表单、列表、详情、取消按钮、HTTP 轮询、权限可见性 | agent WebSocket、签名、命令生成、本地文件访问 |
| admin-api | 管理员鉴权和权限、字段校验、提示词渲染、任务状态、PostgreSQL、审计、agent WebSocket、双向验签 | 执行 Codex、访问 MYFORGE_ROOT、解析完整 RON、写 mybevy |
| Rust myforge-agent | 主动连接、握手、验签、路径二次校验、固定 profile 执行、进程取消、输出截断、artifact/audit 摘要 | 管理员鉴权、自由解释任务类型、接受任意命令、发布资源 |
| 外部 myforge | Codex skill/项目上下文、可选规则副本和审核脚本、artifact 目标位置 | WebSocket、任务状态、管理员权限、服务端审计 |
| game-server / NATS / mybevy | 无 | P0 全部链路 |

admin-api 是唯一控制面。auth-http 不参与。agent 只接受来自其配置中 server 公钥签名的消息，admin-api 只接受配置中已登记 agent 公钥签名的消息。

## 4. 标识符和字段语义

### 4.1 核心字段

| 字段 | 创建请求 | 生成方 | 语义和约束 |
|------|----------|--------|------------|
| projectId | 必填 | admin-web 选择，admin-api 校验 | myforge 逻辑项目标识。1 至 128 个 ASCII 字符，匹配 ^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$。必须与 agent 配置绑定的 projectId 完全一致。 |
| agentId | 必填 | admin-web 选择，admin-api 校验 | agent 全局唯一标识。格式同 projectId。必须存在于 server 的 agent 公钥映射中。 |
| requestId | 不得提交 | admin-api | 小写 UUID v4。创建任务时生成，作为数据库主键、WebSocket 幂等键和日志关联键。 |
| artifactFile | 必填 | 调用方 | 生成结果在 MYFORGE_ROOT 下的相对路径。P0 必须位于 artifacts/fangyuan/ 下并以 .ron 结尾。 |
| consumerTargetFile | 可选 | 调用方 | 未来交付给 mybevy 或资源发布流程的相对目标路径元数据。P0 不解析到本机目录、不检查文件存在性、不写入该路径。传入时必须位于 project/assets/fangyuan/ 下并以 .ron 结尾。 |
| rulesFile | 必填键，可为 null | 调用方 | 非 null 时是 agent 在 MYFORGE_ROOT 下读取的规则副本相对路径，必须位于 rules/fangyuan/ 下并以 .md 结尾；null 明确表示空工作区无规则执行。省略字段仍是非法请求。 |
| prompt | 必填 | 调用方 | 结构化生成参数对象，不是自由 shell 文本，也不是完整 Codex 提示词。admin-api 根据它生成 renderedPrompt。 |
| renderedPrompt | 不得提交 | admin-api | 由固定模板和已校验 prompt 渲染，写入签名后的 command.execute。 |
| commandPreview | 不得提交 | admin-api | 仅用于数据库和 admin-web 展示的非可执行字符串。agent 不接收、不解析、不执行该字段。 |
| dangerFullAccess | 不得提交 | agent 本机配置 | capabilities 中的只读布尔值；dispatch 后作为任务权限快照返回。HTTP 创建请求和 command.execute 都不得携带或切换它。 |
| profile | 不得提交 | admin-api | P0 固定为 codex_exec。任何其他值都必须拒绝。 |

所有 HTTP JSON 字段使用 camelCase。所有 PostgreSQL 字段使用 snake_case。数据库时间使用 timestamptz，HTTP 时间返回 UTC RFC 3339 字符串，WebSocket 签名消息使用 Unix epoch 毫秒整数。

### 4.2 相对路径统一规则

artifactFile、非 null rulesFile 和可选 consumerTargetFile 的提交格式统一使用 /。非 null 路径都必须通过以下词法校验：

- UTF-8 编码后长度为 1 至 512 字节。
- 不得以 / 开头，不得以 / 结尾，不得包含连续的 //。
- 不得包含反斜杠、NUL、U+0000 至 U+001F 控制字符、冒号、双引号、尖括号、竖线、问号或星号。
- 每个路径段必须非空，不得为 . 或 ..，不得以空格或点结尾。
- 不得是 Windows drive、UNC、设备路径、URI 或任何绝对路径。
- 按 POSIX 路径规则规范化后必须与原字符串完全一致。
- HTTP 入参是 JSON 字符串，不做 URL decode；不得通过百分号编码绕过上述检查。

admin-api 做第一次词法校验，agent 必须独立做第二次相同校验。对会访问 MYFORGE_ROOT 的 artifactFile 和非 null rulesFile，agent 还必须：

1. 对 MYFORGE_ROOT 做 canonicalize，得到 rootReal。
2. 非 null rulesFile 必须已存在；canonicalize 后必须仍位于 rootReal 内且是普通文件。
3. artifactFile 的父目录可以尚不存在。agent 从 rootReal 起逐段检查所有已存在祖先；每个祖先 canonicalize 后必须仍位于 rootReal 内且是目录。首个不存在祖先之后的路径只按已校验相对段拼接，由 Codex 在执行期间创建。
4. artifactFile 已存在时，也必须 canonicalize 文件本身并确认仍位于 rootReal 内且是普通文件。
5. 任一已存在路径段经 symlink、junction 或 reparse point 解析到 rootReal 外时拒绝；执行结束后读取 artifact 时再次 canonicalize 并执行同一边界和普通文件检查。

consumerTargetFile 只执行词法校验，不与 MYFORGE_ROOT 拼接，不调用 canonicalize，不检查存在性。

## 5. Typed HTTP 创建请求

### 5.1 唯一请求结构

POST /api/v1/myforge/tasks/fangyuan-blueprint 的请求体必须是：

~~~json
{
  "agentId": "dev-pc-001",
  "projectId": "myforge-local",
  "artifactFile": "artifacts/fangyuan/home_preview.ron",
  "consumerTargetFile": "project/assets/fangyuan/home_preview.ron",
  "rulesFile": "rules/fangyuan/方圆灵构蓝图规则.md",
  "prompt": {
    "theme": "火属性洞府",
    "primitiveLimit": 200,
    "bounds": {
      "width": 40,
      "depth": 40,
      "height": 20
    },
    "requirements": [
      "中心有圆相炉心",
      "周围有方相阵基和三层平台",
      "不要生成地面以下几何体"
    ]
  }
}
~~~

不支持把 theme、primitiveLimit、bounds 或 requirements 平铺到顶层。请求体和 prompt 中的未知字段必须返回 INVALID_REQUEST，不能静默忽略。

无规则执行仍必须显式提交 `"rulesFile": null`。提供非 null 路径时，文件缺失必须返回 MYFORGE_RULES_FILE_MISSING，不能静默降级为无规则执行。

### 5.2 prompt 字段约束

| 字段 | 必填 | 约束 |
|------|------|------|
| theme | 是 | 去除首尾空白后 UTF-8 长度 1 至 200 字节；保存和签名前使用去除首尾空白后的值。 |
| primitiveLimit | 是 | JSON 整数，1 至 1000。1000 是当前方圆蓝图硬上限。 |
| bounds | 是 | 只允许 width、depth、height，三个字段均为 JSON 整数，范围 1 至 1000。 |
| requirements | 是 | 数组长度 1 至 32；每项去除首尾空白后 UTF-8 长度 1 至 500 字节；总 UTF-8 长度不超过 8192 字节；不得包含控制字符；规范化后不得重复。 |

allowedKinds 不由调用方提交，P0 固定为 cube 和 sphere。admin-api 生成提示词时必须固定加入以下约束：

- 只允许 cube 和 sphere。
- 不得超过 primitiveLimit 和 1000 的较小值。
- 不生成地面以下几何体。
- 不生成 rotation、quaternion、euler、angular_velocity、rotate、spin 或等价旋转字段。
- 不生成任意脚本、shader、外部贴图、外部模型路径、动态 VFX 或网络行为。
- 只允许修改 artifactFile 指定的目标产物。
- rulesFile 非 null 时必须依据指定规则副本；为 null 时固定提示“未提供规则文件”，只应用模板中的强制约束。

现有 Fastify 64 KiB body limit 继续适用。renderedPrompt 的 UTF-8 长度上限为 16 KiB，超限返回 MYFORGE_PROMPT_TOO_LARGE。

### 5.3 提示词和进程执行的唯一口径

admin-api 使用固定模板渲染 renderedPrompt。requirements 作为“业务约束数据”逐条编号插入固定段落，不能替换系统安全段落、rulesFile 或 artifactFile。

agent 不执行 commandPreview，也不使用 shell 拼接字符串。codex_exec profile 的权限模式只由 agent 本机严格布尔配置决定。默认 `MYFORGE_CODEX_DANGEROUS_FULL_ACCESS=false` 时等价于 Rust 的直接进程调用：

~~~text
executable: MYFORGE_CODEX_BIN，默认 codex
argv:
  exec
  --sandbox
  workspace-write
  --ephemeral
  --color
  never
  <renderedPrompt 作为单独一个 argv>
cwd:
  canonicalize(MYFORGE_ROOT)
shell:
  false
~~~

当本机明确设置 `MYFORGE_CODEX_DANGEROUS_FULL_ACCESS=true` 时，唯一参数差异是用单个 `--dangerously-bypass-approvals-and-sandbox` 替换 `--sandbox workspace-write`。这与本机 PowerShell 7 `codex-admin` 的核心权限参数等价，Codex 将不再受 MYFORGE_ROOT 工作区沙箱约束，属于对同一 OS 用户整机权限的完全信任。

P0 不从 HTTP 或 WebSocket 消息读取 executable、argv、cwd、环境变量、sandbox、dangerFullAccess、model、profile 配置名或 additional writable directory。不得通过远程消息加入或移除最高权限参数，也不得加入 --add-dir。执行环境继续使用固定 allowlist；其中 APPDATA、CODEX_HOME、HOME、USERPROFILE 用于复用运行 agent 的同一 Windows 用户的本机 Codex 认证，私钥、token 和 MYFORGE_* 控制变量不传给子进程。

MYFORGE_SHELL 是旧方案兼容配置，不是执行配置。agent 配置加载器必须能读取该可选字符串，去除首尾空白并拒绝控制字符或超过 64 字节的值；设置后只记录一次 MYFORGE_SHELL_IGNORED 脱敏 warning，并在配置摘要中显示 legacyShellConfigured=true，不显示原值。codex_exec、dry-run 和审核器都不得读取或使用它。P0 新部署应删除该变量，后续可以移除兼容解析。

commandPreview 可以按相同固定参数生成人类可读文本，但该文本不要求可被 shell 反向解析，且永远不能成为执行输入。任务 queued 且尚未绑定连接时显示 `danger_full_access=unresolved`；dispatch 时使用注册 capability 重写为精确 argv，并将 `danger_full_access=true/false` 快照落库和写入生命周期审计。

### 5.4 本地 dry-run 契约

dry-run 只能由 agent 本地环境变量 MYFORGE_DRY_RUN=true 启用，admin-api、admin-web 和 command.execute 都不得携带 dryRun、executionMode、替代 executable 或替代命令。agent 注册时 capabilities.dryRun 表示当前进程实际运行模式，不表示 server 可以远程切换。

dry-run 收到 command.execute 后仍必须完成验签、TTL、防重放、requestId 幂等、MYFORGE_ROOT、非 null rulesFile、artifactFile 已存在祖先和 typed input 全部校验。校验通过后：

1. 发送 command.started，executionMode=dry_run。
2. 不启动 Codex，不启动任何替代子进程，不运行审核器。
3. 不创建、覆盖、删除或修改 artifactFile 及 MYFORGE_ROOT 内任何文件。
4. 只读观察 artifactFile；已存在时返回实际 hash/bytes/modifiedAt，缺失时返回 exists=false 和其余摘要字段 null。
5. 返回 command.result，executionMode=dry_run、status=completed、exitCode=null、audit.status=skipped、audit.reasonCode=dry_run、errorCode/errorMessage=null。
6. stdoutPreview 使用固定格式 DRY_RUN_OK requestId=<UUID> artifactFile=<relative-path>，stderrPreview 为空；bytes 和 truncated 字段按普通结果规则计算。

dry-run 下 artifact 缺失不是 MYFORGE_TARGET_FILE_MISSING，因为该模式明确不生成文件；artifact 已存在也只能表示 pre-existing observation，不能声称由本任务生成。任何前置校验失败仍使用 command.error，不能用 dry-run 绕过安全边界。

MYFORGE_DRY_RUN=true 时 Codex binary probe 仍要执行并上报真实 capabilities.codexExec，但 binary 缺失不使 preflight 失败；server 只要 dryRun=true 就可以下发 typed task。MYFORGE_DRY_RUN=false 时 Codex 不可用是 MYFORGE_CODEX_UNAVAILABLE preflight 错误，agent 不注册。profiles 在两种模式都只报告 codex_exec，因为 dry_run 是该固定 profile 的本地执行模式，不是可由 server 选择的新 profile。

## 6. 身份、密钥和签名

### 6.1 配置和身份绑定

admin-api 配置示例：

~~~env
MYFORGE_ENABLED=true
MYFORGE_SERVER_PRIVATE_KEY_PATH=./keys/myforge_server_private.pem
MYFORGE_SERVER_PUBLIC_KEY_PATH=./keys/myforge_server_public.pem
MYFORGE_AGENT_PUBLIC_KEYS_JSON={"dev-pc-001":{"projectId":"myforge-local","publicKeyPath":"./keys/myforge_agent_dev_pc_001_public.pem","label":"开发机"}}
MYFORGE_AUTH_TTL_MS=60000
MYFORGE_COMMAND_TTL_MS=60000
MYFORGE_CLOCK_SKEW_MS=5000
MYFORGE_HEARTBEAT_INTERVAL_MS=15000
MYFORGE_HEARTBEAT_TIMEOUT_MS=45000
MYFORGE_QUEUE_TTL_MS=900000
MYFORGE_COMMAND_TIMEOUT_MS=600000
MYFORGE_CANCEL_TIMEOUT_MS=10000
MYFORGE_MAX_OUTPUT_BYTES=1048576
MYFORGE_WS_MAX_MESSAGE_BYTES=16777216
MYFORGE_WS_WRITE_TIMEOUT_MS=5000
~~~

apps/myforge-agent 配置示例：

~~~env
ADMIN_API_WS_URL=wss://example.com/api/v1/myforge/ws
MYFORGE_AGENT_ID=dev-pc-001
MYFORGE_PROJECT_ID=myforge-local
MYFORGE_AGENT_PRIVATE_KEY_PATH=./keys/myforge_agent_private.pem
MYFORGE_AGENT_PUBLIC_KEY_PATH=./keys/myforge_agent_public.pem
MYFORGE_SERVER_PUBLIC_KEY_PATH=./keys/myforge_server_public.pem
MYFORGE_ROOT=C:\project\myforge
MYFORGE_CODEX_BIN=C:\path\to\codex.exe
MYFORGE_CODEX_DANGEROUS_FULL_ACCESS=false
MYFORGE_AUTH_TTL_MS=60000
MYFORGE_COMMAND_TTL_MS=60000
MYFORGE_CLOCK_SKEW_MS=5000
MYFORGE_HEARTBEAT_INTERVAL_MS=15000
MYFORGE_MAX_COMMAND_TIMEOUT_MS=600000
MYFORGE_CANCEL_TIMEOUT_MS=10000
MYFORGE_MAX_OUTPUT_BYTES=1048576
MYFORGE_WS_MAX_MESSAGE_BYTES=16777216
MYFORGE_WS_WRITE_TIMEOUT_MS=5000
MYFORGE_DRY_RUN=false
MYFORGE_AUDIT_ENABLED=false
MYFORGE_AUDIT_PROGRAM=tools/fangyuan-audit
MYFORGE_AUDIT_TIMEOUT_MS=30000
# MYFORGE_SHELL=powershell
~~~

MYFORGE_AGENT_PUBLIC_KEYS_JSON 的 key 是全局唯一 agentId。value 中 projectId 与 WebSocket query、消息体和任务 projectId 必须全部相同。publicKeyPath 相对路径按 admin-api 进程 cwd 解析。

“known agent”的唯一权威是当前成功解析的 MYFORGE_AGENT_PUBLIC_KEYS_JSON，不是 myforge_agents 历史行，也不要求 agent 曾经注册。admin-api 每次启动时必须在接受 HTTP 请求前：

1. 在内存中完整解析当前配置，逐项加载公钥、验证 Ed25519 类型并计算 fingerprint；任一项非法时启动失败且不修改数据库。
2. 配置全部有效后开启数据库事务，将 myforge_agents 现有行统一标记 configured=false、status=offline。
3. 在同一事务按 agentId upsert projectId、label、fingerprint、configured=true、status=offline；保留历史 last_seen_at。
4. 提交事务后才开始接受 HTTP 和 WebSocket 请求，不能只跳过坏 agent。

因此，配置存在但从未连接的 agent 会出现在 GET /myforge/agents 中，status=offline，hostname/platform/agentVersion/forgeRootSummary/capabilities/lastSeenAt 全部为 null，并且可以创建 queued、queueReason=agent_offline 的任务。配置中不存在的 agent 即使数据库有历史行也不是 known agent，不能连接或创建任务，创建接口返回 MYFORGE_AGENT_NOT_FOUND。默认 agent 列表只返回 configured=true；历史任务仍保留被移除 agentId。

#### 6.1.1 安全布尔解析

MYFORGE_ENABLED、MYFORGE_DRY_RUN、MYFORGE_AUDIT_ENABLED、MYFORGE_CODEX_DANGEROUS_FULL_ACCESS 不得复用会把未知字符串静默转成 false 的宽松 parseBoolean。四者使用独立 strictBoolean：

- 只有环境变量 key 完全缺失时使用默认值；四者默认均为 false。key 已存在但去除首尾 ASCII whitespace 后为空属于非法配置。
- 非空值只接受大小写敏感的 true、false、1、0；true/1 为 true，false/0 为 false。
- TRUE、False、yes、on、tru、任意其他非空值必须以 MYFORGE_CONFIG_INVALID 使 admin-api 启动或 agent preflight/--check 失败。
- 尤其 MYFORGE_DRY_RUN 和 MYFORGE_CODEX_DANGEROUS_FULL_ACCESS 的非法值必须 fail closed，不能回退后改变是否真实执行或执行权限。

配置测试至少覆盖缺失默认、空字符串拒绝、四个合法值、大小写错误，以及两个 agent 执行开关的 `tru`。错误日志只记录变量名和“invalid boolean”，不回显其他配置。

#### 6.1.2 数值范围和协商

所有数值环境变量只接受 ^[0-9]+$ 十进制安全整数，不接受符号、空格、单位、小数或指数。缺失使用下表默认值，越界以 MYFORGE_CONFIG_INVALID 失败。

| 配置 | 端点 | 默认 | 合法闭区间 |
|------|------|------|------------|
| MYFORGE_AUTH_TTL_MS | 双端 | 60000 | 5000..300000 |
| MYFORGE_COMMAND_TTL_MS | 双端 | 60000 | 5000..300000 |
| MYFORGE_CLOCK_SKEW_MS | 双端 | 5000 | 0..30000 |
| MYFORGE_HEARTBEAT_INTERVAL_MS | 双端 | 15000 | 1000..60000 |
| MYFORGE_HEARTBEAT_TIMEOUT_MS | server | 45000 | 3000..180000 |
| MYFORGE_QUEUE_TTL_MS | server | 900000 | 10000..86400000 |
| MYFORGE_COMMAND_TIMEOUT_MS | server | 600000 | 1000..1800000 |
| MYFORGE_MAX_COMMAND_TIMEOUT_MS | agent | 600000 | 1000..1800000 |
| MYFORGE_CANCEL_TIMEOUT_MS | 双端 | 10000 | 1000..30000 |
| MYFORGE_MAX_OUTPUT_BYTES | 双端 | 1048576 | 4096..4194304 |
| MYFORGE_WS_MAX_MESSAGE_BYTES | 双端 | 16777216 | 524288..33554432 |
| MYFORGE_WS_WRITE_TIMEOUT_MS | 双端 | 5000 | 1000..30000 |
| MYFORGE_AUDIT_TIMEOUT_MS | agent | 30000 | 1000..120000 |

本地启动不变量：

- 2 * MYFORGE_CLOCK_SKEW_MS 必须小于 AUTH_TTL 和 COMMAND_TTL。
- server 的 HEARTBEAT_TIMEOUT 必须大于等于 2 * HEARTBEAT_INTERVAL + CLOCK_SKEW。
- server 的 CANCEL_TIMEOUT 必须小于等于 COMMAND_TIMEOUT；agent 的 CANCEL_TIMEOUT 必须小于等于 MAX_COMMAND_TIMEOUT。
- WS_MAX_MESSAGE_BYTES 必须大于等于 RESULT_FIXED_RESERVE_BYTES + 12 * 4096 = 311296；表中更高的最小值保留额外协议余量。16 MiB 默认值可在最坏转义下保留 stdout/stderr 各 1 MiB 的默认上限。
- WS_WRITE_TIMEOUT 必须小于 AUTH_TTL 和 COMMAND_TTL。

server.challenge 和 agent.register 必须分别签名携带各自 limits。P0 握手兼容规则：

1. HEARTBEAT_INTERVAL 必须完全相同；不同则发送 MYFORGE_LIMIT_MISMATCH 并关闭，不能进入持续 heartbeat timeout 循环。
2. server.challenge 的 challengeLifetimeMs = expiresAtMs - timestampMs 必须精确等于 server 本地 AUTH_TTL，并在签名 limits.authTtlMs 中宣告同一值。
3. agent 验证 challenge 签名后、发送 hello 前，必须检查 challengeLifetimeMs 和 limits.authTtlMs 相等且不超过 agent 本地 AUTH_TTL。超过时 agent 使用 agent 私钥发送 protocol.error MYFORGE_LIMIT_MISMATCH：connectionId=challengeId、requestId=null，error message TTL 取 min(agent 本地 AUTH_TTL, challenge 宣告 AUTH_TTL)，随后关闭；不得继续 hello。
4. challenge 通过后，agentHelloRegisterTtlMs = min(agent 本地 AUTH_TTL, challenge.limits.authTtlMs)。agent.hello 和紧随其后的 agent.register 都必须精确使用 expiresAtMs=timestampMs+agentHelloRegisterTtlMs，包括 register 尚未被 server 接受的阶段。
5. server 对 hello/register 先使用自身本地 AUTH_TTL 作为接收上限，再验证消息 lifetime 精确等于 challenge 中宣告的 server AUTH_TTL。register.limits.authTtlMs 还必须大于等于该 challenge TTL；不满足时返回 MYFORGE_LIMIT_MISMATCH 并关闭。
6. register 成功后，每类后续消息的发送 TTL 为 min(sender 本地 TTL, peer advertised TTL)。接收方始终把自己的本地 TTL 视为最大允许值；消息 expiresAtMs-timestampMs 超过本地上限时返回 MYFORGE_LIMIT_MISMATCH。
7. 时间戳是否过期仍只使用接收方本地 CLOCK_SKEW。两台机器的实际时钟偏差必须不大于 min(server skew, agent skew)；否则返回 MYFORGE_MESSAGE_EXPIRED。P0 推荐双端配置相同 skew，但不要求数值相等。
8. effectiveWsMax = min(server ws max, agent ws max)。
9. RESULT_FIXED_RESERVE_BYTES 固定为 262144。effectiveMaxOutput = min(server max output, agent max output, floor((effectiveWsMax - RESULT_FIXED_RESERVE_BYTES) / 12))。stdoutPreview 和 stderrPreview 各自最多 effectiveMaxOutput 个“最终合法 UTF-8、尚未 JSON 转义”的字节；每字节最坏转成六字节 \u00XX，两路合计为 12 倍。effectiveMaxOutput 必须至少 4096，否则 MYFORGE_LIMIT_MISMATCH。
10. 每个 task 的 timeoutMs = min(server COMMAND_TIMEOUT, agent MAX_COMMAND_TIMEOUT)，maxOutputBytes = effectiveMaxOutput；两者在 dispatched claim 时持久化并下发。agent 再以本地上限校验，不得自行放大。
11. cancelTimeoutMs = min(server cancel timeout, agent cancel timeout)，server 用它生成并持久化 cancel deadline。

真实 Codex 默认命令上限为 600000 毫秒。该默认值用于覆盖高推理强度模型生成结构化蓝图时可能超过两分钟的正常执行；部署方仍可在 1000..1800000 毫秒范围内按模型、provider 和任务复杂度双端协商调整。server 和 agent 必须同时配置，最终任务只使用两端较小值。

静态不兼容在握手阶段使用 protocol.error MYFORGE_LIMIT_MISMATCH，并在双端脱敏日志记录字段名、local、peer 和 effective 值。已注册连接收到超出协商 timeout、output、TTL 或 frame 的消息时同样返回该错误并关闭，避免两个配置不同的实例持续互拒而没有诊断。

双端配置测试至少覆盖：默认值可握手、heartbeat interval 不同被拒、server challenge TTL 超过 agent max 被拒、hello/register TTL 精确使用双方较小值、后续 TTL 取较小值、command timeout 取较小值、output 受较小 frame 反向收紧，以及低到无法得到 4096 字节 output budget 时明确拒绝。输出边界测试必须构造两路全为 U+0000 的 preview，验证 12 倍转义公式在等号边界恰好通过、增加 1 字节即被截断，并验证默认 16 MiB frame 保留每路 1 MiB budget。

### 6.2 密钥格式

P0 固定使用 Ed25519：

- 私钥必须是 PKCS#8 PEM，头为 -----BEGIN PRIVATE KEY-----。
- 公钥必须是 SubjectPublicKeyInfo PEM，头为 -----BEGIN PUBLIC KEY-----。
- 不接受 OpenSSH 公钥、raw seed、PKCS#1、RSA 或 ECDSA。
- signature 是 64 字节 Ed25519 签名，编码使用 RFC 4648 base64url，无 padding。
- public_key_fingerprint 为公钥 SPKI DER 的 SHA-256 小写十六进制。
- 私钥不得写入日志、数据库、WebSocket、HTTP 响应或 Git。

### 6.3 规范化签名载荷

每条应用层 WebSocket JSON 消息都必须签名。签名输入按以下唯一算法生成：

1. 使用能检测重复 object member name 的 UTF-8 JSON decoder 解析完整消息；任何层级出现重复 key 时立即拒绝，不能采用 first-wins 或 last-wins。
2. 只移除顶层 signature 字段；嵌套同名字段不作特殊处理。
3. 使用 RFC 8785 JSON Canonicalization Scheme 对剩余 object 规范化。
4. 所有 JSON number token 必须匹配 0|-?[1-9][0-9]*，数值绝对值不得超过 9007199254740991；拒绝 -0、小数、指数、NaN、Infinity 和超出安全整数范围的值。
5. 不做 Unicode NFC/NFD 转换，使用 JSON 中原始 Unicode scalar value。
6. 在规范化 JSON UTF-8 字节前添加 ASCII 域分隔前缀 MYFORGE-WS-V1 和一个 LF 字节。
7. 使用发送方 Ed25519 私钥对完整字节序列签名，并将签名编码为无 padding base64url。

概念表达：

~~~text
signingBytes = UTF8("MYFORGE-WS-V1\n") || JCS(message without top-level signature)
signature = BASE64URL_NOPAD(Ed25519.Sign(privateKey, signingBytes))
~~~

P0 签名 JSON 必须满足 RFC 7493 I-JSON 可互操作约束：frame 必须是合法 UTF-8，string 和 key 只能包含 Unicode scalar value；拒绝 lone high/low surrogate、错误 surrogate pair、无效 UTF-8 和不能由 Unicode scalar value 表示的输入。JSON 属性顺序和空白不参与语义，但经过 JCS 后必须产生完全相同的字节。接收方先按消息声明的身份选择公钥，再使用同一算法验签。unknown field 仍参与签名，验签成功后再由严格 schema 拒绝；不能在签名前丢弃 unknown field。

发送方生成 signature 后，必须对“含 signature 的完整 object”再次做 JCS，并把该紧凑 UTF-8 结果作为最终 WebSocket text frame；不得 pretty-print 或附加前后空白。签名验证仍按第 2 步移除顶层 signature 后重算。该规则使 frame 字节数和第 6.1.2/8.8 节预算可确定。

Node.js 与 Rust 不得直接使用会静默覆盖 duplicate key 的默认 object parser 作为唯一入口。两端共识测试必须使用固定 Ed25519 fixture key，至少覆盖：

- 中文、有效非 BMP Unicode 字符及转义/未转义的等价输入。
- 嵌套 object、array、字段乱序、空白差异和 null。
- duplicate key 在顶层及嵌套层均拒绝。
- lone surrogate、无效 UTF-8、浮点、指数、-0 和超安全整数均拒绝。
- Node.js 生成的 signingBytes 十六进制和签名由 Rust 验证，Rust 生成的结果由 Node.js 验证。
- 修改任一业务字段、connectionId、timestampMs、expiresAtMs 或 nonce 后验签失败。

测试向量必须断言 exact signingBytes，不得只断言“各自签名后各自能验证”。

### 6.4 时间、TTL、nonce 和防重放

每条签名消息都包含：

- protocolVersion：固定整数 1。
- timestampMs：发送方生成消息时的 Unix epoch 毫秒。
- expiresAtMs：消息过期时间。
- nonce：恰好 16 个加密安全随机字节的无 padding base64url。
- signature：发送方签名。

接收顺序为：frame 大小和 UTF-8/I-JSON 词法检查、只解析选择公钥所需字段、JCS/验签、完整严格 schema、时间窗口、connectionId、nonce 防重放、业务处理。nonce 只能在验签、schema、时间和 connectionId 全部通过后写入缓存，防止伪造或无效消息污染缓存。

有效消息必须满足：

~~~text
timestampMs <= nowMs + MYFORGE_CLOCK_SKEW_MS
expiresAtMs >= nowMs - MYFORGE_CLOCK_SKEW_MS
0 < expiresAtMs - timestampMs <= 接收方本地对应 TTL 上限
~~~

challenge、hello、register、heartbeat、started、result、error 使用 MYFORGE_AUTH_TTL_MS。execute 和 cancel 使用 MYFORGE_COMMAND_TTL_MS。

双方都必须维护接收方向的 replay cache：

- admin-api 对 agent 消息使用 connectionId + projectId + agentId + nonce 作为 key；握手 hello 使用 challengeId + projectId + agentId + nonce。
- myforge-agent 对 server 消息使用 connectionId + serverPublicKeyFingerprint + nonce 作为 key；初始 challenge 使用 challengeId + serverPublicKeyFingerprint + nonce。
- value 至少保存到 expiresAtMs + MYFORGE_CLOCK_SKEW_MS。缓存是进程级而不是 socket 级，连接关闭或重连时不能提前删除尚未过期 entry。
- check-and-insert 必须对同一 key 原子化；并发到达的同 nonce 消息最多一条可以通过。
- 同 key 再次出现返回 MYFORGE_REPLAY_DETECTED，且不得进入业务处理。
- 进程重启会清空内存 cache，但每次连接都使用新的随机 challengeId 作为 connectionId，旧连接消息因 connectionId 不匹配而拒绝；server 持久化 requestId 状态和第 10 节幂等规则继续阻止业务重复。

requestId 是业务幂等键，不能代替 nonce；nonce 是单条消息防重放键，不能代替 requestId。replay cache 与 requestId 幂等必须同时实现。

## 7. WebSocket 接入和握手

### 7.1 连接要求

agent 主动连接：

~~~text
GET /api/v1/myforge/ws?agentId=dev-pc-001&projectId=myforge-local
Sec-WebSocket-Protocol: myserver.myforge.v1
~~~

- 非本地环境必须使用 WSS。
- query 中 agentId 和 projectId 必须通过第 4 节格式校验，并存在于公钥配置。
- agent WebSocket 不使用管理员 JWT。
- admin-api 的 TLS 和 IP allowlist 控制面策略同样适用于 WebSocket upgrade。
- 未协商 myserver.myforge.v1 子协议时拒绝 upgrade。
- 单条 text frame 超过 MYFORGE_WS_MAX_MESSAGE_BYTES 时关闭连接。
- P0 只接受 UTF-8 text JSON frame，不接受 binary frame。

### 7.2 握手状态

连接状态严格为：

~~~text
connected -> challenged -> authenticated -> registered -> closed
~~~

server.challenge、agent.hello、agent.register 和 protocol.error 用于握手阶段。只有 registered 连接可以收发 heartbeat、execute、cancel、started、result、command.error 和后续 protocol.error。

1. upgrade 完成后 admin-api 发送 server.challenge。
2. agent 验证 server 签名、时间和 query identity 后发送 agent.hello。
3. admin-api 验证 agent 签名、challenge 和 identity，将 challenge 标记为已消费。
4. 双方将本次 challengeId 固定为 connectionId；该值只在当前 WebSocket 连接有效。
5. agent 在同一连接发送带 connectionId 的 agent.register。
6. admin-api 验证并持久化 metadata，将连接标记 registered/online。
7. agent 按 heartbeat interval 发送 agent.heartbeat。

hello 必须在 challenge 过期前到达；challenge 只能消费一次。register 必须在 hello 后 10 秒内到达。乱序、重复 hello 或未注册先发业务消息均返回协议错误并关闭连接。

agent.register 及其后的每条双向应用消息都必须包含 connectionId，并与 socket 当前 challengeId 完全一致。旧 connectionId 即使签名和 TTL 仍有效，也不能用于新 socket。同一 agentId 只允许一个 registered 连接。新的合法连接完成注册后替换旧连接并关闭旧连接；旧连接的 close handler 不得把新连接误标为 offline。若旧连接有进行中任务，按第 9 节的断线和取消规则处理；禁止在新连接上自动重放。

### 7.3 连接队列和并发模型

WebSocket 保证 frame 到达顺序，但实现不得为每个 message event 启动互不等待的异步 handler。双端每个 socket 都必须建立：

- 一个容量固定为 64 个完整 frame 的 inbound FIFO 和唯一 dispatcher。dispatcher 按 frame 到达顺序逐条完成验签、schema、replay 检查和该消息的状态提交后，才处理下一条。
- 一个容量固定为 64 个完整 frame 的 outbound FIFO 和唯一 writer。只有 writer 可以调用底层 WebSocket send；所有 challenge/hello/register/heartbeat/execute/cancel/started/result/error 都经同一队列，禁止绕过、插队、丢弃或按消息类型重排。
- 每个 outbound item 带 completion future；只有 WebSocket 库确认完整 text frame 已接受写入后才算 send success，并且等待不得超过发送端 MYFORGE_WS_WRITE_TIMEOUT_MS。超时视为 writer failure 并关闭连接。send success 不表示 peer 已处理，业务确认仍由 started/result 等协议消息完成。
- 队列满时施加 backpressure，不得丢 frame 或另开并发 handler/send；无法在 MYFORGE_WS_WRITE_TIMEOUT_MS 内入 outbound queue，或传输库无法暂停已满的 inbound queue 时，关闭连接。连接关闭或 writer 失败时，所有未完成 future 以 send failure 完成。

admin-api 的同一 registered connection 还必须有一个 connection operation mutex。调度器和 cancel API 使用同一锁序：

1. 获取 connection operation mutex。
2. 锁定/条件更新 task row。
3. 提交数据库状态。
4. 将对应 execute 或 cancel 加入该 connection 的 outbound FIFO，并等待 writer completion。
5. 处理 send success/failure后释放 mutex。

因此有且只有两种竞态结果：

- cancel 先获得锁且 task 仍 queued：直接提交 queued -> cancelled，调度器后续 claim 失败，不发送 execute 或 cancel。
- dispatch 先获得锁：必须在 execute frame 写入成功或明确失败后才释放；cancel 随后获得锁时，若仍可取消，只能把 cancel 排在已成功写入的 execute 后。

execute 在 writer completion 前失败，task 转 failed/MYFORGE_DISPATCH_FAILED；completion 成功后再断线使用 MYFORGE_AGENT_DISCONNECTED。cancel 在 writer completion 前失败，task 转 failed/MYFORGE_CANCEL_DELIVERY_FAILED；completion 成功后、cancelled result 前断线使用 MYFORGE_CANCEL_UNCONFIRMED。

agent 的 inbound dispatcher 收到合法 command.execute 后，只同步登记 active request 并启动一个受所有权管理的 execution worker，然后返回处理下一条 frame；不得在 dispatcher 内等待 Codex 完成，否则后续 cancel 无法及时处理。execution worker 在 codex_exec 模式成功启动子进程、或在 dry_run 模式完成全部前置校验并进入模拟步骤后，将 command.started 加入 outbound writer 并等待 send success；随后等待子进程或执行只读观察、artifact 和 audit，最终经同一 writer 发送 result。started send 失败时必须终止刚启动的子进程并关闭连接。cancel handler 串行定位 active request 并向 worker 发终止信号。

admin-api 的 inbound dispatcher 必须完整等待 hello -> register、started -> result 及每次 heartbeat/result 的数据库事务提交，不得并发修改同一 task。高成本 Codex、artifact hash 和 audit 已在 agent worker 完成，server handler 只做有界验签、schema 和数据库操作。不同 socket 的 dispatcher/writer 可以并行；共享 task 仍由 connectionId 校验和数据库 row lock 保护。

并发测试必须使用可暂停的 fake writer/handler：阻塞 execute writer completion 后并发调用 cancel，断言 wire 顺序只能是 execute -> cancel；让 started DB handler 暂停并立即注入 result，断言 result 只有在 started 提交后处理；另用两个 socket 证明不同 connection 可以并行。测试还要覆盖 execute/cancel enqueue 失败分别落 MYFORGE_DISPATCH_FAILED/MYFORGE_CANCEL_DELIVERY_FAILED。

## 8. WebSocket 消息契约

以下示例中的 timestampMs、expiresAtMs、nonce 和 signature 均为必填。除明确标记为 null 的字段外，不得使用 null 代替缺失字段。所有消息使用严格 schema，unknown field 拒绝。

示例复用 requestId 便于对照字段，但 command.cancel 分支与 completed result 分支是互斥示例，不表示同一任务先取消后又完成。

### 8.1 server.challenge

方向：admin-api -> myforge-agent。使用 server 私钥签名。

~~~json
{
  "protocolVersion": 1,
  "type": "server.challenge",
  "challengeId": "67da7da9-a653-4d6e-9e81-f5f8baf874bb",
  "challenge": "base64url-random-32-bytes",
  "agentId": "dev-pc-001",
  "projectId": "myforge-local",
  "limits": {
    "authTtlMs": 60000,
    "commandTtlMs": 60000,
    "clockSkewMs": 5000,
    "heartbeatIntervalMs": 15000,
    "heartbeatTimeoutMs": 45000,
    "commandTimeoutMs": 600000,
    "cancelTimeoutMs": 10000,
    "maxOutputBytes": 1048576,
    "wsMaxMessageBytes": 16777216
  },
  "timestampMs": 1783694400000,
  "expiresAtMs": 1783694460000,
  "nonce": "base64url-random-16-bytes",
  "signature": "base64url-ed25519-signature"
}
~~~

challengeId 为小写 UUID v4，challenge 为 32 个随机字节。两者都必须与当前 socket 上 server 保存的 challenge 完全一致。limits 是 server 本地已验证配置的只读快照，参与签名；agent 必须在发送 hello 前执行第 6.1.2 节兼容检查。

### 8.2 agent.hello

方向：myforge-agent -> admin-api。使用 agent 私钥签名。

~~~json
{
  "protocolVersion": 1,
  "type": "agent.hello",
  "challengeId": "67da7da9-a653-4d6e-9e81-f5f8baf874bb",
  "challenge": "base64url-random-32-bytes",
  "agentId": "dev-pc-001",
  "projectId": "myforge-local",
  "timestampMs": 1783694401000,
  "expiresAtMs": 1783694461000,
  "nonce": "base64url-random-16-bytes",
  "signature": "base64url-ed25519-signature"
}
~~~

消息 identity、WebSocket query、公钥映射和 challenge identity 必须四者一致。expiresAtMs-timestampMs 必须精确等于第 6.1.2 节 agentHelloRegisterTtlMs；server 只按自身 AUTH_TTL 上限及当前 challenge 宣告值验证，不依赖尚未收到的 agent limits。

### 8.3 agent.register

方向：myforge-agent -> admin-api。使用 agent 私钥签名。

~~~json
{
  "protocolVersion": 1,
  "type": "agent.register",
  "connectionId": "67da7da9-a653-4d6e-9e81-f5f8baf874bb",
  "agentId": "dev-pc-001",
  "projectId": "myforge-local",
  "hostname": "DESKTOP-1LG9IK4",
  "platform": "windows",
  "agentVersion": "0.1.0",
  "forgeRootSummary": {
    "name": "myforge",
    "configured": true
  },
  "capabilities": {
    "profiles": ["codex_exec"],
    "codexExec": true,
    "fangyuanBlueprint": true,
    "audit": "unavailable",
    "dryRun": false,
    "dangerFullAccess": false,
    "maxConcurrentTasks": 1
  },
  "limits": {
    "authTtlMs": 60000,
    "commandTtlMs": 60000,
    "clockSkewMs": 5000,
    "heartbeatIntervalMs": 15000,
    "maxCommandTimeoutMs": 600000,
    "cancelTimeoutMs": 10000,
    "maxOutputBytes": 1048576,
    "wsMaxMessageBytes": 16777216
  },
  "timestampMs": 1783694402000,
  "expiresAtMs": 1783694462000,
  "nonce": "base64url-random-16-bytes",
  "signature": "base64url-ed25519-signature"
}
~~~

platform 只允许 windows、linux、macos。audit 只允许 available 或 unavailable。dryRun 和 dangerFullAccess 必须分别等于当前进程 MYFORGE_DRY_RUN 与 MYFORGE_CODEX_DANGEROUS_FULL_ACCESS 的解析结果，连接存续期间不得切换。forgeRootSummary 不得包含绝对路径；name 是 MYFORGE_ROOT 的末级目录名，长度 1 至 128 字节。register 的 expiresAtMs-timestampMs 与 hello 使用同一个 agentHelloRegisterTtlMs。limits 是 agent 本地已验证上限的只读快照，参与签名；server 在把连接标记 registered 前完成协商并把 effective limits 保存在连接上下文。server 只保存和展示摘要。

P0 maxConcurrentTasks 固定为 1。capabilities.codexExec 是 binary probe 的真实结果；dryRun=false 时必须为 true，dryRun=true 时可为 true 或 false。capabilities.dangerFullAccess 只用于展示、dispatch 时生成精确 commandPreview 和审计快照；它不能被 server 回写或扩大 server 允许的 profile。

### 8.4 agent.heartbeat

方向：myforge-agent -> admin-api。使用 agent 私钥签名。

~~~json
{
  "protocolVersion": 1,
  "type": "agent.heartbeat",
  "connectionId": "67da7da9-a653-4d6e-9e81-f5f8baf874bb",
  "agentId": "dev-pc-001",
  "projectId": "myforge-local",
  "sequence": 12,
  "state": "running",
  "activeRequestId": "2d0465b1-dc92-46d2-bc45-c90ed9724f5a",
  "timestampMs": 1783694415000,
  "expiresAtMs": 1783694475000,
  "nonce": "base64url-random-16-bytes",
  "signature": "base64url-ed25519-signature"
}
~~~

sequence 是 0 至 2147483647 的整数，可回绕。state 只允许 idle 或 running。idle 时 activeRequestId 必须为 null；running 时必须是当前 requestId。

超过 MYFORGE_HEARTBEAT_TIMEOUT_MS 未收到有效 heartbeat，server 将 agent 标记 offline 并关闭连接。WebSocket ping/pong 可以用于传输层保活，但不能替代签名 heartbeat。

### 8.5 command.execute

方向：admin-api -> myforge-agent。使用 server 私钥签名。

~~~json
{
  "protocolVersion": 1,
  "type": "command.execute",
  "connectionId": "67da7da9-a653-4d6e-9e81-f5f8baf874bb",
  "requestId": "2d0465b1-dc92-46d2-bc45-c90ed9724f5a",
  "taskType": "fangyuan.blueprint.generate",
  "agentId": "dev-pc-001",
  "projectId": "myforge-local",
  "profile": "codex_exec",
  "input": {
    "artifactFile": "artifacts/fangyuan/home_preview.ron",
    "consumerTargetFile": "project/assets/fangyuan/home_preview.ron",
    "rulesFile": "rules/fangyuan/方圆灵构蓝图规则.md",
    "prompt": {
      "theme": "火属性洞府",
      "primitiveLimit": 200,
      "bounds": {
        "width": 40,
        "depth": 40,
        "height": 20
      },
      "requirements": [
        "中心有圆相炉心",
        "周围有方相阵基和三层平台"
      ]
    },
    "renderedPrompt": "由 admin-api 固定模板生成的完整提示词"
  },
  "timeoutMs": 600000,
  "maxOutputBytes": 1048576,
  "timestampMs": 1783694420000,
  "expiresAtMs": 1783694480000,
  "nonce": "base64url-random-16-bytes",
  "signature": "base64url-ed25519-signature"
}
~~~

consumerTargetFile 未提交时必须在签名消息中显式为 null；rulesFile 无规则执行时也必须显式为 null。timeoutMs 和 maxOutputBytes 必须与当前 connection effectiveLimits.commandTimeoutMs/maxOutputBytes 完全相同；仅“小于本地上限”不足以通过，防止两端对 task watchdog 和截断边界理解不同。不匹配返回 MYFORGE_LIMIT_MISMATCH。cwd、command、args、shell、dryRun 和 dangerFullAccess 字段在该 schema 中不存在。

agent 必须在执行前重新校验 taskType、profile、identity、路径、prompt 限制、TTL 和 requestId 幂等性。任一失败不得启动子进程。

### 8.6 command.started

方向：myforge-agent -> admin-api。使用 agent 私钥签名。codex_exec 只在子进程成功启动后发送；dry_run 在全部前置校验通过、进入无副作用模拟步骤时发送。

~~~json
{
  "protocolVersion": 1,
  "type": "command.started",
  "connectionId": "67da7da9-a653-4d6e-9e81-f5f8baf874bb",
  "requestId": "2d0465b1-dc92-46d2-bc45-c90ed9724f5a",
  "agentId": "dev-pc-001",
  "projectId": "myforge-local",
  "executionMode": "codex_exec",
  "startedAtMs": 1783694422000,
  "timestampMs": 1783694422000,
  "expiresAtMs": 1783694482000,
  "nonce": "base64url-random-16-bytes",
  "signature": "base64url-ed25519-signature"
}
~~~

executionMode 只允许 codex_exec 或 dry_run，并且必须与本连接 agent.register 的 dryRun 值对应。server 在接收时记 receiveNowMs；command.started 必须同时满足：

~~~text
dispatchedAtMs - MYFORGE_CLOCK_SKEW_MS <= startedAtMs
startedAtMs <= receiveNowMs + MYFORGE_CLOCK_SKEW_MS
startedAtMs <= commandExecuteExpiresAtMs + MYFORGE_CLOCK_SKEW_MS
~~~

agent 必须在 command.execute 尚未过期时决定启动；startedAtMs 晚于接收窗口、早于下发容差窗口或属于另一 executionMode 时拒绝并记录 MYFORGE_PROTOCOL_STATE_INVALID。

### 8.7 command.cancel

方向：admin-api -> myforge-agent。使用 server 私钥签名。

~~~json
{
  "protocolVersion": 1,
  "type": "command.cancel",
  "connectionId": "67da7da9-a653-4d6e-9e81-f5f8baf874bb",
  "requestId": "2d0465b1-dc92-46d2-bc45-c90ed9724f5a",
  "agentId": "dev-pc-001",
  "projectId": "myforge-local",
  "reasonCode": "ADMIN_CANCELLED",
  "cancelRequestedAtMs": 1783694430000,
  "cancelDeadlineAtMs": 1783694440000,
  "timestampMs": 1783694430000,
  "expiresAtMs": 1783694440000,
  "nonce": "base64url-random-16-bytes",
  "signature": "base64url-ed25519-signature"
}
~~~

reasonCode P0 固定为 ADMIN_CANCELLED。cancelRequestedAtMs/cancelDeadlineAtMs 必须分别等于 server 持久化的 cancel_requested_at/cancel_deadline_at，且 deadline-requestedAt 必须等于当前 connection effectiveLimits.cancelTimeoutMs；timestampMs >= cancelRequestedAtMs、timestampMs < cancelDeadlineAtMs，expiresAtMs 必须小于等于 cancelDeadlineAtMs。agent 接收时若 nowMs > cancelDeadlineAtMs + clock skew，必须返回 MYFORGE_MESSAGE_EXPIRED 且不把过期 cancel 当作已确认终止。不匹配协商值返回 MYFORGE_LIMIT_MISMATCH。消息不包含管理员用户名或自由文本原因，避免把控制面身份数据带入本地执行器。

### 8.8 command.result

方向：myforge-agent -> admin-api。使用 agent 私钥签名。

~~~json
{
  "protocolVersion": 1,
  "type": "command.result",
  "connectionId": "67da7da9-a653-4d6e-9e81-f5f8baf874bb",
  "requestId": "2d0465b1-dc92-46d2-bc45-c90ed9724f5a",
  "agentId": "dev-pc-001",
  "projectId": "myforge-local",
  "executionMode": "codex_exec",
  "status": "completed",
  "exitCode": 0,
  "stdoutPreview": "生成完成",
  "stderrPreview": "",
  "stdoutBytes": 12,
  "stderrBytes": 0,
  "stdoutTruncated": false,
  "stderrTruncated": false,
  "artifactFile": "artifacts/fangyuan/home_preview.ron",
  "consumerTargetFile": "project/assets/fangyuan/home_preview.ron",
  "artifact": {
    "exists": true,
    "sha256": "64-char-lowercase-hex",
    "bytes": 12345,
    "modifiedAtMs": 1783694450000
  },
  "audit": {
    "status": "passed",
    "errors": 0,
    "warnings": 0,
    "primitiveCount": 180,
    "mainCode": null,
    "reasonCode": null,
    "findingsPreview": []
  },
  "errorCode": null,
  "errorMessage": null,
  "startedAtMs": 1783694422000,
  "completedAtMs": 1783694450000,
  "timestampMs": 1783694451000,
  "expiresAtMs": 1783694511000,
  "nonce": "base64url-random-16-bytes",
  "signature": "base64url-ed25519-signature"
}
~~~

command.result 的以下顶层字段在每种状态都必须出现，不能靠缺失表示 null：executionMode、status、exitCode、stdoutPreview、stderrPreview、stdoutBytes、stderrBytes、stdoutTruncated、stderrTruncated、artifactFile、consumerTargetFile、artifact、audit、errorCode、errorMessage、startedAtMs、completedAtMs 及签名 envelope。

- executionMode 只允许 codex_exec 或 dry_run，并与 command.started 和本连接注册能力一致。
- stdoutPreview/stderrPreview 始终为 string；stdoutBytes/stderrBytes 始终为非负安全整数；truncated 始终为 boolean。bytes 是截断前原始字节数。agent 先把无效 UTF-8 替换为 U+FFFD，再按最终 string 的 UTF-8 字节截断；每个 preview 分别最多 effective maxOutputBytes，计数点位于 JSON/JCS 转义之前。
- artifactFile 必须与 task 完全一致；consumerTargetFile 必须与 task 相同字符串或同为 null。
- artifact 始终是 object，包含 exists、sha256、bytes、modifiedAtMs。exists=true 时后三项分别为 64 位小写十六进制和非负安全整数；exists=false 时后三项必须全部为 null。
- errorCode 为 null 或匹配 ^[A-Z][A-Z0-9_]{0,63}$。errorMessage 为 null 或 UTF-8 长度 1..512 字节，禁止 U+0000..U+001F 和 U+007F 控制字符。
- audit 始终是 object，包含 status、errors、warnings、primitiveCount、mainCode、reasonCode、findingsPreview。mainCode 为 null 或匹配 ^[a-z0-9][a-z0-9_.-]{0,63}$。findingsPreview 始终为 array，最多 20 条，每条只允许：
  - severity：info、warning、error 之一。
  - code：匹配 ^[a-z0-9][a-z0-9_.-]{0,63}$。
  - fieldPath：UTF-8 长度 1..256 字节，无控制字符。
  - message：UTF-8 长度 1..512 字节，无控制字符。
- completedAtMs 始终为安全整数。server 以接收 result 时的 receiveNowMs 校验 completedAtMs <= receiveNowMs + clock skew；startedAtMs 非 null 时必须满足 startedAtMs <= completedAtMs，并与已持久化 command.started 完全一致。startedAtMs=null 只允许启动前取消。

result frame 大小证明：

~~~text
serializedResultBytes
  <= 6 * stdoutPreviewUtf8Bytes
   + 6 * stderrPreviewUtf8Bytes
   + RESULT_FIXED_RESERVE_BYTES
  <= 12 * effectiveMaxOutput + 262144
  <= effectiveWsMax
~~~

六倍覆盖任意合法控制字符编码为 \u00XX 的最坏情况。RESULT_FIXED_RESERVE_BYTES 覆盖完整 JSON 属性名/标点、两个最长 512 字节路径、20 条 capped findings、errorMessage、artifact/audit 数值、时间、ID、nonce 和 signature；非输出字符串禁止控制字符后，其 quote/backslash 转义最多双倍，按上述 caps 合计远低于 262144。

agent 必须在签名前构造完整 result、执行 schema/cap、JCS 和最终 text frame 字节检查。任何按本节生成的 result 都必须小于等于 effectiveWsMax；若实现错误导致仍超限，不得发送 oversized frame，而应清空两个 preview 和 findings，发送最小 status=failed、errorCode=MYFORGE_OUTPUT_TOO_LARGE、audit.status=skipped/reasonCode=execution_failed 的 result，并在本地记录原始计算值。

audit object 的 nullability：

| audit.status | errors / warnings | primitiveCount | mainCode | reasonCode | findingsPreview |
|--------------|-------------------|----------------|----------|------------|-----------------|
| passed | 非负整数 | 非负整数或 null | null | null | 可为空 |
| warning | 非负整数 | 非负整数或 null | 非空 code | null | 至少 1 条 |
| failed | 非负整数 | 非负整数或 null | 非空 code | null | 至少 1 条 |
| skipped | null | null | null | dry_run / execution_failed / artifact_missing / rules_not_provided / cancelled | 空数组 |
| unavailable | null | null | null | auditor_not_configured | 空数组 |

command.result 状态依赖字段：

| executionMode / 场景 | status | exitCode | startedAtMs | artifact | audit | errorCode | errorMessage |
|----------------------|--------|----------|-------------|----------|-------|-----------|--------------|
| dry_run 校验成功 | completed | null | 必填整数 | exists 可 true/false，仅只读观察 | skipped，reasonCode=dry_run | null | null |
| codex_exec 成功，审核通过 | completed | 0 | 必填整数 | exists=true | passed | null | null |
| codex_exec 成功，本地未配置审核器 | completed | 0 | 必填整数 | exists=true | unavailable，reasonCode=auditor_not_configured | null | null |
| codex_exec 成功，显式无规则且本地配置审核器 | completed | 0 | 必填整数 | exists=true | skipped，reasonCode=rules_not_provided | null | null |
| codex_exec 成功，审核 warning | completed_with_errors | 0 | 必填整数 | exists=true | warning | FANGYUAN_BLUEPRINT_AUDIT_WARNING | 非空 |
| codex_exec 成功，审核 failed | completed_with_errors | 0 | 必填整数 | exists=true | failed | FANGYUAN_BLUEPRINT_AUDIT_FAILED | 非空 |
| 已启动后 timeout | failed | null | 必填整数 | 按结束时实际观察 | skipped，reasonCode=execution_failed | MYFORGE_COMMAND_TIMEOUT | 非空 |
| 已启动后非零退出 | failed | 非零整数 | 必填整数 | 按结束时实际观察 | skipped，reasonCode=execution_failed | MYFORGE_COMMAND_FAILED | 非空 |
| 已启动后其他运行错误 | failed | 整数或 null | 必填整数 | 按结束时实际观察 | skipped，reasonCode=execution_failed | MYFORGE_COMMAND_FAILED | 非空 |
| result 序列化仍超协商 frame | failed | 整数或 null | 必填整数 | 按结束时实际观察 | skipped，reasonCode=execution_failed | MYFORGE_OUTPUT_TOO_LARGE | 非空 |
| Codex 退出 0 但 artifact 缺失 | completed_with_errors | 0 | 必填整数 | exists=false | skipped，reasonCode=artifact_missing | MYFORGE_TARGET_FILE_MISSING | 非空 |
| 启动子进程前收到 cancel | cancelled | null | null | 按取消时实际观察 | skipped，reasonCode=cancelled | MYFORGE_COMMAND_CANCELLED | 非空 |
| 启动子进程后 cancel 并确认终止 | cancelled | 整数或 null | 必填整数 | 按终止后实际观察 | skipped，reasonCode=cancelled | MYFORGE_COMMAND_CANCELLED | 非空 |

子进程 spawn/preflight 失败使用 command.error，不发送 result。completed_with_errors 用于审核 warning/failed，以及 Codex 已成功退出但目标 artifact 缺失；后一种情况仍必须保留 stdoutPreview、stderrPreview、原始字节数、截断标记、exitCode=0 和起止时间。正常输出截断只设置 truncated，不改变 status。超时或取消终止进程时，agent 必须返回终止前已经读取到的 stdout/stderr；若管道未能在结果 deadline 前完整排空，对应 truncated 必须为 true，不得把已捕获内容回退为空。除 completed 外所有 result 的 errorCode 和 errorMessage 都必须非 null；completed 两者必须为 null。

### 8.9 command.error

方向：myforge-agent -> admin-api。使用 agent 私钥签名。只用于 command.execute 在子进程启动前被拒绝，不能替代已启动任务的 command.result。

~~~json
{
  "protocolVersion": 1,
  "type": "command.error",
  "connectionId": "67da7da9-a653-4d6e-9e81-f5f8baf874bb",
  "requestId": "2d0465b1-dc92-46d2-bc45-c90ed9724f5a",
  "agentId": "dev-pc-001",
  "projectId": "myforge-local",
  "errorCode": "MYFORGE_TARGET_PATH_INVALID",
  "errorMessage": "artifactFile is outside the allowed path",
  "retryable": false,
  "timestampMs": 1783694421000,
  "expiresAtMs": 1783694481000,
  "nonce": "base64url-random-16-bytes",
  "signature": "base64url-ed25519-signature"
}
~~~

若 server 消息无法解析出可信的合法 requestId，agent 不发送 command.error，只记录脱敏安全日志并以 WebSocket policy violation 关闭连接。

### 8.10 protocol.error

方向：双向。发送方使用自己的私钥签名。用于握手、签名、TTL、防重放、消息 schema、协议版本或状态顺序错误，不用于表示 Codex 业务执行结果。

~~~json
{
  "protocolVersion": 1,
  "type": "protocol.error",
  "connectionId": "67da7da9-a653-4d6e-9e81-f5f8baf874bb",
  "agentId": "dev-pc-001",
  "projectId": "myforge-local",
  "requestId": null,
  "errorCode": "MYFORGE_AGENT_SIGNATURE_INVALID",
  "errorMessage": "message signature is invalid",
  "fatal": true,
  "timestampMs": 1783694421000,
  "expiresAtMs": 1783694481000,
  "nonce": "base64url-random-16-bytes",
  "signature": "base64url-ed25519-signature"
}
~~~

错误可明确关联到已解析的合法任务时 requestId 为该 UUID，否则必须为 null。challenge 发出后 connectionId 使用 challengeId；upgrade 完成但 challenge 尚未建立时为 null。P0 的握手失败、签名失败、协议版本错误和乱序消息均 fatal=true；发送后使用 WebSocket 1008 policy violation 关闭连接。若 frame 超大、JSON 无法解析、无法安全选择验签公钥或无法生成签名错误消息，可以直接关闭，不回显原始输入。

command.execute 已通过 server 验签，但 agent 在 taskType、profile、路径或本地 preflight 校验中拒绝执行时使用 command.error。无法确认消息来源时只能使用 protocol.error 或直接关闭，绝不启动子进程。

## 9. 任务状态机、离线和取消

### 9.1 状态机

P0 task status 只允许：

~~~text
queued
dispatched
running
completed
completed_with_errors
failed
cancelled
~~~

合法转换：

~~~text
queued -> dispatched
queued -> cancelled
queued -> failed

dispatched -> running
dispatched -> failed
dispatched -> cancelled

running -> completed
running -> completed_with_errors
running -> failed
running -> cancelled
~~~

completed、completed_with_errors、failed、cancelled 是终态，不得再转换。command.error 使 dispatched -> failed。除“command.cancel 在子进程启动前生效并返回 cancelled、startedAtMs=null”外，command.result 只允许用于已发送 command.started 的任务。

### 9.2 创建和离线策略

- 创建接口先在同一数据库事务中生成 requestId 并写入 queued。
- 匹配 agent 已 registered、heartbeat 有效且 idle 时，调度器按第 7.3 节获取 connection operation mutex，再用条件更新原子 claim 该 queued task 为 dispatched，同时清空 queue_reason，并写入当前 connection_id、由 capabilities.dryRun 决定的 execution_mode、协商后的 timeout_ms/max_output_bytes、dispatched_at、command_expires_at 和 command_digest；随后把签名 command.execute 入同一 outbound FIFO 并等待 writer completion，最后释放 mutex。
- execute enqueue 或 writer completion 失败时从 dispatched 转 failed，errorCode 为 MYFORGE_DISPATCH_FAILED；不得回到 queued，因为无法证明 frame 从未部分进入传输层。
- agent offline 或 busy 时保持 queued，并分别记录 queueReason=agent_offline 或 agent_busy；不允许调用方选择“直接失败”或提交 queue policy。
- 每个 agent 同时最多一个 dispatched/running 任务，其余按 created_at、request_id 稳定排序等待。
- queued 超过 MYFORGE_QUEUE_TTL_MS 后转 failed，errorCode 为 MYFORGE_QUEUE_EXPIRED。
- agent 注册、当前任务终态或定时扫描时，admin-api 尝试下发下一条未过期 queued task。
- dispatched 在 command.execute expiresAt + clock skew 前未收到 started、result 或 command.error 时转 failed，errorCode 为 MYFORGE_COMMAND_EXPIRED。
- cancel_requested_at 为 null 的 running task 超过 started_at + timeout_ms + clock skew 仍无 result 时，server 关闭对应 agent socket 并转 failed，errorCode 为 MYFORGE_COMMAND_TIMEOUT；agent 按断线规则终止子进程，后到冲突结果按第 10.2 节拒绝。
- 已写 cancel_requested_at 的任务不再执行普通 command timeout 转换，只执行第 9.3 节 cancel deadline；两个 watchdog 不得竞争写不同终态。
- 任何终态转换都必须写 completed_at；各状态时间只在首次合法转换时写入，重复消息不得覆盖。

agent 断线时：

- queued 不受影响。
- 未请求取消的 dispatched 或 running 任务转 failed，errorCode 为 MYFORGE_AGENT_DISCONNECTED。
- 已写入 cancel_requested_at 但尚未收到 cancelled result 的任务转 failed，errorCode 为 MYFORGE_CANCEL_UNCONFIRMED；不能把断线等同于已确认终止。
- agent 必须在检测到 WebSocket 断线时终止当前子进程，不得离线继续执行。
- dispatched/running 任务不得在重连后自动重放，管理员必须创建新任务。

admin-api 重启时将 agent 标为 offline；遗留 dispatched/running 若 cancel_requested_at 非 null 则转 failed、errorCode=MYFORGE_CANCEL_UNCONFIRMED，否则转 failed、errorCode=MYFORGE_SERVER_RESTARTED。只有 queued 可以在 agent 重连后继续调度。

### 9.3 取消语义

- queued：数据库原子更新为 cancelled，不发送 command.cancel，不设置 cancel_deadline_at。
- dispatched/running：cancel API 按第 7.3 节先获取同一 connection operation mutex，再锁定 task row；仍非终态时在同一事务写 cancel_requested_at 和 cancel_deadline_at=cancel_requested_at+协商后的 cancelTimeoutMs，提交后把携带同一 deadline 的 command.cancel 入同一 outbound FIFO并等待 writer completion，最后释放 mutex。状态暂时保持不变。
- command.result status=cancelled 是唯一的 agent 终止确认。agent 必须在 cancelDeadlineAtMs 前停止整个子进程树并发送该 result；server 只在 receiveNowMs <= cancelDeadlineAtMs + MYFORGE_CLOCK_SKEW_MS 时接受。仅收到 WebSocket close、进程 kill 请求或 heartbeat idle 都不算确认。
- 重复取消已 cancelled 任务返回成功并保持 cancelled，保证幂等。
- completed、completed_with_errors 或 failed 返回 409 MYFORGE_TASK_NOT_CANCELLABLE。
- 已请求取消但重复调用时返回成功，不改变 cancel_requested_at/cancel_deadline_at；server 可以用新 nonce 和签名重发同一 requestId、同一 deadline 的 cancel，不能延长 deadline。
- agent 收到同 requestId、同 cancelDeadlineAtMs 的重复 cancel 时不得重置本地 timer；仍在终止中则继续，已生成 cancelled result 则用新 envelope 重发相同 semantic result。不同 deadline 视为 MYFORGE_DUPLICATE_REQUEST_CONFLICT。
- agent 收到 cancel 后必须尽力终止整个子进程树，并回传 command.result status=cancelled、errorCode=MYFORGE_COMMAND_CANCELLED。
- cancel 写 socket 失败时 server 关闭连接并立即转 failed，errorCode=MYFORGE_CANCEL_DELIVERY_FAILED。
- cancel 后 socket 在 cancelled result 前关闭时立即转 failed，errorCode=MYFORGE_CANCEL_UNCONFIRMED。
- 到达 cancel_deadline_at + MYFORGE_CLOCK_SKEW_MS 仍无 cancelled result 时 server 关闭连接并转 failed，errorCode=MYFORGE_CANCEL_TIMEOUT。
- 取消与自然完成、command.error、执行 timeout 的竞争由 task row transaction 顺序唯一决定：其他终态先提交时 cancel API 看到终态并返回 409；cancel_requested_at 先提交时取消获得优先级，之后只接受 status=cancelled 的 result，任何 command.error 或 completed/completed_with_errors/failed result 都作为 MYFORGE_DUPLICATE_RESULT_CONFLICT 拒绝，任务保持非终态直到 cancelled result 或 cancel deadline 失败。agent 在启动前收到 cancel 时应停止后续 preflight/spawn 并立即返回取消 result。

## 10. 消息幂等和冲突处理

### 10.1 agent 对 execute 的幂等

agent 按 requestId 保存 active 和最近完成记录，保留时间至少为 command timeout + command TTL：

- 首次合法 execute：记录 canonical digest 后启动。
- 同 requestId、同 digest、仍 active：不得再次启动；可使用新 timestamp/nonce/signature 重发 command.started。
- 同 requestId、同 digest、已完成：不得再次启动；使用缓存结果内容和新 envelope 重发 command.result。
- 同 requestId、不同 digest：返回 MYFORGE_DUPLICATE_REQUEST_CONFLICT，不执行。
- agent 重启后不恢复或重放旧任务；server 的断线策略会把旧 dispatched/running 任务置为 failed。

command digest 是 command.execute 移除顶层 signature、timestampMs、expiresAtMs 和 nonce 后，对剩余 object 做 JCS，再计算 SHA-256 小写十六进制。上述四个 envelope 字段不属于任务语义，因此同一命令用新时间、TTL、nonce 和签名重发时 digest 保持不变。

### 10.2 server 对 started/result/error 的幂等

- 重复 started 且 requestId、agent identity、startedAtMs 相同：幂等成功，不重复改变状态。
- 终态 result 的 semantic digest 与数据库 result_digest 相同：幂等接受，不重复写业务审计。
- 同 requestId 的终态内容不同：保留第一个终态，拒绝后到消息，记录 MYFORGE_DUPLICATE_RESULT_CONFLICT 安全审计。
- 已终态后到达 started 或 command.error：拒绝并记录非法状态转换。
- requestId 不存在，connectionId 与 task.connection_id 不匹配，agent/project 不匹配，executionMode 与 task.execution_mode 不匹配，或 artifactFile/consumerTargetFile 与任务记录不一致：拒绝，不修改任务。

result semantic digest 的算法与 command digest 相同：移除顶层 signature、timestampMs、expiresAtMs 和 nonce，对剩余 object 做 JCS 后计算 SHA-256。这样 agent 可以用新 envelope 重发相同结果，但不能改变 status、输出、artifact、audit、错误或业务时间。

## 11. 持久化契约

### 11.1 myforge_agents

P0 至少包含：

~~~sql
CREATE TABLE myforge_agents (
  agent_id varchar(128) PRIMARY KEY,
  project_id varchar(128) NOT NULL,
  label varchar(128) NULL,
  public_key_fingerprint varchar(64) NOT NULL,
  configured boolean NOT NULL DEFAULT true,
  status varchar(32) NOT NULL DEFAULT 'offline',
  hostname varchar(255) NULL,
  platform varchar(32) NULL,
  agent_version varchar(64) NULL,
  forge_root_summary_json jsonb NULL,
  capabilities_json jsonb NULL,
  limits_json jsonb NULL,
  effective_limits_json jsonb NULL,
  last_registered_at timestamptz NULL,
  connected_at timestamptz NULL,
  last_seen_at timestamptz NULL,
  disconnected_at timestamptz NULL,
  created_at timestamptz NOT NULL DEFAULT current_timestamp,
  updated_at timestamptz NOT NULL DEFAULT current_timestamp
);
~~~

status 只允许 online/offline。configured 由启动配置同步流程维护；configured=false 的历史行不得连接或接收新任务。limits_json 保存 agent.register 的本地上限，effective_limits_json 保存第 6.1.2 节协商结果，未注册 agent 两者为 null。public_key_fingerprint 以 server 配置中的公钥为准，不信任 agent 自报值。

### 11.2 myforge_task_runs

P0 至少包含：

~~~sql
CREATE TABLE myforge_task_runs (
  request_id uuid PRIMARY KEY,
  task_type varchar(64) NOT NULL,
  project_id varchar(128) NOT NULL,
  agent_id varchar(128) NOT NULL,
  status varchar(32) NOT NULL,
  queue_reason varchar(32) NULL,
  execution_mode varchar(32) NULL,
  danger_full_access boolean NULL,
  connection_id uuid NULL,
  artifact_file varchar(512) NOT NULL,
  consumer_target_file varchar(512) NULL,
  rules_file varchar(512) NULL,
  prompt_json jsonb NOT NULL,
  rendered_prompt text NOT NULL,
  command_preview text NOT NULL,
  command_digest varchar(64) NULL,
  command_expires_at timestamptz NULL,
  timeout_ms int NOT NULL,
  max_output_bytes int NOT NULL,
  stdout_preview text NULL,
  stderr_preview text NULL,
  stdout_bytes bigint NULL,
  stderr_bytes bigint NULL,
  stdout_truncated boolean NOT NULL DEFAULT false,
  stderr_truncated boolean NOT NULL DEFAULT false,
  exit_code int NULL,
  artifact_json jsonb NULL,
  audit_json jsonb NULL,
  result_digest varchar(64) NULL,
  error_code varchar(64) NULL,
  error_message text NULL,
  created_by_admin_id bigint NULL,
  created_by_admin_username varchar(64) NULL,
  created_at timestamptz NOT NULL DEFAULT current_timestamp,
  queue_expires_at timestamptz NOT NULL,
  dispatched_at timestamptz NULL,
  started_at timestamptz NULL,
  cancel_requested_at timestamptz NULL,
  cancel_deadline_at timestamptz NULL,
  completed_at timestamptz NULL,
  updated_at timestamptz NOT NULL DEFAULT current_timestamp
);
~~~

rules_file 为 null 只表示创建请求显式选择无规则执行。danger_full_access 在 queued 且尚未绑定 agent 连接时为 null，dispatch 时必须与该连接签名注册的 capabilities.dangerFullAccess 一致，并与精确 command_preview 一起成为任务和生命周期审计的不可远程切换快照。数据库使用 `(execution_mode IS NULL) = (danger_full_access IS NULL)` CHECK 固化该不变量；升级时先把旧版本已写入 execution_mode、尚无权限快照的历史行回填为 false（旧执行器固定为 workspace-write），再幂等增加约束。

必须为 agent_id + status + created_at、project_id + created_at 和 created_at 建索引。status 应有 CHECK constraint，值与第 9 节完全一致。queue_reason 只允许 agent_offline、agent_busy 或 null，并且只允许 queued 状态为非 null。execution_mode 只允许 codex_exec、dry_run 或 null：从未 dispatched 的 queued 及其直接 failed/cancelled 终态可为 null；一旦 dispatched 就必须非 null且永不清空。connection_id、command_expires_at 和 command_digest 在 dispatched 时一并写入。cancel_requested_at 与 cancel_deadline_at 必须同为 null 或同为非 null，queued 直接取消是唯一不写二者的 cancelled 路径。

prompt_json 保存规范化后的 typed prompt，不保存任意原始请求。rendered_prompt 和 command_preview 可能包含业务内容，只在具有 myforge.task.read 权限的控制面返回，不写普通应用日志。

### 11.3 审计

以下动作写 admin_audit_logs：

~~~text
myforge_task_create
myforge_task_dispatch
myforge_task_started
myforge_task_complete
myforge_task_fail
myforge_task_cancel_request
myforge_task_cancelled
~~~

签名失败、未知 agent、identity mismatch、过期消息、nonce replay、重复冲突、非法状态转换和超大 frame 写 security_audit_logs。

审计只记录 requestId、taskType、agentId、projectId、状态、路径摘要、错误码、管理员、必要 timing，以及 executionMode、dangerFullAccess 和非可执行 commandPreview 快照；不记录私钥、公钥正文、完整 signature、完整 renderedPrompt 或完整 stdout/stderr。

## 12. HTTP API 和响应

所有 HTTP 路由位于 /api/v1，使用现有 JwtAuthGuard、RolesGuard 和统一响应格式。成功响应包含 ok: true；失败响应沿用：

~~~json
{
  "ok": false,
  "error": "MYFORGE_TARGET_PATH_INVALID",
  "message": "artifactFile is invalid"
}
~~~

### 12.1 GET /api/v1/myforge/agents

权限：myforge.agent.read。

返回配置或持久化过的 agent 列表：

~~~json
{
  "ok": true,
  "items": [
    {
      "agentId": "dev-pc-001",
      "projectId": "myforge-local",
      "label": "开发机",
      "configured": true,
      "status": "online",
      "hostname": "DESKTOP-1LG9IK4",
      "platform": "windows",
      "agentVersion": "0.1.0",
      "forgeRootSummary": {
        "name": "myforge",
        "configured": true
      },
      "capabilities": {
        "profiles": ["codex_exec"],
        "codexExec": true,
        "fangyuanBlueprint": true,
        "audit": "unavailable",
        "dryRun": false,
        "maxConcurrentTasks": 1
      },
      "limits": {
        "authTtlMs": 60000,
        "commandTtlMs": 60000,
        "clockSkewMs": 5000,
        "heartbeatIntervalMs": 15000,
        "maxCommandTimeoutMs": 600000,
        "cancelTimeoutMs": 10000,
        "maxOutputBytes": 1048576,
        "wsMaxMessageBytes": 16777216
      },
      "effectiveLimits": {
        "authTtlMs": 60000,
        "commandTtlMs": 60000,
        "serverClockSkewMs": 5000,
        "agentClockSkewMs": 5000,
        "heartbeatIntervalMs": 15000,
        "heartbeatTimeoutMs": 45000,
        "commandTimeoutMs": 600000,
        "cancelTimeoutMs": 10000,
        "maxOutputBytes": 1048576,
        "wsMaxMessageBytes": 16777216
      },
      "lastSeenAt": "2026-07-10T12:00:15.000Z"
    }
  ],
  "total": 1
}
~~~

limits/effectiveLimits 用于定位部署不兼容；从未注册的 configured agent 两者为 null。不得返回公钥、fingerprint 以外的密钥信息或完整 MYFORGE_ROOT。

### 12.2 GET /api/v1/myforge/tasks

权限：myforge.task.read。

支持可选 query：projectId、agentId、status、limit、offset。limit 默认 20，范围 1 至 100；offset 为 0 至 100000 的整数。按 created_at DESC、request_id DESC 稳定排序。

返回 items、total、limit、offset。列表项包含 requestId、taskType、projectId、agentId、status、queueReason、executionMode、dangerFullAccess、artifactFile、consumerTargetFile、createdBy、createdAt、dispatchedAt、startedAt、cancelRequestedAt、cancelDeadlineAt、completedAt、durationMs、errorCode，不返回 stdout/stderr 和 renderedPrompt 全文。

### 12.3 GET /api/v1/myforge/tasks/:requestId

权限：myforge.task.read。

返回：

~~~json
{
  "ok": true,
  "task": {
    "requestId": "2d0465b1-dc92-46d2-bc45-c90ed9724f5a",
    "taskType": "fangyuan.blueprint.generate",
    "projectId": "myforge-local",
    "agentId": "dev-pc-001",
    "status": "running",
    "queueReason": null,
    "executionMode": "codex_exec",
    "dangerFullAccess": true,
    "artifactFile": "artifacts/fangyuan/home_preview.ron",
    "consumerTargetFile": "project/assets/fangyuan/home_preview.ron",
    "rulesFile": "rules/fangyuan/方圆灵构蓝图规则.md",
    "prompt": {},
    "commandPreview": "codex exec --dangerously-bypass-approvals-and-sandbox --ephemeral --color never <renderedPrompt:...> [danger_full_access=true]",
    "stdoutPreview": null,
    "stderrPreview": null,
    "stdoutBytes": null,
    "stderrBytes": null,
    "stdoutTruncated": false,
    "stderrTruncated": false,
    "exitCode": null,
    "artifact": null,
    "audit": null,
    "errorCode": null,
    "errorMessage": null,
    "createdBy": {
      "adminId": 1,
      "username": "admin"
    },
    "createdAt": "2026-07-10T12:00:00.000Z",
    "dispatchedAt": "2026-07-10T12:00:01.000Z",
    "startedAt": "2026-07-10T12:00:02.000Z",
    "cancelRequestedAt": null,
    "cancelDeadlineAt": null,
    "completedAt": null
  }
}
~~~

renderedPrompt 默认不返回，避免重复展示内部固定安全模板；commandPreview 和结构化 prompt 足够用于后台核对。

### 12.4 POST /api/v1/myforge/tasks/fangyuan-blueprint

权限：myforge.task.create。请求体只允许第 5 节结构。

成功使用 HTTP 202：

~~~json
{
  "ok": true,
  "requestId": "2d0465b1-dc92-46d2-bc45-c90ed9724f5a",
  "status": "dispatched",
  "queueReason": null,
  "executionMode": "codex_exec",
  "createdAt": "2026-07-10T12:00:00.000Z",
  "queueExpiresAt": "2026-07-10T12:15:00.000Z"
}
~~~

agent offline 或 busy 时同样返回 202，但 status 为 queued、executionMode=null，queueReason 分别为 agent_offline 或 agent_busy。在线下发时 executionMode 由该连接注册的 dryRun 能力确定。写 socket 失败时任务已经持久化，返回 202、status=failed 和 errorCode=MYFORGE_DISPATCH_FAILED，调用方继续使用 requestId 查询详情。known agent 规则以第 6.1 节配置映射为准；只有配置缺失才返回 404，配置存在但从未注册仍创建 queued。已知 agent 与 projectId 不匹配返回 409，不创建任务。

### 12.5 POST /api/v1/myforge/tasks/:requestId/cancel

权限：myforge.task.cancel。P0 请求体必须为空或空 object，不接受任意原因。

~~~json
{
  "ok": true,
  "requestId": "2d0465b1-dc92-46d2-bc45-c90ed9724f5a",
  "status": "running",
  "cancelRequested": true,
  "cancelDeadlineAt": "2026-07-10T12:00:40.000Z"
}
~~~

queued 任务成功取消时 status 为 cancelled、cancelRequested=false、cancelDeadlineAt=null。dispatched/running 在 agent 确认前保持原状态并返回 cancelRequested=true 和固定 cancelDeadlineAt；重复请求返回同一 deadline。

## 13. 权限和 admin-web

新增权限点：

~~~text
myforge.agent.read
myforge.task.read
myforge.task.create
myforge.task.cancel
~~~

P0 角色矩阵：

| 角色 | agent.read | task.read | task.create | task.cancel |
|------|------------|-----------|-------------|-------------|
| viewer | 否 | 否 | 否 | 否 |
| operator | 否 | 否 | 否 | 否 |
| admin | 是 | 是 | 是 | 是 |
| super_admin | 是 | 是 | 是 | 是 |

admin-web 的菜单、路由和按钮可见性必须使用同一权限常量，但前端隐藏不能替代后端 @Permissions() 校验。

admin-web 详情页只对 queued、dispatched、running 状态轮询。建议间隔 2 秒至 5 秒，并避免上一请求未完成时再次发起。进入 completed、completed_with_errors、failed、cancelled 后停止轮询。P0 不增加浏览器 WebSocket 或 SSE。

列表和详情必须展示 executionMode。dry_run 结果应明确标识“本地无副作用模拟”，artifact.exists=false 不显示为生成失败；只有 task status/errorCode 决定失败。cancelRequestedAt 非 null 时按钮禁用并显示 cancelDeadlineAt，避免重复操作看起来会延长期限。

## 14. 失败码

### 14.1 HTTP 和编排

| 失败码 | 建议 HTTP | 含义 |
|--------|-----------|------|
| MYFORGE_DISABLED | 503 | 功能未启用或 server key 未就绪 |
| MYFORGE_AGENT_NOT_FOUND | 404 | agentId 未配置或不存在 |
| MYFORGE_AGENT_PROJECT_MISMATCH | 409 | agentId 与 projectId 绑定不一致 |
| MYFORGE_TASK_NOT_FOUND | 404 | requestId 不存在 |
| MYFORGE_TASK_NOT_CANCELLABLE | 409 | 任务已进入不可取消终态 |
| MYFORGE_TARGET_PATH_INVALID | 400 | artifact/rules/consumer path 词法或越界校验失败 |
| MYFORGE_PROMPT_INVALID | 400 | prompt 字段不符合约束 |
| MYFORGE_PROMPT_TOO_LARGE | 413 | 渲染后提示词超限 |
| MYFORGE_QUEUE_EXPIRED | 任务终态 | queued 超过保留时间 |
| MYFORGE_DISPATCH_FAILED | 任务终态 | 已 claim 为 dispatched，但 WebSocket 写入失败 |
| MYFORGE_CANCEL_DELIVERY_FAILED | 任务终态 | cancel 无法写入当前 agent socket |
| MYFORGE_CANCEL_UNCONFIRMED | 任务终态 | cancel 后断线或 server 重启，未收到终止确认 |
| MYFORGE_CANCEL_TIMEOUT | 任务终态 | cancel deadline 内未收到 cancelled result |
| INSUFFICIENT_PERMISSION | 403 | 沿用现有后台权限错误 |

一般 schema 错误使用 INVALID_REQUEST 400。缺少或非法管理员 token 沿用现有 MISSING_TOKEN、INVALID_TOKEN 等错误。

### 14.2 WebSocket 身份和协议

~~~text
MYFORGE_AGENT_AUTH_FAILED
MYFORGE_AGENT_UNKNOWN
MYFORGE_IDENTITY_MISMATCH
MYFORGE_SERVER_SIGNATURE_INVALID
MYFORGE_AGENT_SIGNATURE_INVALID
MYFORGE_MESSAGE_EXPIRED
MYFORGE_REPLAY_DETECTED
MYFORGE_LIMIT_MISMATCH
MYFORGE_MESSAGE_IJSON_INVALID
MYFORGE_MESSAGE_SCHEMA_INVALID
MYFORGE_PROTOCOL_VERSION_UNSUPPORTED
MYFORGE_PROTOCOL_STATE_INVALID
MYFORGE_DUPLICATE_REQUEST_CONFLICT
MYFORGE_DUPLICATE_RESULT_CONFLICT
MYFORGE_AGENT_BUSY
MYFORGE_AGENT_DISCONNECTED
MYFORGE_SERVER_RESTARTED
MYFORGE_OUTPUT_TOO_LARGE
~~~

MYFORGE_OUTPUT_TOO_LARGE 表示 result 构造未满足已协商预算或接收 frame 已超本地硬上限。agent 侧按第 8.8 节发送最小 failed result，不发送 oversized frame；无法解析 requestId 的超大入站 frame 直接关闭。正常 preview 截断只设置 truncated 字段。

### 14.3 本地执行

~~~text
MYFORGE_CONFIG_INVALID
MYFORGE_ROOT_MISSING
MYFORGE_ROOT_INVALID
MYFORGE_TARGET_PATH_INVALID
MYFORGE_RULES_FILE_MISSING
MYFORGE_CODEX_UNAVAILABLE
MYFORGE_AUDITOR_INVALID
MYFORGE_PROFILE_UNSUPPORTED
MYFORGE_COMMAND_EXPIRED
MYFORGE_COMMAND_SPAWN_FAILED
MYFORGE_COMMAND_TIMEOUT
MYFORGE_COMMAND_FAILED
MYFORGE_COMMAND_CANCELLED
MYFORGE_TARGET_FILE_MISSING
FANGYUAN_BLUEPRINT_AUDIT_WARNING
FANGYUAN_BLUEPRINT_AUDIT_FAILED
~~~

errorMessage 必须简短、可展示且不泄露私钥、签名、环境变量、完整绝对路径或完整命令行。详细本地诊断留在 agent 日志。

MYFORGE_CONFIG_INVALID 和 MYFORGE_AUDITOR_INVALID 是启动/preflight 错误；agent 尚未注册时不会生成 task result。MYFORGE_LIMIT_MISMATCH 是已签名 WebSocket 协议错误，用于双端配置或协商值不兼容。

MYFORGE_TARGET_FILE_MISSING 只用于 Codex 已退出 0 但 artifact 不存在的 `completed_with_errors` result；它不能抹掉本次 Codex 的 stdout、stderr、exitCode 或时间信息。

MYFORGE_SHELL_IGNORED 只是一条本地兼容性 warning event，不是 task errorCode。

## 15. Artifact 和审核摘要

agent 执行结束后至少检查：

- artifactFile 是否存在且为普通文件。
- 解析后的文件仍位于 canonical MYFORGE_ROOT 内。
- SHA-256、字节数和修改时间。
- codex_exec 下检查修改时间不早于任务开始时间减 clock skew；不满足时作为 finding 展示。dry_run 只读观察 pre-existing artifact，不做新鲜度断言。

P0 审核器只由 agent 本地受信配置决定，不做 PATH 搜索或自动发现：

- MYFORGE_AUDIT_ENABLED=false：不要求 program 存在，register capabilities.audit=unavailable。
- MYFORGE_AUDIT_ENABLED=true：MYFORGE_AUDIT_PROGRAM 必填，必须使用第 4.2 节同等路径规则，位于 tools/ 下，canonicalize 后仍在 MYFORGE_ROOT 内，且是可直接启动的普通可执行文件。配置非法时 preflight/--check 以 MYFORGE_AUDITOR_INVALID 失败，agent 不注册。
- 程序在启动后被删除或变为不可执行属于本次 audit failed，不降级为 unavailable。

真实 codex_exec 退出 0、artifact 存在且 rulesFile 非 null 时，audit available 的 agent 使用直接进程参数调用：

~~~text
executable:
  canonicalize(MYFORGE_ROOT / MYFORGE_AUDIT_PROGRAM)
argv:
  --format
  json
  --rules
  <rulesFile>
  --artifact
  <artifactFile>
cwd:
  canonicalize(MYFORGE_ROOT)
shell:
  false
timeout:
  MYFORGE_AUDIT_TIMEOUT_MS
~~~

server 消息不得覆盖 program、argv、cwd、timeout 或启停状态。审核器 stdout 必须是单个 UTF-8 JSON object，只允许 status、errors、warnings、primitiveCount、mainCode、findings 字段；agent 校验后截取最多 20 条 findings 映射为 findingsPreview。审核器 stderr 只写 agent 本地脱敏日志，不并入 Codex stderrPreview。

状态区别：

- passed/warning/failed：审核器已实际启动并返回，或已尝试启动但发生 timeout、spawn、exit/schema 错误。运行异常统一映射 failed，mainCode 使用 auditor_spawn_failed、auditor_timeout、auditor_exit_failed 或 auditor_output_invalid，任务为 completed_with_errors。
- unavailable：真实 Codex 成功且 artifact 存在，但本地 MYFORGE_AUDIT_ENABLED=false；reasonCode=auditor_not_configured。它不表示审核通过。
- skipped：本次有明确不运行原因，只允许 dry_run、execution_failed、artifact_missing、rules_not_provided、cancelled。rules_not_provided 表示本地审核器已配置但任务显式没有规则输入，因此不伪造审核结论。

审核 warning/failed 不删除 artifact，分别使用 FANGYUAN_BLUEPRINT_AUDIT_WARNING / FANGYUAN_BLUEPRINT_AUDIT_FAILED。dry-run 永远 skipped，不因本地配置了审核器而运行它。

## 16. 安全和可靠性边界

- WSS 保护传输机密性；Ed25519 签名提供消息来源和完整性，两者不可互相替代。
- task.create 是高风险控制面权限，只授予 admin/super_admin。
- prompt 是受限业务数据，但 Codex 仍是自动化执行器；默认模式可写 MYFORGE_ROOT。dangerFullAccess=true 时 Codex 可使用运行 agent 的 OS 用户权限访问整机，MYFORGE_ROOT 不再是 OS 沙箱边界，agent 必须部署在专用且可完全信任的主机/用户下。
- 子进程环境使用最小 allowlist，不把 server 私钥、agent 私钥或后台 token 传给 Codex。
- agent 日志不得记录完整 signature、私钥、完整 renderedPrompt 或未截断输出。
- admin-api 不读取本地 artifact 内容，不通过 WebSocket 上传完整文件。
- 每个 agent 串行执行，避免两个 Codex 任务同时修改同一工作区。
- timeout 或取消必须终止子进程树；仅终止直接父进程不满足要求。
- agent 退出、断线和 server 重启都不得造成任务静默停留在 running。

## 17. P0 后续扩展边界

以下事项必须另立设计，不得通过给 P0 command.execute 增加自由字段实现：

- game-server 触发或消费结果。
- NATS 通知。
- myforge 到 mybevy 的资源发布和审批。
- 对象存储、版本仓库或跨机器 artifact 传输。
- 新 taskType、新执行 profile 或并发执行。
- 浏览器 WebSocket/SSE。
- 通用 agent 管理、远程终端或文件操作。

扩展新 taskType 时必须新增独立 typed HTTP DTO、固定 prompt renderer、固定执行 profile、权限点和状态测试。不得复用一个任意 command 字段。

## 18. 实现顺序

1. 落地数据库、配置、权限常量和 typed DTO。
2. 落地 Ed25519/JCS 共识测试向量，Node.js 与 Rust 必须对同一消息产生相同 signing bytes，并能互相验签。
3. 落地 WebSocket challenge、hello、register、heartbeat 和连接生命周期。
4. 落地 task create/list/detail/cancel、队列和状态机。
5. 落地 Rust preflight、路径校验、固定 codex_exec、输出截断、timeout 和取消。
6. 落地 artifact/audit 摘要与结果幂等。
7. 落地 admin-web 权限、表单、列表、详情和轮询。
8. 先通过 dry-run 闭环，再在用户确认后执行真实 codex exec。

## 19. 关键结论

- P0 唯一业务任务是 fangyuan.blueprint.generate。
- 普通调用方只提交 typed request，不能提交 command、args、cwd、profile 或 shell。
- command.execute 只是 server 到 agent 的内部签名消息，profile 固定为 codex_exec。
- agent 通过直接进程参数调用 Codex，cwd 固定为 canonical MYFORGE_ROOT；最高权限只能由本机 MYFORGE_CODEX_DANGEROUS_FULL_ACCESS 开启，commandPreview 永不执行。
- artifactFile 和非 null rulesFile 是 MYFORGE_ROOT 内受控相对路径；artifact 父目录可由 Codex 创建，consumerTargetFile 只作元数据。
- admin-web 只使用 HTTP 轮询，不使用浏览器 WebSocket 或 SSE。
- requestId 提供任务幂等，nonce 和 TTL 提供消息防重放，Ed25519/JCS 保证 Node.js 与 Rust 的签名一致性。
- P0 不接 game-server、NATS、资源发布或 mybevy 写入。
