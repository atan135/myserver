# 生产拓扑与 Room 迁移设计

## 1. 文档定位

本文是 MyServer 走向生产可用和多实例部署时的正式设计总纲，重点约束公网暴露边界、服务多实例能力、客户端生产接入模型、`game-proxy` 路由持久化、room ownership、room transfer 和连接迁移路线。

相关文档：

- [整体架构](../总览/整体架构.md)
- [外部客户端接入说明](../协议与客户端/外部客户端接入说明.md)
- [game-proxy 热切换代理设计](../游戏服与接入层/game-proxy热切换代理设计.md)
- [空房接管式灰度规范](../游戏服与接入层/空房接管式灰度规范.md)
- [old/new/proxy 三进程 rollout 演练入口](./三进程灰度演练手册.md)

当前代码与配置优先于本文。本文会区分：

- `当前实现`：仓库现在已经具备或已经预留的能力。
- `生产目标`：上线形态应满足的服务边界与一致性要求。
- `后续阶段`：尚未落地，但必须提前预留边界的演进方向。

## 2. 生产公网暴露边界

当前生产玩家主链路暴露 `auth-http` 和 `game-proxy`；仓库生产配置已为公网聊天准备同一公网边缘上的专用 Caddy WSS 域名入口，但 DNS、证书和真实 WSS 链路仍须在部署环境验收。这里的“增加入口”只表示复用 Caddy `443/TCP` 的域名路由，不表示公开 `chat-server` 原始端口：

| 入口 | 协议 | 生产职责 |
|------|------|----------|
| `auth-http` | HTTP/HTTPS | 登录、session、access token、game ticket、入口服务地址下发 |
| `game-proxy` | KCP/TCP fallback 或后续网关协议 | 客户端游戏长连接入口、ticket 接入鉴权、路由到内部 `game-server` |
| `chat.game.zergzerg.cn` | WSS over `443/TCP` | Caddy 终止 TLS 并把 WebSocket 转发到 internal `chat-server:9011`；不解析聊天包或 Protobuf |

其它服务默认内网化：

| 服务 | 生产暴露策略 |
|------|--------------|
| `game-server` 玩家协议口 | 不直接暴露公网；只由 `game-proxy` 或内部通道访问 |
| `game-server admin` | 内网控制面；只允许 `auth-http`、`admin-api` 或控制面访问 |
| `game-proxy admin` | 内网控制面；已有 token 鉴权、生产默认 token 拒绝和写操作日志审计，生产仍需网络隔离、RBAC 和持久审计 |
| `admin-api` / `admin-web` | 运营控制面，需独立鉴权、网络隔离和权限收口；不属于玩家公网主入口 |
| `chat-server` | `9001/TCP` 与 `9011/WS` 均为内网原始 listener，不直接公开；正式客户端只使用 Caddy 提供的专用 WSS 边缘入口 |
| `match-service` | 内网能力服务；生产不作为客户端直连 gRPC 默认值 |
| `announce-service` | 内网能力服务；生产不作为客户端直连 HTTP 默认值 |
| `mail-service` | 内网能力服务；生产不作为客户端直连 HTTP 默认值 |
| Redis / NATS / PostgreSQL | 只允许内网服务访问 |

仅本地开发或手工调试可以临时直连 `game-server:7000`、`chat-server:9001`、`match-service:9002`、`announce-service:9004`、`mail-service:9003` 来定位协议或服务问题。测试、预发和线上的正式客户端必须使用部署提供的公网入口；内部消费者才通过 registry endpoint 或 instance id 解析目标。固定内部 host/port 不进入正式客户端依赖，公网聊天也不得绕过 Caddy 直连 `9001/9011`。

### 2.1 公网聊天 DNS、边缘和连接生命周期

- `chat.game.zergzerg.cn` 的 `A` 记录必须指向现有 Caddy 入口服务器的公网 IPv4。只有该服务器已配置可达的公网 IPv6、云安全组和主机防火墙同时允许 `80/443 TCP`、并确认 Caddy 监听 IPv6 时才发布 `AAAA`；不得发布不可达或仅内网可达的 IPv6。初次切换可使用 `300` 秒 TTL，稳定后按运维策略提高。解析目标、IPv4/IPv6 可达性和证书签发必须在实际环境查询确认，仓库静态配置不能替代该验证。
- `CADDY_CHAT_HOST` 独立站点只接受 `GET /` 的合法 WebSocket Upgrade。其它 HTTP 方法、普通 HTTP、错误路径或缺少 Upgrade 的请求固定返回 `426`，不得进入 `chat-server:9011`。
- Caddy 直接从公网接入时使用直连 peer 生成 `X-Forwarded-For`，并覆盖 `X-Request-ID`、设置 `X-Real-IP` 后才转发。`chat-server` 只在 TCP peer 属于生产 internal 子网 `172.30.0.0/24` 时解析代理头，不信任公网客户端自报地址。
- Caddy 握手入口使用 `5s` header 读取超时、`16KB` header 上限和 `2m` HTTP keep-alive 空闲超时；到上游使用 `3s` 建连、`5s` 响应头超时。升级后的聊天连接仍由 `chat-server` 的 `HEARTBEAT_TIMEOUT_SECS=30` 限制应用层空闲时间。
- Caddy 对聊天上游使用 Docker DNS 的 `dynamic a chat-server 9011` 与 `round_robin`：只在新 Upgrade 时选择实例，升级后的长连接固定到该实例，不使用跨请求 cookie。它不是跨实例聊天 push 的替代；目标玩家仍由 Redis online route 的 owner 和实例级 Core NATS subject 决定。
- 当前正式 release topology 明确固定 `chat-server` 的 `deploy.replicas: 1` 和 `SERVICE_INSTANCE_ID=chat-server-1`。`server-apply-release.sh` 在更新前拒绝多于一个既有副本，并在启动 chat-server 后要求恰好一个副本；不得使用 `docker compose --scale chat-server` 覆盖此门禁，否则多个容器会拥有相同 instance ID。
- 聊天站点访问日志删除完整 URI 及 Authorization、Cookie、Sec-WebSocket-Protocol 字段，只保留边缘生成的 request ID、固定路由分类和常规状态信息。ticket 只能位于首个 `ChatAuthReq` binary message，不能放入 URL、query、cookie、subprotocol 或握手头。
- Compose 中 Caddy 同时连接 `edge` 和 `internal`，`chat-server` 只连接 `internal`；`9001/9011` 均不映射到宿主机。云安全组和主机防火墙对公网只开放 `80/TCP`、`443/TCP`、`4000/UDP`；`22/TCP` 只允许固定运维来源，所有 TCP fallback 和 `9001/9011` 必须拒绝公网入站。
- Caddy reload、容器替换、证书维护或上游替换不承诺保持已有 WebSocket 连接。客户端必须按带随机抖动的指数退避重连，并在 ticket 失效时重新签票；这些操作只影响 Caddy `443/TCP` 的聊天连接，不改动或重启 `game-proxy` 的 `4000/UDP` KCP 链路。

