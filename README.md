# MyServer

通用游戏后端框架第一版最小闭环，当前包含：

- `apps/auth-http`：Node.js + Express 登录服
- `apps/game-server`：Rust + Tokio TCP 游戏服
- `packages/proto`：玩家协议与内部控制协议
- `docs`：架构与协议文档
- `scripts`：环境检查与本地启动辅助脚本
- `tools/mock-client`：无真实客户端依赖的联调工具

## 当前已完成

- 单仓库多服务结构
- HTTP 健康检查与元信息接口
- HTTP 游客登录接口
- HTTP access token 查询接口
- HTTP game ticket 签发接口
- access token 和 game ticket 已写入 Redis
- Rust TCP 服务监听
- TCP 包头解析
- TCP `AuthReq` / `PingReq` / `RoomJoinReq` 基础处理
- TCP 鉴权除了验签，还会校验 Redis 中的 ticket 是否存在
- Node mock client 端到端联调脚本
- mock client 异常路径测试模式
- Rust warning 清理
- 基础协议与文档

## 当前未完成

- MariaDB 接入
- 内部 gRPC 控制面
- 自动化测试
- 限流、审计、持久化账号

## 文档

- [架构设计](./docs/architecture.md)
- [协议设计](./docs/protocol.md)

## 环境变量

### auth-http

参考 [apps/auth-http/.env.example](./apps/auth-http/.env.example)

- `PORT`
- `HOST`
- `NODE_ENV`
- `LOG_LEVEL`
- `REDIS_URL`
- `SESSION_TTL_SECONDS`
- `TICKET_SECRET`
- `TICKET_TTL_SECONDS`

### game-server

参考 [apps/game-server/.env.example](./apps/game-server/.env.example)

- `GAME_HOST`
- `GAME_PORT`
- `RUST_LOG`
- `REDIS_URL`
- `TICKET_SECRET`
- `HEARTBEAT_TIMEOUT_SECS`
- `MAX_BODY_LEN`

`auth-http` 和 `game-server` 必须使用相同的：

- `REDIS_URL`
- `TICKET_SECRET`

否则 TCP 鉴权会失败。

## 启动前置条件

### 1. 启动 Redis

如果你的 Redis 可执行文件不在 PATH，可以直接用本机路径启动：

```powershell
& "C:\Program Files\Redis\redis-server.exe"
```

### 2. 环境检查

```powershell
.\scripts\check-env.ps1
```

## 启动

### 启动 HTTP 登录服

```powershell
cd apps/auth-http
npm install
npm run dev
```

### 启动 Rust 游戏服

```powershell
cd apps/game-server
cargo run
```

## 正常链路联调

```powershell
npm run flow:mock-client -- --scenario happy --http-base-url http://127.0.0.1:3000 --host 127.0.0.1 --port 7000 --room-id room-a
```

## 异常路径联调

### 无效 ticket

```powershell
npm run flow:mock-client -- --scenario invalid-ticket --http-base-url http://127.0.0.1:3000 --host 127.0.0.1 --port 7000
```

### 未鉴权直接加房

```powershell
npm run flow:mock-client -- --scenario unauth-room-join --host 127.0.0.1 --port 7000
```

### 未知消息号

```powershell
npm run flow:mock-client -- --scenario unknown-message --host 127.0.0.1 --port 7000
```

### 超长消息体

```powershell
npm run flow:mock-client -- --scenario oversized-room-join --http-base-url http://127.0.0.1:3000 --host 127.0.0.1 --port 7000 --max-body-len 4096
```

## 常用参数

- `--http-base-url`
- `--host`
- `--port`
- `--room-id`
- `--guest-id`
- `--ticket`
- `--timeout-ms`
- `--scenario`
- `--max-body-len`

## 当前 Redis 存储内容

- `session:<accessToken>`：登录会话 JSON
- `ticket:<sha256(ticket)>`：ticket 对应的 `playerId`

## 下一步建议

1. 为 HTTP 和 TCP 增加自动化测试
2. 接入 MariaDB 持久化账号与审计
3. 增加限流和风控
4. 增加内部控制面
5. 增加 Redis 失效、重复 ticket、异常断线等测试
