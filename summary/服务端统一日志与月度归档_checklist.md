# 服务端统一日志与月度归档 Checklist

## 目标

统一 MyServer 线上 Rust 与 Node.js 服务的应用日志目录、日轮转、实例隔离和月度归档。Rust 服务继续使用现有 `tracing + tracing-subscriber + tracing-appender`，Node.js server 全部使用 `log4js`；生产文件日志统一落到宿主机 `/data/myserver/log/<service>/`，当前月和上一个自然月保留原始日文件，上上个月及更早的完整月份按服务和日志类型压缩归档，校验成功后删除对应原始文件。所有压缩归档永久保留，不提供自动删除。

本清单覆盖生产 Compose 中的 Rust 服务 `game-server`、`game-proxy`、`chat-server`、`match-service`，以及 Node.js server `auth-http`、`admin-api`、`announce-service`、`mail-service`、`metrics-collector`。`admin-web`、Caddy、PostgreSQL、Redis、NATS、Docker Engine 和当前未进入生产 Compose 的 `myforge-agent` 不属于本轮文件日志统一范围。

本清单是 `summary/07_可观测性容量与故障恢复_checklist.md` 中日志契约和部署准入部分的专项落地清单；两者出现冲突时，以当前代码、正式专题文档和本清单后续审阅结论为准。

## 已确认实施策略

- Rust 线上服务保留当前日志技术栈，不更换日志框架。
- 所有 Node.js server 统一使用 `log4js`，包括当前直接使用 `console` 的 `metrics-collector`。
- 生产同时启用 console 和文件日志；文件日志是长期保留事实源，Docker `local` driver 仅保留受限的短期 stdout 副本。
- 日志目录按服务隔离，日文件名包含 `SERVICE_INSTANCE_ID`，禁止同一主机多实例共同写一个文件。
- 运行日志和 admin audit 分目录、分归档包管理；admin audit 保持 JSONL、追加写和敏感操作审计语义。
- 日文件统一按 UTC 自然日切分，归档月份也按 UTC 计算。
- 当前月和上一个自然月保留原始日文件；上上个月及更早的完整月份进入归档候选。
- 月归档使用 `.tar.zst`，必须先生成 manifest、完成压缩和校验，再删除 manifest 精确列出的原始文件。
- 正式归档包不可覆盖；归档后出现迟到文件时生成带序号的 supplement 包。
- 所有正式归档包永久保留，归档工具不得包含按时长、数量或容量删除归档的逻辑。
- MyServer 不实现磁盘容量阈值监控或告警，容量提醒由云厂商负责。
- 同盘归档只解决原始日志整理问题，不声明为异机备份或灾难恢复能力。

## 基础原则

- [ ] 日志不得记录密码、完整 token、JWT、game ticket、邮件正文、完整附件或其他敏感 payload。
- [ ] 应用容器只获得自身 daily/audit 写权限，不得读取其他服务日志或写入 archive 目录。
- [ ] 生产启用文件日志后，日志目录不可写必须阻止服务静默进入无文件日志状态。
- [ ] 日志归档必须并发互斥、可重复执行、失败安全，并禁止跟随符号链接或删除约定根目录外的文件。
- [ ] 归档删除只能作用于已经进入成功归档 manifest 的精确文件列表，不使用宽泛 glob 或递归删除。
- [ ] 每个阶段完成后按改动范围执行验证；需要启动服务、Docker、WSL 发布验证或真实依赖时，先按仓库协作约定向用户确认。

## 阶段 1：统一日志契约与范围冻结

- 开始时间：
- 结束时间：
- 开发总结：
- 验证记录：

- [ ] 定义宿主机目录契约：`/data/myserver/log/<service>/daily`、`audit` 和 `archive/<year>`。
- [ ] 定义容器内目录契约，确保每个容器只挂载自身 daily 和必要的 audit 子目录。
- [ ] 定义运行日文件名，至少包含规范化 service name、`SERVICE_INSTANCE_ID`、UTC 日期和 `.log` 扩展名。
- [ ] 定义 admin audit 日文件名，至少包含实例 ID、UTC 日期和 `.jsonl` 扩展名。
- [ ] 定义运行日志、admin audit、Docker stdout 和数据库审计表之间的职责边界。
- [ ] 固定 `LOG_LEVEL`、`LOG_ENABLE_CONSOLE`、`LOG_ENABLE_FILE`、`LOG_DIR` 的跨语言语义、默认值和 production 覆盖规则。
- [ ] 明确生产服务启动时对非法日志级别、非法实例 ID、目录创建失败和目录不可写的稳定失败行为。
- [ ] 审阅日志字段和格式，确定时间戳、level、service、instance、target/category、message 及结构化附加字段的最低契约。
- [ ] 验证契约覆盖本清单列出的九个生产应用服务，且未把 admin-web、基础设施或 myforge-agent 误纳入。

