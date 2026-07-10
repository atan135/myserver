# 方圆灵构 myforge-agent 蓝图生成服务端调用设计

## 1. 文档定位

本文描述 MyServer 如何通过远程 `admin-api` 调用本地 `apps/myforge-agent`，并由 `myforge-agent` 在外部 AI 工作区 `C:\project\myforge` 中执行 AI 生成命令，生成或更新方圆灵构 RON 蓝图资源。

这不是运维远程终端能力，也不是通用远程 shell 平台。它的目标是服务端触发客户端资源生成流程，例如让本地 `apps/myforge-agent` 在 `MYFORGE_ROOT` 下执行：

```powershell
codex exec "请根据 rules/fangyuan/方圆灵构蓝图规则.md 生成 artifacts/fangyuan/home_preview.ron ..."
```

`apps/myforge-agent` 是本项目 MyServer monorepo 内新增的本地连接器 app。`myforge` 是独立 Git 项目，作为 AI skill、规则、脚本、审核器和生成产物所在的工作区，不是 MyServer 或 mybevy 的子目录。本机开发示例路径可以是：

```text
C:\project\myforge
```

方圆灵构蓝图规则的当前参考来源位于外部 `mybevy` 仓库：

```text
C:\project\mybevy\docs\世界观\方圆灵构蓝图规则.md
```

该 mybevy 路径只作为规则来源说明。P0 执行 AI 命令时不进入 mybevy 仓库；`myforge` 应在自己的仓库内维护规则副本、生成脚本、审核器适配和产物目录。MyServer 侧不得硬编码 `C:\project\myforge` 或 `C:\project\mybevy`，只通过 `myforge-agent` 上报的 `MYFORGE_ROOT` 摘要和任务相对路径记录结果。

## 2. 目标

P0 目标：

- `admin-web` 提供任务创建入口、任务列表和任务结果展示。
- `apps/myforge-agent` 作为本地进程主动连接远程 `admin-api`。
- `admin-api` 可以创建一次性方圆灵构蓝图生成任务。
- `myforge-agent` 在 `MYFORGE_ROOT` 指向的外部 `myforge` 工作区内执行受控命令，优先是 `codex exec ...`。
- `myforge-agent` 将执行状态、stdout / stderr 摘要、目标文件路径和校验结果返回 `admin-api`。
- `admin-api` 持久化任务结果，并提供给 `admin-web` 查询和展示。

非目标：

- P0 不接入 `game-server`，具体玩法触发、资源加载和热更形式待定。
- 不做通用交互式终端。
- 不做 PTY 会话管理。
- 不做远程文件浏览器或任意文件写入平台。
- 不复制 `remote-client/localapp` 的设备审批、auth_code、终端 session、文件读写等完整功能。
- 不把 `myforge` 或真实客户端仓库纳入 MyServer monorepo；只新增 `apps/myforge-agent` 连接器。
- 不让玩家客户端直接触发本地 shell。

## 3. 总体拓扑

```text
admin-web
  -> 创建方圆灵构蓝图生成任务
  -> 查询任务状态和结果

admin-api
  -> 接收 admin-web 请求
  -> 创建 myforge-agent task
  -> WebSocket 下发 signed command
  -> PostgreSQL 记录任务、事件和结果
  -> 向 admin-web 返回任务状态和结果摘要

apps/myforge-agent
  -> 主动连接远程 admin-api
  -> 定位 MYFORGE_ROOT
  -> 将终端工作目录切到 C:\project\myforge
  -> 在 myforge 工作区执行 codex exec
  -> 回传执行结果和文件校验摘要

C:\project\myforge
  -> 提供 AI skill、规则副本、生成脚本、审核器和 artifacts
```

`admin-api` 是控制入口，因为它已经具备管理员鉴权、权限矩阵、审计、PostgreSQL 和受控访问边界。`auth-http` 不参与该能力。NATS 或 `game-server` 通知作为后续玩法接入方案评估，不进入 P0 闭环。

## 4. 服务边界

### 4.1 `admin-api`

负责：

