# MyServer 架构设计

## 1. 文档定位

本文描述的是仓库当前已经形成的实际架构，而不是早期的骨架规划。

- 代码基准：以当前仓库实现为准
- 适用范围：`CLAUDE.md`、`README.md`、`docs/*` 中涉及的整体架构说明
- 说明：部分专题设计文档会比代码实现更超前，本文只描述当前真实存在的服务边界、通信关系和已落地的主链路

---

## 2. 当前仓库现状

仓库已经是一个多服务 monorepo，包含登录、游戏接入、游戏逻辑、聊天、匹配、邮件、管理后台、协议包、服务注册中心、脚本和测试工程。

当前目录概览：

```text
.
├─ apps/
│  ├─ auth-http/        # 登录服
│  ├─ admin-api/        # 管理后台 API
│  ├─ admin-web/        # 管理后台前端
│  ├─ game-server/      # 游戏逻辑服
│  ├─ game-proxy/       # 游戏接入代理
│  ├─ chat-server/      # 聊天服
│  ├─ match-service/    # 匹配服务
│  ├─ mail-service/     # 邮件服务
│  └─ simple-client/    # Unity 测试客户端
├─ packages/
│  ├─ proto/            # 共享协议定义
│  └─ service-registry/ # Redis 服务注册中心包
├─ tools/
│  └─ mock-client/      # Node.js 联调工具
├─ scripts/             # 本地启动与辅助脚本
├─ db/                  # 数据库初始化脚本
└─ docs/                # 设计文档
```

---

## 3. 技术选型

| 层级 | 技术 |
|------|------|
| Node.js 服务 | Node.js 18+、Express |
| Rust 长连接服务 | Rust、Tokio、tracing |
| 前端后台 | Vue 3、Vite、Element Plus |
| 玩家协议 | 自定义包头 + Protobuf |
| 内部服务协议 | TCP + Protobuf / gRPC |
| 缓存与协调 | Redis |
| 持久化 | MariaDB / MySQL |
| 配置表 | CSV + Rust 侧运行时热加载 |

---

## 4. 服务清单

固定入口端口以 `apps/port.txt` 为准；未纳入该清单的附属管理口或内部端口，以各服务代码默认值为准。

| 服务 | 默认端口 | 技术栈 | 主要职责 |
|------|----------|--------|----------|
| `auth-http` | `3000` | Node.js + Express | 登录、发 access token、发 game ticket、部分安全审计、调用游戏服管理接口 |
| `admin-api` | `3001` | Node.js + Express | 管理员认证、审计查询、玩家管理、GM 接口、监控接口 |
| `admin-web` | `3002` | Vue 3 + Vite | 管理后台前端 |
| `game-server` | `7000` | Rust + Tokio | 玩家鉴权、房间生命周期、帧推进、配置表热加载、游戏逻辑与管理接口 |
| `game-server admin` | `7500` | Rust + Tokio | 供 `auth-http` / `admin-api` 调用的内部管理口 |
| `game-proxy` | `4000` | Rust + Tokio KCP | 客户端游戏入口，转发到 `game-server` 本地 socket |
| `game-proxy admin` | `7101` | Rust + Tokio | 查看上游、切换路由、维护模式 |
| `chat-server` | `9001` | Rust + Tokio | 单聊、群聊、聊天历史、邮件通知推送 |
| `match-service` | `9002` | Rust + tonic gRPC | 匹配池、撮合、向 `game-server` 发起房间协作 |
| `mail-service` | `9003` | Node.js + Express | 邮件 CRUD、邮件通知发布、部分服务注册接入 |

---

## 5. 总体拓扑

