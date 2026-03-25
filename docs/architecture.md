# 通用游戏后端框架架构设计 v0.1

## 当前仓库现状

- 当前仓库仅包含 `docs/prompts` 下的提示词文档。
- 目前不存在 HTTP 服务、游戏服、协议定义、脚本或测试代码。
- 当前目标是先建立单仓库多服务结构，并产出第一版可扩展骨架。

## 第一版架构结论

### 技术选型

- HTTP 登录服：Node.js 22 + Express 5
- TCP 游戏服：Rust 1.94 + Tokio
- 玩家协议：TCP + 自定义包头 + Protobuf
- 内部服务通信：优先 gRPC，第一版目录先预留接口定义
- 数据存储：MariaDB 10.5 + Redis 5
- 日志：HTTP 服使用结构化 JSON 日志；Rust 游戏服使用 `tracing`

### 为什么优先 Rust

- 当前房间规模为 5-10 人，不需要为极端吞吐过早引入复杂 C++ 内存管理。
- Rust 更适合长期演进的游戏长连接服务器，内存安全和并发安全成本更低。
- Tokio 适合承载连接、心跳、会话、消息路由等异步网络场景。
- 后续如果确实出现单机极限性能瓶颈，再评估对局逻辑热点是否需要独立 C++ 化。

### 服务边界

- `auth-http`：账号登录、游客登录、签发 HTTP token、签发 game ticket、管理工具统一入口
- `game-server`：TCP 接入、鉴权、心跳、消息路由、房间会话、基础状态管理
- `proto`：玩家协议、内部控制协议、错误码和消息号
- `scripts`：开发启动、环境检查、手工校验脚本
- `docs`：架构、协议、开发与测试说明

### 数据职责

- MariaDB：用户信息、账号绑定、审计落库、静态业务数据
- Redis：会话、ticket、短期限流、在线态辅助缓存

### 通信链路

- 玩家 <-> `game-server`：TCP 二进制协议
- 客户端/工具 <-> `auth-http`：HTTP JSON
- `auth-http` <-> `game-server`：后续使用 gRPC；第一版先定义协议和接口占位

## 玩家 TCP 协议设计

### 包结构

```text
| magic(2) | version(1) | flags(1) | msgType(2) | seq(4) | bodyLen(4) | body(N) |
```

字段约定：

- `magic`：固定协议头，快速过滤非法流量
- `version`：协议版本
- `flags`：压缩、保留位
- `msgType`：消息号
- `seq`：请求序号
- `bodyLen`：消息体长度
- `body`：Protobuf 编码内容

### 第一版允许的基础消息

- `AUTH_REQ`
- `AUTH_RES`
- `PING_REQ`
- `PING_RES`
- `ROOM_JOIN_REQ`
- `ROOM_JOIN_RES`
- `ERROR_RES`

### 安全边界

- 未鉴权连接仅允许 `AUTH_REQ`、`PING_REQ`
- 严格限制最大包长
- 严格限制单位时间消息数
- 非法 `msgType`、超长包、状态机非法流转直接断开

## 管理控制面设计

- 管理控制面不复用玩家 TCP 通道
- 第一版由 `auth-http` 作为统一入口
- `auth-http` 后续通过内部协议调用 `game-server`
- 第一版项目结构中先预留 `admin` 协议和服务接口
- 第一版只允许参数或配置级修改，不允许任意代码热更新

## 第一版范围

- 单仓库多服务目录
- HTTP 登录服骨架
- Rust 游戏服骨架
- 基础协议文件
- 开发与启动说明
- 环境变量模板
- 基础脚本与测试入口占位

## 后续范围

- 真正的登录闭环
- ticket 签发和校验
- TCP 拆包封包实现
- Redis 和 MariaDB 实际接入
- 管理控制面 API
- 自动化测试与压测
- 安全强化

## 建议目录结构

```text
.
├─ apps/
│  ├─ auth-http/
│  └─ game-server/
├─ packages/
│  └─ proto/
├─ scripts/
├─ docs/
│  ├─ architecture.md
│  └─ protocol.md
└─ README.md
```

## 默认假设

- 当前优先本地开发和单机启动
- 当前不依赖 Docker 才能继续工作
- 当前不要求一次性完成全部业务逻辑
- 当前优先建立可维护结构，而不是抢先堆实现

## 下一阶段

- 生成 monorepo 骨架
- 生成 HTTP 服务入口与基础路由
- 生成 Rust 游戏服入口与模块拆分
- 生成 `.proto` 文件
- 生成 README、协议文档和脚本
