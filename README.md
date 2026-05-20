# MyServer

MyServer 是一个通用游戏后端框架 monorepo，当前已经形成完整的多服务形态：登录、游戏接入、游戏逻辑、聊天、匹配、邮件、公告、管理后台、协议包、服务注册中心、联调工具和本地脚本都在同一仓库内维护。

本文是面向开发者的快速入口。具体协议、接口行为、实现状态和任务拆解以 `docs/` 下专题文档与当前代码为准；`docs/prompts/` 仅保留初始设计阶段的历史提示词，不再作为当前设计依据。

## 当前架构

核心链路：

```text
Client / mock-client
  -> auth-http -> Redis / MySQL
  -> game-proxy -> game-server -> rooms / runtime / admin / configs

admin-web -> admin-api -> game-server admin / Redis / MySQL

game-server <-> match-service
mail-service -> Redis Pub/Sub -> chat-server
announce-service / mail-service / game-server / game-proxy -> service registry
```

当前已经稳定落地的能力：

- 多服务 monorepo 基本形态已完成。
- 登录、access token、game ticket、游戏接入、游戏逻辑、后台、聊天、匹配、邮件、公告都有独立服务。
- `game-server` 已有房间生命周期、帧推进、配置加载、内部管理口和部分具体游戏逻辑。
- `game-proxy` 已支持 KCP / TCP fallback、ticket 本地校验、静态上游和基于注册中心的动态发现。
- `chat-server`、`mail-service`、`announce-service` 当前独立部署；`game-proxy` 不负责聊天、邮件或公告转发。
- Redis 用于 session、ticket、限流、服务注册、metrics/heartbeat 和部分 Pub/Sub 通知。
- MariaDB / MySQL 用于账号、审计、游戏事件、公告、邮件等持久化数据。

仍需注意的当前缺口：

- `admin-api` 后端接口尚未真正执行基于角色的接口授权。
- `/api/admin/monitoring/*` 当前没有鉴权，不应直接暴露到公网。
- `game-server` admin 侧 GM 广播、踢人、封禁仍未形成完整端到端闭环。
- `game-proxy` 还没有 IP 黑名单、单 IP / 单账号连接上限和成熟公网加密方案。
- `chat-server` 当前只校验 ticket 签名和过期时间，不查询 Redis ticket 记录。
- 部分专题文档描述目标设计，不等于代码已经全部落地。

## 仓库结构

```text
apps/
├── auth-http/         # Node.js + Express 登录服
├── game-proxy/       # Rust + Tokio KCP 接入代理，保留 TCP fallback
├── game-server/      # Rust + Tokio 游戏逻辑服
├── chat-server/      # Rust + Tokio 聊天服
├── match-service/    # Rust + tonic gRPC 匹配服务
├── announce-service/ # Node.js HTTP 公告服务
├── mail-service/     # Node.js HTTP 邮件服务
├── admin-api/        # Node.js + Express 管理后台 API
├── admin-web/        # Vue 3 + Vite + Element Plus 管理前端
└── simple-client/    # Unity 测试客户端工程
packages/
├── proto/            # 共享 Protobuf 协议
└── service-registry/ # Redis 服务注册中心包
tools/
└── mock-client/      # Node.js 无客户端联调工具
scripts/              # 本地启动、环境检查、数据初始化辅助脚本
db/                   # 数据库初始化脚本
docs/                 # 当前正式设计文档
```

## 服务与端口

固定入口端口以 `apps/port.txt` 为准。内部服务端口主要用于本地开发默认值，部署和联调时应优先看实际配置、环境变量和服务注册中心。

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

- [整体架构](./docs/architecture.md)
- [协议设计](./docs/protocol.md)
- [文档校准状态汇总](./summary.md)

游戏服与接入层：

- [Rust 游戏服开发指南](./docs/game-server-rust-guide.md)
- [帧同步与房间生命周期设计](./docs/game-server-frame-sync-design.md)
- [game-proxy 热切换代理设计](./docs/game-proxy-hot-update-design.md)
- [更新策略拆分](./docs/game-server-update-strategy.md)
- [空房接管式灰度规范](./docs/game-server-room-rollout-spec.md)
- [空房接管式灰度任务清单](./docs/game-server-room-rollout-task-list.md)
- [底层框架路线图](./docs/game-server-framework-roadmap.md)
- [大世界常驻 Room 热更新设计](./docs/persistent-world-hot-update-design.md)
- [网络延迟补偿设计](./docs/network-lag-compensation-design.md)

配置、场景与具体游戏逻辑：

- [CSV 配置表设计](./docs/game-server-csv-config-design.md)
- [CSV 热更现状清单](./docs/game-server-csv-hot-reload-status.md)
- [场景地图格式设计](./docs/game-server-scene-map-format-design.md)
- [背包系统设计](./docs/game-server-inventory-design.md)
- [战斗 ECS 设计](./docs/game-server-combat-ecs-design.md)

周边服务、后台与安全：

- [服务注册中心设计](./docs/service-registry-design.md)
- [聊天与邮件系统设计](./docs/game-server-chat-design.md)
- [匹配服务设计](./docs/match-service-design.md)
- [管理后台设计](./docs/admin-panel.md)
- [监控设计](./docs/monitoring-design.md)
- [安全设计](./docs/security-design.md)
- [限流与安全现状](./docs/rate-limit-and-security.md)
- [游戏服务安全分层与敏感操作处理指南](./docs/game-security-operation-guide.md)

## 环境依赖

- Node.js 18+：`auth-http`、`admin-api`、`admin-web`、`announce-service`、`mail-service`、`mock-client`
- Rust 1.75+：`game-server`、`game-proxy`、`chat-server`、`match-service`
- Redis：session、ticket、限流、服务注册、metrics、Pub/Sub
- MariaDB / MySQL：账号、审计、游戏事件、公告、邮件等持久化数据

数据库初始化脚本：

```powershell
mysql -uroot -p < db/init.sql
```

## 常用命令

安装 Node.js workspace 依赖：

```powershell
npm install
```

本地启动脚本：

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\check-env.ps1
powershell -ExecutionPolicy Bypass -File .\scripts\dev-auth.ps1
powershell -ExecutionPolicy Bypass -File .\scripts\dev-game.ps1
powershell -ExecutionPolicy Bypass -File .\scripts\dev-proxy.ps1
powershell -ExecutionPolicy Bypass -File .\scripts\dev-chat.ps1
powershell -ExecutionPolicy Bypass -File .\scripts\dev-match.ps1
powershell -ExecutionPolicy Bypass -File .\scripts\dev-announce.ps1
```

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
- `chat-server` 当前仍保留独立聊天协议定义，具体见 `apps/chat-server/src/proto/chat.proto`

## Git 提交规则

- 除非用户明确要求，不要提交 git commit 或执行 push。
- 提交按功能模块拆分，标题使用 `<type>: <中文主题>`。
- 提交正文应说明关键改动和原因；涉及端口、配置、协议、脚本或跨服务联动时要写明影响范围。
