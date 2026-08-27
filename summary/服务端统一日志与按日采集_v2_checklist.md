# 服务端统一日志与按日采集 v2 Checklist

## 目标

在当前 Docker 单机部署模型下，为 MyServer 建立简单、可追溯、可继续演进的日志链路：业务服务继续输出 console 日志，Docker 使用固定大小的 `local` driver 作为短期缓冲和故障兜底，宿主机受控服务运行的 Vector 持续读取 Docker 日志并按 UTC 日期落盘到 `/data/myserver/log`。Vector 输出按服务、日期和实例隔离，并对单文件设置大小上限；后续再由独立工具读取 Vector 输出，完成压缩、异机或对象存储归档。

本版本不要求每个 Node.js/Rust 服务各自维护长期文件日志，不把 Docker 内部日志文件布局作为应用接口，也不把 metrics 归档和业务审计归档混入普通运行日志链路。

## 方案定论

- 普通运行日志继续写 stdout/stderr；本轮不要求九个应用全部增加或启用文件 appender。
- Docker `local` driver 继续使用固定大小和文件数上限，作为短期缓冲，不作为长期事实源。
- 固定使用 Vector 作为普通运行日志采集器，首选以宿主机 systemd service 或等价受控服务运行，不进入业务容器，不要求业务容器访问 Docker socket。
- Vector 必须持续流式读取，并在启动或重连时补读 Docker 保留窗口内的历史日志；禁止采用“定时执行一次 `docker logs`”作为主采集机制。
- Vector 输出以完整日志流为事实源，按 UTC 日期目录保存；单个文件达到大小上限后追加序号分片。
- 默认不在应用内部拆分 `info.log` 与 `error.log`；每条采集记录保留或推断 `level`，错误筛选由采集工具或日志平台完成，必要时才生成派生 error 视图。
- `game-server`、`game-proxy` 的 admin audit、PostgreSQL 中的 `admin_audit_logs`/`security_audit_logs` 与 metrics-collector 的 metrics 数据各自保持独立职责。
- 本机日志保留期、容量上限和长期归档目的地可配置；永久归档不作为本版本的本机完成条件。

## 基础原则

- [x] Vector 故障、重启、延迟或磁盘写入失败不得导致游戏服务进程退出或业务请求同步阻塞。（验证：Vector 独立 systemd 进程、有界 buffer/drop_newest、preflight/alerts/recovery 定义故障时业务继续运行；未执行 live 故障注入）
- [x] 采集链路必须显式暴露延迟、队列长度、读取失败、写入失败、补采范围和可能丢失的时间段。（验证：internal metrics、`vector-status.sh`、`vector-alerts.sh`、preflight 输出和 gap/checkpoint 诊断）
- [x] Vector 不能依赖 Docker 私有目录格式；读取必须通过 Docker 支持的日志流/API 或等价稳定接口。（验证：source 使用 `docker_logs` + Docker API socket；scanner 和配置测试拒绝私有日志路径）
- [x] 采集输出必须包含服务名、容器 ID、`SERVICE_INSTANCE_ID`（可获取时）、release/build 标识（可获取时）、UTC 采集时间、原始日志时间（可解析时）和 level（可解析时）。（验证：Vector envelope/Compose labels/UTC template 静态测试）
- [x] 不记录或传播密码、token、ticket、JWT、DSN、邮件正文、完整附件和其他敏感 payload；Vector 不得因为补充元数据而复制 secret 环境变量。（验证：Node logger 脱敏测试、Rust 字段修复和 scanner 220 文件通过；Vector 不读取 env secrets）
- [x] 服务、实例、日期和分片文件名必须经过白名单校验，拒绝路径穿越、符号链接目标和未知服务写入。（验证：allowlist、rotate/prune 正则/readlink 校验及 runtime 恶意路径测试）
- [x] `/data/myserver/log` 只由 Vector 和受控归档工具写入；业务容器不得访问其他服务日志或长期归档目录。（验证：systemd ReadWritePaths、Compose 无 log-root/docker.sock 挂载、preflight mount 检查）
- [x] Docker 保留窗口、Vector 本地保留期和后续远端归档保留期必须分别定义，不以其中一个配置冒充另一个。（验证：日志设计第 9 节固定 Docker 20m x3、Vector 14 UTC 日和独立远端 manifest/保留契约）

