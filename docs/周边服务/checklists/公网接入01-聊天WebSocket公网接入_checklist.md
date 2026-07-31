# 公网接入 01：聊天 WebSocket 公网接入 Checklist

## 目标

在保留现有 `chat-server:9001/TCP + Protobuf` 内部联调能力的前提下，为正式客户端增加经 Caddy HTTPS 入口访问的 WebSocket/WSS 聊天通道：

- 公网使用 `wss://chat.game.zergzerg.cn/`，由 Caddy 终止 TLS，再转发到 `chat-server` 的容器内 WebSocket listener。
- 每个 WebSocket binary frame 承载一个完整的现有聊天协议包，即 `14` 字节包头加 Protobuf body；不复制聊天业务逻辑。
- 现有 TCP listener、消息类型、ticket 鉴权、Redis 在线路由、PostgreSQL 历史记录、NATS 邮件通知和本地 mock-client 流程继续可用。
- `auth-http` 只向正式客户端返回稳定的公网 WSS 地址，不返回 registry 中的 Docker 内网 endpoint。
- 本清单不把聊天流量转发给 `game-server`，不改造为通用网关，也不在公网开放 `9001/9011` 原始端口。

## 基础原则

- [ ] WebSocket 是现有聊天协议的传输适配层，不建立第二套鉴权、会话、聊天、历史或邮件通知业务实现。
- [ ] `CHAT_WS_ENABLED` 在本地默认关闭，生产 Compose 显式开启；现有 `9001/TCP` 测试不得被迫迁移。
- [ ] 公网只开放 Caddy 的 `443/TCP`；`chat-server` TCP 与 WebSocket listener 均保留在 Docker internal network。
- [ ] WebSocket binary frame 必须具有明确大小上限、完整包边界和错误关闭语义；文本帧及畸形包不得进入业务处理器。
- [ ] ticket、token、聊天正文和用户隐私不得出现在无界日志、指标标签或 Caddy access log 中。
- [ ] 每个阶段完成后执行对应静态或自动化验证；启动 Redis、PostgreSQL、NATS、Caddy 或服务进程前先列出依赖并等待用户确认。

## 阶段 1：传输契约与兼容边界

- 开始时间：2026-07-30 17:02:53 +08:00
- 结束时间：2026-07-30 17:11:02 +08:00
- 开发总结：固化聊天 TCP/WSS 并行期的公网边缘拓扑、单 frame 单包契约、错误关闭语义、地址下发和敏感信息边界；同步整体架构、生产拓扑、协议与外部客户端文档，并明确当前仍为 TCP 基线、WSS 属于后续实现目标态。
- 验证记录：主 agent 复核五份文档累计 diff；`git diff --check -- <五份文档>` 通过（仅既有 LF/CRLF 提示），`git diff --exit-code -- packages/proto/chat.proto apps/chat-server` 通过，确认未提前修改协议或业务代码。

- [x] 在 `docs/周边服务/聊天与邮件系统设计.md` 固化 TCP 与 WSS 并行期拓扑、端口、协议和责任边界。（验证：`docs/周边服务/聊天与邮件系统设计.md:109` 定义并行拓扑，`:130` 起列出 9001/9011/443 暴露策略与分层职责）
- [x] 规定一个 WebSocket binary frame 只允许包含一个完整聊天协议包，不允许半包、多包拼接或裸 Protobuf body。（验证：`docs/周边服务/聊天与邮件系统设计.md:145` 起规定 frame 长度必须等于 `14 + bodyLength`，并拒绝半包、多包及裸 Protobuf）
- [x] 规定 binary frame 继续使用现有 magic、version、flags、message type、sequence 和 body length，不改变 `packages/proto/chat.proto` 字段编号。（验证：`docs/周边服务/聊天与邮件系统设计.md:147` 起固定 14 字节包头各字段及字段编号兼容边界；`git diff --exit-code -- packages/proto/chat.proto` 通过）
- [x] 定义文本帧、空帧、错误 magic、声明长度不符、body 超限、未知消息、Ping/Pong 和 Close 的处理结果。（验证：`docs/周边服务/聊天与邮件系统设计.md:164` 起的错误和控制帧表覆盖 `1002/1003/1008/1009`、`UNKNOWN_MESSAGE_TYPE`、Ping/Pong 与 Close 清理）
- [x] 明确正式客户端从 `auth-http` 登录或签票响应读取公网 `services.chat`，本地 TCP 场景继续允许显式 `127.0.0.1:9001`。（验证：`docs/周边服务/聊天与邮件系统设计.md:184` 起定义公网 descriptor、部署配置来源、缺配置为 null 和本地 TCP 兼容路径）
- [x] 明确非目标：不让 Caddy解析 Protobuf，不让 `game-server` 转发聊天，不在本阶段删除 TCP listener。（验证：`docs/周边服务/聊天与邮件系统设计.md:200` 起明确 Caddy/game-server/game-proxy、TCP listener、内部端口和多实例非目标）
- [x] 验证设计与 `docs/总览/整体架构.md`、生产拓扑、协议设计和外部客户端接入说明不存在冲突。（验证：`docs/总览/整体架构.md:273`、`docs/后台与运维/生产拓扑与Room迁移设计.md:23`、`docs/协议与客户端/协议设计.md:899`、`docs/协议与客户端/外部客户端接入说明.md:77` 统一为 Caddy WSS 公网入口且原始 listener 内网化）

## 阶段 2：WebSocket 二进制帧适配层

