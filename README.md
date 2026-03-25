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
- HTTP 登录、access token、game ticket
- Redis 会话与 ticket 存储
- Rust TCP 鉴权、心跳、错误响应
- 房间核心闭环：加入、离开、准备、房间快照广播、owner 转移
- Node mock client 单客户端与双客户端联调场景
- 协议与使用文档

## 正常房间流验证

```powershell
npm run flow:mock-client -- --scenario happy --http-base-url http://127.0.0.1:3000 --host 127.0.0.1 --port 7000 --room-id room-a
```

## 双客户端房间联调

用于验证：

- 第二人加入房间
- 双方收到房间快照广播
- 房主离开后 owner 自动转移

```powershell
npm run flow:mock-client -- --scenario two-client-room --http-base-url http://127.0.0.1:3000 --host 127.0.0.1 --port 7000 --room-id room-b
```

## 异常流验证

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

## 下一步建议

1. 为这些场景加自动化测试
2. 接入 MariaDB 持久化账号与审计
3. 增加限流和风控
4. 增加内部控制面
5. 增加开始游戏 / 结束游戏状态流转
