# MyServer

MyServer 是一个通用游戏后端框架 monorepo，当前已经形成多服务形态：登录、游戏接入、游戏逻辑、聊天、匹配、邮件、公告、管理后台、协议包、服务注册中心、联调工具和本地脚本都在同一仓库内维护。

本文是面向开发者的快速入口。具体协议、接口行为、实现状态和任务拆解以 `docs/` 下专题文档与当前代码为准；`docs/历史归档/初始设计稿/` 仅保留初始设计阶段的历史提示词，不再作为当前设计依据。

## 当前架构

核心链路：

```text
mybevy client / mock-client
  -> auth-http -> Redis / PostgreSQL
  -> game-proxy -> game-server -> rooms / runtime / admin / configs

admin-web -> admin-api -> game-server admin / Redis / PostgreSQL

game-server <-> match-service
mail-service -> Core NATS -> chat-server
announce-service / mail-service / game-server / game-proxy -> service registry
```

正式玩家入口是 `auth-http` 和 `game-proxy`。`chat-server`、`match-service`、`mail-service`、`announce-service` 等服务默认作为内网能力服务部署；测试、预发和线上环境的跨服务消费者应通过 Redis service registry endpoint 发现这些服务，不应把固定端口表或静态上游配置作为内部服务直连依据。

当前已经稳定落地的能力：

- 多服务 monorepo 基本形态已完成。
- 登录、access token、game ticket、游戏接入、游戏逻辑、后台、聊天、匹配、邮件、公告都有独立服务。
- `game-server` 已有房间生命周期、帧推进、配置加载、内部管理口和部分具体游戏逻辑。
- `game-proxy` 已支持 KCP / TCP fallback、ticket 本地校验和基于注册中心的动态发现；静态上游仅保留为 development/local 调试或定位问题的方式。
- `chat-server`、`mail-service`、`announce-service` 当前独立部署；`game-proxy` 不负责聊天、邮件或公告转发。
- Redis 用于 session、ticket、限流、服务注册和 metrics 快照；Core NATS 用于邮件通知、session kick 和 metrics 采集通道。
- PostgreSQL 用于账号、审计、游戏事件、公告、邮件等持久化数据。

仍需注意的当前缺口：

- `admin-api` 已有后端角色守卫和监控接口鉴权，但管理员 JWT 仍缺少 session/version/blacklist，登录失败限流和锁定也还需补齐。
- `game-server` admin 侧 GM 广播、踢人、封禁仍未形成完整端到端闭环。
- `game-proxy` 还没有 IP 黑名单、单 IP / 单账号连接上限和成熟公网加密方案。
- `chat-server` 已校验 ticket 签名、过期和 ticket version，但仍不查询单张 Redis ticket 记录。
- 部分专题文档描述目标设计，不等于代码已经全部落地。

## 仓库结构

```text
apps/
├── auth-http/         # Node.js + NestJS 登录服
├── game-proxy/       # Rust + Tokio KCP 接入代理，保留 TCP fallback
├── game-server/      # Rust + Tokio 游戏逻辑服
├── chat-server/      # Rust + Tokio 聊天服
├── match-service/    # Rust + tonic gRPC 匹配服务
├── announce-service/ # Node.js HTTP 公告服务
├── mail-service/     # Node.js HTTP 邮件服务
├── admin-api/        # Node.js + NestJS 管理后台 API
└── admin-web/        # Vue 3 + Vite + Element Plus 管理前端
packages/
├── proto/            # 共享 Protobuf 协议
├── authority-core/   # 服务端和客户端共用的控制机迁移/快照/输入基础结构
└── service-registry/ # Redis 服务注册中心包
tools/
└── mock-client/      # Node.js 无客户端联调工具
scripts/              # 本地启动、环境检查、数据初始化辅助脚本
db/                   # 数据库初始化脚本
docs/                 # 当前正式设计文档
```

## 外部客户端

正式游戏客户端已迁移到独立仓库 `mybevy`，不作为 MyServer monorepo 的子目录维护。本机开发示例路径可以是 `C:\project\mybevy`，其他环境应按实际 clone 路径配置，不要依赖该绝对路径。

如脚本或本地工具需要访问外部客户端，统一通过 `MYSERVER_CLIENT_ROOT` 指定；未设置时，本仓库仅使用 `tools/mock-client` 做服务端联调。本仓库不再保留 Unity 历史 demo，不参与协议同步、常规联调或测试准入。

