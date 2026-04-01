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
- MariaDB 玩家账号与认证审计落库
- MariaDB 游戏连接与房间事件审计落库
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

- Redis：session、ticket、短期在线态
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

当前脚本会自动创建或更新 `player_accounts` 中的密码账号字段：`login_name`、`display_name`、`password_algo`、`password_salt`、`password_hash`。这一步先解决“测试账号入库”，后续再把真实账号登录接口接上即可直接复用。
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

## 下一步建议

1. 增加限流和风控
2. 增加内部控制面
3. 增加开始游戏 / 结束游戏状态流转


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