```text
                       +-------------------+
                       |    admin-web      |
                       |  Vue 3 / Vite     |
                       +---------+---------+
                                 |
                                 v
                       +-------------------+
                       |    admin-api      |
                       | Express + JWT     |
                       +----+---------+----+
                            |         |
                            |         v
                            |   game-server admin
                            |
                            v
                         Redis / MySQL


+-------------+      +-------------------+      +-------------------+
|   Client    | ---> |    auth-http      | ---> |  Redis / MySQL    |
| Unity/mock  |      | login + ticket    |      | session/ticket/...|
+------+------+      +---------+---------+      +-------------------+
       |                       |
       | ticket + proxy addr   |
       v                       |
+--------------+               |
|  game-proxy  | --------------+
| KCP ingress  | ---> local socket / named pipe ---> +-------------------+
+------+-------+                                     |    game-server     |
       |                                             | room/runtime/admin |
       |                                             +----+----------+----+
       |                                                  |          |
       |                                                  |          v
       |                                                  |     match-service
       |                                                  |
       |                                                  v
       |                                             service-registry
       |
       +-------------------------------> chat-server
                                         ^
                                         |
                                   Redis Pub/Sub
                                         |
                                     mail-service
```

---

## 6. 核心链路

### 6.1 玩家登录与进游戏

1. 客户端通过 HTTP 调用 `auth-http` 登录。
2. `auth-http` 校验账号或游客身份，签发：
- `accessToken`
- `game ticket`
- 当前配置下发的 `gameProxyHost/gameProxyPort`
3. 客户端使用 ticket 连接 `game-proxy`。
4. `game-proxy` 将连接转发到 `game-server` 的本地 socket。
5. `game-server` 校验 ticket 签名与 Redis 中的 ticket 记录，成功后建立会话。

当前主链路特征：

- 登录入口是 HTTP JSON
- 进入游戏入口是 `game-proxy`
- 游戏逻辑服不直接暴露给公网客户端
- `game-server` 负责最终鉴权和房间/逻辑状态

### 6.2 房间与对局

`game-server` 当前已经具备房间主循环和多种房间策略：

- 房间创建、加入、离开、准备、开始、结束
- 房主转移
- 观战者加入
- 断线重连恢复
- 帧推进与定时快照
- 最近输入历史保留
- `RoomManager + RoomRuntimePolicy + RoomLogic` 的结构化运行时

当前房间策略至少包括：

- `persistent_world`
- `disposable_match`
- `sandbox`
- `movement_demo`

### 6.3 管理后台

管理后台分为两层：

- `admin-web`：前端页面与操作入口
- `admin-api`：鉴权、权限控制、审计、GM 指令、监控查询

`admin-api` 会通过 `game-server` 的 admin 通道执行内部控制命令，例如：

- 广播
- 发物品
- 踢人
- 封禁

### 6.4 聊天与邮件

- `chat-server` 负责聊天会话、聊天历史和在线推送
- `mail-service` 负责邮件 CRUD
- `mail-service` 通过 Redis Pub/Sub 通知 `chat-server`
- `chat-server` 再把邮件通知推送给在线玩家

### 6.5 匹配服务

- `match-service` 对外提供 gRPC 匹配接口
- `game-server` 内置 `MatchClient`
- 房间创建、玩家进入、玩家离开、对局结束等事件会回调到 `match-service`

注意：匹配服务当前是“部分闭环”，接口和主流程已存在，但实际建房与超时清理仍未完全成熟。

### 6.6 服务发现与监控

- `packages/service-registry` 提供 Redis 注册/发现能力
- `game-server` 已支持按实例注册
- `game-proxy` 已支持从注册中心发现上游 `game-server`
- `mail-service` 已有自己的 registry 注册逻辑
- 各服务会把 metrics/heartbeat 写入 Redis
- `admin-api` 提供监控聚合接口，`admin-web` 提供监控页面

注意：服务发现目前仍是“部分接入”，并非所有服务都已经统一使用同一套注册中心实现。

---

## 7. 数据职责

### 7.1 Redis

Redis 当前承担以下职责：

- `auth-http` 的 session
- game ticket 存储
- 限流与账号锁定相关数据
- 服务注册中心
- metrics 与 heartbeat
- `mail-service -> chat-server` 的 Pub/Sub 通知

### 7.2 MySQL / MariaDB