- 接收 `admin-web` 发起的蓝图生成请求；内部服务触发留待后续玩法明确后再设计。
- 维护在线 `myforge-agent` 列表。
- 对任务进行权限校验、审计和持久化。
- 生成带签名的一次性命令消息。
- 接收 `myforge-agent` 返回结果并验签。
- 提供任务列表、任务详情和结果摘要给 `admin-web` 展示。

不负责：

- 直接修改外部 `myforge`、`mybevy` 文件或本机任意目录。
- 在远程服务器上执行 Codex。
- 解析完整 RON 语义并替代客户端审核器。

### 4.2 `apps/myforge-agent`

负责：

- 主动连接 `admin-api` WebSocket。
- 使用本地配置的 agent key 完成连接鉴权。
- 定位 `MYFORGE_ROOT`。
- 将子进程工作目录限制为 `MYFORGE_ROOT`。
- 在允许目录内执行受控命令。
- 捕获 stdout / stderr、退出码、耗时和目标文件摘要。
- 可选调用 `myforge` 工作区内置校验命令，生成审核结果。

不负责：

- 承担正式服务端业务逻辑。
- 绕过 `admin-api` 权限控制。
- 接受玩家直连请求。
- 维护方圆灵构规则事实源；规则和 skill 属于外部 `myforge` 工作区。

### 4.3 `myforge`

`myforge` 是外部 AI 工作区，不是连接 `admin-api` 的网络服务。

负责：

- 维护 Codex skill、提示词模板、规则副本、生成脚本和审核器。
- 保存方圆灵构生成产物，例如 `artifacts/fangyuan/home_preview.ron`。
- 作为 `myforge-agent` 执行命令时的 cwd。

不负责：

- 主动连接远程 `admin-api`。
- 管理 WebSocket、签名、任务状态或审计。
- 直接通知 `game-server`。

### 4.4 `game-server`

P0 中 `game-server` 不直接连接 `myforge-agent` 或 `myforge`，也不订阅蓝图生成结果。

后续如果玩法需要服务端触发或消费蓝图生成结果，应在玩法形态确定后另行设计。无论采用 NATS、内部管理口、资源发布流程还是配置热更，`game-server` 都只应处理结构化结果或已发布资源，不处理本地终端原始控制权。

## 5. 身份与签名

P0 使用简化双向签名，不引入 localapp 的 auth_code 和设备审批模型。

建议配置：

`admin-api`：

```env
MYFORGE_ENABLED=true
MYFORGE_SERVER_PRIVATE_KEY_PATH=./keys/myforge_server_private.pem
MYFORGE_SERVER_PUBLIC_KEY_PATH=./keys/myforge_server_public.pem
MYFORGE_AGENT_PUBLIC_KEYS_JSON={}
MYFORGE_COMMAND_TTL_MS=60000
MYFORGE_COMMAND_TIMEOUT_MS=120000
MYFORGE_MAX_OUTPUT_BYTES=1048576
```

`apps/myforge-agent`：

```env
ADMIN_API_WS_URL=wss://example.com/api/v1/myforge/ws
MYFORGE_AGENT_ID=dev-pc-001
MYFORGE_PROJECT_ID=myforge-local
MYFORGE_AGENT_PRIVATE_KEY_PATH=./keys/myforge_agent_private.pem
MYFORGE_AGENT_PUBLIC_KEY_PATH=./keys/myforge_agent_public.pem
MYFORGE_SERVER_PUBLIC_KEY_PATH=./keys/myforge_server_public.pem
MYFORGE_ROOT=C:\project\myforge
MYFORGE_SHELL=powershell
```

握手流程：

1. `myforge-agent` 连接 `admin-api`：`/api/v1/myforge/ws?agentId=dev-pc-001&projectId=myforge-local`。
2. `admin-api` 发送 challenge。
3. `myforge-agent` 使用 agent 私钥签名 `challenge + agentId + projectId + timestamp`。
4. `admin-api` 使用已配置的 agent 公钥校验签名。
5. 握手成功后，`myforge-agent` 上报能力：平台、`MYFORGE_ROOT` 摘要、可用 profile、Codex 是否可用。
6. 后续每条 `command.execute` 由 `admin-api` 私钥签名。
7. `myforge-agent` 验 server 签名后执行。
8. `myforge-agent` 返回 `command.result` 时使用 agent 私钥签名。