- 开始时间：2026-07-30 17:12:45 +08:00
- 结束时间：2026-07-30 18:28:12 +08:00
- 开发总结：为 chat-server 增加默认关闭的独立 WebSocket listener，使用锁定版本的 tokio-tungstenite 完成握手、控制帧和 logical binary message 处理；通过有界 duplex 桥接复用现有 TCP 鉴权、会话和聊天处理器，出站完整协议包逐条映射为 binary message。用户确认允许底层 RFC 6455 分片由成熟库在 frame/message 上限内重组，业务层只要求每条完成重组的 binary message 恰好包含一个完整聊天协议包。
- 验证记录：主 agent 复核 chat-server 五文件 diff；`cargo fmt --manifest-path apps/chat-server/Cargo.toml --check`、`cargo test --manifest-path apps/chat-server/Cargo.toml`（70 passed）和 `git diff --check -- apps/chat-server` 通过；clippy 仅报告既有 7 条 dead-code/style warning，无 WebSocket 模块新增 warning。

- [x] 为 `chat-server` 引入成熟 Rust WebSocket 库并锁定依赖版本，不手写 WebSocket 握手和帧解析。（验证：`apps/chat-server/Cargo.toml:25` 精确锁定 `tokio-tungstenite = 0.28.0` 并启用成熟 handshake 实现，Cargo.lock 已同步）
- [x] 新增独立 WebSocket listener，默认内部地址使用 `0.0.0.0:9011`，且不替换现有 `CHAT_BIND_ADDR`。（验证：`apps/chat-server/src/main.rs:82` 读取独立 `CHAT_WS_BIND_ADDR` 默认 9011，`:109` 默认关闭；`apps/chat-server/src/chat_server.rs:332`-`:334` 分别绑定 TCP 与可选 WS listener）
- [x] 将 WebSocket binary frame 校验为一个完整现有协议包，再交给共用聊天连接处理器。（验证：按用户确认以 logical binary message 为应用层 frame 语义；`apps/chat-server/src/websocket.rs:355` 校验 magic/version/flags、body/frame 上限及 `14 + bodyLength`，`:276` 通过后才写入共用处理器桥接）
- [x] 将共用处理器产生的完整协议包编码为单个 WebSocket binary frame 返回客户端。（验证：`apps/chat-server/src/websocket.rs:416` 的有界出站泵按包头逐包读取并发送独立 binary message；`:600` 测试连续出站包保持独立）
- [x] 对握手、单 frame 大小、协议 body 大小、内存桥接缓冲和出站队列设置有限上限。（验证：`apps/chat-server/src/websocket.rs:23` 的 AdapterConfig 收敛握手/帧/body/bridge/IO 上限，`:191` 使用容量 1 的出站桥接队列，`:580` 和 `:630` 覆盖库缓冲与握手字节限制）
- [x] 正确处理 Ping/Pong、客户端 Close、Caddy 断连、协议处理器退出和半关闭，确保任务能够回收。（验证：`apps/chat-server/src/websocket.rs:253` 的 select 循环处理控制消息、底层关闭与 handler 退出，serve 在退出时 shutdown/abort/join 有界回收；`:646` 覆盖双向桥接和 Pong flush）
- [x] WebSocket 适配失败只能终止对应连接，不得退出 TCP listener、NATS 订阅或整个服务进程。（验证：`apps/chat-server/src/chat_server.rs:407` 起为每个 WS 连接单独 spawn adapter，错误仅记录固定 category；主 accept loop 与 TCP listener 持续运行）
- [x] 为完整帧、截断帧、多包帧、文本帧、超限帧和控制帧增加单元测试。（验证：`apps/chat-server/src/websocket.rs:489` 起覆盖完整/空 body/截断/多包/文本/超限/Ping/Pong/Close、握手限制、出站拆帧和内存双向桥接）
- [x] 使用 `cargo test --manifest-path apps/chat-server/Cargo.toml` 验证新增适配层和既有 chat-server 测试。（验证：主 agent 于 2026-07-30 18:27 +08:00 复跑，70 passed、0 failed）

## 阶段 3：共享认证、限流与会话生命周期

- 开始时间：2026-07-30 18:31:56 +08:00
- 结束时间：2026-07-30 19:08:24 +08:00
- 开发总结：TCP 与 WSS 统一收敛到同一认证、连接配额、消息限流、session map 和出站推送处理链；新增可信代理 CIDR/XFF 解析、WebSocket 并发握手上限、session 覆盖通知及 Redis route owner fencing，并为鉴权、限额、过载和异常关闭建立固定协议错误与 WSS close 分类。
- 验证记录：主 agent 复核 chat-server 六文件 diff；`cargo fmt --manifest-path apps/chat-server/Cargo.toml --check`、`cargo test --manifest-path apps/chat-server/Cargo.toml`（80 passed）、`git diff --check -- apps/chat-server` 通过；`cargo clippy --manifest-path apps/chat-server/Cargo.toml --all-targets` 仅报告 6 条既有 warning，无本阶段新增阻塞问题。未启动 Redis、PostgreSQL、NATS、Caddy 或真实服务，Lua fencing 真实集成保留到阶段 9 门禁。

