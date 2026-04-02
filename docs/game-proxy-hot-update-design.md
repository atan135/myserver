# game-proxy 热切换代理设计（草案）

这份文档描述新的 `apps/game-proxy` 项目。

当前确认后的职责边界是：

- `auth-http`：选服、登录、下发客户端资源列表、版本信息和进入游戏所需票据
- `game-server`：处理游戏后端业务逻辑
- `game-proxy`：位于客户端与 `game-server` 之间，负责 `game-server` 的热切换、连接接力和代理转发

`game-proxy` 明确使用 Rust 实现。

当前额外确认的网络通信约束是：

- 客户端 <-> `game-proxy`：使用 `KCP`
- `game-proxy` <-> `game-server`：使用 `UDS`（Unix Domain Socket）

这两个约束是后续实现的基础前提，不再按 TCP 透明代理思路直接落地。

## 1. 目标

`game-proxy` 要解决的核心问题不是客户端资源热更新，而是服务端热更新时的接入稳定性。

它要处理的事情是：

- 客户端不直接连接具体 `game-server`
- 客户端统一连接 `game-proxy`
- `game-proxy` 决定当前会话应该接到哪台 `game-server`
- 当 `game-server` 发生热更新、替换、切流时，由 `game-proxy` 负责连接层处理
- 尽量减少客户端对 `game-server` 物理节点变化的感知

第一版目标：

- 单客户端先通过 `KCP` 连 `game-proxy`
- `game-proxy` 再通过 `UDS` 连到目标 `game-server`
- 支持配置化切换上游 `game-server`
- 支持热更新过程中的新老服切流
- 支持维护期、摘流和只读接入控制

第一版不做：

- 客户端资源热更新决策
- 客户端版本强更判断
- 玩法逻辑处理
- 房间逻辑处理
- 房间状态托管
- 跨服状态迁移

## 2. 职责边界

### 2.1 auth-http 负责什么

- 登录
- access token / game ticket 发放
- 选服
- 下发客户端资源列表
- 下发客户端版本信息
- 下发客户端应该连接的 `game-proxy` 地址

因此客户端资源热更新、资源清单和版本信息仍然由 `auth-http` 负责。

### 2.2 game-server 负责什么

- 游戏协议处理
- 鉴权后的房间与对局逻辑
- 帧同步、输入处理、房间生命周期
- 游戏业务协议处理

在新的接入模型下，`game-server` 对外不再直接作为客户端接入点，而是作为 `game-proxy` 的内部上游服务，优先通过 `UDS` 暴露连接入口。

### 2.3 game-proxy 负责什么

- 作为客户端统一长连接入口
- 校验最小接入前提，例如票据是否存在、proxy 自身是否允许接入
- 建立到目标 `game-server` 的上游连接
- 双向转发客户端与 `game-server` 的数据
- 控制新连接进入新服或旧服
- 在热更新时执行摘流、切流和连接层治理
- 记录代理接入日志和切换日志

### 2.4 game-proxy 不负责什么

- 不负责客户端资源版本决策
- 不负责 manifest / CDN 地址下发
- 不负责修改 `game-server` 业务协议
- 不负责房间业务判断
- 不负责成为新的权威游戏状态节点

## 3. 推荐拓扑

```text
client
  |\
  | \-- HTTP --> auth-http
  |
  \---- KCP --> game-proxy ---- UDS --> game-server
```

推荐流程：

1. 客户端请求 `auth-http`
2. `auth-http` 完成登录、选服、资源列表和版本信息下发
3. `auth-http` 返回 `game-proxy` 地址以及进入游戏所需票据
4. 客户端连接 `game-proxy`
5. `game-proxy` 根据当前路由策略把 KCP 会话桥接到目标 `game-server` 的 UDS 上游
6. 后续客户端与 `game-server` 业务流量经 `game-proxy` 双向转发

## 4. 为什么需要 game-proxy

如果客户端直接连接 `game-server`，那么服务端热更新时会遇到这些问题：

- 客户端拿到的是具体 `game-server` 地址，切服成本高
- 服务端滚动替换时，客户端容易直接断线
- 新老服切换缺少统一入口
- 后续要做多服切流、维护摘流、分批放量时，没有一个稳定的连接层抓手

加入 `game-proxy` 后，连接治理会更清晰：

- 客户端只认 `game-proxy`
- `game-server` 可在后面替换
- 新老服切换先在 `game-proxy` 侧完成
- 客户端接入地址与真实游戏服节点解耦

## 5. game-proxy 的第一版定位

第一版建议把它定义为：

- 轻状态 KCP 接入代理层
- `game-server` 热切换网关
- 上游路由与摘流控制层

不是：

- 登录服
- 资源热更新服
- 业务网关
- 分布式状态中台

## 6. Rust 技术选型

这里明确采用 Rust 实现 `game-proxy`。

建议继续使用 `Tokio`，原因很直接：

- KCP 接入和 UDS 上游都适合 Rust 异步 IO
- 与 `game-server` 当前技术栈一致
- 更容易复用现有协议包、超时、日志和结构化事件风格
- 后续做背压、连接数控制、摘流和上游切换时更稳