P0 依赖 HTTPS / WSS 保护传输内容。签名用于确认消息来源和防篡改，不额外加密 command payload。

## 6. 任务类型

P0 只定义一个核心业务任务：

```text
fangyuan.blueprint.generate
```

任务输入示例：

```json
{
  "taskType": "fangyuan.blueprint.generate",
  "projectId": "myforge-local",
  "agentId": "dev-pc-001",
  "artifactFile": "artifacts/fangyuan/home_preview.ron",
  "consumerTargetFile": "project/assets/fangyuan/home_preview.ron",
  "rulesFile": "rules/fangyuan/方圆灵构蓝图规则.md",
  "prompt": {
    "theme": "火属性洞府",
    "primitiveLimit": 200,
    "allowedKinds": ["cube", "sphere"],
    "bounds": { "width": 40, "depth": 40, "height": 20 },
    "requirements": [
      "中心有圆相炉心",
      "周围有方相阵基和三层平台",
      "不要生成地面以下几何体",
      "不要生成 rotation、quaternion、euler 或 spin"
    ]
  }
}
```

`artifactFile` 和 `rulesFile` 必须是相对 `MYFORGE_ROOT` 的路径。`consumerTargetFile` 只表示未来交付给 mybevy 或资源发布流程时的目标路径，P0 不由 `myforge-agent` 直接写入该路径。

路径限制：

- 不允许绝对路径。
- 不允许 `../`。
- 不允许 Windows drive。
- 不允许反斜杠作为提交格式。
- 规则文件默认应位于 `rules/fangyuan/`。
- 方圆蓝图输出默认应位于 `artifacts/fangyuan/`。
- `consumerTargetFile` 默认可使用 mybevy 资源路径格式，例如 `project/assets/fangyuan/home_preview.ron`，但只能作为元数据。

## 7. 命令生成

`admin-api` 不应直接接受任意 shell 字符串作为业务输入。它应根据 `taskType` 和任务参数生成受控命令。

P0 推荐命令模板：

```text
codex exec "<生成提示词>"
```

生成提示词由 `admin-api` 拼接，必须包含：

- 规则文档路径：`rules/fangyuan/方圆灵构蓝图规则.md`
- 目标输出文件路径，即 `artifactFile`。
- 主题、数量、范围、结构和颜色要求。
- 禁止事项：非 cube / sphere、超过预算、地面以下几何体、旋转字段、shader、脚本、外部模型路径等。
- 输出要求：直接修改目标 RON 文件，并在 stdout 摘要说明改动。

示例：

```text
请根据 rules/fangyuan/方圆灵构蓝图规则.md，生成 artifacts/fangyuan/home_preview.ron。

需求：
- 主题：火属性洞府
- 几何体数量：不超过 200
- 只允许 cube 和 sphere
- 范围：width=40, depth=40, height=20
- 结构：中心有圆相炉心，周围有方相阵基和三层平台
- 不要生成地面以下几何体
- 不要生成 rotation、quaternion、euler、angular_velocity、rotate 或 spin
- 不要生成 shader、脚本、外部贴图、模型路径或动态 VFX 字段
```

`myforge-agent` 执行子进程时工作目录应为：

```text
%MYFORGE_ROOT%
```

而不是 MyServer 仓库根目录、`apps/myforge-agent` 目录，也不是 mybevy 仓库根目录。

## 8. WebSocket 消息

### 8.1 `agent.hello`

`myforge-agent -> admin-api`

```json
{
  "type": "agent.hello",
  "agentId": "dev-pc-001",
  "projectId": "myforge-local",
  "timestamp": "2026-07-10T12:00:00.000Z",
  "signature": "base64..."
}
```

### 8.2 `agent.register`

`myforge-agent -> admin-api`

```json
{
  "type": "agent.register",
  "agentId": "dev-pc-001",
  "projectId": "myforge-local",
  "hostname": "DESKTOP-1LG9IK4",
  "platform": "win32",
  "agentApp": "apps/myforge-agent",
  "forgeRoot": "C:\\project\\myforge",
  "capabilities": {
    "codexExec": true,
    "shell": "powershell",
    "fangyuanBlueprint": true
  }
}
```

### 8.3 `command.execute`

