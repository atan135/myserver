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

生产默认只允许公网暴露两个入口：

| 入口 | 协议 | 生产职责 |
|------|------|----------|
| `auth-http` | HTTP/HTTPS | 登录、session、access token、game ticket、入口服务地址下发 |
| `game-proxy` | KCP/TCP fallback 或后续网关协议 | 客户端游戏长连接入口、ticket 接入鉴权、路由到内部 `game-server` |

其它服务默认内网化：

| 服务 | 生产暴露策略 |
|------|--------------|
| `game-server` 玩家协议口 | 不直接暴露公网；只由 `game-proxy` 或内部通道访问 |
| `game-server admin` | 内网控制面；只允许 `auth-http`、`admin-api` 或控制面访问 |
| `game-proxy admin` | 内网控制面；已有 token 鉴权、生产默认 token 拒绝和写操作日志审计，生产仍需网络隔离、RBAC 和持久审计 |
| `admin-api` / `admin-web` | 运营控制面，需独立鉴权、网络隔离和权限收口；不属于玩家公网主入口 |
| `chat-server` | 内网能力服务；生产不作为客户端直连接口默认值 |
| `match-service` | 内网能力服务；生产不作为客户端直连 gRPC 默认值 |
| `announce-service` | 内网能力服务；生产不作为客户端直连 HTTP 默认值 |
| `mail-service` | 内网能力服务；生产不作为客户端直连 HTTP 默认值 |
| Redis / NATS / PostgreSQL | 只允许内网服务访问 |

本地开发、测试环境可以临时直连 `game-server:7000`、`chat-server:9001`、`match-service:9002`、`announce-service:9004`、`mail-service:9003` 来定位协议或服务问题，但这些直连方式不是生产默认，也不应写入正式客户端依赖。

## 3. 客户端生产接入模型

正式客户端位于外部 `mybevy` 仓库。本仓库不维护正式客户端代码，访问外部客户端路径时只能通过 `MYSERVER_CLIENT_ROOT` 表达，不能硬编码依赖 `C:\project\mybevy`。

生产接入模型：

1. `mybevy` 只依赖 `auth-http` 作为登录入口。
2. `mybevy` 从 `auth-http` 获取 access token、game ticket 和 `game-proxy` 地址。
3. `mybevy` 使用 ticket 连接 `game-proxy`。
4. 游戏房间、输入、快照、重连、观战、迁移通知都通过 `game-proxy -> game-server` 主链路完成。
5. 聊天、邮件、公告、匹配等能力通过服务端入口收敛，或后续通过游戏协议 / BFF / 内部聚合接口暴露给客户端。

生产不采用以下默认模型：

- 客户端直连 `chat-server`。
- 客户端直连 `mail-service`。
- 客户端直连 `announce-service`。
- 客户端直连 `match-service`。
- 客户端绕过 `game-proxy` 直连 `game-server`。

`tools/mock-client` 只用于服务端联调和回归验证，可以覆盖直连调试路径，但不能作为正式客户端边界依据。`apps/simple-client` 是已废弃的 Unity 历史 demo，不参与生产协议同步或测试准入。

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
| `chat-server` | 独立服务；当前可作为内部聊天能力 | 内网多实例服务；由服务端入口或聚合层调用，不作为生产客户端直连默认 | 协议收敛、会话路由、服务发现和横向扩展策略 |
| `match-service` | gRPC 匹配服务；可与 `game-server` 协作建房 | 内网多实例服务；匹配池状态可分片或共享，建房目标可路由 | 匹配池分片、跨实例一致性、目标 game-server 选择策略 |
| `announce-service` | 独立 HTTP 服务；接入服务注册 | 内网多实例服务；公告读写经 API/BFF 或服务端入口收敛 | 缓存一致性、权限、对客户端暴露路径收敛 |
| `mail-service` | 独立 HTTP 服务；通过 NATS 通知 `chat-server` | 内网多实例服务；邮件读写经 API/BFF 或服务端入口收敛 | 幂等投递、通知去重、客户端入口收敛 |
| `admin-api` | 后台 API 已有审计、玩家管理和部分 GM 入口 | 内网或受控公网控制面；RBAC、审计、命令闭环 | RBAC 闭环、管理口安全、GM 命令完整实现 |
| `admin-web` | 本地 Vite 前端 | 受控后台前端；通过安全入口访问 `admin-api` | 部署鉴权、网络隔离 |
| `metrics-collector` | 订阅 NATS metrics 并写 Redis 快照 | 多实例或单活均可；需要幂等聚合和明确 key 归属 | 多实例聚合策略、指标保留策略 |
| Redis | 共享协调与缓存 | 生产高可用；承载 session、ticket、注册中心、route store 或锁 | HA、持久化策略、key schema 和过期策略 |
| NATS | metrics、session kick、邮件通知 | 生产高可用；内部事件通道 | HA、重放/持久化边界、消息幂等 |
| PostgreSQL | 账号、审计、业务持久化 | 生产高可用；承载业务真持久化数据 | 备份、迁移、读写容量和事务边界 |