## 3. 客户端生产接入模型

正式客户端位于外部 `mybevy` 仓库。本仓库不维护正式客户端代码，访问外部客户端路径时只能通过 `MYSERVER_CLIENT_ROOT` 表达，不能硬编码依赖 `C:\project\mybevy`。

生产接入模型：

1. `mybevy` 只依赖 `auth-http` 作为登录入口。
2. `mybevy` 从 `auth-http` 获取 access token、game ticket 和 `game-proxy` 地址。
3. `mybevy` 使用 ticket 连接 `game-proxy`。
4. 游戏房间、输入、快照、重连、观战、迁移通知都通过 `game-proxy -> game-server` 主链路完成。
5. 聊天使用 `auth-http` 按部署配置下发的专用 Caddy WSS 入口；邮件、公告、匹配等其他能力仍通过服务端入口收敛，或后续通过游戏协议 / BFF / 内部聚合接口暴露给客户端。

生产不采用以下默认模型：

- 客户端直连 `chat-server:9001/9011` 原始 listener 或使用 internal registry endpoint；专用 Caddy WSS 边缘入口不属于这里禁止的“原始直连”。
- 客户端直连 `mail-service`。
- 客户端直连 `announce-service`。
- 客户端直连 `match-service`。
- 客户端绕过 `game-proxy` 直连 `game-server`。

`tools/mock-client` 只用于服务端联调和回归验证，可以覆盖直连调试路径，但不能作为正式客户端边界依据。本仓库不再保留 Unity 历史 demo，不参与生产协议同步或测试准入。

## 4. 多实例定义

本文中的“多实例”指同一服务名下可同时运行多个进程实例，并且实例有稳定的 `instance_id` 或 `server_id`，可被注册中心、控制面或网关发现。

多实例能力分为四档：

| 档位 | 定义 |
|------|------|
| `单实例可运行` | 当前能启动一个实例，主要面向本地或简单部署 |
| `多实例可启动` | 能启动多个实例，但客户端入口、路由或状态一致性仍可能依赖人工配置 |
| `多实例可路由` | 有服务发现、健康状态和基础路由，调用方可选择目标实例 |
| `多实例生产可用` | 有状态归属、持久化路由、故障切换、审计、权限和明确的一致性规则 |

## 5. 服务能力矩阵

| 服务 | 当前实现 | 生产目标 | 主要缺口 |
|------|----------|----------|----------|
| `auth-http` | 单实例可运行；使用 Redis/PostgreSQL 处理 session、ticket、审计 | 多实例生产可用；HTTP 层可水平扩展，ticket/session 依赖共享 Redis/PostgreSQL | 网关层限流、统一配置、灰度和完整安全审计 |
| `game-proxy` | 多 upstream 发现和切换基础能力；route store 默认内存态，可选 Redis 持久化 rollout session、room route、player route；admin HTTP 口已有 token 鉴权和基础输入校验；`complete-if-drained` 可选经 `auth-http` 校验旧服真实 drain status 后再结束 rollout | 多实例生产可用；route store 持久化，共享 room/player route，支持 sticky 或共享路由 | 多 proxy 一致性、admin RBAC/持久审计、L7 session relay、生产部署平台 stop hook 接入 |
| `game-server` | 单实例稳定运行；已有 server id、注册中心接入、room runtime、drain 基础和受控 graceful shutdown 安全闸 | 多实例生产可用；room ownership 唯一、room route 可恢复、room transfer 可校验 | transfer payload 闭环、唯一 owner 仲裁、room route 持久化、故障恢复 |
| `chat-server` | TCP/WSS 共用会话处理器；Redis owner-fenced online route + 实例级 Core NATS 在线 push；Caddy 仅均衡新 WSS 连接，release topology 受单副本门禁保护 | 原始 listener 内网化，正式客户端经专用 Caddy WSS 入口接入；多实例生产化需受控部署、唯一 instance ID 和故障验证 | 多副本编排、唯一实例身份签发、容量压测、故障/摘流演练和监控告警闭环 |
| `match-service` | gRPC 匹配服务；可与 `game-server` 协作建房 | 内网多实例服务；匹配池状态可分片或共享，建房目标可路由 | 匹配池分片、跨实例一致性、目标 game-server 选择策略 |
| `announce-service` | 独立 HTTP 服务；接入服务注册 | 内网多实例服务；公告读写经 API/BFF 或服务端入口收敛 | 缓存一致性、权限、对客户端暴露路径收敛 |
| `mail-service` | 独立 HTTP 服务；通过 NATS 通知 `chat-server` | 内网多实例服务；邮件读写经 API/BFF 或服务端入口收敛 | 幂等投递、通知去重、客户端入口收敛 |
| `admin-api` | 后台 API 已有审计、玩家管理和部分 GM 入口 | 内网或受控公网控制面；RBAC、审计、命令闭环 | RBAC 闭环、管理口安全、GM 命令完整实现 |
| `admin-web` | 本地 Vite 前端 | 受控后台前端；通过安全入口访问 `admin-api` | 部署鉴权、网络隔离 |
| `metrics-collector` | 订阅 NATS metrics 并写 Redis 快照 | 多实例或单活均可；需要幂等聚合和明确 key 归属 | 多实例聚合策略、指标保留策略 |
| Redis | 共享协调与缓存 | 生产高可用；承载 session、ticket、注册中心、route store 或锁 | HA、持久化策略、key schema 和过期策略 |
| NATS | metrics、session kick、邮件通知 | 生产高可用；内部事件通道 | HA、重放/持久化边界、消息幂等 |
| PostgreSQL | 账号、审计、业务持久化 | 生产高可用；承载业务真持久化数据 | 备份、迁移、读写容量和事务边界 |

### 5.1 测试/线上统一启动收敛契约

测试、预发和线上环境仍应先以健康门禁保证 Redis、NATS、PostgreSQL 等基础设施可用；基础设施门禁通过后，所有应用服务必须允许单次无序批量启动。应用服务之间不得用 Compose `depends_on`、脚本顺序或 restart loop 保证正确性。

