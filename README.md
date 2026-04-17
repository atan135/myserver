# MyServer - 通用游戏后端框架

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
```

## 核心架构

- **认证流程**: HTTP 登录 → Redis ticket → KCP game-proxy → UDS game-server
- **存储**: Redis(session/ticket/注册中心) + MariaDB(账号/审计)
- **日志**: 统一日志模型 (LOG_LEVEL/CONSOLE/FILE/DIR)

## 重要文档

| 文档 | 说明 |
|------|------|
| [架构设计](./docs/architecture.md) | 整体技术选型与服务边界 |
| [协议设计](./docs/protocol.md) | 玩家 TCP 协议、消息号、房间规则 |
| [帧同步与房间生命周期](./docs/game-server-frame-sync-design.md) | 房间框架、帧推进、RoomLogic 抽象 |
| [game-proxy 热切换代理](./docs/game-proxy-hot-update-design.md) | KCP 接入层、热更新、路由策略 |
| [CSV 配置系统](./docs/game-server-csv-config-design.md) | 编译期代码生成、运行时热更新 |
| [服务注册中心](./docs/service-registry-design.md) | Redis 服务发现与心跳 |
| [聊天与邮件系统](./docs/game-server-chat-design.md) | 单聊/群聊、邮件、公告架构 |
| [匹配服务](./docs/match-service-design.md) | 匹配池、撮合算法、gRPC 接口 |
| [网络延迟补偿](./docs/network-lag-compensation-design.md) | 帧同步/状态同步/射击回溯设计 |
| [限流与风控](./docs/rate-limit-and-security.md) | 分层防护策略 |
| [管理后台](./docs/admin-panel.md) | admin-api/admin-web 使用说明 |

## 关键命令

```powershell
# 启动 auth-http (登录服)
cd apps/auth-http && npm start

# 启动 game-server (Rust TCP 游戏服)
cd apps/game-server && cargo run

# 启动 game-proxy (Rust KCP 接入代理)
cd apps/game-proxy && cargo run

# 启动 chat-server (Rust TCP 聊天服)
cd apps/chat-server && cargo run

# 启动 match-service (Rust gRPC 匹配服务)
cd apps/match-service && cargo run

# 启动 announce-service (Node.js HTTP 公告服务)
cd apps/announce-service && npm start

# 启动 mail-service (Node.js HTTP 邮件服务)
cd apps/mail-service && npm start

# 启动 admin-api (管理后台 API)
cd apps/admin-api && npm start

# 启动 admin-web (管理前台)
cd apps/admin-web && npm run dev

# 运行测试
npm test

# 房间联调 (mock-client)
npm run flow:mock-client -- --scenario happy --http-base-url http://127.0.0.1:3000 --host 127.0.0.1 --port 7000 --room-id room-a
```

## 测试账号

- `test001 / Passw0rd!`
- `test002 / Passw0rd!`
- `gm001 / AdminPass123!`

## 环境依赖

- Redis (session/ticket/服务注册/限流)
- MariaDB (账号、认证审计、房间事件)
- Node.js 18+ (auth-http/admin-api/announce-service/mail-service)
- Rust 1.75+ (game-server/game-proxy/chat-server/match-service)

## 数据库

- `db/init.sql` 初始化脚本
- `myserver_auth` - 账号库
- `myserver_game` - 游戏库

## 协议常量

- **MAGIC**: `0xCAFE` - 所有服务间通信的固定魔数，用于协议头校验
  - game-server: `apps/game-server/src/protocol.rs`
  - chat-server: `apps/chat-server/src/protocol.rs`
  - mock-client: `tools/mock-client/src/protocol.js`
  - 新增 server 必须使用相同值 `0xCAFE`

## Git 提交规则

- **除非我（用户）主动要求提交 git，否则你不能将修改提交到 git**
- 所有 git 操作（commit、push 等）都必须等待我明确指令