## 阶段 2：Node.js 统一 log4js 实现

- 开始时间：
- 结束时间：
- 开发总结：
- 验证记录：

- [ ] 建立可由五个 Node.js server 复用的统一 `log4js` 配置模块，避免各服务继续复制并分化 dateFile 配置。
- [ ] 统一 console appender 和 dateFile appender 的时间戳、level、category、无 ANSI 文件输出和异常序列化行为。
- [ ] dateFile 使用立即带日期的文件名，禁止依赖跨日后重命名当前 `app.log` 才获得日期。
- [ ] 日文件名加入经过校验的 `SERVICE_INSTANCE_ID`，防止多实例并发写同一文件。
- [ ] 移除 `admin-api` 当前 `daysToKeep: 7` 等应用内自动删除策略，所有服务均不得自行清理历史文件。
- [ ] 为 `metrics-collector` 增加 `log4js` 依赖、统一日志配置和四个日志环境变量。
- [ ] 将五个 Node.js server 运行期的 `console.log`、`console.warn` 和 `console.error` 收敛到分级 logger；仅允许在 logger 尚未成功初始化时使用最小 stderr fallback。
- [ ] 正常退出和 fatal 退出前完成 `log4js.shutdown`，并为 shutdown 失败定义不会无限阻塞进程的处理方式。
- [ ] 补齐各服务 `.env.example`，默认目录使用各自本地开发路径，production 路径由 Compose 覆盖。
- [ ] 为配置解析、日文件命名、console/file 开关、无 appender fallback、错误序列化和优雅关闭增加定向测试。

## 阶段 3：Rust 日志文件契约统一

- 开始时间：
- 结束时间：
- 开发总结：
- 验证记录：

- [ ] 保留 `tracing + tracing-subscriber + tracing-appender`，不引入替代日志框架。
- [ ] 统一 `game-server`、`game-proxy`、`chat-server`、`match-service` 的文件 appender 初始化和错误处理语义。
- [ ] 使用现有 daily rolling 能力生成包含 `SERVICE_INSTANCE_ID` 和 UTC 日期的独立日文件。
- [ ] 确认 console 输出允许 ANSI、文件输出禁止 ANSI，且两类输出受同一个 `LOG_LEVEL` filter 控制。
- [ ] 确认只启用 console、只启用 file、两者同时启用及两者都关闭时的行为与统一契约一致。
- [ ] 对日志目录不可创建、不可写、实例 ID 非法和 filter 非法增加稳定失败测试。
- [ ] 验证跨 UTC 零点切换后旧文件关闭、新文件创建，且不会覆盖已有日文件。
- [ ] 验证同一服务两个不同实例 ID 在同一宿主机目录下写入不同文件。

## 阶段 4：admin audit 按日轮转

- 开始时间：
- 结束时间：
- 开发总结：
- 验证记录：

- [ ] 盘点 `game-server` 和 `game-proxy` 当前 admin audit 写入、失败阻断、字段脱敏和追加写语义。
- [ ] 将固定 `admin-audit.jsonl` 调整为按 UTC 日期和 `SERVICE_INSTANCE_ID` 隔离的 JSONL 日文件。
- [ ] 保持 admin audit 与普通 tracing 日志分离，不通过普通文本 appender 降级结构化审计记录。
- [ ] 跨日切换时保证单条 JSON 不被拆分、重复或写入错误日期文件。
- [ ] 保持敏感写操作在 audit 目录不可写或追加失败时的既有安全优先行为。
- [ ] 为并发追加、跨日切换、进程重启、文件预先存在、目录不可写和部分写失败增加测试。
- [ ] 定义 runtime 与 audit 独立月归档包，禁止将两类日志混入同一个压缩包。

