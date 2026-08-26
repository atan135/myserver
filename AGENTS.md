# MyServer

MyServer 是一个通用游戏后端框架仓库，当前定位是多服务 monorepo：登录、游戏接入、游戏逻辑、聊天、匹配、邮件、公告、管理后台、协议包、服务注册中心、联调工具和本地脚本都在同一仓库内维护。

## 文档定位

本文件是给 AI 和协作者使用的项目入口说明，只保留整体设计理念、架构边界、基础设定和文档导航。具体功能细节、协议字段、接口行为、实现状态和任务拆解应阅读对应 `docs/` 文档或直接查看代码。

优先级约定：

- 当前代码与配置优先于文档。
- `docs/总览/整体架构.md` 是当前整体架构的主说明。
- 专题设计以 `docs/` 下对应文档为准；部分专题文档可能描述目标态，不等于已经全部落地。
- `docs/历史归档/初始设计稿/` 已不再使用，仅保留项目初始设计阶段的历史提示词。AI 或协作者了解项目时不需要读取该目录，也不要以其中内容作为当前设计依据。

## 整体架构理念

- `auth-http` 负责 HTTP 登录、会话、ticket 和登录安全边界。
- `game-proxy` 作为客户端游戏接入层，屏蔽后端 `game-server` 实例与路由细节。
- `game-server` 是游戏逻辑核心，负责玩家鉴权、房间生命周期、帧推进、配置表热加载、内部管理接口和主要游戏运行时。
- `auth-http` 和 `game-proxy` 是正式玩家入口；`chat-server`、`match-service`、`announce-service`、`mail-service` 是围绕游戏主链路拆出的默认内网能力服务。
- `admin-api + admin-web` 组成运营后台，通过独立控制面访问审计、玩家管理、GM 入口和监控能力；灰度排空等高风险写操作采用预检、独立审批和关联审计，具体能力闭环以 `docs/总览/整体架构.md` 和代码为准。
- Redis 用于 session、ticket、限流、服务注册和 metrics 快照；Core NATS 用于邮件通知、session kick 和 metrics 采集通道。
- PostgreSQL 用于账号、审计、游戏事件、公告和邮件等持久化数据。
- 玩家协议与内部控制协议尽量收敛到 `packages/proto`；个别服务仍保留本地 proto，具体以代码和协议文档为准。
- 测试、预发和线上环境的跨服务消费者应通过 Redis service registry endpoint 发现服务；固定端口表只表示本地开发默认监听或外部稳定入口，不应作为内部服务直连依据。`game-proxy` 静态上游、TCP fallback 和 mock-client 直连 `game-server:7000` 仅限 development/local 调试或定位问题。

简化拓扑：

```text
mybevy client / mock-client
  -> auth-http -> Redis / PostgreSQL
  -> game-proxy -> game-server -> rooms / runtime / admin / configs

admin-web -> admin-api -> game-server admin / Redis / PostgreSQL

game-server <-> match-service
mail-service -> Core NATS -> chat-server
announce-service / mail-service / game-server / game-proxy -> service registry
```

## 仓库结构

```text
apps/
├── auth-http/         # Node.js + NestJS 登录服
├── game-proxy/       # Rust + Tokio KCP 接入代理，保留本地 TCP fallback
├── game-server/      # Rust + Tokio 游戏逻辑服
├── chat-server/      # Rust + Tokio 聊天服
├── match-service/    # Rust + tonic gRPC 匹配服务
├── announce-service/ # Node.js HTTP 公告服务
├── mail-service/     # Node.js HTTP 邮件服务
├── admin-api/        # Node.js + NestJS 管理后台 API
└── admin-web/        # Vue 3 + Vite + Element Plus 管理前端
packages/
├── authority-core/   # 控制机迁移、快照与输入基础结构
├── game-protocol/    # 玩家协议包头、消息 ID、包大小限制和 KCP 参数共享 crate
├── proto/            # 共享 Protobuf 协议
└── service-registry/ # Redis 服务注册中心包
tools/
├── load-test/        # Rust 受控压测与服务诊断工具
└── mock-client/      # Node.js 无客户端联调工具
scripts/              # 本地启动、环境检查、数据初始化辅助脚本
db/                   # 数据库初始化脚本
docs/                 # 当前正式设计文档
```