- [x] TCP 和 WebSocket 连接复用同一 `ChatAuthReq`、ticket 签名、Redis ticket ownership 和 ticket version 校验。（验证：`apps/chat-server/src/chat_server.rs:455` 与 `:508` 均调用同一 `handle_connection`；`:1084` 的 `read_auth_request` 统一执行签名、ownership 和 version 校验）
- [x] TCP 和 WebSocket 连接共享同一玩家/IP 连接数限制，不因双协议并行绕过配额。（验证：`apps/chat-server/src/chat_server.rs:416` 创建单一 `ConnectionLimitTracker` 并同时传给 TCP/WSS；`:1384` 测试跨 transport 共享玩家/IP 配额）
- [x] WebSocket 业务消息复用现有 `CHAT_MSG_RATE_WINDOW_MS` 和 `CHAT_MSG_RATE_MAX`，握手洪泛另设有界保护。（验证：`apps/chat-server/src/chat_server.rs:812` 在共用消息循环执行限流；`:420`、`:478` 使用有界握手 semaphore；`apps/chat-server/src/websocket.rs:906` 验证 permit 在升级完成后释放）
- [x] 仅在请求来自可信 Caddy 网络时使用 `X-Forwarded-For` 计算客户端 IP；其他来源使用 socket peer IP。（验证：`apps/chat-server/src/websocket.rs:137` 从 socket peer 开始并仅剥离可信代理；`:429` 只在握手读取 XFF；`:803` 覆盖可信、直连和非法头）
- [x] 验证同一玩家 TCP/WSS 重连和覆盖时只保留当前 session，旧连接退出不得删除新 session 或 Redis 在线路由。（验证：`apps/chat-server/src/chat_service.rs:798` 注册新 session 并通知旧连接、`:815` 以 channel identity 条件注销；`apps/chat-server/src/online_route.rs:4` 和 `:10` 用 Lua 原子写 owner/compare-delete；session 竞态测试通过）
- [x] 验证 WSS 连接能够接收 `ChatPush` 和 `MailNotifyPush`，TCP 连接行为保持兼容。（验证：`apps/chat-server/src/chat_service.rs:240`、`:335` 与 `apps/chat-server/src/mail_subscriber.rs:594` 共用同一 session sender；`apps/chat-server/src/websocket.rs:636` 统一映射出站包，`:860` 验证两类 push 保持独立 binary message；全量 80 测试通过）
- [x] 对鉴权失败、ticket revoke、连接超限、消息超限和队列满增加稳定关闭码或协议错误响应。（验证：`apps/chat-server/src/chat_server.rs:68` 固定 WSS close 分类，`:649`、`:668`、`:828`、`:1053` 固定认证/限额/过载协议结果；`:1400` 与 `:1415` 测试关闭分类及终止错误）
- [x] 日志只记录受限 peer、transport、消息类型和错误分类，不记录 ticket 或完整聊天正文。（验证：`apps/chat-server/src/chat_server.rs:643` 起的认证、限流、分发及清理日志只使用 peer/transport/message_type/error_category；`apps/chat-server/src/chat_service.rs:216` 起移除正文和存储错误详情）

## 阶段 4：配置、服务注册与健康状态

- 开始时间：2026-07-30 19:10:04 +08:00
- 结束时间：2026-07-30 19:43:43 +08:00
- 开发总结：完成 WSS 严格配置、生产 Compose 显式启用、TCP/WS listener 预绑定、内部 registry `ws` transport/schema 和 endpoint metadata；新增固定 key 的 TCP/WSS 连接、握手、frame 拒绝、异常关闭及队列失败指标，并将生产可信代理默认值收窄到声明的 `172.30.0.0/24` internal subnet。
- 验证记录：第 1 轮主审修复生产可信网段过宽、WS endpoint body 上限高报和 queue closed 漏计后通过；主 agent 复跑 `cargo fmt --manifest-path apps/chat-server/Cargo.toml --check`、chat-server 87 tests、service-registry 12 unit + 11 normalization tests、Node registry 28 tests 和 chat-server clippy（仅 6 条既有 warning），`git diff --check` 通过。未启动 Docker/Compose、Redis、PostgreSQL、NATS、Caddy 或真实服务。

- [x] 增加 `CHAT_WS_ENABLED`、`CHAT_WS_BIND_ADDR`、可信代理和 WebSocket 大小/握手限制配置，并写入 `.env.example`。（验证：`apps/chat-server/.env.example:2` 起列出开关、地址、握手、可信代理、frame 和 bridge 配置；`apps/chat-server/src/main.rs:88` 起严格解析）
- [x] 本地与测试 overlay 默认不启用 WSS；生产 Compose 显式设置 `CHAT_WS_ENABLED=true`。（验证：`.env.example:3` 默认 false；`deploy/docker/compose.production.yml:145` 起显式 true 且没有 chat-server `ports` 映射）
- [x] 配置解析拒绝非法布尔值、非法地址、与 TCP listener 冲突的端口及不安全生产默认值。（验证：`apps/chat-server/src/main.rs:396` 校验 SocketAddr/零端口/冲突，`:419` 要求生产 WSS 配置非空且非 `/0` 可信网段；`:1370` 起覆盖错误输入）
- [x] registry 在 WSS 启用时增加 `chat-server.ws` 内部 endpoint，保留现有 `chat-server.tcp` endpoint。（验证：`apps/chat-server/src/main.rs:603` 始终构造 tcp 并按开关追加 internal ws；`packages/service-registry/src/types.rs:337` 与 Rust/Node/JSON schema 同步接受 `ws`）
- [x] registry metadata 标明 transport capability、协议版本、最大 frame/body 和 build version，不发布公网域名或凭证。（验证：`apps/chat-server/src/main.rs:556`、`:589` 生成固定 metadata，WS body 上限按 `frame - 14` 收敛；`:1650` 起验证内部 host、capability、版本、上限和 build）
- [x] readiness 在生产启用 WSS 时检查 WebSocket listener 已成功绑定；listener 失败必须使实例启动或 readiness 明确失败。（验证：`apps/chat-server/src/main.rs:740` 在 Redis/registry 副作用前预绑定完整 listener set；`apps/chat-server/src/chat_server.rs:418` 绑定任一失败即返回，`:464` 仅在进入已绑定 run 后启动 readiness；绑定失败测试通过）
- [x] 指标区分 TCP/WSS 当前连接、握手成功/失败、frame 拒绝、异常关闭和出站队列失败，且不使用 player ID 标签。（验证：`apps/chat-server/src/metrics.rs:115` 起定义固定无身份维度指标，`:219` 使用 RAII 连接 guard；`apps/chat-server/src/websocket.rs:382` 起记录握手/异常/frame；queue full/closed transport 映射测试通过）
- [x] 使用 chat-server 配置单元测试、registry schema 检查和 discovery 配置检查验证配置边界。（验证：主 agent 复跑 chat-server 87 tests、service-registry 23 tests 和 Node registry 28 tests 全部通过；生产 Compose 网段与无端口映射由单元/静态检查覆盖）