部署侧必须显式设置 `REGISTRY_ENABLED=true`、`DISCOVERY_REQUIRED=true`。应用进程启动后异步注册、发现并连接依赖：required endpoint 暂缺时保持进程存活和 not-ready，optional endpoint 暂缺时进入 degraded 并保留无关业务。发布系统统一等待全部 required readiness 和稳定窗口，而不是逐个启动并等待下一个服务。

三个 Rust 核心服务已接入共享 dependency state、动态 `/livez` / `/readyz`、异步依赖收敛和 unhealthy-first registry publication；production 默认使用 120 秒启动收敛窗口、10 秒 Ready 稳定窗口和 60 秒依赖 stale 窗口。`game-server` 还已完成 lease-first 资源事务和实例级 socket 收敛。详细控制流、接口 schema、依赖分类、状态机、稳定错误码和故障 fixture 见 [应用服务启动契约与故障基线](./应用服务启动契约与故障基线.md)。应用顺序启动只能作为本地临时兼容措施，不能作为生产正确性前提。

### 5.2 注册后接流量门禁

测试、预发和线上部署不能把“进程已启动”视为“实例可接流量”。服务进程启动后，应先完成自身 endpoint 注册，并确认 registry 中可观察到该 endpoint 和持续 heartbeat；只有后续健康检查/readiness 通过后，实例才允许进入流量目标。

接流量门禁适用于所有入口和控制面：

- 健康检查/readiness 通过前，不得把实例加入 LB、DNS、网关 upstream、admin/control target 或 rollout 目标。
- `game-server`、`match-service`、`chat-server` 等 registry-dependent services 必须先确保自身注册记录和 heartbeat 可见，再进入可被发现或可被控制面选择的状态。
- `auth-http`、`admin-api`、`mail-service`、`announce-service` 等 Node 服务当前存在启动注册失败不一定 fail-fast 的现实限制；部署健康检查必须兜底校验 registry 可见性，避免出现“进程已启动但 registry 不可见”仍被加入流量的情况。

gateway/control services 还必须先验证必要上游 endpoint 可发现，再允许自身接流量：

- `game-proxy` 依赖 `game-server.proxy-local` 可发现后，才允许加入客户端游戏入口 upstream。
- `auth-http` 依赖 `game-proxy.client` 可发现后，才允许对外提供会返回游戏入口的登录链路。
- `admin-api` 依赖 `game-server.admin` 和 `game-proxy.admin` 可发现后，才允许进入可执行控制命令的 target。

rollout 或扩容时，新实例同样必须先完成 endpoint 注册、heartbeat 可见和 readiness 校验，才能被纳入 rollout 目标；失败实例应停留在隔离状态，由部署系统回滚、重试或人工处理，不能通过本地默认 host/port 绕过 registry 直接接流量。

本节只定义部署门禁流程和边界，不定义具体健康检查接口或检查项。健康检查必须验证的具体内容由后续小节继续拆分。

### 5.3 健康检查必检项

健康检查需要区分 liveness 与 readiness。liveness 只表示进程、事件循环或主线程仍存活，不代表实例已经可以接入流量；readiness 失败必须阻止实例进入 LB、DNS、网关 upstream、admin/control target 或 rollout 目标，但不应 kill 进程。启动收敛超时由发布系统判定失败、报警或回滚，应用进程继续保持 not-ready，禁止依靠 restart loop 表达失败。

readiness 必须至少验证以下 registry 相关条件：

1. Redis registry 可访问：实例能够连接 registry Redis；使用的 key prefix 与当前环境一致；能够读取/写入必要 registry key，或至少能够读取自身注册记录和依赖发现所需 key。
2. 自己注册成功：当前进程对应的 service instance record 已存在；`endpoint name`、`protocol`、`host`、`port`、`socket`、`visibility`、`healthy` 等基本字段符合当前配置；heartbeat 未过期；`instance id` 与当前进程一致。
3. 必要依赖 endpoint 可发现：接流量前必须按自身角色验证关键上游 endpoint 能通过 registry 发现，不能回退到本地默认 host/port 绕过 registry。

必要依赖 endpoint 的最小清单：

| 服务 | readiness 必须可发现 |
|------|----------------------|
| `game-proxy` | `game-server.proxy-local` |
| `auth-http` | `game-proxy.client` |
| `admin-api` | `game-server.admin`、`game-proxy.admin` |
| `game-server` | `match-service.grpc` |
| `match-service` | 无应用级 readiness required endpoint；`game-server.internal` 是建房请求级 capability，暂缺时 degraded 但 gRPC 仍可 Ready |
| `mail-service` | `game-server.admin` |
| `metrics-collector` | 不注册也不消费 service registry；依赖 Core NATS metrics 通道和 Redis metrics snapshots，不属于 registry endpoint 检查 |

未列出的 registry 参与服务如果没有额外上游发现依赖，readiness 仍必须验证 Redis registry 可访问、自身注册记录存在和 heartbeat 未过期；`admin-web` 作为前端入口应验证 `admin-api` 入口可用，但不属于 service registry 实例。

Node 服务当前可能出现注册失败只打日志的情况，因此 readiness 必须兜底验证 registry 可见性，避免“进程已启动但自身或依赖不可发现”的实例接入流量。

当前三个 Rust 核心服务使用内网 `GET /livez` 和 `GET /readyz`：发布门禁只使用 `/readyz`，`/livez` 只用于区分进程存活与依赖未收敛。readiness 返回安全的结构化 dependency 状态，不返回连接地址、socket、URL 或凭据。production 监听端口分别为 `game-server:7600`、`game-proxy:7601`、`match-service:7603`，均不映射到宿主机公网。

### 5.4 测试/线上统一部署步骤

测试、预发和线上必须使用同一套部署状态机，不能为不同环境维护不同流程；环境之间只允许在环境变量、实例规模、域名或入口地址上存在差异。该状态机以 `REGISTRY_ENABLED=true`、`DISCOVERY_REQUIRED=true` 为准入前提，测试环境演练应完整覆盖与线上一致的注册、健康检查、接流量、下线、注销步骤，并把这些步骤作为测试/线上准入依据。

统一部署步骤如下：