`admin-api -> myforge-agent`

```json
{
  "type": "command.execute",
  "requestId": "uuid",
  "taskType": "fangyuan.blueprint.generate",
  "agentId": "dev-pc-001",
  "projectId": "myforge-local",
  "profile": "codex_exec",
  "cwd": ".",
  "command": "codex exec \"...\"",
  "artifactFile": "artifacts/fangyuan/home_preview.ron",
  "consumerTargetFile": "project/assets/fangyuan/home_preview.ron",
  "timeoutMs": 120000,
  "maxOutputBytes": 1048576,
  "issuedAt": "2026-07-10T12:00:00.000Z",
  "expiresAt": "2026-07-10T12:01:00.000Z",
  "signature": "base64..."
}
```

### 8.4 `command.started`

`myforge-agent -> admin-api`

```json
{
  "type": "command.started",
  "requestId": "uuid",
  "agentId": "dev-pc-001",
  "startedAt": "2026-07-10T12:00:02.000Z",
  "signature": "base64..."
}
```

### 8.5 `command.result`

`myforge-agent -> admin-api`

```json
{
  "type": "command.result",
  "requestId": "uuid",
  "agentId": "dev-pc-001",
  "status": "completed",
  "exitCode": 0,
  "stdout": "生成完成...",
  "stderr": "",
  "artifactFile": "artifacts/fangyuan/home_preview.ron",
  "consumerTargetFile": "project/assets/fangyuan/home_preview.ron",
  "artifact": {
    "exists": true,
    "sha256": "hex...",
    "bytes": 12345,
    "modifiedAt": "2026-07-10T12:00:30.000Z"
  },
  "audit": {
    "status": "passed",
    "errors": 0,
    "warnings": 0,
    "primitiveCount": 180,
    "mainCode": ""
  },
  "startedAt": "2026-07-10T12:00:02.000Z",
  "completedAt": "2026-07-10T12:00:30.000Z",
  "signature": "base64..."
}
```

`stdout` 和 `stderr` 必须按 `maxOutputBytes` 截断。完整日志如需保留，应由 `myforge-agent` 写本地日志文件，`admin-api` 只保存摘要。

## 9. 持久化建议

P0 可新增两张表。

### 9.1 `myforge_agents`

```sql
CREATE TABLE myforge_agents (
  agent_id varchar(128) PRIMARY KEY,
  project_id varchar(128) NOT NULL,
  label varchar(128) NULL,
  public_key_fingerprint varchar(128) NOT NULL,
  status varchar(32) NOT NULL DEFAULT 'offline',
  capabilities_json jsonb NULL,
  last_seen_at timestamptz NULL,
  created_at timestamptz NOT NULL DEFAULT current_timestamp,
  updated_at timestamptz NOT NULL DEFAULT current_timestamp
);
```

### 9.2 `myforge_task_runs`

```sql
CREATE TABLE myforge_task_runs (
  request_id uuid PRIMARY KEY,
  task_type varchar(64) NOT NULL,
  project_id varchar(128) NOT NULL,
  agent_id varchar(128) NOT NULL,
  status varchar(32) NOT NULL,
  artifact_file varchar(512) NULL,
  consumer_target_file varchar(512) NULL,
  prompt_json jsonb NULL,
  command_preview text NULL,
  stdout_preview text NULL,
  stderr_preview text NULL,
  exit_code int NULL,
  artifact_json jsonb NULL,
  audit_json jsonb NULL,
  error_code varchar(64) NULL,
  error_message text NULL,
  created_by_admin_id bigint NULL,
  created_by_admin_username varchar(64) NULL,
  created_at timestamptz NOT NULL DEFAULT current_timestamp,
  started_at timestamptz NULL,
  completed_at timestamptz NULL
);
```

所有创建、下发、完成、失败和取消操作都应写 `admin_audit_logs`。

## 10. 权限建议

新增权限点：

```text
myforge.agent.read
myforge.task.read
myforge.task.create
myforge.task.cancel
```

第一阶段可以只给 `admin` / `super_admin` 全量权限，不给 `operator` 默认创建任务权限。原因是该能力会在本地执行命令并生成客户端资源产物，后续还可能进入资源发布流程，风险高于普通 GM 广播。