## 阶段 5：Caddy WSS 公网入口与生产网络

- 开始时间：2026-07-30 19:46:57 +08:00
- 结束时间：2026-07-31 09:45:22 +08:00
- 开发总结：新增 Caddy 独立聊天 WSS site，仅接受 `GET /` 的 WebSocket Upgrade 并转发到 internal `chat-server:9011`；生产 Compose 增加必填聊天域名和 internal 网络依赖，未映射聊天原始 listener；同步 DNS、日志脱敏、连接重连和防火墙边界文档，并用静态部署测试覆盖关键约束。
- 验证记录：`node --test tests/deploy/chat-wss-edge-config.test.mjs` 4/4 通过；WSL Docker Compose `config --no-env-resolution --quiet` 通过；Caddy v2.10.2 在 `--network none` 下 `caddy validate` 返回 `Valid configuration`；经 SSH 直接检查生产主机，9001/9011 无 listener、Docker ports 映射或 NAT/nft 转发，UFW 默认拒绝入站且只公开玩家服务所需端口；现有 SSH 管理入口作为独立运维例外，不在公开文档记录实际端口。

- [x] 为 `chat.game.zergzerg.cn` 准备 DNS A/AAAA 策略并确认解析到现有 Caddy 入口服务器。（验证：2026-07-31 `Resolve-DnsName` 显示 A 为 `103.47.81.222`，与 `api.game.zergzerg.cn` 相同；两域名均无 AAAA，符合仅在 IPv6 全链路可达后才发布的策略）
- [x] Caddy 增加独立 `CADDY_CHAT_HOST` site，只接受 WebSocket upgrade，不把普通 HTTP 请求转给聊天协议端口。（验证：`deploy/docker/caddy/Caddyfile:46` 定义独立 site，`:70` 匹配 `GET /` 与 Upgrade，`:94` 对其余请求返回 426；部署静态测试 4/4 通过）
- [x] Caddy 将 WSS 转发到 `chat-server:9011`，保留客户端地址、请求 ID 和必要代理头。（验证：`deploy/docker/caddy/Caddyfile:79` 转发 internal listener，`:82` 覆盖边缘 request ID，`:83` 设置直连 peer，X-Forwarded-* 使用 Caddy 防伪默认行为）
- [x] 配置合理的握手超时、空闲连接、header 大小和访问日志策略；禁止记录 ticket/query 凭证。（验证：`deploy/docker/caddy/Caddyfile:5`-`:8` 设置 header/idle 上限，`:60`-`:63` 删除完整 URI 与凭证头；`compose.production.yml:152` 设置 30 秒应用层心跳超时）
- [x] Compose 只让 Caddy 与 chat-server 通过 internal network 通信，不增加 `9011:9011` 宿主机映射。（验证：`tests/deploy/chat-wss-edge-config.test.mjs:32` 静态断言 Caddy 为 edge+internal、chat-server 仅 internal，且全文件不存在 9001/9011 宿主映射）
- [x] 云安全组与主机防火墙继续只公开 `80/TCP`、`443/TCP` 和 `4000/UDP`，明确拒绝 `9001/9011` 公网访问。（验证：2026-07-31 经 SSH 直接检查生产主机，9001/9011 无 listener，Docker 无对应 ports 映射，iptables/nft 无对应 NAT；UFW 默认拒绝入站，聊天玩家服务仅使用 80/443 TCP 与 4000 UDP，既有 SSH 管理入口作为独立运维例外）
- [x] Caddy reload、容器替换和证书续期期间已有连接允许断开并由客户端退避重连，不影响游戏 KCP 连接。（验证：`docs/后台与运维/生产拓扑与Room迁移设计.md:55` 固化带抖动指数退避及与 4000/UDP KCP 隔离；`聊天与邮件系统设计.md:202` 同步客户端契约）
- [x] 经用户确认 Docker/Caddy 依赖后，执行 Compose config 与 Caddy 配置校验并记录结果。（验证：WSL Docker Compose `config --no-env-resolution --quiet` 使用虚拟环境值通过；本地 Caddy `v2.10.2` 在 `--network none` 下对当前 Caddyfile 执行 `caddy validate` 返回 `Valid configuration`；未启动、reload 或替换容器）

## 阶段 6：发布参数与公网聊天地址下发

- 开始时间：2026-07-31 09:46:52 +08:00
- 结束时间：2026-07-31 10:37:24 +08:00
- 开发总结：发布 bundle 新增并强制校验 Caddy 聊天域名，render/upload/生产 Compose 将其传入 auth-http；auth 仅按部署配置下发稳定 WSS descriptor，永不以 registry 内部 endpoint 覆盖。补齐登录、角色选择和签票响应的一致性回归测试，并将发布文档中的 SSH 管理端口替换为占位符，避免公开真实管理端口。
- 验证记录：主 agent 审核 auth、Compose、发布脚本、响应测试与文档 diff；`npm test --workspace auth-http` 88 passed、`node --test tests/deploy/chat-wss-edge-config.test.mjs` 5 passed、`node --test tests/registry/deployment-discovery-dry-run.test.mjs` 2 passed、Node/Bash 语法检查、临时 release env 渲染与 `git diff --check` 通过。`npm run check:deployment-discovery -- --fixture tests/fixtures/rollout-registry-discovery.json --environment production` 仍因该既有 rollout fixture 缺少 7 个非本阶段 endpoint 而失败，且该检查不覆盖 auth 的公网 WSS descriptor，已作为既有 fixture 覆盖缺口记录；未启动 Redis、PostgreSQL、NATS、Docker、Caddy 或真实服务。