## 外部客户端

正式游戏客户端已迁移到独立仓库 `mybevy`，不作为 MyServer monorepo 的子目录维护。本机开发示例路径可以是 `C:\project\mybevy`，其他环境应按实际 clone 路径配置，不要依赖该绝对路径。

如脚本或本地工具需要访问外部客户端，统一通过 `MYSERVER_CLIENT_ROOT` 指定；未设置时，本仓库仅使用 `tools/mock-client` 做服务端联调。本仓库不再保留 Unity 历史 demo，不参与协议同步、常规联调或测试准入。

## 服务与端口

固定入口端口以 `apps/port.txt` 为准；下表只表示本地开发默认监听或外部稳定入口。测试、预发和线上环境的内部跨服务访问应通过 Redis service registry endpoint 发现目标服务，不应按表内默认端口直连。

| 服务 | 默认端口 | 说明 |
|------|----------|------|
| `auth-http` | `3000` | 正式玩家 HTTP 登录、session、ticket 入口 |
| `admin-api` | `3001` | 管理后台 API |
| `admin-web` | `3002` | 本地 Vite 管理前端 |
| `game-proxy` | `4000` | 正式玩家游戏接入入口 |
| `game-server` | `7000` | 游戏服玩家协议本地默认监听；测试/预发/线上由接入层或服务发现路由 |
| `game-server admin` | `7500` | 内部管理口本地默认监听 |
| `game-proxy admin` | `7101` | 代理内部管理口，代码默认值 |
| `chat-server` | `9001` | 内网聊天能力服务本地默认监听 |
| `match-service` | `9002` | 内网匹配能力服务本地默认监听 |
| `mail-service` | `9003` | 内网邮件能力服务本地默认监听 |
| `announce-service` | `9004` | 内网公告能力服务本地默认监听 |

## 文档导航

整体与协议：

- [整体架构](./docs/总览/整体架构.md)
- [协议设计](./docs/协议与客户端/协议设计.md)
- [外部客户端接入说明](./docs/协议与客户端/外部客户端接入说明.md)
- [生产拓扑与 Room 迁移设计](./docs/后台与运维/生产拓扑与Room迁移设计.md)

游戏服与接入层：

- 当前实现与代码阅读：[游戏服与接入层文档索引](./docs/游戏服与接入层/README.md)、[Rust 游戏服开发指南](./docs/游戏服与接入层/Rust游戏服开发指南.md)、[帧同步与房间生命周期设计](./docs/游戏服与接入层/帧同步与房间生命周期设计.md)、[game-proxy 热切换代理设计](./docs/游戏服与接入层/game-proxy热切换代理设计.md)
- 更新与灰度边界：[更新策略拆分](./docs/游戏服与接入层/游戏服更新策略拆分.md)、[空房接管式灰度规范](./docs/游戏服与接入层/空房接管式灰度规范.md)、[空房接管式灰度任务清单](./docs/游戏服与接入层/空房接管式灰度任务清单.md)
- 路线图与算法/目标设计：[底层框架路线图](./docs/游戏服与接入层/游戏服底层框架路线图.md)、[大世界常驻 Room 热更新设计](./docs/游戏服与接入层/大世界常驻Room热更新设计.md)、[因果留影与风门远行服务端设计](./docs/游戏服与接入层/任务与世界/因果留影与风门远行服务端设计.md)、[网络延迟补偿设计](./docs/游戏服与接入层/网络延迟补偿设计.md)

配置与场景：

- [CSV 配置表设计](./docs/配置与场景/CSV配置表设计.md)
- [CSV 热更现状清单](./docs/配置与场景/CSV热更现状清单.md)
- [场景地图格式设计](./docs/配置与场景/场景地图格式设计.md)

具体游戏逻辑：

- [游戏业务模块开发规范](./docs/游戏服与接入层/游戏业务模块开发规范.md)
- [背包系统设计](./docs/游戏服与接入层/背包与物品/背包系统设计.md)
- [战斗 ECS 设计](./docs/游戏服与接入层/战斗/战斗ECS设计.md)

周边服务与后台：