## 服务与端口

固定入口端口以 `apps/port.txt` 为准。下表只表示本地开发默认监听或外部稳定入口；测试、预发和线上环境的内部跨服务访问应通过 Redis service registry endpoint 发现目标服务，不应按表内默认端口直连。`admin-web:3002` 是本地 Vite 开发端口，不属于后端入口端口清单。

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

- [Rust 游戏服开发指南](./docs/游戏服与接入层/Rust游戏服开发指南.md)
- [帧同步与房间生命周期设计](./docs/游戏服与接入层/帧同步与房间生命周期设计.md)
- [game-proxy 热切换代理设计](./docs/游戏服与接入层/game-proxy热切换代理设计.md)
- [更新策略拆分](./docs/游戏服与接入层/游戏服更新策略拆分.md)
- [空房接管式灰度规范](./docs/游戏服与接入层/空房接管式灰度规范.md)
- [空房接管式灰度任务清单](./docs/游戏服与接入层/空房接管式灰度任务清单.md)
- [底层框架路线图](./docs/游戏服与接入层/游戏服底层框架路线图.md)
- [大世界常驻 Room 热更新设计](./docs/游戏服与接入层/大世界常驻Room热更新设计.md)
- [因果留影与风门远行服务端设计](./docs/游戏服与接入层/因果留影与风门远行服务端设计.md)
- [网络延迟补偿设计](./docs/游戏服与接入层/网络延迟补偿设计.md)

配置、场景与具体游戏逻辑：

- [CSV 配置表设计](./docs/配置与场景/CSV配置表设计.md)
- [CSV 热更现状清单](./docs/配置与场景/CSV热更现状清单.md)
- [场景地图格式设计](./docs/配置与场景/场景地图格式设计.md)
- [背包系统设计](./docs/游戏服与接入层/背包系统设计.md)
- [战斗 ECS 设计](./docs/游戏服与接入层/战斗ECS设计.md)

周边服务、后台与安全：

- [服务注册中心设计](./docs/周边服务/服务注册中心设计.md)
- [聊天与邮件系统设计](./docs/周边服务/聊天与邮件系统设计.md)
- [匹配服务设计](./docs/周边服务/匹配服务设计.md)
- [管理后台设计](./docs/后台与运维/管理后台设计.md)
- [监控设计](./docs/安全与监控/监控设计.md)
- [安全设计](./docs/安全与监控/安全设计.md)
- [限流与安全现状](./docs/安全与监控/限流与安全现状.md)
- [游戏服务安全分层与敏感操作处理指南](./docs/安全与监控/游戏服务安全分层与敏感操作处理指南.md)

## 环境依赖

- Node.js 18+：`auth-http`、`admin-api`、`admin-web`、`announce-service`、`mail-service`、`mock-client`
- Rust 1.88+：`game-server`、`game-proxy`、`chat-server`、`match-service`
- Redis：session、ticket、限流、服务注册、metrics 快照
- NATS：邮件通知、session kick、metrics 采集
- PostgreSQL：账号、审计、游戏事件、公告、邮件等持久化数据

本地二进制工具优先放在项目根目录的 `bin/` 下，例如：

- `bin/nats-server.exe`

脚本和环境检查会优先使用 `bin/` 中的可执行文件；未找到时再回退到系统 `PATH` 或常见安装目录。

数据库初始化脚本：

```powershell
psql -U postgres -f db/init.sql
powershell -ExecutionPolicy Bypass -File .\scripts\reset-dev-data.ps1 -Confirm
```

`db/init.sql` 是 PostgreSQL 本地 bootstrap 脚本。当前阶段没有线上持久库升级需求，不维护独立 migration runner；正式状态见 [数据库初始化说明](./docs/数据库/数据库初始化说明.md)。

## 常用命令

安装 Node.js workspace 依赖：

```powershell
npm install
```

本地环境检查：

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\check-env.ps1
```

核心开发栈一键启动：

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\dev-stack.ps1
```

默认会启动 Redis、NATS、`auth-http`、`game-server`、`game-proxy`、`admin-api`、`admin-web` 和 `metrics-collector`。`chat-server`、`match-service`、`announce-service`、`mail-service`、`myforge-agent` 需要分别通过 `-WithChat`、`-WithMatch`、`-WithAnnounce`、`-WithMail`、`-WithMyforgeAgent` 启用；未启用 `match-service` 时，`game-server` 会按本地开发环境允许降级跳过匹配通知。`mail-service` 需要可用的 PostgreSQL，`myforge-agent` 需要先正确配置 `apps/myforge-agent/.env` 中的密钥、外部工作区和 Codex 路径。