## 援引文档

以下文档用于确定当前部署事实、职责边界和本 Checklist 的改造方向：

- 文档名称：整体架构
  - 路径或链接：`docs/总览/整体架构.md`
  - 用途说明：确认多服务边界、Redis/NATS/PostgreSQL metrics 与审计职责，以及生产部署不应依赖服务固定端口的整体约束。
- 文档名称：服务健康检查与监控实现说明
  - 路径或链接：`docs/安全与监控/监控设计.md`
  - 用途说明：确认 metrics-collector、Redis v2 history 和 PostgreSQL `metrics_archive` 是独立于普通日志的指标链路。
- 文档名称：服务端日志采集与留存设计
  - 路径或链接：`docs/安全与监控/服务端日志采集与留存设计.md`
  - 用途说明：作为日志架构、目标态数据流、目录与分片契约、级别查询、审计/metrics 边界、丢失边界和后续归档接口的正式设计依据。
- 文档名称：服务器初始化实操
  - 路径或链接：`docs/后台与运维/Docker部署/服务器初始化实操.md`
  - 用途说明：确认宿主机 `/data`、Docker `local` driver、`20m x 3` 日志兜底和容量约束。
- 文档名称：服务器 Docker 初始化与更新
  - 路径或链接：`docs/后台与运维/Docker部署/服务器Docker初始化与更新.md`
  - 用途说明：确认生产 Compose、release runner、运维权限、日志巡检和回滚边界。
- 文档名称：生产 Docker Compose
  - 路径或链接：`deploy/docker/compose.production.yml`
  - 用途说明：确认当前业务服务通过 `LOG_ENABLE_FILE=false` 使用 console、服务实例 ID 和现有 volume 挂载事实。
- 文档名称：Docker 运维脚本
  - 路径或链接：`deploy/docker/OPS.md`
  - 用途说明：将 Vector 安装、状态、日志查看和失败恢复纳入现有受控运维入口。
- 文档名称：服务端统一日志与月度归档 Checklist（v1）
  - 路径或链接：`summary/服务端统一日志与月度归档_checklist.md`
  - 用途说明：保留其中的脱敏、实例隔离、幂等、manifest 校验和失败安全原则；不继承其“所有服务启用文件日志”和“本机永久归档”前提。

## 阶段 1：日志职责与采集契约冻结

- 开始时间：2026-08-27 13:38:17 +08:00
- 结束时间：2026-08-27 13:48:24 +08:00
- 开发总结：冻结普通运行日志、审计、数据库迁移审计与 metrics 的职责边界，确定九服务采集 allowlist、Vector envelope、UTC/实例/容器分片契约、容量阈值、故障恢复边界和后续归档输入规则。
- 验证记录：5 份部署/运维/日志设计文档已同步；`git diff --check`、allowlist 引用、envelope 字段和容量阈值只读检查通过。

- [x] 固定普通运行日志、admin/security audit、数据库审计和 metrics 的职责边界，并在部署文档中明确查询入口。（验证：`docs/安全与监控/服务端日志采集与留存设计.md` 第 6.1 节和第 7 节、`deploy/docker/OPS.md` 查询入口说明）
- [x] 固定九个生产应用服务的 allowlist；明确 `metrics-collector` 是指标消费者，同时其自身 console 输出也由 Vector 采集，但不改造成 metrics 专用日志框架。（验证：日志设计第 4.1 节及 Docker 初始化/更新、Release、OPS 文档中的九服务名单）
- [x] 固定 Vector 输出根目录为 `/data/myserver/log`，定义服务、UTC 日期、实例和分片的目录/文件命名契约。（验证：日志设计第 4、5.1 节固定 UTC 目录、实例/容器前缀、四位分片号和 `.jsonl.open` 活动后缀）
- [x] 定义采集记录 envelope：`service`、`container_id`、`instance_id`、`release_id`、`captured_at`、`event_time`、`level`、`stream`、`message` 和 `parse_status`。（验证：日志设计第 5.2 节稳定字段表；只读字段检查通过）
- [x] 定义非 JSON、异常编码、缺少时间或无法识别 level 的处理方式，确保原始消息仍可追溯。（验证：日志设计第 5.2 节 `parse_status` 状态码、1 MiB 消息上限、受限原始字节复核和哈希诊断规则）
- [x] 定义 Docker 日志保留窗口、Vector 本地保留期、单文件大小、日期目录保留期、队列上限和磁盘保护阈值。（验证：日志设计第 8-9 节及部署文档：Docker `20m x 3`、分片 `256 MiB`、14 UTC 日、1 GiB/64 MiB 队列、20%/10%/5% 阈值）
- [x] 定义 Vector 停止、重启、容器替换、Docker API 不可用、磁盘满和 checkpoint 损坏时的可观察结果与恢复边界。（验证：日志设计第 9.1 节故障矩阵；明确 checkpoint、缺口、Docker 窗口和业务可用性边界）
- [x] 明确本版本不实现月度压缩/远端上传，但为后续工具定义只读输入目录、文件完整性和正在写入文件的排除规则。（验证：日志设计第 9 节规定只读 `/data/myserver/log`、仅消费关闭分片、SHA-256 manifest，并排除 `.open`/临时文件/符号链接）