- [服务注册中心设计](./docs/周边服务/服务注册中心设计.md)
- [聊天与邮件系统设计](./docs/周边服务/聊天与邮件系统设计.md)
- [匹配服务设计](./docs/周边服务/匹配服务设计.md)
- [管理后台设计](./docs/后台与运维/管理后台设计.md)
- [游戏服务压力测试框架设计](./docs/后台与运维/游戏服务压力测试框架设计.md)
- [监控设计](./docs/安全与监控/监控设计.md)
- [服务端日志采集与留存设计](./docs/安全与监控/服务端日志采集与留存设计.md)

安全：

- [安全设计](./docs/安全与监控/安全设计.md)
- [限流与安全现状](./docs/安全与监控/限流与安全现状.md)
- [游戏服务安全分层与敏感操作处理指南](./docs/安全与监控/游戏服务安全分层与敏感操作处理指南.md)

## 基础设定

项目内受控或便携的本地二进制工具放在项目根目录 `bin/` 下，但具体解析顺序以对应脚本和制品配置为准。Redis 和 NATS 的本地脚本优先查找 `bin/`，再按脚本约定回退到系统 `PATH` 或常见安装目录；数据库迁移只接受 `db/config/sqlx-cli.json` 登记并校验过的 `bin/sqlx.exe`，不得回退到 `PATH`；Rust、Protobuf 和 PostgreSQL 客户端使用已安装的用户工具链或系统 `PATH`。

日志配置统一使用以下环境变量模型：

- `LOG_LEVEL`
- `LOG_ENABLE_CONSOLE`
- `LOG_ENABLE_FILE`
- `LOG_DIR`

Node.js 服务使用 `log4js`，Rust 异步服务使用 `tracing + tracing-subscriber + tracing-appender`。

当前生产 Docker Compose 以 console 日志为主，业务服务的 `LOG_ENABLE_FILE` 默认关闭；Docker `local` logging driver 的固定大小/文件数轮转只是短期缓冲。目标态由[服务端日志采集与留存设计](./docs/安全与监控/服务端日志采集与留存设计.md)定义：宿主机受控服务运行的 Vector 持续读取 Docker 日志，按 UTC 日期和固定大小分片写入宿主机 `/data/myserver/log`，再由独立工具负责后续压缩或远端归档。Vector 尚未落地前，日常日志入口仍是 `docker logs`，不要将目标目录或 Docker 私有日志文件视为已经可用的长期接口。

常见配置来源：

- 本地端口登记与同步来源：`apps/port.txt`；未登记端口以对应服务配置和代码为准
- Node.js 服务示例配置：各服务 `.env.example`
- Rust 服务示例配置：各服务 `.env.example` 或启动脚本
- 数据库配置与迁移：`db/config/`、`db/bootstrap/`、`db/migrations/`、`db/seeds/`
- 本地数据库重置：`scripts/reset-dev-data.ps1`
- 数据库历史兼容与 catalog 对照：`db/init.sql`，不作为新增 Schema 的入口
- 根 npm 脚本：`package.json`
- 本地 PowerShell 辅助脚本：`scripts/`

## Windows 与 WSL 工作区边界

Windows 原生工作区 `H:\project\MyServer` 是本项目唯一的日常开发工作区和未提交改动事实源。

- 所有代码、配置和文档修改均在 Windows 工作区完成。
- 依赖安装、代码生成、格式化、静态检查、单元测试、集成测试、本地服务启动和客户端联调均在 Windows 下执行。
- Git 源码提交原则上从 Windows 工作区创建。
- 不得为了执行 Linux 命令而从 WSL 的 `/mnt/h/project/MyServer` 直接运行项目构建、测试或依赖安装。

WSL 原生工作区（例如 `~/src/MyServer`）仅用于 Linux 发布和远端运维：