1. 注册：进程按 5.1 顺序启动后，先向 Redis service registry 发布自身 endpoint，并开始维持 heartbeat。发布内容必须与当前环境配置一致，包括 service name、endpoint name、protocol、host、port、socket、visibility 和 instance id 等基础字段。
2. 健康检查：部署系统或控制面按 5.3 的 readiness 必检项确认 registry 可访问、自身注册记录存在、heartbeat 未过期，并确认必要依赖 endpoint 可发现。readiness 通过前，实例只能停留在隔离状态，不能被入口、控制面或 rollout 选择。
3. 接流量：readiness 通过后，实例才允许进入对应流量目标，包括 LB、DNS、gateway upstream、admin/control target 或 rollout target。接流量动作应以 registry 和控制面观察到的健康状态为准，不能因为进程存活就直接加入目标。
4. 下线：实例退出服务前，先从接新流量路径移除，或进入 drain 状态；随后等待现有连接、房间、后台任务、异步通知或控制面操作达到安全收敛。该步骤只定义下线状态边界，滚动发布中的旧实例 drain 和 route store 清理细节由后续滚动发布流程单独定义。
5. 注销：安全收敛后停止 heartbeat，执行 deregister，并确认 registry 中该实例不再可被发现。注销完成前，部署系统不得把同一实例视为已完全退出。

`game-server` 线上发布还必须为每个并存实例分配不同的 `SERVICE_INSTANCE_ID` 和 `GLOBAL_ID_WORKER_ID`。实例据此派生独立的 `proxy-local` / `internal` socket 并精确发布路径；`game-proxy` 在严格发现环境只消费 registry 中 healthy 的 `proxy-local.socket`，不得回退到固定路径。production compose 的 `GAME_SERVER_INSTANCE_ID`、`GAME_SERVER_WORKER_ID` 只是注入入口，发布平台必须保证值在同一环境内唯一。

本地 dev-stack 可以继续保留简化启动顺序和非严格发现下的 local fallback，用于单机开发和快速联调；但 dev-stack 的简化流程不能作为测试、预发或线上准入依据。任何测试/预发/线上演练都必须覆盖上述完整状态机，避免出现本地默认 host/port、静态 upstream 或手工绕过 registry 的部署路径。

本节只定义统一部署流程和状态边界，不实现脚本、健康检查接口、LB/DNS/gateway 更新接口或服务启动逻辑。

### 5.5 滚动发布下线流程

滚动发布、缩容或实例替换时，旧实例的正常退出必须走显式下线流程，不能只依赖进程退出或 registry TTL 过期。下线流程至少包含以下阶段：

1. 移出接新流量：对旧 `game-server` 开启 drain 后，registry publication 转为 unhealthy，proxy 后续发现不再把旧 socket 作为健康默认 route；玩家 TCP 和 `proxy-local` listener 仅为已有 room 的离线成员保留鉴权和 reconnect transport，`AuthReq` 拒绝不属于本实例离线 room 的新角色并返回 `SERVER_DRAINING_REJECT_NEW_SESSION`，业务层同时拒绝创建新 room、匹配建房等新分配。已有连接任务、房间以及 admin/internal 控制通道继续运行。其他服务同样应先从 LB、DNS、gateway upstream、admin/control target 或 rollout target 移除，禁止新房间、新匹配分配或新的普通流量继续选中旧实例。
2. 等待业务收敛：等待旧实例现有连接、房间、匹配分配、邮件附件发放、异步通知和控制面操作达到安全收敛。`game-server` 必须先进入 drain，再由已鉴权 `RequestServerShutdownReq` 显式武装退出；控制面可在 blocker 清零前发出请求进入有界等待，不需要用轮询竞态抢占“刚好归零”的瞬间。
3. 安全停服并统一清理：`RequestServerShutdownRes` 使用三态：`ok=true, shutdown_armed=true` 表示无 blocker 并立即 graceful shutdown；`ok=false, shutdown_armed=true` 表示连接或 room blocker 仍在但已接受默认 `300s` 有界等待；`ok=false, shutdown_armed=false` 表示拒绝或未能武装。等待超时不会强杀现有会话或 room，而会解除武装，控制面可在状态继续收敛后重试；重复请求不会重置当前期限。退出后统一 cleanup 释放本实例 listener/socket、deregister、compare-and-delete 本实例 lease 并关闭 stores。异常退出时 heartbeat TTL 可以作为最终摘除兜底，但 TTL 不能作为正常滚动发布下线的主路径。
4. 清理或降级 route store：移除或标记旧实例对应的 upstream、room route、player route 和 rollout target，确保新请求不会继续被导向旧实例。若 route store 清理失败，必须降级为旧实例不可接新流量、不可选，并保留故障状态供重试或人工处理，不能因为清理失败而重新放开旧实例。
5. 完成验证：确认旧实例在 registry 中不可发现，route store 不再把新连接、新建房、重连、匹配或控制面请求导向旧实例，并保留 drain、deregister、route store 更新或降级结果的日志和审计记录。

`game-server` 的正式写入口由 admin-api 提供：`POST /api/v1/rollouts/game-server/:instanceId/drain` 和 `POST /api/v1/rollouts/game-server/:instanceId/shutdown`。drain 要求 `game.config.write`，并由控制面将权限范围绑定为影响全部 world 的 `worldId=*`、`serviceName=game-server` 和显式 instance；shutdown 使用 emergency-only 的 `service.shutdown`，范围绑定为 `serviceName=game-server` 和显式 instance，不伪造 world 范围。两者都要求 Bearer/JWT、high-risk preflight/execute 二阶段确认、独立审批、签名断言和审计；shutdown 执行阶段还要求匹配目标的有效 break-glass grant。`:instanceId` 必须显式给出，admin-api 只从 Redis registry 的健康 `game-server.admin` endpoint 精确解析目标，不接受固定内部地址或 direct endpoint override。`auth-http` 对应历史写接口继续返回 `410 CONTROL_PLANE_ONLY`，只读状态接口不受影响。审计记录身份、目标、原因和结果，不记录 token、断言签名或内部 endpoint 凭据。

本节定义滚动发布和扩缩容时的旧实例退出状态机及当前 admin-api 控制入口；部署平台 stop hook 的调用编排仍由发布流程实现。

### 5.6 stop hook 目标实例发现

生产部署平台的 stop hook、preStop 或 shutdown hook 负责把“要停哪个实例”解析成可调用的控制面 endpoint。hook 的输入必须是稳定身份，例如环境名、service name、target instance id / server id、rollout epoch 或控制面 owner；不得把 host、port、AuthBaseUrl、ProxyAdminUrl、old/new admin host/port 作为测试、预发或线上停服路径的主要输入。

hook 必须使用与 rollout drill 和控制面相同的 service registry discovery 解析目标 endpoint，例如 `game-server.admin`、`game-proxy.admin`、`auth-http.internal`。`scripts/ops/rollout-three-process-drill.ps1` 当前会通过 registry discovery 解析 old/new `game-server.admin`、`game-proxy.admin` 和 `auth-http.internal`；只有 registry disabled 或 discovery non-required 的 local/manual drill 才允许固定目标 fallback。生产 hook 应复用同一发现边界，而不是重新维护一套固定地址表。严格发现环境下，registry 不可用、目标 endpoint 不存在、heartbeat 过期、instance id 不匹配或解析结果不唯一时，hook 必须失败并阻止继续停服，等待重试或人工处理。