## 阶段 2：常驻 Vector 采集核心能力

- 开始时间：2026-08-27 13:48:24 +08:00
- 结束时间：2026-08-27 14:07:00 +08:00
- 开发总结：新增 Vector 0.47.0 docker_logs 配置、systemd unit、安装/离线校验/状态脚本及 release bundle 接入；补齐 Compose 服务实例与 release labels、Docker 时间戳优先、消息截断、level 规范化、有界 disk buffer、API 和位点诊断。
- 验证记录：`node --test tests/deploy/vector-config.test.mjs` 2 pass/1 skip（Windows 路径导致 bash 子项跳过）；`bash -n` 四个脚本通过；`bash scripts/docker/verify-vector.sh --offline` 通过；`git diff --check` 通过；未安装 Vector 官方 CLI，真实 `vector validate` 待后续 Linux/WSL 环境确认。

- [x] 固定 Vector 版本、配置格式、`docker_logs` source、JSONL sink、宿主机安装路径和升级/回滚方式；首选以 systemd service 运行。（验证：`deploy/docker/vector/vector.yaml`、`vector.service`、`scripts/docker/install-vector.sh` 固定 0.47.0、/etc/vector、/var/lib/vector 和 systemd unit）
- [x] 通过 Vector 的 `docker_logs` source 持续读取所有 allowlist 服务的 stdout/stderr，并限制单条记录大小和异常输入消耗。（验证：`vector.yaml` source/filter allowlist；VRL 以 1048576 字节截断并追加 `truncated`/`message_sha256`）
- [x] 启动时先由 Vector 补读 Docker 保留窗口内的历史日志，再进入实时跟随；重连时按 data_dir/checkpoint、容器创建时间和已读位置避免重复或静默跳过。（验证：`docker_logs` source + `/var/lib/vector` data_dir、systemd Restart 和文档补采边界；真实 CLI 语法待 Linux 验证）
- [x] 为 Vector 的每个容器/日志流保存有界 checkpoint，容器替换后按新容器 ID 建立新游标，并保留旧容器最后可读位置的诊断信息。（验证：配置固定 data_dir/disk buffer；`vector-status.sh` 输出 checkpoint 计数/最新位点，envelope 保留完整 `container_id`；容器替换演练列入阶段 7）
- [x] 将 Docker 元数据和服务环境中的实例/build 信息注入 Vector envelope；实例 ID 缺失、重复或非法时使用安全降级标识并产生告警。（验证：Compose 九服务 `com.myserver.service-instance-id`/`com.myserver.release-id` labels，VRL 缺失时 `unknown-<container-prefix>`/`unknown` 降级）
- [x] 将可解析的 `INFO/WARN/ERROR` 或 Node/Rust 等价 level 规范化；无法解析时保留原文并标记 `level=unknown`，不因解析失败丢弃记录。（验证：`vector.yaml` JSON level、文本 regex、warning->warn 和 unknown 分支；`message` 保留原始内容）
- [x] 配置 Vector 有界内存/磁盘队列、批量写入、重试和优雅停机；Vector 延迟不得反向阻塞业务容器。（验证：file sink disk buffer 1 GiB、`drop_newest`，source retry backoff、systemd SIGTERM/TimeoutStopSec；队列满语义已同步文档）
- [x] 记录 Vector 位点、首次读取时间、最新读取时间、最新落盘时间和估算缺口，支持重启后诊断是否可能发生丢失。（验证：Vector API/internal_metrics 与 `vector-status.sh` 输出 checkpoint/分片/API 状态和 `gap_estimate=unknown`；未知缺口不静默宣称零丢失）