数据库当前主要承担：

- 玩家账号
- 登录审计与安全审计
- 游戏连接审计
- 房间事件审计
- 邮件业务数据

仓库中已有统一初始化脚本：

- `db/init.sql`

---

## 8. 协议分层

### 8.1 玩家游戏协议

玩家与 `game-server` 使用自定义包头 + Protobuf：

```text
| magic(2) | version(1) | flags(1) | msgType(2) | seq(4) | bodyLen(4) | body(N) |
```

共享消息定义位于：

- `packages/proto/game.proto`

当前协议已不只是基础登录与房间消息，还包括：

- 房间主流程
- 观战
- 断线重连
- 匹配建房
- 移动输入与纠正
- 背包/仓库/属性/外观推送

### 8.2 管理控制协议

管理控制面不复用玩家通道，使用独立的 admin 协议：

- `packages/proto/admin.proto`

当前由 `auth-http` / `admin-api` 通过内部 TCP 管理口调用 `game-server`。

### 8.3 匹配内部协议

匹配服务使用 gRPC：

- `packages/proto/match.proto`

主要包括两类接口：

- 对外匹配接口
- 对内房间协作接口

### 8.4 聊天协议

`chat-server` 当前有独立的聊天协议定义，未放在 `packages/proto` 下统一管理：

- `apps/chat-server/src/proto/chat.proto`

---

## 9. 仓库分层原则

### 9.1 `apps/`

放可启动的业务服务和客户端工程。

### 9.2 `packages/`

放跨服务共享的协议或基础包。

当前已包含：

- `proto`
- `service-registry`

### 9.3 `tools/`

放联调、验证、测试辅助工具。

### 9.4 `scripts/`

放本地开发脚本和环境辅助脚本。

当前常用脚本包括：

- `scripts/check-env.ps1`
- `scripts/dev-auth.ps1`
- `scripts/dev-game.ps1`
- `scripts/dev-proxy.ps1`
- `scripts/dev-chat.ps1`
- `scripts/dev-match.ps1`
- `scripts/seed-auth-test-accounts.ps1`

---

## 10. 当前实现状态总结

当前已经形成的稳定架构能力：

- 多服务 monorepo 基本形态已完成
- 登录、ticket、游戏接入、游戏逻辑、后台、邮件、聊天、匹配都已有独立服务
- `game-server` 已有较完整的房间运行时框架
- `game-proxy` 已具备静态上游和基于注册中心的动态发现能力
- `admin-web + admin-api` 已能支撑审计、玩家管理、GM、监控
- Redis 与 MySQL 都已经在多条主链路中实际使用

当前仍需注意的事实：

- 服务发现尚未完全统一到一套实现上
- `auth-http` 登录响应当前只下发 `gameProxyHost/gameProxyPort`，不是完整的服务地址表
- 部分专题文档描述的是目标设计，不等于已经全部落地

---

## 11. 当前端口约定

当前端口规范已经按 `apps/port.txt` 收口，联调时按下面的口径理解：

- 固定入口端口以 `apps/port.txt` 为准：
- `auth-http` -> `3000`
- `admin-api` -> `3001`
- `game-proxy` -> `4000`
- `game-server` -> `7000`
- `game-server admin` -> `7500`
- `auth-http`、`admin-api`、`game-server`、`game-proxy` 的默认配置已同步到上述固定端口
- `game-proxy admin` 当前仍使用代码默认值 `7101`，属于代理自身的内部管理口，不在 `apps/port.txt` 的固定入口清单里
- `chat-server`、`match-service`、`mail-service` 属于内部服务；文档中的 `9001/9002/9003` 主要用于本地开发与默认示例，部署时仍应以实际配置和注册中心信息为准

---

## 12. 相关文档

- `README.md`
- `CLAUDE.md`
- `docs/protocol.md`
- `docs/game-server-frame-sync-design.md`
- `docs/service-registry-design.md`
- `docs/match-service-design.md`
- `docs/admin-panel.md`