## 阶段 5：生产目录初始化与 Compose 挂载

- 开始时间：
- 结束时间：
- 开发总结：
- 验证记录：

- [ ] 在生产初始化流程中创建九个服务的 daily/archive 目录，以及 game-server、game-proxy 的 audit 目录。
- [ ] 根据 Node 与 Rust 容器实际 UID/GID 设置最小目录所有权和权限，不以 world-writable 规避权限差异。
- [ ] 为每个服务使用独立 bind mount，禁止把整个 `/data/myserver/log` 根目录暴露给所有容器。
- [ ] 在 Compose 中为九个服务设置 `LOG_ENABLE_CONSOLE=true`、`LOG_ENABLE_FILE=true` 和统一容器内 `LOG_DIR`。
- [ ] 设置 game-server、game-proxy 的 admin audit 容器路径，并挂载各自 audit 目录。
- [ ] 保持 archive 目录仅宿主机归档工具可写，不挂载到应用容器。
- [ ] 保留 Docker `local` logging driver 的短期 stdout 大小和文件数上限，避免 console/file 双写导致 Docker 日志无限增长。
- [ ] 生产 preflight 检查目录存在、权限正确、非符号链接、挂载目标匹配服务且剩余配置没有把文件日志重新关闭。
- [ ] 使用 Compose config/dry-run 验证九个服务的环境变量和 bind mount，不启动真实服务前先取得用户确认。

## 阶段 6：安全月度归档工具

- 开始时间：
- 结束时间：
- 开发总结：
- 验证记录：

- [ ] 新增 Linux 宿主机归档入口，固定允许的日志根目录和服务 allowlist，拒绝空路径、相对路径、根目录、未知服务和符号链接目标。
- [ ] 支持 `--dry-run`、指定服务、指定月份、处理全部到期月份、仅验证 archive 和工具自检模式。
- [ ] 按 UTC 月份选择候选：当前月与上一个月不处理，上上个月及更早的完整月份可归档。
- [ ] 使用 `flock` 或等价机制阻止两个归档进程同时处理同一日志根目录。
- [ ] runtime 与 audit 分别生成 source manifest，记录服务、实例、日志类型、月份、文件名、字节数、mtime 和 SHA-256。
- [ ] 只接受符合统一命名契约的普通文件，拒绝符号链接、硬链接异常、设备文件、目录和仍在变化的文件。
- [ ] 在目标 archive 目录创建 `.tar.zst.tmp`，压缩完成后执行 archive 列表、解压和 manifest/SHA-256 校验。
- [ ] 全部校验成功后在同一文件系统内原子改名为正式 `.tar.zst`，再按 manifest 精确删除原始日文件。
- [ ] 任一步骤失败时返回非零状态，不删除原始文件，不留下可被识别为正式归档的半成品。
- [ ] 正式归档已存在时只校验、不覆盖；发现未进入既有 archive 的迟到文件时生成 `supplement-<序号>` 包。
- [ ] 工具不得实现 archive 删除、archive retention、按磁盘容量清理或磁盘容量告警逻辑。
- [ ] 输出机器可读执行报告，包含处理月份、归档路径、文件数、原始/压缩字节数、校验结果、删除结果和失败原因，但不输出日志正文。

## 阶段 7：systemd 调度与运维入口

- 开始时间：
- 结束时间：
- 开发总结：
- 验证记录：

- [ ] 提供归档专用 systemd oneshot service 和 timer，使用绝对路径、受限权限和固定日志根目录。
- [ ] timer 定期运行幂等归档扫描，错过执行时间后可补跑所有到期月份，而不是只处理单个硬编码月份。
- [ ] systemd service 失败时保留非零结果和 journal 记录，但不接入 MyServer 自建告警通道。
- [ ] 提供安装、启用、手工 dry-run、手工归档、状态查看、失败重试和 archive 校验命令。
- [ ] 确认归档服务账户可读取 daily/audit、写入 archive 和删除已归档原文件，但不能修改应用配置、数据库或其他 `/data/myserver` 数据。
- [ ] 确认压缩工具及版本前置条件，缺少 `tar`、`zstd`、`sha256sum` 或 `flock` 时安全失败且不删除日志。
- [ ] 不新增磁盘阈值监控、容量采集、通知 webhook、邮件告警或归档自动清理任务。