如果后续需要让策划或内容角色使用，应增加更细的 profile 权限，例如只允许 `fangyuan.blueprint.generate`，不能执行通用 command。

## 11. admin-web 展示闭环

P0 的完成闭环是 `admin-web` 可以看到任务执行结果，不要求 `game-server` 消费。

推荐 `admin-web` 第一阶段提供：

- agent 在线状态列表。
- 任务创建表单。
- 任务列表：任务类型、agent、状态、创建人、创建时间、耗时。
- 任务详情：prompt 参数、artifact 路径、consumer target 路径、stdout / stderr 摘要、exit code、错误码。
- 审核结果：`passed` / `warning` / `failed`、primitive 数量、主错误 code、finding 摘要。
- artifact 摘要：是否存在、sha256、文件大小、修改时间。

`admin-web` 可以通过轮询 `GET /api/v1/myforge/tasks/:requestId` 获取状态变化。P0 不要求新增浏览器 WebSocket 或 SSE；如果后续任务耗时较长，再考虑推送式刷新。

`admin-api` 获取 `myforge-agent` 返回后，不应把原始终端输出直接发给 `game-server`。P0 只保存 stdout / stderr 摘要和结构化 artifact / audit 信息，并返回给 `admin-web` 展示。

## 12. game-server 接入待定

`game-server` 的具体接入形式待玩法确定后再设计。可能方向包括：

- `admin-api` 发布结构化 NATS 事件，例如 `myserver.myforge.task_result`。
- `game-server` 通过内部管理口按任务或资源版本拉取结果摘要。
- 资源发布流程将 artifact 发布到 mybevy / 资源仓库 / 对象存储，再由游戏服或客户端消费。
- 后台人工确认后触发配置热更或运行时加载。

无论选择哪种方式，都不能假设远程 `game-server` 能直接读取本地开发机上的 `myforge-agent`、`myforge` 或 `mybevy` 文件。

## 13. 校验与审核

`myforge-agent` 至少返回文件摘要：

- 文件是否存在。
- 相对路径。
- sha256。
- 文件大小。
- 修改时间。

如 `myforge` 工作区提供方圆蓝图审核命令，`myforge-agent` 应在 Codex 生成后运行审核，并返回：

- `passed` / `warning` / `failed`
- error 数量。
- warning 数量。
- primitive 数量。
- 主错误 code。
- 简短 finding 摘要。

审核规则以 `myforge` 项目内的规则副本、生成脚本和审核器实现为准。该规则副本可以从 `mybevy/docs/世界观/方圆灵构蓝图规则.md` 同步而来，但 MyServer 不直接读取 mybevy 文档，也不复制完整蓝图解析器，只记录审核摘要。

## 14. 失败处理

常见失败码：

```text
MYFORGE_AGENT_OFFLINE
MYFORGE_AGENT_AUTH_FAILED
MYFORGE_SERVER_SIGNATURE_INVALID
MYFORGE_COMMAND_EXPIRED
MYFORGE_COMMAND_TIMEOUT
MYFORGE_COMMAND_FAILED
MYFORGE_OUTPUT_TOO_LARGE
MYFORGE_ROOT_MISSING
MYFORGE_TARGET_PATH_INVALID
MYFORGE_TARGET_FILE_MISSING
FANGYUAN_BLUEPRINT_AUDIT_FAILED
```

失败策略：

- `admin-api` 创建任务后，如果 agent 离线，任务进入 `queued` 或直接 `failed`，由接口参数决定。
- 命令超时后，`myforge-agent` 应终止子进程并返回 `MYFORGE_COMMAND_TIMEOUT`。
- 生成成功但审核失败时，任务状态可以是 `completed_with_errors`，并保留 artifact 摘要。
- `admin-web` 应展示失败阶段、错误码、简短错误信息和可截断的 stdout / stderr 摘要。

## 15. P0 接口草案

`admin-api` HTTP：

```text
GET  /api/v1/myforge/agents
GET  /api/v1/myforge/tasks
GET  /api/v1/myforge/tasks/:requestId
POST /api/v1/myforge/tasks/fangyuan-blueprint
POST /api/v1/myforge/tasks/:requestId/cancel
```