### 5.1 测试/线上统一启动顺序

测试、预发和线上环境必须先启动基础设施，再启动会注册或消费服务发现的内部服务，最后启动玩家入口和控制面。这里的 strict discovery 指 `DISCOVERY_REQUIRED=true`，或 `NODE_ENV` / `APP_ENV` 进入测试、预发、线上等严格发现环境。

统一启动顺序如下：

1. Redis registry / 业务 Redis：先提供 service registry、session、ticket、route store、metrics snapshot 等共享状态能力。
2. NATS：再提供 metrics、session kick、邮件通知等内部事件通道。
3. PostgreSQL：再提供账号、审计、公告、邮件等持久化数据入口。
4. registry-dependent services：启动 `game-server`、`match-service`、`chat-server`、`mail-service`、`announce-service` 等内部能力服务。`game-server`、`match-service`、`chat-server` 在严格发现下需要 registry 可用并完成注册；`mail-service`、`announce-service` 当前 Node 注册失败仍主要依赖日志和后续健康检查兜底。`mail-service` 的附件发放请求依赖发现 `game-server.admin`，应在后续健康检查阶段验证。`metrics-collector` 不注册也不消费 service registry，但依赖 NATS metrics 和 Redis snapshots，应在 Redis / NATS 可用后随本批或紧随本批启动。
5. gateway/control services：最后启动 `auth-http`、`game-proxy`、`admin-api`、`admin-web` 等入口和控制面。`game-proxy` 启动需要发现 `game-server.proxy-local`；`auth-http` 登录返回依赖发现 `game-proxy.client`；`admin-api` 控制面依赖发现 `game-server.admin` 和 `game-proxy.admin`。

这样排序的原因是：registry-dependent services 需要 Redis registry 已经可写，才能发布自身 endpoint 并维持 heartbeat；gateway/control services 属于发现消费者，启动或请求时依赖上游 endpoint 已经存在；NATS 和 PostgreSQL 分别是事件通道和持久化基础设施，应在业务服务启动前就绪。

本地开发可以继续使用 dev-stack 或现有脚本的默认顺序，并允许在非严格发现下使用文档标注的 local fallback。测试、预发和线上不能依赖本地默认 host/port 或 `REGISTRY_ENABLED=false` 跑通链路，必须先保证基础设施可用，并启用 strict discovery。

启动完成后的下一阶段应继续做健康检查、endpoint 完整性检查、实例唯一性检查、route store 检查和接流量控制。本文本节只定义启动顺序，不实现健康检查或流量切换逻辑。

### 5.2 注册后接流量门禁

测试、预发和线上部署不能把“进程已启动”视为“实例可接流量”。服务进程启动后，应先完成自身 endpoint 注册，并确认 registry 中可观察到该 endpoint 和持续 heartbeat；只有后续健康检查/readiness 通过后，实例才允许进入流量目标。

接流量门禁适用于所有入口和控制面：

- 健康检查/readiness 通过前，不得把实例加入 LB、DNS、网关 upstream、admin/control target 或 rollout 目标。
- `game-server`、`match-service`、`chat-server` 等 registry-dependent services 必须先确保自身注册记录和 heartbeat 可见，再进入可被发现或可被控制面选择的状态。
- `mail-service`、`announce-service` 等 Node 服务当前存在注册失败只打日志的现实限制；部署健康检查必须兜底校验 registry 可见性，避免出现“进程已启动但 registry 不可见”仍被加入流量的情况。

gateway/control services 还必须先验证必要上游 endpoint 可发现，再允许自身接流量：

- `game-proxy` 依赖 `game-server.proxy-local` 可发现后，才允许加入客户端游戏入口 upstream。
- `auth-http` 依赖 `game-proxy.client` 可发现后，才允许对外提供会返回游戏入口的登录链路。
- `admin-api` 依赖 `game-server.admin` 和 `game-proxy.admin` 可发现后，才允许进入可执行控制命令的 target。