多实例场景下，hook 必须显式指定目标实例或控制面 owner，不能从发现结果中随机挑选一个 endpoint。任何会改变实例状态的写操作，包括 drain、shutdown、deregister、route store 清理或 rollout complete，都必须携带 `targetInstanceId`、server id、rollout epoch 或等价选择依据，并在被调用服务侧校验该身份与当前实例和当前 rollout 会话一致；只给 service name 而不指定实例的写操作不允许进入测试、预发或线上流程。

固定端口、`AuthBaseUrl`、`ProxyAdminUrl`、old/new admin host/port 只能作为本地开发、manual fallback 或故障排查时的显式临时参数使用。使用这些 fallback 时必须在命令、日志或审计中标明 source 不是 registry；测试、预发和线上 stop hook 禁止依赖固定端口或静态 URL 跑通停服流程。

hook 每次执行后都必须写入可回放日志和审计记录，至少包含环境名、service name、target instance id / server id、rollout epoch、解析出的 endpoint、`source=registry`、执行原因、调用结果和失败原因。发生发现失败、身份不匹配或多 endpoint 歧义时，也必须记录 reason，便于复盘停服是否被正确阻断。

### 5.7 异常退出与 heartbeat TTL 摘除

当前 service registry 使用独立 heartbeat key 判断实例是否仍可发现。Rust `service-registry` client 默认 heartbeat interval 为 10 秒、TTL 为 30 秒；Node registry client 也默认每 10 秒对 heartbeat key 执行 `setex`，TTL 为 30 秒。实例异常退出后，进程不再刷新 heartbeat key，Redis 会在 TTL 过期后自动删除该 key。

registry discovery 查询实例时会过滤 heartbeat key 不存在的实例。因此异常退出后，实例记录即使仍残留在 registry instance key 中，也应在 heartbeat TTL 过期后不再被新的 discovery 结果返回，入口、控制面或依赖服务的新选择不应再选中该实例。

TTL 摘除只作为异常退出兜底，不是正常下线流程。滚动发布、缩容、维护停服和受控重启仍必须先移出接新流量、等待业务收敛，然后显式停止 heartbeat 并执行 deregister；不能把等待 TTL 过期当作正常注销路径。

部署和监控需要覆盖异常退出演练，并在至少一个 TTL 窗口内校验以下结果：

1. 目标实例的 service registry heartbeat key 已过期或不存在。
2. registry discovery 对应 service / endpoint 不再返回该 instance id。
3. 入口、控制面和依赖服务的新请求不再路由或选择到该实例。

已有连接、旧绑定、room route、player route、rollout target 或其他 route store 状态不由 registry TTL 自动清理。异常退出后的旧连接断开、重连降级、房间迁移、route store 清理或故障标记，仍必须依赖各服务自身的降级、探活、清理和控制面策略；不能只靠 registry TTL 保证全链路摘除。

`metrics-collector` 不参与 service registry heartbeat，也不注册为 service registry 实例。它写入的 v2 latest `_reported_at` 只表示指标快照的新鲜度；legacy 兼容窗口可能写入的 `metrics:heartbeat:*` 同样不能作为服务发现实例是否存活、是否可被 discovery 返回或是否可接流量的依据。

### 5.8 Redis key prefix 环境隔离

当前 service registry 写入 Redis 的 key 形态为 `<prefix>service:<service>:instances:<instance>` 和 `<prefix>heartbeat:<service>:<instance>`。Rust `service-registry` client 与 Node registry client 都使用 `REGISTRY_KEY_PREFIX` 作为 service registry key prefix；未设置时会 fallback 到 `REDIS_KEY_PREFIX`，再未设置则使用空字符串。

测试、预发和线上如果共享同一个 Redis，必须显式设置不同的 `REGISTRY_KEY_PREFIX`，例如 `test:`、`staging:`、`prod:`，或包含 cluster / namespace 的 prefix。测试和线上不能共享相同 registry key prefix，也不能一边使用空 prefix 一边复用同一个 Redis；否则不同环境会互相发现实例、误判 heartbeat，并可能把 stop hook、rollout 或 control plane 请求误路由到错误环境的实例。

生产模板当前的空 prefix 只适合 Redis 独占部署。只要测试、预发、线上或多个集群复用同一个 Redis，部署层就必须覆盖 `REGISTRY_KEY_PREFIX`，并把该 prefix 纳入 readiness、rollout drill 和故障排查输出，确保控制面与业务服务观察到的是同一环境的 registry。

`REDIS_KEY_PREFIX` 是业务 Redis key 隔离前缀，可以与 `REGISTRY_KEY_PREFIX` 相同，也可以分开管理。如果两者分开，部署和运维文档需要同时说明 route store、session、ticket、metrics snapshot 等业务 key 的隔离策略，避免 service registry 已隔离但业务状态仍跨环境混用。

`metrics-collector` 当前常态写入 `metrics:v2:*`；只有显式 legacy 兼容窗口才写 `metrics:<service>:<instance>:<bucket>` / `metrics:heartbeat:*`。这些都不是 service registry prefix 使用者。共享 Redis 时，metrics key 是否需要按环境隔离应单独评估和配置，不能把 `REGISTRY_KEY_PREFIX` 视为 metrics snapshot 的自动隔离手段。

## 6. game-proxy 单实例与多实例边界

### 6.1 当前单实例边界

当前 `game-proxy` 可以作为单一公网游戏入口，选择一个或多个内部 `game-server` upstream。它已经具备 room route、player route 和 rollout session 元数据；默认仍是进程内内存态，启用 `PROXY_ROUTE_STORE_BACKEND=redis` 后可在 Redis 中保存带 `store_revision` 的 route store 快照，并在 proxy 重启后恢复。

单 proxy 生产化前至少需要：

- admin 接口认证、权限和审计。
- 生产启用 Redis route store 持久化，或接入统一控制面。
- route 更新的 CAS 校验；Redis backend 当前已具备单 key 快照级 Lua CAS。
- 上游健康状态与运维状态分离。
- 重启后恢复 `rollout_epoch`、room route、player route；当前 Redis backend 已覆盖这三类数据的单 proxy 最小闭环。

当前 Redis route store 的边界：