因此建议新增：

- `apps/game-proxy`：Rust + Tokio

建议实现时显式拆成两层 transport：

- `KcpFrontend`
- `UdsUpstream`

## 7. 第一版核心能力

第一版建议只实现这几项：

### 7.1 统一接入

- 客户端只通过 `KCP` 连 `game-proxy`
- `game-proxy` 接到连接后决定目标 `game-server` 的 `UDS` 上游

### 7.2 上游路由

- 支持当前激活的 `game-server` 节点配置
- 支持把新连接打到新服
- 支持把旧连接留在旧服，直到旧服自然摘空

### 7.3 热切换控制

- 支持旧服 `draining`
- 支持新服 `active`
- 支持拒绝新连接进入旧服
- 支持运维手动切换当前默认上游

### 7.4 双向透明转发

- 客户端 KCP payload 到 `game-server` 的业务包不做改写
- `game-server` 回包到客户端的业务包不做改写
- `game-proxy` 主要只理解连接控制层，不理解房间协议细节

### 7.5 接入日志

- 哪个客户端接到了哪个 `game-server`
- 切换前后流量进了哪台服
- 哪些连接因摘流或维护被拒绝

## 8. 热更新场景定义

这里说的“热更新”不是客户端资源热更新，而是服务端节点更新。

建议把场景拆成两类：

### 8.1 无状态切流

适用于：

- 新服已启动
- 新连接切到新服
- 旧连接继续在旧服完成生命周期
- 旧服自然清空后下线

这是第一版最推荐支持的模式。

### 8.2 强制切换

适用于：

- 紧急维护
- 旧服必须快速摘除

这种模式意味着：

- 旧连接可能被主动断开
- 客户端需要重新通过 `auth-http -> game-proxy` 进入

第一版可以只支持“断开重连”，不做无损迁移。

## 9. 推荐连接模型

建议 `game-proxy` 保持“每个客户端 KCP 会话，对应一个上游 UDS 连接”的模型。

```text
client kcp session <-> proxy session <-> upstream game-server uds stream
```

优点：

- 实现简单
- 易于跟踪会话
- 适合第一版热切换

第一版不建议做：

- 多路复用
- 连接池复用同一个上游业务连接
- 代理层包级业务解析与重组

## 10. 与 auth-http 的关系

`auth-http` 已经负责选服和客户端资源相关信息，因此 `game-proxy` 不需要重复做一遍。

推荐交互方式：

- `auth-http` 返回 `proxyHost/proxyPort`
- `auth-http` 返回进入游戏所需 `ticket`
- 客户端携带 `ticket` 去连 `game-proxy`
- `game-proxy` 再把后续 `AUTH_REQ` 透明转发给 `game-server`

如果后续需要，也可以让 `auth-http` 把“推荐的逻辑分区 / game cluster”写进 ticket 或额外字段，再由 `game-proxy` 选择具体 `game-server`。

## 11. 与 game-server 的关系

`game-proxy` 的第一原则是对 `game-server` 保持尽量透明。

建议第一版：

- 不改现有 `game.proto`
- 不改现有 `AUTH_REQ`
- 不改现有房间消息
- 不在代理层增加业务字段注入

也就是说：

- 客户端连到 `game-proxy`
- `game-proxy` 建立到 `game-server` 的 UDS 连接
- 后续游戏协议原样透传

这样可以把改动风险控制在最小范围。

需要注意的是，这里“协议透明”不等于“传输层透明”：

- 外部接入层是 `KCP`
- 内部上游层是 `UDS`
- 透明的是业务包体和消息语义，不是 socket 类型本身

## 12. 是否需要 proxy 专属握手

第一版建议谨慎处理，不要一上来就把 `proxy` 握手做得很重。

这里有两个可选方案：

### 方案 A：纯透明代理

- 客户端连上 `game-proxy`
- `game-proxy` 立即建立到目标 `game-server` 的 UDS 上游连接
- 后续直接透传 `AUTH_REQ`

适合第一版，改动最小。

### 方案 B：增加轻量 proxy 握手

- 客户端先发一个 `ProxyConnectReq`
- 里面带 ticket、region、server_group 等接入信息
- `game-proxy` 选择上游后返回 `ProxyConnectRes`
- 再进入透明透传阶段

这个方案更利于后续扩展，但会引入新的接入协议。

基于你当前仓库进度，我建议第一版优先走方案 A，先把“服务端热切换代理”建立起来。

如果后续要增强路由能力，再增加 `proxy.proto`。

## 13. 上游路由模型

建议定义一个最小 `UpstreamRegistry`：

- 当前 `active` 的 `game-server`
- 正在 `draining` 的 `game-server`
- 可选的 `disabled` 节点

建议每个上游节点至少有这些属性：

- `server_id`
- `uds_path`
- `state`
- `weight`
- `tags`

其中 `state` 建议：

- `active`
- `draining`
- `disabled`

路由规则第一版建议很简单：