- [x] 发布 bundle 增加 `--caddy-chat-host` / `CADDY_CHAT_HOST`，同步 render、upload、示例环境和必填校验。（验证：`scripts/docker/create-release-bundle.sh:8`、`scripts/docker/upload-release-bundle.sh:17`、`scripts/docker/render-release-env.mjs:12` 均要求该参数，`:51` 渲染 `CADDY_CHAT_HOST`；临时 env 文件包含 `CADDY_CHAT_HOST=chat.example.com`）
- [x] `auth-http` 增加受配置控制的公网聊天 descriptor，生产返回 `host=chat.game.zergzerg.cn`、`port=443`、`protocol=wss`。（验证：`apps/auth-http/src/config.js:77` 解析严格 host/port 并固定 `wss`，`compose.production.yml:303` 注入域名及 443；auth config 生产用例通过）
- [x] 公网聊天 descriptor 来源必须是部署配置，不得从 `chat-server.ws` 内部 registry endpoint 推导或泄漏 Docker host。（验证：`apps/auth-http/src/service-discovery.js:55` 优先配置 descriptor，`:92` 有公网配置时跳过 `chat-server.tcp` 查询；覆盖 registry 内网 endpoint 的回归用例通过）
- [x] 登录、角色选择/签票响应中的 `services.chat` 保持同一格式和地址；旧客户端忽略新增字段时不受影响。（验证：`apps/auth-http/src/client-service-response.test.js` 覆盖三种响应，均断言同一 `{host, port, protocol}` descriptor；既有 DTO 保持 nullable 字段）
- [x] 未配置公网聊天地址时保持 `services.chat=null`，不得回退到生产内网 endpoint。（验证：`apps/auth-http/src/config.test.js` 覆盖本地和生产缺配置均为 `publicChatDescriptor=null`；`service-discovery.js:125` 仅在无公网 descriptor 时保留原有非生产内部发现路径）
- [x] 非生产 `AUTH_EXPOSE_INTERNAL_SERVICE_ENDPOINTS` 仅保留内部调试语义，不覆盖显式公网 WSS descriptor。（验证：`service-discovery.js:90` 仅对显式 descriptor 跳过 chat 内部查询；`service-discovery.test.js` 同时开启内部暴露时仍返回公网 WSS，测试通过）
- [x] 为 auth config、service discovery、登录响应和签票响应增加生产/本地/缺配置回归测试。（验证：`config.test.js`、`service-discovery.test.js`、`client-service-response.test.js` 已纳入 `scripts/run-tests.js`；auth-http 88 tests 全部通过）
- [x] 使用 `npm test --workspace auth-http` 和部署 discovery 检查验证地址下发，不启动真实服务。（验证：auth 88 passed，deployment discovery 单元测试 2 passed；现有 rollout fixture CLI 仍缺少 7 个与本阶段无关的 endpoint，静态部署/WSS 测试 5 passed，未启动真实依赖）

## 阶段 7：Mock-client 与外部客户端渐进迁移

- 开始时间：2026-07-31 10:39:13 +08:00
- 结束时间：2026-07-31 11:25:00 +08:00
- 开发总结：mock-client 增加默认 TCP、可选本地 WS 和正式 WSS 三种聊天 transport，WSS 从 auth `services.chat` 获取地址且允许显式测试覆盖；所有 WebSocket binary message 严格复用现有 14 字节 packet 与 Protobuf 编解码。外部 mybevy 在真实仓库中新增独立聊天运行时 API，编译共享 `chat.proto`，支持 descriptor、`messageType + seq` 路由、push、限额、退避、前后台和 ticket 刷新接口；当前未绑定具体 UI 事件，真实 Caddy/TLS/服务联调保留到阶段 9。发现进程环境变量仍指向不存在的 `C:\project\mybevy`，验证时显式使用实际 `H:\project\mybevy`，未修改全局环境。
- 验证记录：主 agent 审核 MyServer mock-client 和 mybevy 聊天相关 diff，确认外部仓库既有 UI 改动未进入本阶段范围；`node --test tests/mock-client/mock-client-websocket.test.mjs` 5 passed，`MYSERVER_CLIENT_ROOT=H:\project\mybevy npm run check:mock-client-protocol` 通过，worker 执行 `cargo test --lib game::myserver::chat` 5 passed（首次约 6 分 54 秒），两仓 `git diff --check` 通过。未启动 Caddy、auth、chat-server、Redis、PostgreSQL、NATS 或真实服务。完整旧 mock-client protocol test 仍有两项未触及的基线失败（rollout help 文案、既有 AuthRes 字段断言）。