rollout 或扩容时，新实例同样必须先完成 endpoint 注册、heartbeat 可见和 readiness 校验，才能被纳入 rollout 目标；失败实例应停留在隔离状态，由部署系统回滚、重试或人工处理，不能通过本地默认 host/port 绕过 registry 直接接流量。

本节只定义部署门禁流程和边界，不定义具体健康检查接口或检查项。健康检查必须验证的具体内容由后续小节继续拆分。

### 5.3 健康检查必检项

健康检查需要区分 liveness 与 readiness。liveness 只表示进程、事件循环或主线程仍存活，不代表实例已经可以接入流量；readiness 失败必须阻止实例进入 LB、DNS、网关 upstream、admin/control target 或 rollout 目标，但不必立即 kill 进程。只有在启动阶段要求严格发现 fail-fast 时，readiness 失败才应触发进程退出或部署回滚。

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
| `match-service` | `game-server.internal` |
| `mail-service` | `game-server.admin` |
| `metrics-collector` | 不注册也不消费 service registry；依赖 Core NATS metrics 通道和 Redis metrics snapshots，不属于 registry endpoint 检查 |

未列出的 registry 参与服务如果没有额外上游发现依赖，readiness 仍必须验证 Redis registry 可访问、自身注册记录存在和 heartbeat 未过期；`admin-web` 作为前端入口应验证 `admin-api` 入口可用，但不属于 service registry 实例。

Node 服务当前可能出现注册失败只打日志的情况，因此 readiness 必须兜底验证 registry 可见性，避免“进程已启动但自身或依赖不可发现”的实例接入流量。

本节只定义健康检查必须验证的内容，不定义具体接口形态，不新增自动化测试，也不要求启动任何服务。

### 5.4 测试/线上统一部署步骤

测试、预发和线上必须使用同一套部署状态机，不能为不同环境维护不同流程；环境之间只允许在环境变量、实例规模、域名或入口地址上存在差异。测试环境演练应完整覆盖与线上一致的注册、健康检查、接流量、下线、注销步骤，并把这些步骤作为测试/线上准入依据。

统一部署步骤如下：

1. 注册：进程按 5.1 顺序启动后，先向 Redis service registry 发布自身 endpoint，并开始维持 heartbeat。发布内容必须与当前环境配置一致，包括 service name、endpoint name、protocol、host、port、socket、visibility 和 instance id 等基础字段。
2. 健康检查：部署系统或控制面按 5.3 的 readiness 必检项确认 registry 可访问、自身注册记录存在、heartbeat 未过期，并确认必要依赖 endpoint 可发现。readiness 通过前，实例只能停留在隔离状态，不能被入口、控制面或 rollout 选择。
3. 接流量：readiness 通过后，实例才允许进入对应流量目标，包括 LB、DNS、gateway upstream、admin/control target 或 rollout target。接流量动作应以 registry 和控制面观察到的健康状态为准，不能因为进程存活就直接加入目标。
4. 下线：实例退出服务前，先从接新流量路径移除，或进入 drain 状态；随后等待现有连接、房间、后台任务、异步通知或控制面操作达到安全收敛。该步骤只定义下线状态边界，滚动发布中的旧实例 drain 和 route store 清理细节由后续滚动发布流程单独定义。
5. 注销：安全收敛后停止 heartbeat，执行 deregister，并确认 registry 中该实例不再可被发现。注销完成前，部署系统不得把同一实例视为已完全退出。

本地 dev-stack 可以继续保留简化启动顺序和非严格发现下的 local fallback，用于单机开发和快速联调；但 dev-stack 的简化流程不能作为测试、预发或线上准入依据。任何测试/预发/线上演练都必须覆盖上述完整状态机，避免出现本地默认 host/port、静态 upstream 或手工绕过 registry 的部署路径。

本节只定义统一部署流程和状态边界，不实现脚本、健康检查接口、LB/DNS/gateway 更新接口或服务启动逻辑。

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