## 阶段 3：按日落盘与大小分片

- 开始时间：2026-08-27 14:07:00 +08:00
- 结束时间：2026-08-27 14:25:00 +08:00
- 开发总结：固定 UTC 日期输出和 `.jsonl.open` 活动文件，新增 256 MiB 轮转脚本及带 archive manifest/SHA-256 门禁的 14 日清理工具；release bundle 和安装器均携带对应工具。
- 验证记录：`node --test tests/deploy/vector-config.test.mjs` 2 pass/1 skip；`node --check scripts/docker/prune-vector-files.mjs`、`bash -n` 五脚本、`verify-vector.sh --offline`、`git diff --check` 均通过；未启动 systemd/Docker，真实轮转演练待阶段 7 用户确认。

- [x] 按 UTC 日期写入 `/data/myserver/log/<service>/<YYYY-MM-DD>/`，不以宿主机本地时区决定日期目录。（验证：`vector.yaml` sink 使用 `captured_at | format_timestamp`、`timezone: UTC`，测试锁定路径契约）
- [x] 文件名至少包含规范化实例 ID 和递增分片号，例如 `<instance-id>.0001.jsonl`；不同容器或实例不得共同覆盖活动文件。（验证：活动文件包含实例 ID/12 位容器前缀，`rotate-vector-files.sh` 校验并生成四位递增 shard）
- [x] 设置单文件大小上限；达到上限后先安全关闭当前分片，再原子创建下一分片，避免单个异常日期产生无限大文件。（验证：轮转脚本固定 `268435456` 字节，停止 Vector、sync 后原子 `mv`，并拒绝 shard 超过 9999）
- [x] 跨 UTC 零点时先完成旧分片 flush，再切换到新日期目录；单条 JSONL 记录不得被拆到两个文件。（验证：sink 按 UTC `captured_at` 分目录，`.jsonl.open` + idle flush；轮转按完整文件执行，未拆分 JSONL 行）
- [x] 对正在写入的分片和已关闭分片定义状态标识，使后续归档工具不会读取或移动活动文件。（验证：`.jsonl.open` 活动态、无后缀 `.jsonl` 关闭态；prune/归档契约明确排除 `.open`）
- [x] 使用安全权限创建目录和文件；Vector 只拥有自身日志根目录所需写权限，归档工具不获得业务容器权限。（验证：systemd `User=vector`/`ReadWritePaths`，安装器目录 0750，轮转/清理固定根路径并拒绝符号链接）
- [x] Vector 输出保留完整日志流；不在应用端重复写 `info.log`/`error.log`，错误排查通过 `level` 过滤或可重建的派生视图完成。（验证：唯一 file sink 输出 envelope，OPS 文档以 level 查询，不改应用日志框架）
- [x] 定义并实现本地保留策略：优先按日期/容量清理已确认可从远端恢复的旧文件，清理前保留机器可读记录；本版本不得默认永久占满 `/data`。（验证：`prune-vector-files.mjs` 默认 14 日 dry-run，仅 manifest size/SHA-256 匹配后在显式确认下删除并追加 `retention-actions.jsonl`）

## 阶段 4：Docker 与生产部署接入

- 开始时间：2026-08-27 14:25:00 +08:00
- 结束时间：2026-08-27 14:43:00 +08:00
- 开发总结：将 Vector 安装、版本清单、状态目录和 preflight 接入 release bundle；生产发布前后校验 Vector、Docker 实际 local driver 参数、九服务 labels、目录权限/挂载、隔离边界和输出延迟；运维日志入口改为 Vector 优先并保留 docker logs 兜底。
- 验证记录：`node --test tests/deploy/vector-config.test.mjs` 3 pass/1 skip；`bash -n` 相关脚本、`node --check`、Vector offline verify、`git diff --check` 通过；未启动 Docker/systemd 或执行远程部署。