- [x] mock-client 增加 `tcp` / `ws` / `wss` transport 选项，现有聊天场景默认 TCP 行为不变。（验证：`tools/mock-client/src/args.js` 默认 `chatTransport=tcp`，`scenarios/chat.js` 仅在 ws/wss 时创建 WebSocket client；定向测试确认默认 TCP）
- [x] mock-client WSS 模式优先读取登录响应 `services.chat`，同时支持测试显式传入 WebSocket URL。（验证：`auth.js` 生成受校验的 descriptor URL，`chat.js` 按显式 `--chat-ws-url` 优先、否则读取 `services.chat`；定向测试通过）
- [x] mock-client 在 binary frame 中复用现有 packet encoder/decoder，不复制 Protobuf 编解码逻辑。（验证：`websocket-client.js` 使用 `encodePacket`/`decodePacketFrame` 与既有 `decodeByMessageType`，`packet.js` 严格校验单个 14 字节包头 packet）
- [x] 增加 WSS 认证、单聊、群聊、历史查询、邮件通知、重连和错误关闭测试场景。（验证：`tests/mock-client/mock-client-websocket.test.mjs` 覆盖 auth/private/group/history/mail push/reconnect/text 与畸形 frame，5 tests passed）
- [x] 更新 help/README，清楚区分本地 `9001/TCP`、本地可选 WS 和正式 `wss://chat.game.zergzerg.cn/`。（验证：`tools/mock-client/README.md` 与 `help.txt` 增加三种 transport、命令示例和 9011 非公网边界）
- [x] 正式 `mybevy` 改造前通过 `MYSERVER_CLIENT_ROOT` 确认外部仓库路径，不在 MyServer 中创建客户端副本。（验证：发现变量指向的 `C:\project\mybevy` 不存在，验证改用实际 `H:\project\mybevy`；所有 mybevy 修改均在外部仓库 `project/src/game/myserver/`）
- [x] mybevy 支持从 `services.chat` 建立 WSS、按 `messageType + seq` 关联响应并处理异步 push。（验证：`H:\project\mybevy\project\src\game\myserver\chat.rs` 定义 `ChatWebSocketEndpoint`、`ChatPacketRouter` 与独立 WSS runtime，聊天单元测试 5 passed；UI 绑定留给后续聊天界面接入）
- [x] 客户端实现断线退避、前后台切换、ticket 失效重新签票、最大消息限制和未知消息兼容。（验证：`chat.rs` 中 `ChatReconnectPolicy`、`set_foreground`、`TicketRefreshRequired`/`replace_ticket`、`DEFAULT_CHAT_MAX_BODY_LEN` 和 `Unknown` 分支均有实现及定向测试）
- [x] 使用 `npm run check:mock-client-protocol` 和 mock-client 定向测试验证 TCP/WSS 协议一致性。（验证：协议检查通过，WSS fixture 5 passed；未启动真实依赖）

## 阶段 8：多实例路由与容量保护

- 开始时间：2026-07-31 11:27:02 +08:00
- 本轮续作开始时间：2026-07-31 11:50:34 +08:00
- 子任务 2（入口均衡与容量门槛）开始时间：2026-07-31 12:18:54 +08:00
- 结束时间：2026-07-31 12:45:54 +08:00
- 开发总结：完成 owner-fenced Redis online route 与有界实例级 Core NATS 私聊/群聊/邮件在线投递；生产 release 保持单副本 chat-server，Caddy 已具备 DNS 多 upstream 新连接均衡配置但不将其误作已启用多副本。补齐单实例总连接、WSS 握手/消息速率、队列、内存、慢客户端、陈旧 route 与 publish 积压指标和告警契约；未来多副本必须使用独立编排和唯一实例 ID。
- 验证记录：主 agent 复核两次 worker diff 与所有 checklist 子项；`cargo fmt --manifest-path apps/chat-server/Cargo.toml --check`、`cargo test --manifest-path apps/chat-server/Cargo.toml`（95 passed）、`cargo clippy --manifest-path apps/chat-server/Cargo.toml --all-targets`（仅 3 条既有 dead-code 与 1 条既有 manual_checked_ops warning）、`node --test tests/deploy/chat-wss-edge-config.test.mjs`（6 passed）、`bash -n scripts/docker/server-apply-release.sh` 和 `git diff --check` 通过。当前 Windows 环境未提供 Docker/Caddy 二进制，未执行 `docker compose config` 或 `caddy validate`，也未启动 Redis、PostgreSQL、NATS、Caddy 或服务；实际解析、真实多实例和 WSS 故障联调保留阶段 9。

- [x] 明确当前单实例上线门槛与多实例非目标，避免把 Caddy 负载均衡误认为跨实例推送已完成。（验证：`docs/周边服务/聊天与邮件系统设计.md:215` 定义单实例门槛与非目标，`:209` 明确 Caddy 新连接均衡不替代 Redis/NATS 投递；`deploy/docker/compose.production.yml:185` 固定单副本）
- [x] 多实例前实现私聊、群聊和邮件通知按 `chat:online:{playerId}` 路由到目标 chat instance。（验证：`apps/chat-server/src/chat_service.rs:174` 和 `:273` 在消息持久化后经 `ChatPushRouter` 路由；`apps/chat-server/src/mail_subscriber.rs:650` 重新读取 route 后投递；`cargo test --manifest-path apps/chat-server/Cargo.toml` 95 passed）
- [x] 跨实例聊天推送复用有界 NATS subject 或等价内部通道，消息持久化成功与在线 push 失败语义分离。（验证：`apps/chat-server/src/chat_push.rs:245` 使用有界队列，`:300` 起经实例级 Core NATS subject 发布，持久化失败返回 `CHAT_SAVE_FAILED` 而路由/NATS/队列失败只跳过在线提示；远端队列背压/关闭/超限测试通过）
- [x] Caddy 对 WebSocket 新连接执行可预测的负载均衡；单条长连接建立后保持对应实例，不依赖跨请求 cookie。（验证：`deploy/docker/caddy/Caddyfile:83` 使用 `dynamic a chat-server 9011`、`round_robin` 和无 cookie 策略；`tests/deploy/chat-wss-edge-config.test.mjs` 6 passed）
- [x] 定义单实例连接数、握手速率、入站消息速率、出站队列、内存和慢客户端阈值。（验证：`compose.production.yml:152`-`:163` 固定生产阈值；`apps/chat-server/src/chat_server.rs:541` 总连接 permit、`:637` 握手速率 gate；`聊天与邮件系统设计.md:215` 定义 256MiB 容量预算）
- [x] 验证目标实例切换、旧在线 route、实例摘流和连接重建不会把消息推给错误玩家。（验证：`apps/chat-server/src/online_route.rs:61` 原子读取 route/owner，`:184` owner-fenced TTL 续期；`apps/chat-server/src/chat_service.rs:1078` 验证仅 current owner session 可接收且跨实例迁移拒绝，95 项 chat-server 测试通过）
- [x] 增加跨实例投递成功/失败、陈旧路由、连接迁移和积压指标及告警条件。（验证：`apps/chat-server/src/metrics.rs:534` 发布容量/拒绝指标，`apps/chat-server/src/chat_push.rs:141` 维护 publish queue gauge；`docs/安全与监控/监控设计.md:204` 定义实例级告警阈值与处置）
- [x] 在多实例能力完成前，生产部署文档明确限制 chat-server 副本数为 `1`。（验证：`deploy/docker/compose.production.yml:185` 为 `deploy.replicas: 1`，`scripts/docker/server-apply-release.sh:49` 与 `:98` 在更新前后强制副本门禁，生产拓扑与 Docker 运维说明已同步）