- 保存内容是 rollout session、room route、player route 的 serde JSON 快照。
- 快照包含 `store_revision`；旧快照缺字段时按 revision `0` 兼容加载。Redis 写入使用 Lua compare-and-set，只有 expected revision 命中时才写入并递增 revision。
- 配置为 `PROXY_ROUTE_STORE_BACKEND=redis` 时，启动加载失败会让 proxy 启动失败，避免静默丢失生产路由状态。
- Redis URL 优先使用 `PROXY_ROUTE_STORE_REDIS_URL`，未设置时依次复用 `REGISTRY_URL`、`REDIS_URL`；key prefix 优先使用 `PROXY_ROUTE_STORE_KEY_PREFIX`，未设置时复用 `REDIS_KEY_PREFIX`。
- 它解决单 proxy 重启丢 route 的最低风险，并降低多 proxy 最后写覆盖风险；但不保存 upstream health/operation state，也不代表多 proxy 并发写入已经强一致。冲突时 admin 写入会返回错误，玩家 join/reconnect/observer 触发的绑定元数据更新只告警并重新加载最新快照。

### 6.2 多 proxy 目标边界

未来允许多个 `game-proxy` 同时作为公网游戏入口时，必须满足二选一或组合策略：

| 策略 | 要求 | 适用边界 |
|------|------|----------|
| sticky proxy | 负载均衡层保证同一玩家或同一连接尽量回到同一 proxy | 降低共享状态读取压力，但不能替代持久化 route store |
| shared route store | 所有 proxy 读取同一份 room/player route | 推荐生产目标，支持 proxy 重启、扩容和故障切换 |
| control plane owner | 控制面统一仲裁 route 更新，proxy 只缓存只读副本 | 适合更强一致性的发布和迁移流程 |

即使使用 sticky，也不能把 proxy 内存视为权威状态。room route、player route、rollout session 必须能从 Redis、数据库或控制面恢复。

当前 Redis route store 可以作为 shared route store 的起点，已经具备单 key 快照级 revision/CAS，能避免无条件最后写覆盖。但多 proxy 生产可用还需要补齐 pub/sub 本地缓存失效、统一控制面 owner、真实 Redis 集成压测，以及必要时更细粒度的锁或冲突合并。否则多个 proxy 同时写不同 route 时仍可能因为整快照 CAS 冲突而需要重试，本地缓存也可能短暂不一致。

多 proxy 场景下，route store 至少要支持：

- `room_id -> owner_server_id`
- `player_id -> current_room_id / preferred_server_id`
- `rollout_epoch`
- `room_version`
- `migration_state`
- `last_transfer_checksum`
- CAS 式更新
- 过期、清理和审计记录

## 7. Room Ownership 与路由版本

生产目标要求任意时刻一个 `room_id` 只能有一个权威 owner。

核心规则：

1. `room owner` 是当前对某个 `room_id` 负责的唯一 `game-server`。
2. `room route` 是外部接入层和控制面识别 owner 的路由记录。
3. `room_version` 每次 owner 切换或关键迁移状态推进时必须单调递增。
4. `rollout_epoch` 标识一次灰度或迁移会话，route 更新必须匹配当前 epoch。
5. `last_transfer_checksum` 绑定最近一次成功导入的 transfer payload。
6. 迁移状态进入 `OwnedByNew` 前，必须先完成 freeze/export/import 校验；对外切 route 前还必须完成新服 ownership confirm。
7. route 更新必须使用 CAS，避免旧控制命令覆盖新 owner。

推荐 route 结构：

```text
RoomRouteRecord {
  room_id,
  owner_server_id,
  migration_state,
  member_count,
  online_member_count,
  empty_since_ms,
  room_version,
  rollout_epoch,
  last_transfer_checksum,
  updated_at_ms,
}
```

唯一 owner 规则：

- 不允许新旧两个 `game-server` 同时对外接受同一 `room_id` 的玩家输入。
- 导入成功但 route 切换失败时，默认仍以旧 owner 为权威，或进入明确的人工处理状态。
- route 切到新 owner 后，旧 owner 必须进入 retired 或 tombstone 状态，拒绝继续处理该 room 的新输入。
- `game-server` 收到不属于自己的 room 请求时，必须返回明确错误，不能本地悄悄创建同名 room。

## 8. Room Transfer Payload 原则

`RoomTransferPayload` 是恢复同一 room 运行态的权威迁移数据，不是客户端展示 snapshot。

设计原则：

- 玩法状态必须可序列化。
- payload schema 必须版本化。
- payload 必须可校验，至少包含 checksum。
- 导出前 room 必须冻结，停止 tick、输入、AI、定时器和随机事件推进。
- 导入后必须能恢复同一 `room_id`、同一关键帧号、同一玩法进度。
- 不支持完整 payload 的玩法，不允许宣称支持 room transfer。
- 连接态不能混入玩法态。

连接态与玩法态必须分离：

| 类型 | 示例 | 是否进入 transfer payload |
|------|------|--------------------------|
| 玩法态 | room phase、frame id、实体、背包、战斗、冷却、buff、AI 黑板、定时器、RNG 状态 | 是 |
| 协议恢复辅助 | recent inputs、waiting inputs、last applied frame | 是，需去重和排序 |
| 连接态 | socket、KCP conv、proxy session、上游 stream、临时发送缓冲、连接 RTT | 否 |
| 鉴权态 | ticket 原文、access token、TLS/KCP 会话密钥 | 通常否；迁移时应通过 resume 或重新鉴权验证 |

payload 最小建议字段见 [空房接管式灰度规范](../游戏服与接入层/空房接管式灰度规范.md)。本文额外要求 payload 包含 schema/version 信息，便于跨版本导入时做兼容判断。