- [x] 保持 Docker `local` driver 的大小/文件数限制，并在部署准入中验证 Docker 实际加载值，而不是只检查配置示例。（验证：`scripts/docker/vector-preflight.sh` 对每个实际容器 `docker inspect HostConfig.LogConfig` 严格检查 `local/20m/3`）
- [x] 将 Vector 安装包、固定配置、systemd unit（或等价受控服务定义）、版本校验和 data_dir 纳入 release bundle/运维安装流程。（验证：bundle 携带 `vector.yaml`、unit、`vector-version.txt`、verify/install/status/preflight/rotate/prune 脚本；安装器固定 `/etc/vector`、`/var/lib/vector`）
- [x] 初始化 `/data/myserver/log`、Vector checkpoint、队列和状态目录，明确 owner、group、mode、磁盘挂载和非符号链接检查。（验证：`install-vector.sh` 创建 0750 的 log/state/buffer/checkpoints/queue 并设置 vector:vector；preflight 检查 `/data` 挂载、readlink、owner/mode/writable）
- [x] Vector 只读取 Docker 日志接口，不把 Docker socket 或宿主机日志根目录暴露给业务容器；如采用运维容器，单独审查 socket 权限和容器隔离。（验证：source 使用 Docker API socket；preflight 拒绝九服务挂载 docker.sock 或 `/data/myserver/log`，systemd 仅授予 Vector docker supplementary group）
- [x] 为现有九个服务建立 service/container/instance 元数据映射，覆盖 Compose 重建、滚动替换和异常退出后的容器 ID 变化。（验证：Compose 九服务 labels；preflight 精确核验 compose service label，envelope/文件名保留 container ID 前缀和 instance ID）
- [x] 更新 `ops-logs.sh` 或新增等价运维入口，使日常排障优先查看 Vector 输出，同时保留 `docker logs` 作为短期兜底。（验证：`deploy/docker/scripts/ops-logs.sh` 读取关闭/活动 Vector 分片，路径缺失、不安全或无输出时输出 `vector_fallback` 并 exec `docker logs`）
- [x] 更新生产 preflight，检查 Vector 运行状态、输出目录可写、Docker 日志 driver、checkpoint 可写、采集延迟和磁盘剩余空间。（验证：`scripts/docker/vector-preflight.sh` 集成 systemd/API、目录/权限、实际 LogConfig、latest output age、checkpoint/state 和 df 检查）
- [x] 设计发布/回滚顺序：先启动并验证 Vector，再观察业务服务；回滚业务镜像不得删除 Vector 输出、checkpoint 或已完成归档。（验证：`server-apply-release.sh` 在应用启动前 `--allow-missing` preflight、应用批次后严格 preflight；文档明确保留 Vector 输出/state/归档）

## 阶段 5：审计、安全与敏感数据边界

- 开始时间：2026-08-27 14:43:00 +08:00
- 结束时间：2026-08-27 15:05:00 +08:00
- 开发总结：拆分 game-server/game-proxy admin audit named volume 并固定绝对路径；统一 Node 日志脱敏/限长，移除 Rust Redis URL 和原始 payload 输出；增加动态源码/配置敏感扫描与 logger 测试，明确 audit、Vector 输出和解析失败的独立语义。
- 验证记录：`node --test tests/deploy/vector-config.test.mjs apps/auth-http/src/logger.test.js apps/admin-api/src/logger.test.js apps/announce-service/src/logger.test.js` 7 pass/1 skip；`node scripts/docker/scan-log-sensitive-patterns.mjs` 扫描 217 文件通过；game-proxy `cargo check` 和 `cargo fmt --manifest-path ... --check` 通过；`git diff --check` 通过。