## 阶段 9：本地兼容与自动化验证

- 开始时间：2026-07-31 12:48:16 +08:00
- 子任务 2（真实 WSS 联调与故障 drill）开始时间：2026-07-31 13:57:57 +08:00
- 子任务 3（既有邮件故障 drill 修复）开始时间：2026-07-31 15:06:13 +08:00
- 结束时间：2026-07-31 15:52:50 +08:00
- 开发总结：本地默认 TCP 兼容、静态协议检查、真实 Caddy WSS/TCP 并行与跨实例联调全部完成。修复既有邮件可靠性 drill 对 service registry 索引/心跳、Unix socket、SIGKILL 状态及 mail-to-game Ed25519 断言配置的过时假设；原 drill 已在 WSL 原生隔离环境全量通过。真实 WSS 仅使用 Caddy internal CA + loopback，不宣称验证公网域名或公网 CA。
- 验证记录：`cargo fmt --manifest-path apps/chat-server/Cargo.toml --check`、`cargo check`、`cargo clippy --all-targets`（仅既有 warning）、`cargo test`（95 passed）、`npm run check:proto`、`npm run check:mock-client-protocol` 与 WSS fixture（5 passed）均已通过；原生 WSL 隔离 `stage9-wss-integration.mjs` 输出 PASS；修复后 `node --test tests/mail/mail-reliability-fault-drill.test.mjs` 为 11/11 通过，`node --test packages/service-registry/node/registry-schema.test.js` 为 11/11 通过，`node --check` 与 `git diff --check -- tests/mail/mail-reliability-fault-drill.test.mjs` 通过。所有测试数据库、容器、网络、进程、socket、二进制和原生 clone 已清理。

- [x] 验证未设置 WSS 配置时，现有 `scripts/dev-stack.ps1`、`dev-chat.ps1` 和 `9001/TCP` mock-client 流程完全不变。（验证：`git diff --exit-code 2231b8b..HEAD -- scripts/dev-stack.ps1 scripts/dev/services/dev-chat.ps1 tools/mock-client` 通过；`tests/mock-client/mock-client-websocket.test.mjs` 断言默认 TCP，5 passed；chat-server 默认 WSS 关闭测试通过）
- [x] 验证本地显式开启 WSS 后 TCP 与 WS 可以同时认证、发送和接收，且共享连接限制和 session 生命周期。（验证：2026-07-31 WSL 原生隔离 clone（`0727752`）的 `stage9-wss-integration.mjs` 输出 `PASS`；以真实 auth ticket 同时建立 Caddy WSS 与 TCP，覆盖私聊/群聊/历史、玩家/IP 共享连接限额、TCP 迁移和 route owner fencing）
- [x] chat-server 执行 `cargo fmt --manifest-path apps/chat-server/Cargo.toml --check`。（验证：主 agent 于 2026-07-31 12:42 +08:00 复跑通过）
- [x] chat-server 执行 `cargo check --manifest-path apps/chat-server/Cargo.toml`。（验证：主 agent 于 2026-07-31 12:53 +08:00 复跑通过；仅 3 条既有 dead-code warning）
- [x] chat-server 执行 `cargo clippy --manifest-path apps/chat-server/Cargo.toml --all-targets` 并区分既有 warning 与新增问题。（验证：通过；仅 `chat_store::enabled`、`GroupListReq`、`PacketHeader.magic` 的既有 dead-code 和 `metrics.rs` 既有 manual_checked_ops warning）
- [x] chat-server 执行 `cargo test --manifest-path apps/chat-server/Cargo.toml`。（验证：95 passed、0 failed）
- [x] 执行 `npm run check:proto`、`npm run check:mock-client-protocol` 和相关 Node 定向测试。（验证：首次因进程级 `MYSERVER_CLIENT_ROOT=C:\project\mybevy` 不存在失败；覆盖为实际 `H:\project\mybevy` 后 `check:proto` 全部 6 项通过、`check:mock-client-protocol` 通过、WSS fixture 5 passed）
- [x] 经用户确认 Redis、PostgreSQL、NATS、auth-http、chat-server、Caddy 等依赖后，运行真实 WSS 单聊/群聊/邮件通知联调。（验证：2026-07-31 隔离真实服务 harness 通过；Caddy internal CA 的 `wss://localhost`、双 chat-server 跨 NATS 推送、PostgreSQL history 与 mail outbox/NATS 恢复均实际验证，所有专用容器、卷、网络、进程和 WSL clone 已清理）
- [x] 验证 TCP/WSS 并行时既有邮件可靠性故障 drill 不回归，并新增 Caddy/WSS 入口验收证据。（验证：修复 drill 的 registry 正式索引/心跳、Ed25519 断言、Unix socket 清理和 signalCode 判断后，WSL 原生 `node --test tests/mail/mail-reliability-fault-drill.test.mjs` 11/11 通过；`stage9-wss-integration.mjs` 已实际验证 Caddy internal-CA WSS、非 Upgrade 426、1002/1003/1009 关闭码、邮件 outbox/NATS 恢复和 TCP/WSS 并行）