当前实现状态（截至 `2026-06-15`）：`game-server` 已完成已鉴权 internal/admin 通道内的 room freeze/export/import/confirm/retire 最小闭环，适用于空房或全员离线 room 的基础 transfer 验证；`ConfirmRoomOwnershipReq/Res` 会在新服校验 room 存在、`OwnedByNew` 状态、`rollout_epoch`、checksum 和 `room_version` 后才返回成功。同时已提供 `TriggerServerRedirectReq/Res` 控制入口，可向旧服上目标 room 的当前在线成员下发 `ServerRedirectPush`。push 成功进入出站队列后，旧服会以 `server_redirect_reconnect_required` 主动请求关闭旧连接；push 排队失败的连接计入失败数，不额外覆盖关闭原因。`GetRolloutDrainStatusReq/Res` 会返回旧服真实 `drain_mode_enabled`、`drain_mode_entered_at_ms`、`drain_mode_reason`、`drain_mode_source`、连接数、仍持有 room、迁移中 room、已 retired tombstone room、route 样本和可接管空房分类；可接管空房仅包含仍为 `Owned` / 对外视作 `OwnedByOld` 且在线成员数为 `0` 的 room，已 `Retired` room 单独计入 `retired_room_count`，不作为旧服排空阻塞项。该状态供 `auth-http` 内部接口、`tools/mock-client` 查询，也可被 `game-proxy` 的 `complete-if-drained` 在配置启用时作为结束 rollout 前的真实排空校验。`RequestServerShutdownReq/Res` 已提供显式武装的受控 graceful shutdown 请求入口：无 blocker 时立即触发，仍有连接或 room blocker 时接受默认 `300s` 有界等待；`ok` 与 `shutdown_armed` 共同表达立即执行、已接受等待和拒绝三态。超时不会强杀会话或 room，解除武装后可由后续显式请求重试；`retired_room_count` 只作为观测字段，不阻塞停服。`tools/mock-client` 已具备收到 push 后主动断线、连接目标入口、重新 `AuthReq` 并优先 `RoomReconnectReq` 的验证场景，也可通过 `request-server-shutdown` 场景人工调用停服安全闸。2026-06-13 已在真实 old/new/proxy/auth 环境中人工跑通 `movement_demo` 空房迁移控制面，并用 mock-client 验证 redirect -> transfer -> proxy reconnect；尚未完成自动测试准入、mybevy 适配、L7 relay、同连接 upstream swap、真实 route metadata 丢失恢复或生产部署平台 stop hook 接入，也不代表 movement/combat/NPC/AI/timer 等完整玩法状态已经可无损迁移。

启动资源边界补充：`game-server` 必须在持有 global-ID worker lease 后才绑定玩家/admin listener 和两个 local socket。启动失败、SIGTERM/Ctrl-C、内部受控 shutdown、lease lost 或关键 listener task 失败统一停止任务、释放 listener/socket、注销自身 registry identity，并以 token compare-and-delete 释放自身 lease。固定 socket 名仍不支持新旧实例重叠；实例级 socket 与独立 lease/identity 的滚动替换由后续阶段完成。

补充实现状态（截至 `2026-06-15`）：`tools/mock-client` 已增加第一阶段显式编排入口，按 old `freeze/export`、new `import`、new `confirm ownership`、proxy room route `upsert`、old `retire` 的保守顺序调用现有控制面。编排会校验 export/import/confirm checksum 和 roomVersion 一致，并在 confirm 成功后才 upsert proxy route，在 proxy upsert 成功后才 retire 旧 room；任一步失败都会返回失败阶段并停止后续步骤。`scripts/ops/rollout-three-process-drill.ps1` 已在其外层补充 old/new/proxy 演练入口，默认 dry-run，只做 preflight、端口探测和步骤命令输出；显式 `-ExecuteSteps` 才调用 rollout start、old drain、transfer、drain status、complete-if-drained 等控制面步骤，旧服 shutdown 请求还需要额外 `-AllowShutdownRequest`。脚本也可读取旧服 PID 或 PID 文件，并在 shutdown 安全闸 `ok=true` 后等待该旧进程退出、写入 `old-process-stop` 报告阶段。该入口仍是可重复演练脚本；真实服务人工验收已完成一轮空房迁移控制面，但还不是自动测试准入，也不包含同连接迁移、真实故障恢复或生产部署平台 stop hook 接入。

## 9. 两阶段迁移路线

### 9.1 阶段一：redirect/reconnect 闭环

第一阶段采用显式重连，目标是先把生产可控的 room route 和 owner 切换跑通。

时序：

```text
old game-server -> ServerRedirectPush -> client
old game-server closes session
client reconnects game-proxy
client sends AuthReq
client sends RoomReconnectReq or RoomJoinReq
game-proxy reads room/player route
game-proxy binds new upstream
new game-server resumes room session
```

阶段一要求：

- `ServerRedirectPush` 能明确携带 `room_id`、`rollout_epoch`、原因和重连要求。
- `ServerRedirectPush` 需要携带目标 proxy 的 `target_host`、`target_port`、`target_server_id` 和 `transport`。
- 客户端断线后重新连接 `game-proxy`。
- proxy 根据持久化或当前 route 将玩家送到正确 owner。
- 旧连接不会继续留在错误 owner 上。
- transfer 流程先覆盖空房接管或低风险玩法。
- 控制面必须按 `old freeze -> old export -> new import -> new confirm ownership -> proxy route CAS upsert -> old retire` 顺序执行；导入/confirm checksum 不一致、roomVersion 不一致、route CAS 失败或任一步失败时不能继续执行后续破坏性步骤。

阶段一不要求：

- 同一连接内换 upstream。
- proxy 深度理解玩法协议。
- 在线有人 room 无感迁移。

当前客户端要求部分闭环：`tools/mock-client` 已能认证进房后监听 `ServerRedirectPush` 并输出结构化结果，也能在 `server-redirect-reconnect` 场景中收到 push 后主动断线、重连到 push 指定入口、重新发送 `AuthReq`，再优先发送 `RoomReconnectReq`，必要时按显式参数 fallback 到 `RoomJoinReq`。旧服已在 redirect push 成功排队后主动关闭旧连接，避免旧连接继续留在错误 owner。mock-client 已在真实 old/new/proxy/auth 环境中验证 redirect -> transfer -> proxy reconnect；外部 `mybevy` 和真实测试客户端仍需要实现同等能力，自动化准入也尚未完成。

### 9.2 阶段二：同连接迁移目标态

第二阶段目标是在客户端连接不变的情况下切换 upstream。该能力尚未落地，必须先完成 proxy 架构升级。

目标模型：

```text
client session
  <-> game-proxy L7 session relay
      <-> old game-server upstream
      <-> new game-server upstream
```

`game-proxy` 需要从透明 `copy_bidirectional` 演进为 L7 session relay：

1. proxy 识别已认证玩家、当前 room、frame/input 序列和 upstream 绑定。
2. proxy 接收控制面迁移命令，暂停向 old upstream 转发新输入。
3. proxy 通知 old upstream 冻结 room。
4. old upstream freeze/export，new upstream import/confirm。
5. route store CAS 切换 owner。
6. proxy 保持 client 连接不变，切换内部 upstream。
7. proxy 对 new upstream 重放 `AuthReq` 和 `RoomReconnectReq`，或使用后续定义的 `ResumeSessionReq`。
8. proxy 将迁移期间客户端输入缓冲，按序排序、去重后交给 new upstream。
9. new upstream 从确认帧继续处理。
10. 失败时 proxy 回滚到 old upstream，释放冻结或按错误策略断开并要求客户端显式重连。

