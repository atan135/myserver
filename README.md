# MyServer

通用游戏后端框架第一版最小闭环，当前包含：

- `apps/auth-http`：Node.js + Express 登录服
- `apps/game-server`：Rust + Tokio TCP 游戏服
- `packages/proto`：玩家协议与内部控制协议
- `docs`：架构与协议文档
- `scripts`：环境检查与本地启动辅助脚本
- `tools/mock-client`：无真实客户端依赖的联调工具

## 统一日志方案

当前两边统一采用相同的日志配置思想：

- `LOG_LEVEL`
- `LOG_ENABLE_CONSOLE`
- `LOG_ENABLE_FILE`
- `LOG_DIR`

### auth-http

- 使用 `log4js`
- 支持 console 输出
- 支持按天滚动文件输出

### game-server

- 使用 `tracing + tracing-subscriber + tracing-appender`
- 支持 console 输出
- 支持按天滚动文件输出
- 保留 Rust 异步服务里更合适的结构化日志能力

Rust 侧我推荐继续用 `tracing`，而不是强行找一个和 Node 完全同名的日志库。原因很直接：

- `tracing` 是 Rust 异步网络服务更合适的日志/事件体系
- 对字段化日志、span、异步上下文更友好
- 和当前 `game-server` 的写法最兼容
- 同时仍然可以实现和 `auth-http` 一样的 console/file 双输出与目录配置

## 当前已完成

- HTTP 登录、access token、game ticket
- Redis 会话与 ticket 存储
- Rust TCP 鉴权、心跳、错误响应
- 房间核心闭环：加入、离开、准备、房间快照广播、owner 转移
- Node mock client 单客户端与双客户端联调场景
- 统一日志配置模型
- 协议与使用文档

## 日志配置

### auth-http

参考 [apps/auth-http/.env.example](./apps/auth-http/.env.example)

- `LOG_LEVEL=info`
- `LOG_ENABLE_CONSOLE=true`
- `LOG_ENABLE_FILE=true`
- `LOG_DIR=logs/auth-http`
- `REDIS_KEY_PREFIX=`

### game-server

参考 [apps/game-server/.env.example](./apps/game-server/.env.example)

- `LOG_LEVEL=info`
- `LOG_ENABLE_CONSOLE=true`
- `LOG_ENABLE_FILE=true`
- `LOG_DIR=logs/game-server`
- `REDIS_KEY_PREFIX=`

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

测试默认使用本地 Redis：

- `TEST_REDIS_URL` 默认 `redis://127.0.0.1:6379`
- 每次测试会自动生成独立 `REDIS_KEY_PREFIX`
- 测试结束后会按前缀清理 Redis key，避免污染开发数据

## 安装依赖

### auth-http

因为新增了 `log4js`，如果你还没安装新依赖，需要执行：

```powershell
cd apps/auth-http
npm install
```

### game-server

因为新增了 `tracing-appender`，如果你要重新验证编译，需要执行：

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

## 下一步建议

1. 接入 MariaDB 持久化账号与审计
2. 增加限流和风控
3. 增加内部控制面
4. 增加开始游戏 / 结束游戏状态流转