当前实现状态（截至 `2026-06-15`）：`game-server` 已完成已鉴权 internal/admin 通道内的 room freeze/export/import/confirm/retire 最小闭环，适用于空房或全员离线 room 的基础 transfer 验证；`ConfirmRoomOwnershipReq/Res` 会在新服校验 room 存在、`OwnedByNew` 状态、`rollout_epoch`、checksum 和 `room_version` 后才返回成功。同时已提供 `TriggerServerRedirectReq/Res` 控制入口，可向旧服上目标 room 的当前在线成员下发 `ServerRedirectPush`。push 成功进入出站队列后，旧服会以 `server_redirect_reconnect_required` 主动请求关闭旧连接；push 排队失败的连接计入失败数，不额外覆盖关闭原因。`GetRolloutDrainStatusReq/Res` 会返回旧服真实 `drain_mode_enabled`、`drain_mode_entered_at_ms`、`drain_mode_reason`、`drain_mode_source`、连接数、仍持有 room、迁移中 room、已 retired tombstone room、route 样本和可接管空房分类；可接管空房仅包含仍为 `Owned` / 对外视作 `OwnedByOld` 且在线成员数为 `0` 的 room，已 `Retired` room 单独计入 `retired_room_count`，不作为旧服排空阻塞项。该状态供 `auth-http` 内部接口、`tools/mock-client` 查询，也可被 `game-proxy` 的 `complete-if-drained` 在配置启用时作为结束 rollout 前的真实排空校验。`RequestServerShutdownReq/Res` 已提供旧服排空后的受控 graceful shutdown 请求入口，必须满足 `drain_mode_enabled == true`、`connection_count == 0`、`owned_room_count == 0`、`migrating_room_count == 0` 才会触发 game-server 自身 graceful shutdown 信号；`retired_room_count` 只作为观测字段，不阻塞停服。`tools/mock-client` 已具备收到 push 后主动断线、连接目标入口、重新 `AuthReq` 并优先 `RoomReconnectReq` 的验证场景，也可通过 `request-server-shutdown` 场景人工调用停服安全闸。2026-06-13 已在真实 old/new/proxy/auth 环境中人工跑通 `movement_demo` 空房迁移控制面，并用 mock-client 验证 redirect -> transfer -> proxy reconnect；尚未完成自动测试准入、mybevy 适配、L7 relay、同连接 upstream swap、真实 route metadata 丢失恢复或生产部署平台 stop hook 接入，也不代表 movement/combat/NPC/AI/timer 等完整玩法状态已经可无损迁移。

补充实现状态（截至 `2026-06-15`）：`tools/mock-client` 已增加第一阶段显式编排入口，按 old `freeze/export`、new `import`、new `confirm ownership`、proxy room route `upsert`、old `retire` 的保守顺序调用现有控制面。编排会校验 export/import/confirm checksum 和 roomVersion 一致，并在 confirm 成功后才 upsert proxy route，在 proxy upsert 成功后才 retire 旧 room；任一步失败都会返回失败阶段并停止后续步骤。`scripts/rollout-three-process-drill.ps1` 已在其外层补充 old/new/proxy 演练入口，默认 dry-run，只做 preflight、端口探测和步骤命令输出；显式 `-ExecuteSteps` 才调用 rollout start、old drain、transfer、drain status、complete-if-drained 等控制面步骤，旧服 shutdown 请求还需要额外 `-AllowShutdownRequest`。脚本也可读取旧服 PID 或 PID 文件，并在 shutdown 安全闸 `ok=true` 后等待该旧进程退出、写入 `old-process-stop` 报告阶段。该入口仍是可重复演练脚本；真实服务人工验收已完成一轮空房迁移控制面，但还不是自动测试准入，也不包含同连接迁移、真实故障恢复或生产部署平台 stop hook 接入。

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

- 生产网络策略只暴露 `auth-http` 和 `game-proxy` 玩家入口。
- `game-server` 玩家协议口不能被公网直连。
- `chat-server`、`match-service`、`announce-service`、`mail-service` 不作为生产客户端直连默认入口。
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
5. 演练入口固化：`scripts/rollout-three-process-drill.ps1` 已提供第一阶段 dry-run/step-runner，并已完成一轮真实 old/new/proxy/auth 空房迁移人工验收；后续需要沉淀为自动测试准入。
6. redirect/reconnect 闭环：补齐外部 `mybevy` 客户端重连、proxy 重新路由和错误 owner 处理验收。
7. room transfer 最小闭环：已具备 freeze/export/import/confirm/retire 控制流，并已用 `movement_demo` 做真实三进程空房迁移验收；后续补真实故障恢复、更多 room policy 和自动化准入。
8. transfer payload trait：按玩法补齐可序列化、版本化、checksum 和兼容性检查。
9. owner 仲裁与审计：补 CAS、route version、rollout epoch、owner tombstone 和迁移审计。
10. 客户端能力收敛：chat/mail/announce/match 经服务端入口或 BFF 收敛，不再要求生产客户端直连内部服务。
11. L7 session relay：设计并实现同连接 upstream swap 所需的 proxy 协议解析、输入缓冲和 resume。
12. 故障演练：覆盖 proxy 重启、game-server 崩溃、导入失败、route 切换失败和客户端中断。