同连接迁移必须具备：

- 暂停 old upstream 的输入转发能力。
- 冻结 room 的控制协议。
- 可校验 export/import。
- 客户端连接不变但服务端 session 可重新绑定。
- Auth/RoomReconnect replay 或 resume 协议。
- 输入缓冲、排序、去重、超时和容量限制。
- 迁移过程中的 push 消息暂停或重放策略。
- 失败回滚策略。
- 完整审计和指标。

阶段二不能建立在透明字节流代理上。只要 proxy 仍主要依赖 `copy_bidirectional`，就只能把同连接迁移视为目标态，而不是当前能力。

## 10. 默认房间策略建议

默认房间策略应按玩法类型配置，最终以 `room policy` 配置为准，不能把下表写死在协议或 proxy 逻辑中。

| 玩法类型 | max players | tick | input rate | snapshot rate | 说明 |
|----------|-------------|------|------------|---------------|------|
| `default_match` | 2-8 | 10-20 Hz | 10-20/s | 2-5/s | 小局对战，优先保证输入顺序和断线恢复 |
| `disposable_match` | 2-8 | 10-20 Hz | 10-20/s | 2-5/s | 生命周期短，适合先验证 redirect/reconnect |
| `movement_demo` | 1-16 | 20 Hz | 20/s | 5-10/s | 移动同步验证，关注纠正和快照连续性 |
| `combat_demo` | 1-16 | 10-20 Hz | 10-20/s | 2-5/s | 需要补齐战斗、冷却、buff 和 RNG transfer |
| `persistent_world` | 20+ 或分片配置 | 10-20 Hz | 10-20/s | 1-5/s | 常驻 room，迁移前必须有完整状态分片和 transfer 设计 |
| `sandbox` | 1-8 | 5-10 Hz | 5-10/s | 1-2/s | 调试玩法，配置可更宽松 |

生产实现应把这些参数放入 `RoomRuntimePolicy` 或外部配置：

- `max_players`
- `tick_rate`
- `input_rate_limit`
- `snapshot_rate`
- `reconnect_timeout`
- `supports_transfer`
- `transfer_schema_version`
- `migration_mode`

## 11. 验收标准

### 11.1 生产边界验收

- 生产网络策略公开 `auth-http`、`game-proxy`，以及目标态 Caddy `443/TCP` 上的专用聊天 WSS 域名路由；不为聊天增加原始端口暴露。
- `game-server` 玩家协议口不能被公网直连。
- `chat-server:9001/9011` 原始 listener、internal registry endpoint、`match-service`、`announce-service` 和 `mail-service` 不作为生产客户端直连默认入口；聊天客户端只接 Caddy WSS 边缘入口。
- admin 和内部端口有网络隔离、鉴权、权限和审计方案。

### 11.2 多实例验收

- 每个服务实例有稳定 `instance_id` 或 `server_id`。
- `game-proxy` 可从注册中心发现多个 `game-server`。
- route store 重启后可恢复；当前 Redis backend 覆盖单 proxy 重启恢复。
- 多 proxy 场景下，同一 `room_id` 的 owner 判断一致；当前已有单 key CAS，但仍需 pub/sub 缓存失效、锁/同步策略或控制面 owner 才能验收。
- route 更新有 CAS 和审计记录。

### 11.3 Room Ownership 验收

- 同一 `room_id` 任意时刻只有一个 owner。
- route version 单调递增。
- rollout epoch 不匹配时拒绝更新。
- checksum 缺失或不匹配时拒绝进入 `OwnedByNew`。
- proxy route 切换前必须先通过新 owner 的 ownership confirm。
- 旧 owner retire 后拒绝处理新输入。

### 11.4 Redirect/Reconnect 验收

- 旧服能下发 `ServerRedirectPush`。
- 客户端能断线后重新连接 `game-proxy`。
- proxy 根据 room/player route 进入正确 owner。
- 重连后 `RoomReconnectReq` 或 `RoomJoinReq` 能恢复目标 room。
- 错误 owner 会返回明确错误，不能创建同名 room。

### 11.5 同连接迁移目标态验收

- proxy 不再依赖纯透明 `copy_bidirectional` 完成迁移。
- proxy 能暂停 old upstream、冻结 room 并缓冲输入。
- export/import checksum 可校验。
- new upstream 能通过 replay auth/reconnect 或 resume 接管会话。
- 输入按序、去重后继续处理。
- 迁移失败可回滚或明确断开并要求客户端重连。

## 12. 后续实现拆分

建议按以下顺序推进：

1. 生产网络边界：部署文档和配置只暴露 `auth-http`、`game-proxy` 玩家入口。
2. route store 持久化：已具备 Redis backend 最小闭环，生产启用 `PROXY_ROUTE_STORE_BACKEND=redis`。
3. 多 proxy 一致性：在 Redis backend 单 key CAS 上补 pub/sub 缓存失效、控制面 owner、真实 Redis 压测和必要的锁/同步策略，或明确 sticky/shared route store/control plane owner 策略。
4. 旧服真实状态联动：`game-proxy` 已可选在 `complete-if-drained` 中校验旧服真实 drain status，game-server 已有受控 graceful shutdown 安全闸，演练脚本已具备指定 PID 退出验证；后续补控制面 owner、展示/告警深化和生产部署平台 stop hook 接入编排。
5. 演练入口固化：`scripts/ops/rollout-three-process-drill.ps1` 已提供第一阶段 dry-run/step-runner，并已完成一轮真实 old/new/proxy/auth 空房迁移人工验收；后续需要沉淀为自动测试准入。
6. redirect/reconnect 闭环：补齐外部 `mybevy` 客户端重连、proxy 重新路由和错误 owner 处理验收。
7. room transfer 最小闭环：已具备 freeze/export/import/confirm/retire 控制流，并已用 `movement_demo` 做真实三进程空房迁移验收；后续补真实故障恢复、更多 room policy 和自动化准入。
8. transfer payload trait：按玩法补齐可序列化、版本化、checksum 和兼容性检查。
9. owner 仲裁与审计：补 CAS、route version、rollout epoch、owner tombstone 和迁移审计。
10. 客户端能力收敛：chat/mail/announce/match 经服务端入口或 BFF 收敛，不再要求生产客户端直连内部服务。
11. L7 session relay：设计并实现同连接 upstream swap 所需的 proxy 协议解析、输入缓冲和 resume。
12. 故障演练：覆盖 proxy 重启、game-server 崩溃、导入失败、route 切换失败和客户端中断。