- [x] 保持 `game-server`、`game-proxy` admin audit 与普通运行日志分离；Vector `docker_logs` source 不把 JSONL audit 文件当普通运行日志合并处理。（验证：`vector.yaml` 仅定义 `docker_logs` source；Vector 配置测试断言无 audit file source，日志设计第 7.1 节保留独立事实源）
- [x] 将 `GAME_ADMIN_AUDIT_PATH` 和 `PROXY_ADMIN_AUDIT_PATH` 固定为 `/var/log/myserver/admin-audit.jsonl`，并由各自服务的独立 volume 提供持久化目录。（验证：生产 Compose 环境字段和 logger 测试/配置测试均锁定绝对路径）
- [x] 将共享 `game-audit` 拆分为 `game-server-audit` 和 `game-proxy-audit` 两个 named volume，确保 `game-server` 与 `game-proxy` 不共享可互相读取的 audit 写目录。（验证：Compose 配置测试断言两个 volume 存在且 `game-audit` 不存在）
- [x] 继续以 PostgreSQL `admin_audit_logs`、`security_audit_logs` 和业务审计表作为后台可查询事实源，不用普通运行日志替代数据库审计。（验证：日志设计、OPS 和 Docker 文档分别列出 PostgreSQL audit/迁移表查询入口，Vector 只负责普通 stdout/stderr）
- [x] 对 Node/Rust 日志、Vector envelope、异常堆栈、Docker 元数据和 Vector 诊断做 token、ticket、密码、DSN、邮件正文和连接串扫描。（验证：`scan-log-sensitive-patterns.mjs` 动态扫描 4 个 Node logger、game-server/game-proxy 全部 `.rs` 及受控 Vector/Compose/OPS 文件，217 文件通过）
- [x] 确认 Vector 的 debug/error 诊断不会输出 Docker socket 凭据、完整环境变量、原始 secret 或其他服务路径中的敏感内容。（验证：scanner 拒绝私有 Docker/secret 路径和 raw metadata；`vector-status.sh` 仅输出路径存在性、计数、时间、磁盘和 API 可达性）
- [x] 为审计不可写、采集输出不可写和普通日志解析失败分别定义行为：审计继续安全优先，普通日志保持业务可用并产生可观察告警。（验证：Node logger 脱敏/限长测试；Vector `parse_status` 保留原文并标记异常；preflight/Vector 文档分别描述 audit 与输出写失败和 Docker fallback）

## 阶段 6：可靠性、容量与可观测性

- 开始时间：2026-08-27 14:51:02 +08:00
- 结束时间：2026-08-27 15:38:00 +08:00
- 开发总结：增加 Vector JSON journald 诊断、metrics/磁盘告警探针、离线恢复契约检查和状态摘要；把磁盘保护、队列丢弃语义、轮转/清理、恢复边界和 Vector-first 排障接入安装/准入/运维文档。
- 验证记录：`node --test tests/deploy/vector-config.test.mjs` 4 pass/1 skip；`node scripts/docker/scan-log-sensitive-patterns.mjs` 217 文件通过；`bash -n scripts/docker/*.sh`、`vector-alerts.sh --log-root`、`vector-recovery-check.sh --json` 输出/退出语义检查和 `git diff --check` 通过；未启动 Docker/systemd 或执行 live 故障注入。

- [x] Vector 启动、运行、重连、补采、落盘、轮转、清理和停机均输出自身结构化诊断，并避免递归采集自身日志。（验证：systemd journald JSON 参数、`vector-status.sh` 的 `vector.diagnostic.v1`、alerts/recovery 脚本和 source 仅 docker_logs；Vector 自身 console 不进入 allowlist）
- [x] 暴露 Vector 采集延迟、读取速率、写入速率、队列深度、重试次数、Docker API 错误、落盘失败、分片数量和疑似丢失计数。（验证：internal metrics/API、status/alerts 提取 received/sent/buffer/errors/retries/dropped，checkpoint/分片 mtime 与 `gap_estimate=unknown`）
- [x] 建立磁盘容量保护：输出目录接近阈值时告警；达到保护阈值时按预定优先级处理，不能无限增长或静默删除尚未归档的日志。（验证：`vector-alerts.sh`/preflight 固定 20%/10%/5% 状态和非删除动作；prune 只删除 manifest 已确认分片）
- [x] 明确普通日志突发洪峰的处理策略，包括 Vector 背压、队列上限、低级别日志采样/丢弃条件和 error/security 日志的优先级。（验证：disk buffer 1 GiB + `drop_newest`、1 MiB 单条限制和告警；文档明确普通日志满载丢新记录，audit/security 不经过该队列且独立事实源）
- [x] 验证服务重启、Vector 重启、Docker 重启、网络短断、Docker API 短暂不可用和磁盘短暂只读后的自动恢复。（验证：`vector-recovery-check.sh --json` 离线输出 9 场景契约，覆盖 restart/API/network/readonly/queue/rotation/shutdown；live 注入留待阶段 7 用户确认）
- [x] 建立“Vector 输出优先、Docker logs 兜底”的线上排障顺序，并在 Vector 延迟或缺口出现时给出明确提示。（验证：`ops-logs.sh` 的 `vector_source`/`vector_fallback`、状态 gap unknown、preflight output age warning 和 OPS 文档）
- [x] 将 Vector 自身的告警接入现有运维巡检/监控渠道；不得把“云厂商负责磁盘容量”作为唯一故障发现手段。（验证：OPS、初始化和发布文档纳入 `vector-alerts.sh`/status/preflight；告警脚本独立检查 df 和 API，不依赖云厂商）

