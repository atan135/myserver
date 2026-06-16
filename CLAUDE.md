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
- `chat-server`、`match-service`、`announce-service`、`mail-service` 是围绕游戏主链路拆出的独立能力服务。
- `admin-api + admin-web` 组成运营后台，通过独立控制面访问审计、玩家管理、GM 入口和监控能力；具体 GM 命令是否闭环以 `docs/总览/整体架构.md` 和代码为准。
- Redis 用于 session、ticket、限流、服务注册和 metrics 快照；Core NATS 用于邮件通知、session kick 和 metrics 采集通道。
- PostgreSQL 用于账号、审计、游戏事件、公告和邮件等持久化数据。
- 玩家协议与内部控制协议尽量收敛到 `packages/proto`；个别服务仍保留本地 proto，具体以代码和协议文档为准。

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
├── admin-web/        # Vue 3 + Vite + Element Plus 管理前端
└── simple-client/    # 已废弃的 Unity 历史 demo，不作为当前客户端事实源
packages/
├── proto/            # 共享 Protobuf 协议
└── service-registry/ # Redis 服务注册中心包
tools/
└── mock-client/      # Node.js 无客户端联调工具
scripts/              # 本地启动、环境检查、数据初始化辅助脚本
db/                   # 数据库初始化脚本
docs/                 # 当前正式设计文档
```

## 外部客户端

正式游戏客户端已迁移到独立仓库 `mybevy`，不作为 MyServer monorepo 的子目录维护。本机开发示例路径可以是 `C:\project\mybevy`，其他环境应按实际 clone 路径配置，不要依赖该绝对路径。

如脚本或本地工具需要访问外部客户端，统一通过 `MYSERVER_CLIENT_ROOT` 指定；未设置时，本仓库仅使用 `tools/mock-client` 做服务端联调。`apps/simple-client` 只保留为历史 Unity demo，不再参与协议同步、常规联调或测试准入。

## 服务与端口

固定入口端口以 `apps/port.txt` 为准；内部服务端口主要用于本地开发默认值，部署和联调时应优先看实际配置、环境变量和服务注册中心。

| 服务 | 默认端口 | 说明 |
|------|----------|------|
| `auth-http` | `3000` | 登录、session、ticket |
| `admin-api` | `3001` | 管理后台 API |
| `admin-web` | `3002` | 本地 Vite 管理前端 |
| `game-proxy` | `4000` | 客户端游戏接入入口 |
| `game-server` | `7000` | 游戏服玩家协议默认端口 |
| `game-server admin` | `7500` | 内部管理口 |
| `game-proxy admin` | `7101` | 代理内部管理口，代码默认值 |
| `chat-server` | `9001` | 内部聊天服务默认值 |
| `match-service` | `9002` | 内部匹配服务默认值 |
| `mail-service` | `9003` | 内部邮件服务默认值 |
| `announce-service` | `9004` | 内部公告服务默认值 |

## 文档导航

整体与协议：

- [整体架构](./docs/总览/整体架构.md)
- [协议设计](./docs/协议与客户端/协议设计.md)
- [外部客户端接入说明](./docs/协议与客户端/外部客户端接入说明.md)
- [生产拓扑与 Room 迁移设计](./docs/后台与运维/生产拓扑与Room迁移设计.md)

游戏服与接入层：

- 当前实现与代码阅读：[Rust 游戏服开发指南](./docs/游戏服与接入层/Rust游戏服开发指南.md)、[帧同步与房间生命周期设计](./docs/游戏服与接入层/帧同步与房间生命周期设计.md)、[game-proxy 热切换代理设计](./docs/游戏服与接入层/game-proxy热切换代理设计.md)
- 更新与灰度边界：[更新策略拆分](./docs/游戏服与接入层/游戏服更新策略拆分.md)、[空房接管式灰度规范](./docs/游戏服与接入层/空房接管式灰度规范.md)、[空房接管式灰度任务清单](./docs/游戏服与接入层/空房接管式灰度任务清单.md)
- 路线图与算法/目标设计：[底层框架路线图](./docs/游戏服与接入层/游戏服底层框架路线图.md)、[大世界常驻 Room 热更新设计](./docs/游戏服与接入层/大世界常驻Room热更新设计.md)、[网络延迟补偿设计](./docs/游戏服与接入层/网络延迟补偿设计.md)

配置与场景：

- [CSV 配置表设计](./docs/配置与场景/CSV配置表设计.md)
- [CSV 热更现状清单](./docs/配置与场景/CSV热更现状清单.md)
- [场景地图格式设计](./docs/配置与场景/场景地图格式设计.md)

具体游戏逻辑：

- [背包系统设计](./docs/游戏服与接入层/背包系统设计.md)
- [战斗 ECS 设计](./docs/游戏服与接入层/战斗ECS设计.md)

周边服务与后台：

- [服务注册中心设计](./docs/周边服务/服务注册中心设计.md)
- [聊天与邮件系统设计](./docs/周边服务/聊天与邮件系统设计.md)
- [匹配服务设计](./docs/周边服务/匹配服务设计.md)
- [管理后台设计](./docs/后台与运维/管理后台设计.md)
- [监控设计](./docs/安全与监控/监控设计.md)

安全：

- [安全设计](./docs/安全与监控/安全设计.md)
- [限流与安全现状](./docs/安全与监控/限流与安全现状.md)
- [游戏服务安全分层与敏感操作处理指南](./docs/安全与监控/游戏服务安全分层与敏感操作处理指南.md)

## 基础设定

本地二进制工具优先放在项目根目录 `bin/` 下，例如 `bin/nats-server.exe`。脚本和环境检查应优先使用该目录中的可执行文件；未找到时再回退到系统 `PATH` 或常见安装目录。

日志配置统一使用以下环境变量模型：

- `LOG_LEVEL`
- `LOG_ENABLE_CONSOLE`
- `LOG_ENABLE_FILE`
- `LOG_DIR`

Node.js 服务使用 `log4js`，Rust 异步服务使用 `tracing + tracing-subscriber + tracing-appender`。

常见配置来源：

- 固定入口端口：`apps/port.txt`
- Node.js 服务示例配置：各服务 `.env.example`
- Rust 服务示例配置：各服务 `.env.example` 或启动脚本
- 数据库初始化：`db/init.sql`
- 根 npm 脚本：`package.json`
- 本地 PowerShell 辅助脚本：`scripts/`

## 开发协作约定

- 修改功能前先看对应代码和专题文档，不要从 `docs/历史归档/初始设计稿/` 推断当前行为。
- 若文档与代码冲突，应以代码为准，并在需要时同步修正文档。
- 模块功能开发完成后，不要直接自动运行项目检测、集成测试、联调脚本或自动启动相关服务；先提示用户需要启动哪些服务和依赖，待用户确认后再执行测试。
- 除非用户明确要求，不要提交 git commit 或执行 push。
- 提交信息按功能模块拆分，标题使用 `<type>: <中文主题>`，正文说明关键改动和原因；涉及端口、配置、协议、脚本或跨服务联动时要写明影响范围。