- 通过 Git 同步并检出已经确认的 commit，不通过目录复制或 rsync 传递未提交源码。
- 执行发布所需的 Linux 编译、Docker 构建、镜像推送、release manifest 生成及产物验证。
- 允许通过 WSL 使用 SSH 等工具查看和调试远端服务器。
- 使用远程服务器前必须先读取项目根目录中仅供本机使用的 `local_help.txt`，以其中登记的 WSL 发行版、SSH 地址、端口、用户、密钥路径和已验证命令为准，不从公开文档猜测连接参数。
- `local_help.txt` 已被 Git 忽略且可能包含敏感本地配置；不得把其中的密码、私钥、生产账号或连接参数复制到受版本控制的代码、文档、日志或提交信息中。
- 通过 WSL 查询线上 PostgreSQL 时使用 `local_help.txt` 中的只读调用模板，默认使用 `READ ONLY` 事务、语句超时和受限查询，不对外开放 PostgreSQL 端口。
- 不在 WSL 工作区进行功能开发、常规代码修改、本地功能测试或多服务联调。
- WSL 构建或远端调试发现代码问题时，应返回 Windows 工作区修改并测试，然后重新提交和同步。
- 对远端服务器默认只执行只读诊断；部署、重启、配置修改、数据库变更和数据修复必须由用户明确授权。

正式发布前，必须确认 WSL 工作区为干净状态，且其 `HEAD` 与准备发布的 Windows Git commit 完全一致。正式推送生成的 `deploy/docker/images.lock.json` 是唯一允许在 WSL 工作区产生并提交的仓库文件；该发布产物提交并推送后，必须先将 Windows 工作区 fast-forward 到该提交，再继续后续开发。

## 本地 Docker 构建环境

本机 Docker 构建统一使用迁移到 H 盘的 WSL Ubuntu 发行版内原生 `dockerd`，不使用也不依赖 Docker Desktop。项目应位于 WSL 原生文件系统中，例如 `~/src/MyServer`，不要从 `/mnt/c` 或 `/mnt/h` 挂载路径执行 Docker 构建，以避免文件系统性能和 Git 换行符问题。

`/etc/docker/daemon.json` 必须保留以下网络配置；如文件已有其他有效配置，应合并字段，不要直接覆盖：

```json
{
  "dns": ["223.5.5.5", "119.29.29.29"],
  "registry-mirrors": ["https://docker.m.daocloud.io"]
}
```

- `dns` 避免 BuildKit 继承 WSL `systemd-resolved` 的 `127.0.0.53` stub 后发生间歇性 Docker Hub DNS 超时。
- `docker.m.daocloud.io` 是当前已验证可用的 Docker Hub mirror。不要加入未经当前网络验证的 mirror。
- 修改 daemon 配置后使用 `sudo systemctl restart docker` 重启 WSL 内 Docker 服务，再用 `docker info` 确认 `Registry Mirrors` 已生效；不要为此启动 Docker Desktop。
- 本地发布镜像使用干净的 WSL 原生 Git worktree 执行 `./scripts/docker/build-and-push.sh --release-tag <tag>`。只有明确要求发布时才添加 `--push`。

## 开发协作约定

- 修改功能前先看对应代码和专题文档，不要从 `docs/历史归档/初始设计稿/` 推断当前行为。
- 若文档与代码冲突，应以代码为准，并在需要时同步修正文档。
- 模块功能开发完成后，不要直接自动运行项目检测、集成测试、联调脚本或自动启动相关服务；先提示用户需要启动哪些服务和依赖，待用户确认后再执行测试。
- `tools/load-test` 的 `validate` 与 dry-run 不连接真实服务；live、远程或写入型诊断仍须先说明目标环境、依赖和影响范围，取得用户确认后再执行，并遵守工具的 profile、白名单和预算门禁。
- 每次最终提交前必须先执行 `npm run fmt:rust:check` 核查 Rust 格式；发现格式差异时先确认变更，再执行 `npm run fmt:rust` 统一修改，并重新运行检查通过后才允许提交。
- 除非用户明确要求，不要提交 git commit 或执行 push。
- 提交信息按功能模块拆分，标题使用 `<type>: <中文主题>`，正文说明关键改动和原因；涉及端口、配置、协议、脚本或跨服务联动时要写明影响范围。

## 文档维护约定

- `summary/` 下的 checklist 完成后，转移并归档到对应 `docs/<领域>/checklists/` 目录；游戏服具体功能的 checklist 归档到 `docs/游戏服与接入层/<功能模块>/checklists/`，跨模块 checklist 保留在该领域根目录，再纳入 Git 提交。
