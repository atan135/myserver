# MyServer

通用游戏后端框架，当前已实现完整的多服务架构：

## 项目结构

```
apps/
├── auth-http/        # Node.js + Express 登录服 (端口 3000)
├── game-server/     # Rust + Tokio TCP 游戏服 (端口 7000)
├── game-proxy/      # Rust + Tokio KCP 接入代理 (端口 4000)
├── chat-server/     # Rust + Tokio TCP 聊天服 (端口 9001)
├── match-service/   # Rust + gRPC 匹配服务 (端口 9002)
├── announce-service/ # Node.js HTTP 公告服务 (端口 9004)
├── mail-service/    # Node.js HTTP 邮件服务 (端口 9003)
├── admin-api/       # Node.js + Express 管理后台 API (端口 3001)
├── admin-web/       # Vue 3 + Element Plus 管理前台 (端口 3002)
├── simple-client/   # Unity 客户端测试工程
tools/
├── mock-client/     # Node.js 无客户端联调工具
packages/
├── proto/           # 玩家协议与内部控制协议 (Protobuf)
├── service-registry/ # 基于 Redis 的服务注册中心
scripts/             # 环境检查与本地启动辅助脚本
db/                  # 数据库初始化脚本
docs/                # 架构与协议文档
```

## 文档导航

- [底层框架路线图](./docs/game-server-framework-roadmap.md)
- [更新策略拆分](./docs/game-server-update-strategy.md)
- [帧同步与房间生命周期设计](./docs/game-server-frame-sync-design.md)
- [game-proxy 热切换代理设计](./docs/game-proxy-hot-update-design.md)
- [CSV 配置表设计](./docs/game-server-csv-config-design.md)
- [CSV 热更现状清单](./docs/game-server-csv-hot-reload-status.md)
- [协议设计](./docs/protocol.md)
- [服务注册中心设计](./docs/service-registry-design.md)
- [聊天与邮件系统设计](./docs/game-server-chat-design.md)
- [匹配服务设计](./docs/match-service-design.md)
- [网络延迟补偿设计](./docs/network-lag-compensation-design.md)
- [限流与风控设计](./docs/rate-limit-and-security.md)
- [管理后台设计](./docs/admin-panel.md)

## 统一日志方案

当前两边统一采用相同的日志配置思想：

- `LOG_LEVEL`
- `LOG_ENABLE_CONSOLE`
- `LOG_ENABLE_FILE`
- `LOG_DIR`

### auth-http / announce-service / mail-service / admin-api

- 使用 `log4js`
- 支持 console 输出
- 支持按天滚动文件输出

### game-server / game-proxy / chat-server / match-service

- 使用 `tracing + tracing-subscriber + tracing-appender`
- 支持 console 输出
- 支持按天滚动文件输出
- 保留 Rust 异步服务里更合适的结构化日志能力

Rust 侧推荐继续用 `tracing`，原因是：
- `tracing` 是 Rust 异步网络服务更合适的日志/事件体系
- 对字段化日志、span、异步上下文更友好
- 和当前 `game-server` 的写法最兼容
- 同时仍然可以实现和 `auth-http` 一样的 console/file 双输出与目录配置

## 当前已完成

### 核心服务
- HTTP 登录、access token、game ticket
- Redis 会话与 ticket 存储
- MariaDB 玩家账号与认证审计落库
- MariaDB 游戏连接与房间事件审计落库
- Rust TCP 鉴权、心跳、错误响应
- Rust KCP 接入代理 (game-proxy)
- 统一日志配置模型

### 房间与帧同步
- 房间核心闭环：加入、离开、准备、房间快照广播、owner 转移
- 房间级帧推进 (RoomManager + RoomRuntimePolicy)
- RoomLogic trait 和多种实现 (TestRoomLogic, PersistentWorldLogic, DisposableMatchLogic, SandboxLogic)
- 观战者支持 (MemberRole + Observer)
- 断线重连恢复 (snapshot + frame_id + recent_inputs)
- 定时快照生成 (每 N 帧)
- 输入历史记录 (最近 300 帧)
- 未来帧输入正确处理

### 微服务
- chat-server: 单聊、群聊、离线消息
- match-service: 匹配池、撮合算法、gRPC 接口
- announce-service: 公告 CRUD、有效公告查询、服务注册与监控上报
- mail-service: 邮件 CRUD、附件领取、Redis Pub/Sub 通知
- admin-api + admin-web: 运营后台 (账号管理、审计日志)
- service-registry: 基于 Redis 的服务注册与发现

### 工具链
- Node mock client 单客户端与双客户端联调场景
- 协议与使用文档
- 启动脚本 (dev-auth.ps1, dev-game.ps1, dev-proxy.ps1, dev-chat.ps1, dev-match.ps1, dev-announce.ps1)

## 日志配置

### auth-http

参考 [apps/auth-http/.env.example](./apps/auth-http/.env.example)

- `LOG_LEVEL=info`
- `LOG_ENABLE_CONSOLE=true`
- `LOG_ENABLE_FILE=true`
- `LOG_DIR=logs/auth-http`
- `REDIS_KEY_PREFIX=`
- `MYSQL_ENABLED=false`
- `MYSQL_URL=mysql://root:password@127.0.0.1:3306/myserver_auth`
- `MYSQL_POOL_SIZE=10`

### game-server

参考 [apps/game-server/.env.example](./apps/game-server/.env.example)

- `LOG_LEVEL=info`
- `LOG_ENABLE_CONSOLE=true`
- `LOG_ENABLE_FILE=true`
- `LOG_DIR=logs/game-server`
- `REDIS_KEY_PREFIX=`
- `MYSQL_ENABLED=false`
- `MYSQL_URL=mysql://root:password@127.0.0.1:3306/myserver_game`
- `MYSQL_POOL_SIZE=10`