创建任务请求：

```json
{
  "agentId": "dev-pc-001",
  "projectId": "myforge-local",
  "artifactFile": "artifacts/fangyuan/home_preview.ron",
  "consumerTargetFile": "project/assets/fangyuan/home_preview.ron",
  "theme": "火属性洞府",
  "primitiveLimit": 200,
  "requirements": [
    "中心有圆相炉心",
    "周围有方相阵基和三层平台"
  ]
}
```

响应：

```json
{
  "ok": true,
  "requestId": "uuid",
  "status": "dispatched"
}
```

## 16. 与 myforge-agent / myforge / mybevy 的关系

MyServer 只保存任务记录和结果摘要，不把生成的 RON 文件复制进 MyServer 仓库。

`apps/myforge-agent` 是 MyServer 仓库内新增的本地连接器 app，负责连接远程 `admin-api`、验签、执行任务和回传结果。它不保存规则事实源，也不作为生成产物仓库。

`myforge` 是独立 Git 项目，负责维护 AI skill、生成规则、提示词模板、执行器、审核器适配和生成产物。P0 本地路径示例：

```text
C:\project\myforge
```

路径规则：

- `myforge-agent` 进程通过 `MYFORGE_ROOT` 定位外部 `myforge` 工作区。
- `admin-api` 不读取 `MYFORGE_ROOT`，只保存 agent 上报的 `forgeRoot` 摘要和任务相对路径。
- `artifactFile`、`rulesFile` 等执行路径必须相对 `MYFORGE_ROOT`。
- `consumerTargetFile` 只表示给 mybevy 或资源发布流程消费时的目标路径，不触发直接写入。
- `MYFORGE_ROOT` 未设置或路径不存在时，`myforge-agent` 返回 `MYFORGE_ROOT_MISSING`。

如果后续需要把生成结果带入服务端运行时，应新增明确的资源发布流程，例如：

1. `myforge-agent` 在 `myforge` 工作区生成并审核 RON。
2. 人工或后台确认。
3. 将资源从 `myforge` 产物目录复制、提交或发布到 mybevy / 资源仓库 / 对象存储。
4. 发布流程将资源同步到客户端或服务端可访问的位置。
5. 如 game-server 后续需要消费该资源，再通过受控热更、资源发布或运行时加载方案接入。

P0 不直接跨机器拷贝本地开发机文件到远程 game-server。

## 17. 落地顺序

建议分阶段实现：

1. `admin-api` 增加 myforge-agent WebSocket 接入、agent 注册和心跳。
2. 增加双向签名、TTL、requestId 和结果验签。
3. 增加 `fangyuan.blueprint.generate` 任务创建接口。
4. 新增 `apps/myforge-agent`，支持 `codex_exec` profile，并限制子进程工作目录为 `MYFORGE_ROOT`。
5. 持久化 task run 和 admin audit。
6. 返回目标文件 sha256、大小和审核摘要。
7. 后台页面展示任务列表、状态、stdout / stderr 摘要和 artifact 信息。
8. P0 验证 `admin-web -> admin-api -> myforge-agent -> MYFORGE_ROOT -> admin-api -> admin-web` 闭环。
9. 玩法形态明确后，再设计 `game-server` 接入方式。

## 18. 关键结论

- 该能力归属“服务端触发本地 `myforge-agent`，在外部 `myforge` 工作区生成客户端资源”，不是运维终端。
- `admin-api` 是合适入口；`auth-http` 不参与。
- `apps/myforge-agent` 是本地连接器和执行器，主动连接远程。
- `myforge` 是 AI skill 和资源生成工作区，不负责连接远程 server。
- P0 只做一次性 `codex exec` 任务，不做交互式终端。
- P0 只要求 admin-web 展示执行结果；`game-server` 接入方式待玩法确定后另行设计。
- 双向签名足够支撑当前需求，命令内容依赖 WSS 保护。
- 任务输入必须是 typed request，由 `admin-api` 生成受控命令，不能把任意 shell 暴露给普通调用方。
- 生成规则和审核事实源在外部 `myforge` 项目内维护；mybevy 可以作为规则来源和产物消费方，MyServer 第一阶段只负责调用、记录和后台展示。
