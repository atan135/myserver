# MyServer - 通用游戏后端框架

## 项目结构

```
apps/
├── auth-http/      # Node.js + Express 登录服 (端口 3000)
├── game-server/    # Rust + Tokio TCP 游戏服 (端口 7000)
├── game-proxy/     # Rust + Tokio KCP 接入代理 (热切换层)
├── chat-server/    # 服务端聊天系统
├── simple-client/  # Unity 客户端测试工程
packages/
├── proto/          # 玩家协议与内部控制协议
├── service-registry/ # 基于 Redis 的服务注册中心
```

## 核心架构

- **认证流程**: HTTP 登录 → Redis ticket → TCP game-server 鉴权
- **存储**: Redis(session/ticket) + MariaDB(账号/审计)
- **日志**: 统一日志模型 (LOG_LEVEL/CONSOLE/FILE/DIR)

## 关键命令

```powershell
# 启动 auth-http
cd apps/auth-http && npm start

# 启动 game-server (Rust)
cd apps/game-server && cargo run

# 启动 game-proxy (Rust)
cd apps/game-proxy && cargo run

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

- Redis (session/ticket 存储)
- MariaDB (账号、认证审计、房间事件)
- Node.js 18+ (auth-http)
- Rust 1.75+ (game-server/proxy)

## 数据库

- `db/init.sql` 初始化脚本
- `myserver_auth` - 账号库
- `myserver_game` - 游戏库

## 协议常量

- **MAGIC**: `0xCAFE` - 所有服务间通信的固定魔数，用于协议头校验
  - game-server: `apps/game-server/src/protocol.rs`
  - chat-server: `apps/chat-server/src/protocol.rs`
  - mock-client: `tools/mock-client/src/index.js`
  - 新增 server 必须使用相同值 `0xCAFE`

## 重要文档

- `docs/protocol.md` - 协议设计
- `docs/game-server-frame-sync-design.md` - 帧同步与房间生命周期
- `docs/game-proxy-hot-update-design.md` - 热切换代理设计

## Git 提交规则

- **除非我（用户）主动要求提交 git，否则你不能将修改提交到 git**
- 所有 git 操作（commit、push 等）都必须等待我明确指令