- 新连接只分到 `active`
- `draining` 不再接新连接
- 已在 `draining` 上的老连接继续保留

## 14. 维护与切流动作

建议 `game-proxy` 支持这些动作：

### 14.1 切新服为 active

- 新服启动并通过健康检查
- `game-proxy` 把它设为 `active`
- 后续新连接全部进入新服

### 14.2 旧服进入 draining

- 不再接受新连接
- 老连接继续存在
- 等房间与玩家自然退出

### 14.3 旧服强制下线

- 仍在该节点上的连接被断开
- 客户端重新走登录或重连流程

### 14.4 全局维护模式

- `game-proxy` 拒绝新的外部连接
- 老连接视策略保留或断开

## 15. 配置来源建议

第一版建议不要把上游路由写死在代码里。

建议抽象一层 `ProxyRouteStore`，底层可以先从本地配置文件读取，后续再切到 Redis 或管理面下发。

建议最小字段：

- `server_id`
- `uds_path`
- `state`
- `updated_at`
- `comment`

如果后续要做更复杂控制，再增加：

- `region`
- `group`
- `build_version`
- `drain_deadline`

## 16. 建议项目结构

建议新增：

```text
apps/game-proxy/
  src/
    main.rs
    config.rs
    transport/
      kcp_frontend.rs
      uds_upstream.rs
    proxy_server.rs
    session.rs
    upstream.rs
    route_store.rs
    drain_controller.rs
    admin_server.rs
```

职责建议：

- `transport/kcp_frontend.rs`：客户端 KCP 接入
- `transport/uds_upstream.rs`：到 `game-server` 的 UDS 上游连接
- `proxy_server.rs`：KCP <-> UDS 桥接和双向代理
- `session.rs`：代理会话模型
- `upstream.rs`：上游 `game-server` 连接建立与读写
- `route_store.rs`：当前上游路由配置
- `drain_controller.rs`：切流、摘流和维护状态控制
- `admin_server.rs`：内部管理接口

## 17. 建议状态机

第一版建议的代理会话状态：

- `Connected`
- `SelectingUpstream`
- `Proxying`
- `Draining`
- `Closed`

状态流：

1. 客户端通过 KCP 连接到 `game-proxy`
2. `game-proxy` 选择当前可用上游
3. 建立到 `game-server` 的 UDS 连接
4. 进入双向代理
5. 若上游进入 `draining`，会话继续直到自然结束
6. 若上游被强制摘除，则会话关闭

## 18. 日志与审计

建议第一版至少记录：

- 客户端连接建立
- 选中的上游 `game-server`
- 上游连接成功 / 失败
- 当前路由版本
- 节点切换动作
- 节点进入 `draining`
- 会话关闭原因

建议关键字段：

- `proxy_session_id`
- `client_addr`
- `upstream_server_id`
- `upstream_addr`
- `route_version`
- `close_reason`

## 19. 风险与约束

这条设计有几个必须先接受的边界：

- 第一版只解决连接层热切换，不解决业务状态迁移
- 正在旧服中的玩家会话，优先采用“保留到自然结束”策略
- 如果旧服必须强制下线，客户端需要重连
- `game-proxy` 不是用来隐藏所有热更新影响，而是把影响收敛到连接层

如果后续要做到真正的无感迁移，那已经不只是代理问题，而是：

- 会话恢复
- 房间状态持久化
- 跨服迁移

这会直接进入更复杂的运行时设计，不建议现在就做。

## 20. 推荐分阶段落地

### PX0：文档确认

- 确认 `auth-http / game-proxy / game-server` 职责边界
- 确认第一版走 `KCP -> UDS` 桥接代理，不处理客户端资源热更新
- 确认第一版只做新老服切流和摘流

### PX1：最小可运行代理

- `apps/game-proxy` Rust 项目骨架
- 支持客户端 KCP 接入
- 支持建立到单个 `game-server` 的 UDS 上游连接
- 支持 KCP <-> UDS 双向透明转发

### PX2：切流与摘流

- 支持 `active / draining / disabled`
- 支持新连接切到新服
- 支持旧服停止接新连接
- 支持管理命令切换默认上游

### PX3：管理与观测

- 连接数
- 上游连接数
- 每个节点当前会话数
- 切流日志
- 基础健康检查

## 21. 当前建议的下一步

建议你先确认以下几点：

1. 第一版是否接受“纯透明代理，不新增 proxy 业务握手”
2. `auth-http` 返回给客户端的是单个 `proxy` 地址，还是按区服返回不同 `proxy`
3. `game-proxy` 第一版是否只支持一个 `active` 上游
4. 旧服热更新时，是否接受“老连接保留，直到自然结束”
5. 强制更新场景下，是否接受“直接断开，客户端重连”

另外新增两条固定实现约束：

6. 客户端 <-> `game-proxy` 固定使用 `KCP`
7. `game-proxy` <-> `game-server` 固定使用 `UDS`

这些点确认后，就可以开始 `apps/game-proxy` 的 Rust 骨架开发。