## 数据库初始化

初始化脚本：

- `db/init.sql`

执行示例：

```powershell
mysql -uroot -p < db/init.sql
```

当前存储职责：

- Redis：session、ticket、短期在线态、服务注册中心
- MariaDB：玩家账号、认证审计、TCP 连接审计、房间事件审计

## auth-http 测试账号录入

前提：

- `apps/auth-http/.env` 里把 `MYSQL_ENABLED=true`
- `MYSQL_URL` 指向你的 `myserver_auth` 库
- 首次可先执行 `mysql -uroot -p < db/init.sql`

一键录入内置测试账号：

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\seed-auth-test-accounts.ps1
```

默认会写入这些账号：

- `test001 / Passw0rd!`
- `test002 / Passw0rd!`
- `gm001 / AdminPass123!`

录入单个账号：

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\seed-auth-test-accounts.ps1 -Account test003 -Password Passw0rd! -DisplayName "Test User 003"
```

从 JSON 批量导入：

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\seed-auth-test-accounts.ps1 -File .\apps\auth-http\scripts\test-accounts.example.json
```

也可以直接在 `apps/auth-http` 下执行：

```powershell
npm run seed:test-accounts
```

## 自动化测试

当前已补两层自动化测试：

- `tests/auth-http.test.mjs`：HTTP 登录服接口测试
- `tests/integration-flow.test.mjs`：拉起 `auth-http + game-server` 后复用 `mock-client` 的端到端测试

执行入口：

```powershell
npm test
```

只跑 HTTP 测试：

```powershell
npm run test:auth-http
```

只跑跨服务集成测试：

```powershell
npm run test:integration
```

协作约定：

- 在本项目中，模块功能开发完成后，不要直接自动运行项目检测、集成测试、联调脚本或自动启动相关服务；应先明确提示用户需要启动哪些项目 / 服务及依赖项，待用户确认已启动后，再根据用户的明确指令执行对应测试。

测试默认使用本地 Redis：

- `TEST_REDIS_URL` 默认 `redis://127.0.0.1:6379`
- 每次测试会自动生成独立 `REDIS_KEY_PREFIX`
- 测试结束后会按前缀清理 Redis key，避免污染开发数据

## 安装依赖

### auth-http

因为新增了 `mysql2`，如果你还没安装新依赖，需要执行：

```powershell
cd apps/auth-http
npm install
```

### game-server

因为新增了 `mysql_async`，如果你要重新验证编译，需要执行：

```powershell
cd apps/game-server
cargo check
```

## 正常房间流验证

```powershell
npm run flow:mock-client -- --scenario happy --http-base-url http://127.0.0.1:3000 --host 127.0.0.1 --port 7000 --room-id room-a
```

## 双客户端房间联调

```powershell
npm run flow:mock-client -- --scenario two-client-room --http-base-url http://127.0.0.1:3000 --host 127.0.0.1 --port 7000 --room-id room-b
```

## mock-client 账号密码登录

单客户端场景可以直接走真实账号登录：

```powershell
npm run flow:mock-client -- --scenario happy --http-base-url http://127.0.0.1:3000 --host 127.0.0.1 --port 7000 --room-id room-account --login-name test001 --password Passw0rd!
```

双客户端场景可以分别指定 A/B 两套账号：

```powershell
npm run flow:mock-client -- --scenario two-client-room --http-base-url http://127.0.0.1:3000 --host 127.0.0.1 --port 7000 --room-id room-account-multi --login-name-a test001 --password-a Passw0rd! --login-name-b test002 --password-b Passw0rd!
```

不传账号参数时，`mock-client` 仍默认走 guest 登录。

## 启动脚本

| 脚本 | 用途 |
|------|------|
| `scripts/check-env.ps1` | 环境检查 |
| `scripts/dev-auth.ps1` | 启动 auth-http |
| `scripts/dev-game.ps1` | 启动 game-server |
| `scripts/dev-proxy.ps1` | 启动 game-proxy |
| `scripts/dev-chat.ps1` | 启动 chat-server |
| `scripts/dev-match.ps1` | 启动 match-service |
| `scripts/dev-announce.ps1` | 启动 announce-service |
| `scripts/seed-auth-test-accounts.ps1` | 录入测试账号 |
| `scripts/test-auth-http-login.ps1` | 测试登录接口 |

## Git 提交规范

- 提交按**功能模块**拆分：一个 commit 只解决一类问题，避免把协议、服务实现、测试工具、文档更新混在同一个提交里。
- 提交标题格式统一为：`<type>: <简短主题>`
- `type` 推荐使用：`feat`、`fix`、`docs`、`refactor`、`test`、`chore`
- 提交标题中的“主题”统一使用中文，不使用英文短语或中英混写主题。
- 标题要直接说明“改了什么”，优先写具体模块或能力，不写空泛描述。
- 提交正文与标题之间保留一个空行。
- 提交正文至少说明两点：
- 这次一起改了哪些关键项
- 为什么要这样改，解决了什么问题，或避免了什么联调/维护风险
- 如果改动涉及端口、配置、协议、脚本或跨服务联动，正文里要明确写出受影响的服务名、关键配置项或关键文件。

示例：

```text
chore: 统一 game-proxy 默认端口配置

将 game-proxy 默认监听端口和 auth-http 默认下发的 GAME_PROXY_PORT 一并调整为 4000，并同步更新 port.txt 与示例环境变量，避免与 game-server 端口段混用，减少联调时连错入口的问题。
```

## 下一步建议

1. 完成 P2：连接恢复、背压治理与安全边界
2. 完成 P3：控制面、观测性和状态持久化
3. 完善匹配服务与各服务的集成