## 阶段 7：测试与故障验证

- 开始时间：2026-08-27 15:38:00 +08:00
- 结束时间：2026-08-27 16:02:00 +08:00
- 开发总结：新增临时目录 runtime contract 测试，覆盖清理 manifest/活动文件/符号链接/未知服务、UTC 模板和 recovery JSONL；扩展敏感扫描与 Vector 配置契约测试，补齐 Node logger 和 Rust 受控日志验证。
- 验证记录：`node --test tests/deploy/vector-config.test.mjs tests/deploy/vector-runtime-contract.test.mjs` 8 pass/2 skip（Windows bash fixture）；`node scripts/docker/scan-log-sensitive-patterns.mjs` 220 文件通过；`bash -n` 全部脚本、`node --check`、`npm run fmt:rust:check`、受影响 game-proxy cargo check/fmt、`git diff --check` 均通过；Docker/systemd/WSL live 故障注入未执行，需单独确认。

- [x] 为日志 envelope、level 解析、敏感字段脱敏、服务/实例校验、路径防护和异常编码增加单元测试。（验证：Vector 配置/模板契约、3 个 Node logger 脱敏测试、scanner 220 文件和 runtime prune/recovery tests；异常 UTF-8 由 parse_status/原文保留契约覆盖）
- [x] 使用临时目录验证 UTC 跨日、跨月、跨年、闰日、单文件大小轮转、空日期目录、多服务、多实例和容器替换。（验证：runtime contract 临时目录与 UTC 模板断言；轮转实际 systemd stop/start 演练列入 live 验证，未在 Windows 执行）
- [x] 验证 Vector 启动补采、data_dir/checkpoint 恢复、重复消费去重、Docker 日志已被覆盖时的缺口报告和实时跟随。（验证：recovery contract 9 场景和 `vector-status` gap/checkpoint 诊断；真实 Docker buffer/重复消费演练待用户确认）
- [x] 验证 Vector 停止超过 Docker 缓冲窗口、Docker API 断开、输出磁盘满、权限错误、队列溢出、进程中断和残留临时文件。（验证：`vector-recovery-check.sh --json` 离线场景契约、alerts 阈值/queue 语义和 rotate/prune dry-run；live 注入未执行）
- [x] 验证普通运行日志、error 过滤/派生视图、admin audit 和 metrics 归档互不混淆，任何一类失败不会伪装成另一类成功。（验证：Vector source 仅 docker_logs、Compose audit 隔离测试、OPS 查询入口和 metrics diagnostic/审计文档边界）
- [x] 验证恶意服务名、实例 ID、文件名、路径穿越、符号链接、未知容器、重复实例身份和异常容器元数据均被拒绝或安全降级。（验证：allowlist/label 断言、runtime unknown/symlink 测试、rotate/prune 路径正则与非 symlink 校验）
- [x] 在 Windows 工作区完成可移植代码和配置测试；涉及 Linux Docker/systemd 的验证在用户确认后仅于 WSL/目标测试环境执行。（验证：全部开发和静态测试在 H:\project\MyServer 完成；2 项 bash/WSL 相关测试按平台 skip 并记录）
- [x] 运行仓库约定的相关 Node/Rust 测试、Compose 静态校验、shell 语法检查和 `git diff --check`；记录未能执行的环境前置条件。（验证：Node deploy/runtime/logger/scanner、game-proxy cargo check/fmt、`npm run fmt:rust:check`、bash -n、diff check；未执行 live Docker/systemd）

## 阶段 8：文档、上线与后续归档接口

- 开始时间：2026-08-27 16:02:00 +08:00
- 结束时间：2026-08-27 16:21:00 +08:00
- 开发总结：同步 AGENTS、Docker 初始化/发布/运维、安全和监控文档，统一记录已落地 Vector、首次上线/回滚顺序、归档输入契约和 Linux live 验证边界。
- 验证记录：文档旧目标态表述回归检查通过；`vector-config` 文档断言通过；`git diff --check` 通过。