## 阶段 8：自动化测试与故障验证

- 开始时间：
- 结束时间：
- 开发总结：
- 验证记录：

- [ ] 在 Windows 工作区运行 Node logger 配置、五个 Node 服务和受影响公共模块的定向测试。
- [ ] 在 Windows 工作区运行四个 Rust 服务的日志配置、日轮转和 admin audit 定向测试。
- [ ] 使用临时目录测试月边界、跨年、闰年、空月份、多实例、runtime/audit 隔离和多个积压月份。
- [ ] 测试归档并发、压缩失败、磁盘写失败、校验失败、删除失败、进程中断、残留临时包和重试恢复。
- [ ] 测试已有正式 archive、合法 supplement、恶意文件名、符号链接、未知服务、路径穿越和根目录误配置拒绝。
- [ ] 验证归档成功后 archive 可完整解压、manifest 哈希一致且仅删除源清单中的文件。
- [ ] 验证归档失败时所有原始文件仍存在，正式 archive 不被覆盖，其他月份和服务文件不受影响。
- [ ] 按 Windows/WSL 工作区边界，在用户确认后仅于 WSL 发布验证中执行 Linux shell、systemd unit 静态检查和临时目录端到端自检。
- [ ] 扫描测试产物与日志，确认不包含 token、ticket、密码、连接串或生产日志正文。

## 阶段 9：文档、发布准入与上线验证

- 开始时间：
- 结束时间：
- 开发总结：
- 验证记录：

- [ ] 更新 `CLAUDE.md` 的 Node/Rust 日志机制、统一目录、日轮转和永久 archive 约束。
- [ ] 更新监控、安全、生产 Docker 初始化、发布、更新和故障排查文档，明确普通日志与 admin audit 的边界。
- [ ] 更新生产配置示例和初始化脚本说明，记录九个服务目录、权限、挂载、环境变量和工具依赖。
- [ ] 在发布准入中检查文件日志开启、目录可写、实例文件名隔离、archive 不挂载到应用容器和 Docker stdout 上限仍生效。
- [ ] 制定上线顺序：目录初始化、工具 dry-run、服务分批启用文件日志、日切换观察、归档 dry-run、首个正式月归档。
- [ ] 制定回滚顺序：停止 timer、保留 archive 和原始文件、关闭文件 appender 或回滚 Compose，不删除已生成归档。
- [ ] 真实线上启用、首次压缩或删除原始日志前，列出目标服务、月份、文件数、预计字节数和回滚边界，并等待用户明确授权。
- [ ] 全部完成后将本 checklist 从 `summary/` 移入 `docs/安全与监控/checklists/` 归档，再纳入对应 Git 提交。

## 最终完成定义

以下项目作为整体完成标准，不要求每个开发阶段都执行，由所有相关阶段完成后统一验收。

- 开始时间：
- 结束时间：
- 验收总结：

- [ ] 九个生产应用服务使用统一四变量日志模型，Rust 保持 tracing 技术栈，Node.js server 全部使用 log4js。
- [ ] 每个服务和实例都在 `/data/myserver/log/<service>/` 下生成独立 UTC 日文件，不发生跨实例文件竞争。
- [ ] game-server、game-proxy 的 admin audit 保持结构化、追加写、安全优先并实现独立日轮转。
- [ ] 当前月和上一个月原始日志不被归档工具处理，上上个月及更早日志可安全生成 runtime/audit 月归档。
- [ ] 只有压缩、manifest 和 SHA-256 全部校验成功后才删除对应原始文件，所有失败路径均保留原始日志。
- [ ] 正式 archive 永不覆盖、永不自动删除，迟到日志通过 supplement 包追加归档。
- [ ] 应用容器不能访问其他服务日志或 archive，归档账户不能修改日志根目录外的数据。
- [ ] 归档 timer 可幂等补跑、失败返回非零并保留执行记录，不包含 MyServer 自建磁盘容量告警。
- [ ] Windows 定向测试、WSL 发布验证、生产 dry-run 和首轮受控归档均有明确验证记录。
- [ ] 相关配置、部署、安全、运维和回滚文档与最终实现一致。