## 阶段 10：灰度、回滚与文档收口

- 开始时间：2026-07-31 15:54:22 +08:00
- 延期时间：2026-07-31 16:23:18 +08:00
- 状态：用户于 2026-07-31 明确接受本阶段外网/灰度/回滚实操延期到后续统一测试；本轮以已有代码、配置、文档与本地隔离验证完成收口，不执行生产操作。
- 结束时间：2026-07-31 16:26:39 +08:00
- 开发总结：发布 bundle、Caddy 入口、auth descriptor、内部 listener 隔离、客户端退避和监控/容量契约已由前序阶段实现并验证；真实公网域名、DNS/Caddy/auth 切换、灰度观察和回滚实操按用户确认延期，不将其描述为已执行。
- 验证记录：`tests/deploy/chat-wss-edge-config.test.mjs`、auth descriptor 回归、mock-client WSS fixture、真实隔离 Caddy internal-CA WSS harness 与邮件故障 drill 均已通过；正式外网验证与灰度/回滚演练为用户接受的后续统一测试项。

- [x] 部署顺序固定为兼容 chat-server -> 内部 WS 验证 -> Caddy/DNS -> auth 公网 descriptor -> mock-client/mybevy 灰度。（验收：代码与设计文档已固化该兼容顺序；DNS/Caddy/auth 的真实切换和客户端灰度按用户确认延期到后续统一测试）
- [x] 灰度期间保留 TCP listener，按客户端版本、WSS 握手率、异常关闭率、消息错误率和重连次数观察。（验收：TCP listener、固定指标与监控阈值已实现；真实灰度观察按用户确认延期）
- [x] 回滚公网 WSS 时先停止 auth 下发或客户端启用，再移除 Caddy 路由；不得先停止仍有客户端使用的 listener。（验收：回滚顺序与客户端退避契约已写入正式设计；真实回滚操作按用户确认延期）
- [x] 明确 DNS、证书、Caddy、auth descriptor、chat-server 和客户端各层回滚步骤及验证命令。（验收：生产拓扑、Docker 发布与聊天设计已覆盖各层边界；公网 DNS/证书与 Caddy 操作验证按用户确认延期）
- [x] 同步整体架构、生产拓扑、外部客户端接入、协议设计、服务发现和聊天邮件设计文档。（验证：前序阶段已同步 `docs/总览/整体架构.md`、生产拓扑、外部客户端接入、协议、服务发现和聊天邮件设计）
- [x] 更新生产初始化、正式 Release、自动发布和端口/防火墙文档，注明 `9011` 仅为容器内端口。（验证：production Compose、release bundle/自动发布文档与生产拓扑均明确 9011 internal-only）
- [x] 确认正式环境不暴露 `9001/9011`，auth 不返回内部 host，Caddy 不转发普通 HTTP 到聊天 listener。（验证：部署静态测试、auth descriptor 回归及此前生产主机只读检查通过；不重复执行外网操作）
- [x] 完成代码、配置、测试、发布脚本、客户端和文档范围核对后，将本清单归档到 `docs/周边服务/checklists/`。（验证：本次归档与提交仅包含 checklist；公网/灰度实操延期说明已保留）

## 最终完成定义

以下项目作为聊天 WebSocket 公网接入的整体完成标准，由全部相关阶段完成后统一验收。

- 开始时间：2026-07-31 16:26:39 +08:00
- 结束时间：2026-07-31 16:26:39 +08:00
- 验收总结：本地、隔离真实服务、协议、容量、跨实例路由、邮件可靠性、Caddy internal-CA WSS 和自动化验证均已完成。用户明确接受正式公网域名、DNS/Caddy/auth 切换、灰度观察、回滚实操及外部 mybevy 公网验收延期到后续统一测试；以下对应条目据此记录为本轮接受完成，不声称已执行外网操作。

- [x] 正式客户端能够通过 `wss://chat.game.zergzerg.cn/` 完成鉴权、单聊、群聊、历史查询和邮件通知接收。（验收：本地真实 Caddy WSS 完成同等业务路径验证；正式公网域名验证按用户确认延期）
- [x] WebSocket 与 TCP 复用同一协议、鉴权、限流、会话和业务实现，不存在行为分叉。（验证：共用 handler、TCP/WSS 并行真实 harness 和 95 项 chat-server 测试通过）
- [x] 本地默认 TCP 流程和既有自动化测试保持可用，WSS 可按配置独立启用验证。（验证：默认 TCP fixture、配置测试和 WSS 独立启用 harness 通过）
- [x] 公网只新增 Caddy `443/TCP` 域名路由，没有公开 chat-server 内部端口。（验证：Compose/edge 静态测试与此前生产主机只读检查通过；不重复执行外网操作）
- [x] auth 下发的是稳定公网 WSS 地址，任何严格/生产响应都不包含 registry 内网 endpoint。（验证：auth descriptor、登录与签票回归测试通过）
- [x] 畸形帧、超限、断线、重连、ticket revoke、慢客户端和 Caddy reload 均有明确且经过验证的行为。（验收：畸形帧、超限、断线、重连、ticket 与队列路径已在本地验证；正式 Caddy reload/证书维护按用户确认延期）
- [x] 单实例容量边界明确；如启用多实例，跨实例在线推送路由已完成并通过故障验证。（验证：生产单副本门禁、容量指标与双实例 Redis/NATS 真实 harness 通过）
- [x] 灰度、回滚、监控、发布脚本和正式文档完整，外部 mybevy 接入通过验收。（验收：文档、监控、发布脚本与 mybevy 单测完成；灰度/回滚实操和 mybevy 公网验收按用户确认延期）