`scripts/dev/services/*.ps1` 是 `dev-stack.ps1` 使用的单服务启动 helper，只建议排查单服务启动问题时手工调用。

Node.js 服务也可以通过根脚本启动：

```powershell
npm run dev:auth
npm run dev:admin-api
npm run dev:admin-web
npm run dev:announce
npm run dev:mail
```

测试账号录入：

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\seed-auth-test-accounts.ps1
```

默认测试账号：

- `test001 / Passw0rd!`
- `test002 / Passw0rd!`
- `gm001 / AdminPass123!`

## 测试与联调

自动化测试入口：

```powershell
npm test
npm run test:auth-http
npm run test:integration
npm run test:security
```

mock-client 房间联调：

主链路应经过 `game-proxy`。本地 TCP fallback 默认是 `game-proxy` 端口加 `10000`，即 `14000`；该方式仅用于 development/local 联调，不作为测试、预发或线上准入路径：

```powershell
npm run flow:mock-client -- --scenario happy --http-base-url http://127.0.0.1:3000 --host 127.0.0.1 --port 14000 --room-id room-a
npm run flow:mock-client -- --scenario two-client-room --http-base-url http://127.0.0.1:3000 --host 127.0.0.1 --port 14000 --room-id room-b
```

MyBevy `arena.robot_sync` 对应的服务端验收场景是 `robot-sync-room`，要求 room policy 为 `robot_sync_room`。本地完整匹配链路建议带匹配服务启动：

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\dev-stack.ps1 -WithMatch
npm run flow:mock-client -- --scenario robot-sync-room --http-base-url http://127.0.0.1:3000 --host 127.0.0.1 --port 14000 --room-id robot-sync-room --policy-id robot_sync_room
```

如果本机 `apps/game-proxy/.env` 覆盖了 TCP fallback，例如 `PROXY_TCP_FALLBACK_PORT=17002`，把命令中的 `--port 14000` 改为实际端口。该场景会验证两个客户端都收到包含双方 `robot_move` 的 `FrameBundlePush`，并验证非法 action、非法 JSON、方向越界和速度越界会被拒绝。

下面是绕过 `game-proxy`、直连 `game-server:7000` 的调试方式，仅用于本地定位游戏服协议或房间逻辑问题，不作为测试、预发或线上路径：

```powershell
npm run flow:mock-client -- --scenario happy --http-base-url http://127.0.0.1:3000 --host 127.0.0.1 --port 7000 --room-id room-a
npm run flow:mock-client -- --scenario two-client-room --http-base-url http://127.0.0.1:3000 --host 127.0.0.1 --port 7000 --room-id room-b
```

协作约定：模块功能开发完成后，不要直接自动启动服务、运行集成测试或联调脚本；应先确认所需依赖和服务已经由使用者准备好，再执行对应测试。

## 基础设定

日志配置统一使用以下环境变量模型：

- `LOG_LEVEL`
- `LOG_ENABLE_CONSOLE`
- `LOG_ENABLE_FILE`
- `LOG_DIR`

Node.js 服务使用 `log4js`，Rust 异步服务使用 `tracing + tracing-subscriber + tracing-appender`。

协议常量：

- `MAGIC = 0xCAFE`
- 玩家协议与内部控制协议尽量收敛到 `packages/proto`
- 外部 `mybevy` 客户端和本仓库 `tools/mock-client` 都应以 `packages/proto` 为协议事实源
- 控制机迁移、控制机 endpoint、权威快照和待处理输入使用 `packages/proto/game.proto` 中的 `Authority*` 消息，并在 Rust 侧由 `packages/authority-core` 复用基础结构
- 聊天协议已收敛到 `packages/proto/chat.proto`；`apps/chat-server` 从共享 proto 编译生成 Rust 绑定

## Git 提交规则

- 除非用户明确要求，不要提交 git commit 或执行 push。
- 提交按功能模块拆分，标题使用 `<type>: <中文主题>`。
- 提交正文应说明关键改动和原因；涉及端口、配置、协议、脚本或跨服务联动时要写明影响范围。