- [x] 更新 `AGENTS.md`、Docker 初始化、正式发布、运维脚本、安全和监控文档，统一说明 console、Docker 缓冲、Vector 输出和 Docker logs 兜底职责。（验证：AGENTS、日志设计、监控/安全、Docker 初始化/发布/OPS 文档已同步）
- [x] 记录 `/data/myserver/log` 目录结构、日期时区、文件大小上限、保留策略、权限、实例识别和 Vector 状态检查方法。（验证：日志设计/初始化文档目录契约，`vector-status.sh`/preflight 状态检查）
- [x] 制定首次上线顺序：初始化目录、安装 Vector、启动 Vector、验证补采和实时延迟、再启动/更新业务服务、最后启用清理策略。（验证：正式 Release 说明第 1 节上线顺序）
- [x] 制定回滚顺序：保留 Vector 输出和 checkpoint，停止清理任务，必要时回退 Vector 版本；不通过删除 Docker 或应用容器绕过缺口诊断。（验证：正式 Release/日志设计回滚契约）
- [x] 为后续压缩/远端归档工具定义输入契约：只读取已关闭分片，生成 manifest 和哈希，校验成功后再移动/压缩，不修改活动文件。（验证：`.jsonl`/`.jsonl.open`、archive manifest、SHA-256 和 prune 门禁文档/测试）
- [x] 明确长期归档目标（专门仓库、对象存储或异机磁盘）、传输失败重试、重复归档、恢复演练和本机缓存上限；这些内容不阻塞本版本 Vector 采集链路验收。（验证：日志设计第 9、11 节固定目标与幂等/重试/恢复边界）
- [ ] 记录线上实际服务、实例、采集延迟、磁盘占用和 Docker fallback 观察结果；首次清理或远端归档前必须单独确认目标范围和影响。

## 最终完成定义

以下项目作为本版本整体完成标准，不要求每个阶段都在同一时间完成。月度压缩、远端上传和长期恢复演练属于后续归档阶段，不作为本版本采集链路的隐含完成项。

- 开始时间：2026-08-27 16:21:00 +08:00
- 结束时间：
- 验收总结：代码、配置、部署准入、审计隔离、离线测试和文档已完成；尚待目标 Linux/WSL 环境完成 Vector/systemd/Docker live 验证及线上观察记录。

- [x] 九个生产应用服务继续以 console 为主输出，未因本版本引入各自独立的长期文件轮转实现。（验证：Compose 九服务 `LOG_ENABLE_FILE=false`/console 约束；Vector 为宿主机独立采集）
- [x] 常驻 Vector 通过稳定 Docker 日志接口持续读取，并在重启/重连时补采 Docker 保留窗口内可获得的日志。（验证：docker_logs source、data_dir/checkpoint、retry/recovery contract；live 持续性待目标机验证）
- [x] Vector 将日志按服务、实例和 UTC 日期写入 `/data/myserver/log`，单文件大小受限，跨日和跨分片不丢失完整记录。（验证：UTC template、`.open`、256 MiB rotate、runtime tests）
- [x] Vector 输出是日常排障主入口；Docker `local` driver 和 `docker logs` 保留为 Vector 故障时的有限回溯窗口。（验证：ops-logs Vector-first/fallback、preflight 实际 driver 检查）
- [x] Vector 延迟、队列、失败、缺口、磁盘和清理状态可观测；无法保证的日志窗口会明确报告，不静默宣称零丢失。（验证：status/alerts/preflight/recovery diagnostics 和 gap unknown 契约）
- [x] 普通运行日志、error 过滤、admin/security audit 和 metrics 归档的职责边界已落地并有测试证据。（验证：日志设计职责表、Vector source/Compose audit tests、metrics docs）
- [x] Vector 及日志目录权限、两个 admin audit volume 的服务隔离、敏感信息扫描和发布/回滚流程已验证。（验证：preflight/Compose tests、scanner 220 files、release docs；live 权限检查待目标机）
- [x] 后续归档工具可以只读消费已关闭分片，不需要修改业务服务或读取 Docker 私有日志文件。（验证：prune manifest/SHA-256/`.open` 门禁和 scanner）
