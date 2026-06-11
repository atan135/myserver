# 安全设计

本文档用于统一描述 MyServer 当前阶段的安全边界、设计原则、落地顺序和验收标准。

目标不是把“未来理想方案”写成“已经完成”，而是把当前已实现能力、明确缺口和下一步要补的安全项拆清楚，供后续开发与文档对齐使用。

相关文档：

- [限流与安全现状](./rate-limit-and-security.md)
- [管理后台设计](./admin-panel.md)
- [协议设计](./protocol.md)
- [帧同步与房间生命周期设计](./game-server-frame-sync-design.md)
- [场景地图格式设计](./game-server-scene-map-format-design.md)
- [底层框架路线图](./game-server-framework-roadmap.md)
- [生产拓扑与 Room 迁移设计](./production-topology-and-room-migration-design.md)

说明：

- 本文中的“当前已实现”以仓库现状为准
- 本文中的“建议新增配置项”只是设计建议，当前不表示代码已经读取
- `docs/prompts/*` 中的提示词不视为正式设计文档，正式口径以本文和现状文档为准

---

## 1. 设计目标

当前安全设计要解决的不是“全行业最高强度安全”，而是当前阶段最容易出事故、最值得先补的基础边界：

1. 保护登录凭证、game ticket、管理员令牌和敏感后台操作
2. 保护玩家长连接入口，避免未鉴权连接、非法包和异常频率请求直接压垮服务
3. 建立服务端权威校验，避免把客户端结果直接当真
4. 让关键操作可追踪、可审计、可回放，出问题后能定位责任和影响范围
5. 明确公网、内网、管理面三类流量的不同安全策略

非目标：

- 当前阶段不实现内核级反作弊
- 当前阶段不实现任意代码热更新
- 当前阶段不自研复杂的应用层加密协议握手
- 当前阶段不引入过重的外部安全基础设施

---

## 2. 安全边界与设计原则

### 2.1 安全边界

本项目至少包含五层边界：

1. 玩家公网 HTTP 边界：生产默认只暴露 `auth-http`
2. 玩家公网长连接边界：生产默认只暴露 `game-proxy`
3. 受控运营入口：`admin-web`、`admin-api`，应通过运营网段、堡垒机、VPN 或独立管理入口访问，不属于玩家公网主入口
4. 内网能力服务边界：`game-server`、`chat-server`、`match-service`、`mail-service`、`announce-service`
5. 数据与凭证边界：Redis、NATS、MariaDB / MySQL、`.env` 密钥、日志与审计库

生产公网暴露总口径以 [生产拓扑与 Room 迁移设计](./production-topology-and-room-migration-design.md) 为准：正式玩家客户端只应依赖 `auth-http` 和 `game-proxy`，其它业务服务默认内网化。本地开发或测试环境可以临时直连内部服务定位问题，但不能作为生产客户端默认模型。

### 2.2 设计原则

1. **服务端权威优先**
   客户端上传的是“输入”和“意图”，不是最终状态；服务端负责鉴权、合法性校验和权威结算。

2. **默认拒绝，最小放行**
   未鉴权连接只能访问极少数白名单消息；管理面、内部控制面默认不暴露到公网。

3. **优先传输层加密，不优先自研包体加密**
   对“协议加密”的首选方案是 HTTPS / TLS / mTLS，而不是先做自定义包头内的 AES/RSA 混合方案。

4. **先审计，再惩罚**
   限流、风控、反作弊、封禁都应尽量带审计事件，方便回溯和复盘。

5. **文档与代码分层对齐**
   已经落地的能力写到“当前现状”；未来配置与能力写到“建议新增”，避免再次出现文档先于实现。

---

## 3. 当前现状

### 3.1 已有安全基础

当前仓库已经具备以下基础能力：

| 模块 | 当前已实现 | 当前缺口 |
|------|------------|----------|
| `auth-http` | IP 限流、Redis 动态 IP / 玩家黑名单、账号锁定、ticket 签发与撤销、维护模式下拦截普通玩家登录和新 game ticket 签发、GM 限时封禁到期后在登录/签票路径惰性恢复账号为 `active`、内部接口可选 service token、安全审计写库；production 下拒绝默认 `TICKET_SECRET`、默认 `GAME_ADMIN_TOKEN` 和空 `INTERNAL_API_TOKEN` | HTTPS/TLS 策略未正式落地；ticket 仍为跨服务复用票据，尚未做用途隔离、换票或重放窗口收敛 |
| `chat-server` | 首包强制鉴权、ticket 签名、过期、Redis ticket 归属与 ticket version 校验、心跳超时、最大包体限制、单连接消息频率限制、单 IP / 单账号本实例连接数限制、有界出站写队列与慢连接背压、在线推送与基础运行指标；production 下拒绝默认或空的 `TICKET_SECRET` | 没有跨实例全局连接数限制、账号级消息频率限制和公网 TLS 策略；生产不作为客户端直连默认入口 |
| `mail-service` | HTTP 路由参数校验、玩家读邮件/详情/标记已读/领取附件复用 game ticket，校验签名、过期、Redis ticket 归属和 ticket version，query/body `player_id` 只能与认证身份一致，邮件详情和领取均校验邮件归属；系统发信入口要求 `MAIL_SERVICE_TOKEN`；过期校验、附件格式校验、领取幂等、基础 HTTP 指标；production 下拒绝默认 `TICKET_SECRET`、默认 `MAIL_SERVICE_TOKEN` 或关闭 `MAIL_PLAYER_AUTH_REQUIRED` | 仍未网关化；共享 ticket 尚未做 mail 专用用途隔离/换票；缺少发信 RBAC、审批流、发信审计查询和公网 HTTPS/TLS 策略 |
| `announce-service` | HTTP 查询参数与公告载荷基础校验；只读 `GET /api/v1/announcements...` 默认要求 `ANNOUNCE_READ_TOKEN` 或 game ticket，game ticket 校验签名、过期、Redis ticket 归属和 ticket version；写接口 `POST/PUT/DELETE /api/v1/announcements...` 已通过 `ANNOUNCE_ADMIN_TOKEN` 做 token 鉴权；基础 HTTP 指标；production 下拒绝关闭 `ANNOUNCE_READ_AUTH_REQUIRED`、默认/弱 `ANNOUNCE_ADMIN_TOKEN`、默认/弱 `TICKET_SECRET`，并拒绝默认/弱或与 admin token 相同的 `ANNOUNCE_READ_TOKEN` | 仍未网关化；共享 ticket 尚未做 announce 专用用途隔离/换票；缺少公告读写 RBAC、审计查询和公网 HTTPS/TLS 策略 |
| `game-proxy` | `AuthReq` 本地 ticket 签名与 Redis 存在性校验、鉴权前消息白名单、单连接预鉴权失败阈值、单连接入站消息频率限制、总连接上限、静态 IP denylist、Redis 动态 IP / 玩家黑名单、单 IP / 单玩家本地连接上限、本地维护开关与 Redis 共享维护模式拦截新 `AuthReq`、接入转发、连接数统计；admin HTTP 口已有 token 鉴权、全权限写 token、只读 token、第一阶段 scoped token 操作级 RBAC、生产默认/弱 token 拒绝、写操作基础输入校验、权限拒绝审计、`X-Admin-Actor` 操作人解析、结构化日志和 JSONL 持久审计 | 成熟的公网加密方案尚未落地；尚未做多 proxy 全局连接限额；proxy admin scoped token RBAC 尚未数据库化或集中策略化，仍缺审批流、审计查询、集中留存、按资源范围授权和统一 trace/request id，多 proxy route store 强一致仍未完全闭环 |
| `game-server` | ticket 签名与 Redis 归属校验、鉴权前消息白名单、心跳超时、最大包体限制、单连接消息频率限制、本实例内单玩家消息频率限制、玩家输入 client timestamp 可配置窗口校验、玩家输入重复内容/过期帧/未来帧/时间戳异常的本实例短窗口计数与可配置拒绝、连接审计、基础权威移动校正、NATS GM 广播订阅并向本实例在线连接推送、NATS session kick 订阅并断开本实例目标玩家连接；admin TCP 敏感写操作 JSONL 审计、可选 actor 强制、写操作前审计文件可写性门禁；internal socket 敏感控制动作结构化 tracing 审计；production 下拒绝默认或空的 `TICKET_SECRET`、`GAME_ADMIN_TOKEN`、`GAME_INTERNAL_TOKEN` 并拒绝关闭 admin 审计 | 没有单 IP 频率限制、跨实例全局玩家频率限制和通用作弊计数；输入异常阈值当前只拒绝后续输入、不主动断开连接 |
| `admin-api` / `admin-web` | JWT 鉴权、管理员密码哈希、Redis 管理员 session/jti 校验、登出撤销、管理员状态实时校验、登录失败锁定、安全审计、后端第一阶段权限矩阵、监控接口鉴权、可信代理 IP 解析、请求级 HTTPS/TLS 强制、来源 IP allowlist、管理员 token 批量撤销、重置密码联动 token version 失效、维护模式共享状态写入、GM 广播通过 NATS 跨实例推送、GM 踢人/封禁通过 NATS session kick 跨实例断开在线连接、GM 限时封禁写入 `ban_expires_at` | 代码侧 TLS/allowlist 不能替代生产网络隔离；权限仍未数据库化，缺少按资源范围授权、权限变更审计查询和审批流 |

说明：

- 当前同一张 ticket 会被 `game-proxy`、`game-server`、`chat-server`、`mail-service`、`announce-service` 复用
- 当前 `game-proxy`、`game-server`、`chat-server`、`mail-service` 与 `announce-service` 都会检查 Redis ticket 记录和 `player-ticket-version:<playerId>`；`chat-server`、`mail-service` 和 `announce-service` 对单张 ticket revoke 已具备精确感知
- 因此不能简单采用“任一服务首次校验成功后立即删除 Redis ticket 记录”的全局单次消费模型
- 如果后续要进一步降低重放风险，更合理的方向是短 TTL、用途隔离、分服务换票，或显式的重放窗口控制
- 维护模式共享状态位于 `${REDIS_KEY_PREFIX}maintenance:global`。开启后 `auth-http` 拦截普通玩家登录和新 game ticket 签发，`game-proxy` 拦截新 `AuthReq`；它不是在线踢人机制，已有在线连接不被主动断开

### 3.2 当前口径

当前正式现状应以这些文档为准：

- [限流与安全现状](./rate-limit-and-security.md)：已实现的限流、ticket、安全边界
- [管理后台设计](./admin-panel.md)：管理员鉴权、审计表、监控接口现状

本文在此基础上补的是“统一设计与后续落地口径”，不是重复定义现状。

---

## 4. 资产与威胁模型

### 4.1 核心资产

需要重点保护的对象包括：

- 管理员账号、玩家账号、游客身份
- `JWT_SECRET`、`TICKET_SECRET`、内部 service token
- game ticket、后台访问 token、配置更新权限
- 房间状态、玩家输入、道具与背包、邮件与奖励
- 审计日志、安全事件、封禁与白名单策略

### 4.2 主要风险

当前阶段最现实的风险包括：

1. 明文传输导致 token / ticket / 管理凭证泄露
2. 未鉴权连接或非法包频繁打入，压垮登录服、代理或游戏服
3. `mail-service` / `announce-service` 这类内网能力服务如果被误作为客户端直连 HTTP 入口，仍需要明确 TLS、网关和限流边界；其中 `mail-service` 已有第一阶段玩家 ticket 鉴权和发信 service token，`announce-service` 只读查询已有第一阶段 read token 或 game ticket 鉴权，但仍未完成网关化、用途隔离和公网 TLS 策略
4. 客户端伪造位置、帧号、时间戳、房间状态或业务结果
5. 敏感后台操作缺少完整审计，出现误操作后无法追踪
6. 管理口、监控口、Redis、MySQL 等控制面被误暴露到公网
7. 黑白名单和连接上限缺失，异常来源无法快速止血

---

## 5. 数据加密（协议加密）

### 5.1 设计结论

“协议加密”在本项目中优先解释为**传输链路加密**，而不是自研应用层加密。

当前建议：

1. 对外 HTTP 统一走 HTTPS
2. 管理控制面与内部服务调用优先走 TLS 或私网 + service token
3. 玩家公网长连接入口优先在 `game-proxy` 或反向代理层做安全传输封装
4. 不在第一版里发明自定义包头加密位、会话密钥协商和包体对称加密

原因：

- 自研应用层加密很容易把密钥协商、重放保护、重连恢复和兼容性复杂度一起带进来
- 当前协议头和 Protobuf 结构本身并不妨碍使用 TLS
- 当前最需要保护的是凭证与控制面，而 TLS 能直接覆盖这部分风险

### 5.2 各链路目标

| 链路 | 当前现状 | 目标策略 |
|------|----------|----------|
| 客户端 -> `auth-http` | 开发期可明文 HTTP | 生产必须 HTTPS |
| 客户端 -> `mail-service` | 本地/测试可直连明文 HTTP；玩家接口默认要求 game ticket，发信入口要求 `MAIL_SERVICE_TOKEN` | 生产不作为客户端直连默认入口；若临时暴露，必须 HTTPS，并继续收敛到可信网关、用途隔离 ticket 或换票模型 |
| 客户端 -> `announce-service` | 本地/测试可直连明文 HTTP；只读查询默认要求 `ANNOUNCE_READ_TOKEN` 或 game ticket；写接口要求 `ANNOUNCE_ADMIN_TOKEN` header | 生产不作为客户端直连默认入口；若临时暴露，必须 HTTPS，并继续收敛到可信网关、用途隔离 ticket 或换票模型，后台 CRUD 继续与玩家读取路径隔离 |
| 浏览器 -> `admin-web` / `admin-api` | 当前未强制 HTTPS | 生产必须 HTTPS，Bearer token 只允许在 TLS 下使用，并限制在运营网段、堡垒机、VPN 或独立管理入口 |
| 客户端 -> `chat-server` TCP | 本地/测试可直连明文 TCP | 生产不作为客户端直连默认入口；若临时暴露，必须在入口层做 TLS 终止或由 `chat-server` 直接支持 TLS |
| 客户端 -> `game-proxy` TCP fallback | 当前明文 TCP | 生产建议在入口层做 TLS 终止，或由 `game-proxy` 直接支持 TLS |
| 客户端 -> `game-proxy` KCP | 当前无正式加密策略 | 生产不建议裸奔公网；保留为后续专项，优先用安全隧道或替换为具备成熟加密方案的入口 |
| `game-proxy` -> `game-server` | 同机可走 UDS / 本地 TCP | 同机可维持本地链路；跨机部署时转为 TLS 或严格私网 |
| `mail-service` -> `game-server` admin 通道 | 当前依赖网络隔离和 `GAME_ADMIN_TOKEN`，发奖写操作会在 `AdminAuthReq` 中携带合法 service actor 供 game-server admin 审计使用；Node client 已限制连接/写入/读取超时和最大响应字节数 | 后续可升级 mTLS / 更细 service identity |
| `admin-api` -> `game-server` admin 通道 | 当前依赖网络隔离和 `GAME_ADMIN_TOKEN`，写操作会透传当前管理员 actor；Node client 已限制连接/写入/读取超时和最大响应字节数 | 后续可升级 mTLS / 更细 service identity |
| 内部 gRPC / HTTP 调用 | 当前默认内网互信 | 开发期 service token，正式环境逐步升级 mTLS |

### 5.3 密钥管理

当前阶段密钥管理遵循：

- 密钥只放 `.env` 或等价注入方式
- 不把明文 token、ticket、密码写入数据库
- 日志中不打印完整 token / ticket / secret
- 生产环境必须替换默认示例密钥
- `auth-http` 在 `NODE_ENV=production` 或 `APP_ENV=production` 时会在配置加载阶段 fail fast：默认或空的 `TICKET_SECRET`、默认或空的 `GAME_ADMIN_TOKEN`、空的 `INTERNAL_API_TOKEN` 都会拒绝启动。`AUTH_STRICT_SECURITY` 仍控制内部接口请求期缺 token 时的拒绝行为，但生产环境不再等到请求阶段才暴露空 token 配置错误。
- `game-server` 在 `NODE_ENV=production` 或 `APP_ENV=production` 时也会在配置加载阶段 fail fast：默认或空的 `TICKET_SECRET`、`GAME_ADMIN_TOKEN`、`GAME_INTERNAL_TOKEN` 都会拒绝启动，`GAME_ADMIN_AUDIT_ENABLED=false` 也会拒绝启动。该保护是内网服务的凭证和审计基线，不表示 `game-server` 应作为生产公网入口暴露。
- `chat-server` 在 `NODE_ENV=production` 或 `APP_ENV=production` 时会在配置加载阶段 fail fast：默认、空值或明显占位的 `TICKET_SECRET` 会拒绝启动。该保护要求聊天服与 ticket 签发侧使用一致的真实密钥，但不改变 `chat-server` 默认内网化的部署边界。
- `mail-service` 在 `NODE_ENV=production` 或 `APP_ENV=production` 时会在配置加载阶段 fail fast：默认、空值或明显占位的 `TICKET_SECRET` / `MAIL_SERVICE_TOKEN` 会拒绝启动，且 `MAIL_PLAYER_AUTH_REQUIRED` 必须为 `true`。`GAME_ADMIN_TOKEN` 仍只用于 mail-service 调用下游 `game-server admin`，不能作为 mail 发信入口 token 复用。
- `announce-service` 在 `NODE_ENV=production` 或 `APP_ENV=production` 时会在配置加载阶段 fail fast：默认、空值或明显占位的 `ANNOUNCE_ADMIN_TOKEN` / `TICKET_SECRET` 会拒绝启动，`ANNOUNCE_READ_AUTH_REQUIRED` 必须为 `true`；如果配置了 `ANNOUNCE_READ_TOKEN`，也必须是非默认强值，且不能与 `ANNOUNCE_ADMIN_TOKEN` 相同。

建议后续补充：

- 支持密钥轮换窗口，例如“当前密钥 + 旧密钥短暂兼容”
- 为 service token 增加版本号或 key id
- 统一记录密钥轮换操作的审计日志

### 5.4 不建议当前阶段采用的方案

当前不建议优先做：

- 自定义包头 `flags` 中直接塞“加密位”并发明自定义对称加密流程
- 在客户端与服务端之间做仅靠固定密钥的包体加密
- 把 KCP 裸协议直接长期开到公网而无额外安全封装

---

## 6. 反作弊基础（客户端校验）

### 6.1 基本立场

Todo 里的“客户端校验”不能理解成“相信客户端”。更合理的口径是：

- 客户端可以上报辅助信息
- 服务端必须做权威校验
- 客户端校验只作为体验优化、异常诊断和辅助证据

### 6.2 反作弊基础分层

#### A. 鉴权前白名单

未鉴权连接只能访问少数必要消息，例如：

- 握手消息
- 鉴权消息
- 必要心跳消息

未鉴权状态下，禁止房间、移动、战斗、背包、GM 等任何业务消息。

#### B. 连接与消息频率

需要至少在 `game-proxy`、`chat-server` 和 `game-server` 这些玩家长连接入口做对应限制；其中对局类输入的强校验仍以 `game-server` 为主：

- 单 IP 建链频率
- 单 IP 在线连接数
- 单玩家并发连接数
- 单连接消息频率
- 单玩家单位时间输入数
- 连续非法包 / 解析失败次数

#### C. 帧号、时间戳与序列校验

对帧同步和状态同步类请求，需要统一校验：

- `frame_id` 不允许无限超前
- 迟到帧和过期帧要有明确处理规则
- `client_timestamp_ms` 已在 `PlayerInputReq` / `MoveInputReq` 落地可配置窗口校验，默认兼容旧客户端；`INPUT_TIMESTAMP_REQUIRED=true` 时缺失或为 `0` 会被拒绝
- `PlayerInputReq` / `MoveInputReq` 已对重复内容、过期帧、未来帧和时间戳异常做本实例短窗口计数；默认 `INPUT_ANOMALY_MAX=0` 只观测，配置阈值后返回 `INPUT_ANOMALY_BLOCKED`
- 请求 `seq` 需要可追踪，方便检测重复包和重放可疑行为

#### D. 服务端权威状态校验

当前框架里，位移校正已经具备基础权威链路；后续要把这个思路扩展到所有高风险业务：

- 位移：速度上限、碰撞、阻挡、越界、传送合法性
- 房间：成员身份、房间阶段、准备状态、观战身份
- 背包：物品存在性、数量、绑定规则、目标槽位合法性
- 战斗：冷却、资源消耗、目标合法性、伤害结算归属
- 运营接口：GM 权限、目标玩家状态、重复操作幂等

#### E. 异常计数与惩罚

建议所有异常输入统一进入“作弊计数”或“异常计数”模型，而不是散落在各模块中各自处理。

建议至少定义这些事件：

- `invalid_msg_type`
- `msg_rate_exceeded`
- `chat_msg_rate_exceeded`
- `packet_too_large`
- `frame_out_of_window`
- `timestamp_skew`
- `movement_speed_exceeded`
- `scene_collision_blocked`
- `duplicate_login`
- `replay_suspected`

建议的惩罚梯度：

1. 记录审计事件
2. 临时降级或限频
3. 断开连接
4. 短时封禁
5. 人工复核后长期封禁

### 6.3 客户端需要配合但不能被信任的字段

客户端后续可补充上传：

- `client_frame_id`
- `client_timestamp_ms`
- 客户端预测位置
- 可选的输入摘要或本地状态摘要

这些字段的用途是：

- 帮助服务端做窗口校验
- 便于回滚与权威纠偏
- 便于分析高延迟与异常行为

它们不应被直接视为最终状态来源。

### 6.4 当前阶段应优先补的反作弊能力

1. 鉴权前消息白名单已在 `game-proxy` 与 `game-server` 落地
2. 单连接消息频率限制和本实例内单玩家消息频率限制已在 `game-server` 落地；单 IP 频率限制和跨实例全局玩家频率限制仍需补齐
3. `frame_id` 超前 / 过期，以及连续相同输入重复处理已在 `game-server` 玩家输入链路接入本实例异常计数；默认观测，配置阈值后拒绝后续输入
4. `client_timestamp_ms` 时间窗校验已在 `game-server` 落地，并纳入玩家输入异常计数；后续可继续补审计聚合和跨实例处置
5. 连续非法包计数与断连
6. 位移异常统一审计事件
7. 共享 ticket 的重放窗口收敛，必要时演进为用途隔离或分服务换票模型

---

## 7. 敏感操作审计

### 7.1 审计目标

所有“能改变状态、能扩大权限、能影响玩家资产或在线流量”的操作，都应有审计。

审计要回答四个问题：

1. 谁做的
2. 对谁做的
3. 做了什么
4. 结果是什么

### 7.2 审计分类

当前建议继续保留三类日志流：

1. `admin_audit_logs`
   记录后台管理操作
2. `security_audit_logs`
   记录安全事件、风控事件、封禁与异常
3. `game_connection_audit_logs`
   记录连接、鉴权、重连、断开等网络链路事件

### 7.3 必须覆盖的敏感操作

以下操作必须具备审计：

- 管理员登录、登出、失败登录
- 管理员 token 批量撤销
- 管理员密码重置
- 玩家状态修改
- GM 广播、发道具、踢人、封禁
- 维护模式开关；当前 `admin-api` 写 Redis 共享状态并记录 `admin_audit_logs`
- 配置热更新、运行时参数调整、回滚
- game ticket 撤销
- service token 校验失败
- 非法包、超频、时间戳异常、重放嫌疑
- 大量认证失败、账号锁定、IP 限流命中

### 7.4 审计字段要求

当前已有表结构已经覆盖了基础字段，但后续建议逐步补齐以下信息：

- `request_id` 或 `trace_id`
- 操作者角色
- 来源服务名
- 操作前后值摘要
- 执行结果：`success` / `failed` / `rejected`
- 原因码
- 目标实体类型和主键

对于敏感字段，必须脱敏：

- 不记录明文密码
- 不记录完整 access token / JWT / game ticket
- 如确有定位需要，只记录哈希值或前缀

### 7.5 审计保留策略

默认建议：

- 热数据保留 30 天
- `critical` 级安全事件可归档延长
- 管理操作和封禁相关日志优先保留更久

### 7.6 当前必须补的控制面缺口

在现有后台设计上，至少应补齐：

1. 监控接口鉴权
2. 后端接口角色校验真正生效
3. 配置热更新与 admin TCP 操作的审计闭环
4. 审计日志中的失败结果和原因码统一

当前 GM 广播、踢人和封禁的 `game-server` 在线连接处置已接入 admin TCP handler。GM 广播主路径由 `admin-api` 发布 NATS `myserver.gm.broadcast`，跨实例推送在线连接；NATS 发布成功时跳过 legacy 单实例 admin TCP，发布失败时才 fallback 并在响应和审计中记录 `globalBroadcast` / `legacyBroadcast` 结果。GM 踢人/封禁同时由 `admin-api` 发布 NATS session kick，跨实例断开在线连接，legacy 单实例 admin TCP 结果会进入审计。GM 封禁也已由 `admin-api` 写入账号持久状态和 `ban_expires_at`。限时封禁不依赖常驻定时器，`auth-http` 在登录和签票路径惰性恢复过期封禁。剩余安全缺口是更细粒度权限矩阵和失败原因标准化的持续收敛。

---

## 8. 防火墙 / 黑白名单

### 8.1 分层思路

这项不能只理解成“系统防火墙”。本项目需要同时做三层：

1. **网络层**
   通过端口绑定、系统防火墙、云安全组控制谁能连进来
2. **接入层**
   通过 `game-proxy` / `chat-server` / `auth-http` / `mail-service` / `announce-service` / `admin-api` 做 IP allowlist / denylist 和连接上限
3. **协议层**
   通过消息白名单、鉴权前白名单、维护模式控制新入口能否继续认证

### 8.2 网络层要求

默认要求如下：

- Redis、MySQL 不直接暴露到公网
- `game-server` admin 端口不对公网开放
- `game-proxy` admin 端口不对公网开放
- `mail-service`、`announce-service`、`chat-server` 默认不作为生产公网入口；如果临时对公网开放，必须明确区分玩家入口与后台/运维入口，并补齐鉴权和 TLS
- `admin-api` 生产环境仅允许运营网段或堡垒机访问
- 本地开发环境默认绑定 `127.0.0.1` 或私有地址

### 8.3 接入层要求

`auth-http`、`game-proxy`、`chat-server`、`mail-service`、`announce-service`、`admin-api` 都应支持相应边界内的访问控制；其中玩家公网入口优先覆盖 `auth-http` 和 `game-proxy`，内网服务和控制面优先覆盖 allowlist、service token 与网络隔离：

- IP denylist：紧急封禁可疑来源
- IP allowlist：管理面和内部控制面优先使用
- 单 IP 连接数上限
- 单 IP 请求频率上限
- 单账号 / 单玩家并发连接上限
- 维护模式下的新登录、新签票和新游戏接入拦截；如后续需要白名单通行，应在 `auth-http` 和 `game-proxy` 同步设计

### 8.4 协议层白名单

协议层至少需要两类白名单：

1. 鉴权前消息白名单
   未鉴权时只放行极少数协议
2. 管理操作白名单
   内部控制面只接受显式列出的管理操作，不接受任意命令透传

### 8.5 黑白名单数据源

当前推荐两级来源：

1. 静态配置
   用于默认网段限制、开发环境固定白名单
2. Redis 动态集合
   用于临时封禁、运营快速止血和跨实例同步

推荐优先级：

1. 管理面先做 allowlist
2. 玩家入口先做 denylist + 限流 + 连接上限
3. 封禁策略应可带 TTL，避免手工清理遗漏

---

## 9. 建议新增配置项

以下配置项为**建议新增**，当前不表示代码已实现读取。

新增前应同步更新现状文档，避免再次出现“文档里有、代码里没有”的情况。

### 9.1 公共配置

```env
PUBLIC_TLS_REQUIRED=false
SERVICE_SHARED_TOKEN=
SECURITY_AUDIT_RETENTION_DAYS=30
ADMIN_IP_ALLOWLIST=
SECURITY_DENYLIST_REDIS_PREFIX=security:denylist:
SECURITY_ALLOWLIST_REDIS_PREFIX=security:allowlist:
```

### 9.2 `game-proxy` / 接入层

当前 `game-proxy` 已读取：

```env
PROXY_ADMIN_TOKEN=dev-only-change-this-proxy-admin-token
# PROXY_ADMIN_READ_TOKEN=dev-only-change-this-proxy-admin-read-token
# PROXY_ADMIN_SCOPED_TOKENS=maint-token:proxy.maintenance.write;route-token:proxy.route.write,proxy.read
PROXY_ADMIN_AUDIT_ENABLED=true
PROXY_ADMIN_AUDIT_PATH=logs/game-proxy/admin-audit.jsonl
PROXY_ADMIN_AUDIT_REQUIRE_ACTOR=false
PROXY_MAX_CONNECTIONS=0
PROXY_MAX_PREAUTH_FAILURES=3
PROXY_MSG_RATE_WINDOW_MS=1000
PROXY_MSG_RATE_MAX=0
PROXY_REDIS_BLOCKLIST_ENABLED=false
PROXY_REDIS_BLOCKLIST_CACHE_TTL_MS=2000
PROXY_IP_DENYLIST=
PROXY_MAX_CONNECTIONS_PER_IP=0
PROXY_MAX_CONNECTIONS_PER_PLAYER=0

AUTH_REDIS_BLOCKLIST_ENABLED=false
AUTH_REDIS_BLOCKLIST_CACHE_TTL_MS=2000
```

说明：

- `PROXY_ADMIN_TOKEN` 用于保护 `game-proxy` admin HTTP 口并允许所有 GET/POST，当前支持 `Authorization: Bearer <token>` 和 `X-Admin-Token: <token>`；可选 `PROXY_ADMIN_READ_TOKEN` 仅允许 GET。`PROXY_ADMIN_SCOPED_TOKENS` 支持额外受限 token，格式为 `token:permission1,permission2;token2:permission3`，当前权限包括 `proxy.read`、`proxy.maintenance.write`、`proxy.rollout.write`、`proxy.route.write`、`proxy.write` 和 `*`。`NODE_ENV=production` 或 `APP_ENV=production` 时，写 token 为空或仍为明显默认值会导致配置加载失败；读 token 如果设置，也不能为空、明显默认或与写 token 相同；scoped token 会拒绝空值、明显默认值、重复值、未知权限，生产下还会拒绝明显弱 token。
- `PROXY_MAX_CONNECTIONS=0` 表示不限制总前端连接数；配置为正整数时，超过上限的新连接会在 session 开始时拒绝。
- `PROXY_MAX_PREAUTH_FAILURES=3` 表示同一连接在鉴权成功前，非法消息或鉴权失败累计达到阈值后关闭连接；配置为 `0` 表示不按失败次数断开。
- `PROXY_MSG_RATE_MAX=0` 表示关闭单连接入站消息频率限制；配置为正整数时，`game-proxy` 在读到完整 packet 后、进入本地鉴权 / 预鉴权白名单 / 上游转发前按 `PROXY_MSG_RATE_WINDOW_MS` 窗口计数，超限返回 `ErrorRes(MSG_RATE_EXCEEDED)`，当前不断开连接且不计入预鉴权失败次数。
- `PROXY_REDIS_BLOCKLIST_ENABLED=false` 表示默认关闭 Redis 动态黑名单；配置为 `true` 后，连接建立早期检查 `${REDIS_KEY_PREFIX}security:blocklist:ip:<ip>`，`AuthReq` 本地 ticket 校验成功后检查 `${REDIS_KEY_PREFIX}security:blocklist:player:<player_id>`。key 存在即封禁；JSON 值可携带 `until` 毫秒时间戳，已过期则视为未封禁。启用后 Redis 查询失败按安全优先 fail-closed，返回 `BLOCKLIST_UNAVAILABLE` 并拒绝新连接或鉴权。
- `PROXY_REDIS_BLOCKLIST_CACHE_TTL_MS=2000` 控制 Redis 动态黑名单短缓存；当前只在连接建立和 `AuthReq` 阶段查询，不按每个 packet 查询。
- `AUTH_REDIS_BLOCKLIST_ENABLED=false` 表示默认关闭 auth-http Redis 动态黑名单；配置为 `true` 后，登录入口早期检查 IP，登录成功且拿到 `playerId` 后、创建 session / ticket 前检查玩家，`/api/v1/game-ticket/issue` 在签发前再次检查玩家。auth-http 与 game-proxy 共用 `${REDIS_KEY_PREFIX}security:blocklist:ip:<ip>` 和 `${REDIS_KEY_PREFIX}security:blocklist:player:<player_id>`，并采用相同 `until` 过期语义。启用后 Redis 查询失败返回 `BLOCKLIST_UNAVAILABLE` 并拒绝登录或签票。
- `AUTH_REDIS_BLOCKLIST_CACHE_TTL_MS=2000` 控制 auth-http 黑名单短缓存；当前不影响健康检查、metrics 或内部接口。
- `PROXY_IP_DENYLIST` 是逗号分隔的静态 IP 或 CIDR 列表，命中的来源会在 session 建立初期被拒绝；为空表示不启用。
- `PROXY_MAX_CONNECTIONS_PER_IP=0` 表示不限制单来源 IP 并发连接数；配置为正整数时，超过上限的新连接会被拒绝，连接关闭时释放计数。
- `PROXY_MAX_CONNECTIONS_PER_PLAYER=0` 表示不限制单玩家已鉴权并发连接数；配置为正整数时，`AuthReq` 本地鉴权成功后会登记玩家连接，超过上限返回 `AuthRes(ok=false, error_code=PLAYER_CONNECTION_LIMIT_EXCEEDED)`，连接关闭或重复鉴权切换玩家时释放旧计数。
- proxy admin 写接口会解析 `X-Admin-Actor`，记录结构化日志审计，并在 `PROXY_ADMIN_AUDIT_ENABLED=true` 时追加 JSONL 持久审计。审计包含 action、操作人、关键目标和 ok/error 结果，不记录 token；缺失 actor 默认记录为 `unknown`，`PROXY_ADMIN_AUDIT_REQUIRE_ACTOR=true` 时拒绝缺失 actor 的写操作。审计文件不可写时，已授权写操作返回 `500` 并记录 warning；权限不足返回 `403 insufficient admin permission`，写操作拒绝会尽量写入 `error=insufficient_permission` 审计，审计失败时至少 structured warn。当前尚未接入 MySQL 审计查询、集中留存、数据库化权限策略、审批流或按资源范围授权。proxy admin 口生产环境仍必须内网隔离，审计和 RBAC 不是公网暴露依据。

仍属于设计目标、当前未读取的接入层配置示例：

```env
CONNECTION_RATE_WINDOW_MS=10000
CONNECTION_RATE_MAX=30
PREAUTH_MSG_ALLOWLIST=
INVALID_PACKET_THRESHOLD=10
IP_ALLOWLIST_ENABLED=false
SECURITY_DENYLIST_REDIS_PREFIX=security:denylist:
```

### 9.3 `game-server` / 业务层

当前已读取并生效的消息频率配置：

```env
MSG_RATE_WINDOW_MS=1000
MSG_RATE_MAX=0
PLAYER_MSG_RATE_WINDOW_MS=1000
PLAYER_MSG_RATE_MAX=0
```

其中 `PLAYER_MSG_RATE_MAX=0` 默认关闭；配置为正整数时，仅限制当前 `game-server` 实例内同一 `player_id` 的合计消息数，不是跨实例全局限额。`NODE_ENV=production` 或 `APP_ENV=production` 时，`game-server` 会拒绝默认或空的 `TICKET_SECRET`、`GAME_ADMIN_TOKEN`、`GAME_INTERNAL_TOKEN`，要求它们替换为高强度生产值；同时拒绝 `GAME_ADMIN_AUDIT_ENABLED=false`。由于生产链路里 `game-server` 通常通过 `game-proxy` 本地 socket 接入，单 IP 频率限制仍应放在 proxy、网关或后续透传协议层；上述 fail-fast 只是内网服务凭证与审计基线保护，不改变 `game-server` 默认内网化的部署边界。

`game-server` admin TCP 第一阶段审计配置：

```env
GAME_ADMIN_AUDIT_ENABLED=true
GAME_ADMIN_AUDIT_PATH=logs/game-server/admin-audit.jsonl
GAME_ADMIN_AUDIT_REQUIRE_ACTOR=false
```

`AdminAuthReq` 继续兼容旧的纯 token body。需要追踪具体操作人时，可以改为 JSON envelope：`{"token":"...","actor":"ops@example.com"}`。旧格式或空 actor 会在审计中记录为 `actor=unknown`、`actor_missing=true`。`admin-api` 调用 `game-server` 内网 admin TCP 写操作时会优先透传当前后台管理员 `username`，没有合法 username 时使用 `admin-<sub>`；actor 必须满足 `[A-Za-z0-9._@-]` 且最长 128 字符，非法值会回退为缺失 actor。当 `GAME_ADMIN_AUDIT_REQUIRE_ACTOR=true` 时，`AdminUpdateConfigReq`、GM 写操作、房间迁移 freeze/export/import/retire 和 redirect 等写操作缺 actor 会在状态变更前被拒绝；`AdminServerStatusReq`、`GetRolloutDrainStatusReq` 等只读状态查询不因缺 actor 被拒绝。JSONL 审计只记录目标摘要、seq/message_type、结果和错误码，不记录 token、ticket、密码、完整 payload 或 room transfer payload。internal socket 不要求人类 actor，但 transfer/redirect 等服务间控制动作会输出 `channel=internal_socket`、`actor=internal_service` 的结构化 tracing 审计日志。

仍属于设计目标、当前未读取的业务层配置示例：

```env
FRAME_LEAD_LIMIT=3
FRAME_LAG_LIMIT=30
CLIENT_TIMESTAMP_SKEW_MS=5000
CHEAT_STRIKE_KICK_THRESHOLD=5
CHEAT_STRIKE_BAN_THRESHOLD=20
TICKET_REPLAY_WINDOW_SECS=300
```

### 9.4 `chat-server` / 内部聊天服务

当前已读取并生效的消息频率配置：

```env
CHAT_MSG_RATE_WINDOW_MS=1000
CHAT_MSG_RATE_MAX=0
CHAT_MAX_CONNECTIONS_PER_PLAYER=0
CHAT_MAX_CONNECTIONS_PER_IP=0
```

`CHAT_MSG_RATE_MAX=0` 默认关闭，避免影响本地联调；生产部署可配置为正整数。限制发生在认证后、读到完整 packet 并解析出 `MessageType` 后、进入单聊、群聊、群组和历史查询 handler 前。超限返回 `ErrorRes(MSG_RATE_EXCEEDED)`，当前不断开连接。该限制是单连接本地状态。

`CHAT_MAX_CONNECTIONS_PER_PLAYER=0` 和 `CHAT_MAX_CONNECTIONS_PER_IP=0` 默认关闭；配置为正整数时，在 `ChatAuthReq` ticket 校验通过后、注册 session / online route 前限制当前 `chat-server` 实例内连接数，超限分别返回 `PLAYER_CONNECTION_LIMIT_EXCEEDED` 或 `IP_CONNECTION_LIMIT_EXCEEDED` 的失败 `ChatAuthRes` 并关闭新连接。该限制不是跨实例全局限额，在线推送仍沿用现有 `player_id -> sender` session map 行为。`NODE_ENV=production` 或 `APP_ENV=production` 时，`chat-server` 会拒绝默认或空的 `TICKET_SECRET`。

### 9.5 `admin-api` / 控制面

```env
ADMIN_API_REQUIRE_TLS=false
ADMIN_API_REQUIRE_IP_ALLOWLIST=false
ADMIN_API_IP_ALLOWLIST=
ADMIN_MONITORING_REQUIRE_AUTH=true
ADMIN_ENFORCE_ROLE_CHECK=true
ADMIN_SESSION_TTL_SECONDS=28800
ADMIN_LOGIN_MAX_FAILURES=5
ADMIN_LOGIN_FAILURE_WINDOW_SECONDS=900
ADMIN_LOGIN_LOCK_SECONDS=900
TRUST_PROXY=false
TRUSTED_PROXIES=
```

当前 `admin-api` 已读取 `ADMIN_SESSION_TTL_SECONDS`、`ADMIN_LOGIN_MAX_FAILURES`、`ADMIN_LOGIN_FAILURE_WINDOW_SECONDS`、`ADMIN_LOGIN_LOCK_SECONDS`、`TRUST_PROXY`、`TRUSTED_PROXIES`、`ADMIN_API_REQUIRE_TLS`、`ADMIN_API_REQUIRE_IP_ALLOWLIST` 和 `ADMIN_API_IP_ALLOWLIST`。`ADMIN_SESSION_TTL_SECONDS` 未配置时跟随 `JWT_EXPIRES_IN` 解析出的秒数；`ADMIN_API_REQUIRE_TLS` 开发默认 `false`，`NODE_ENV=production` 默认 `true`；`ADMIN_API_REQUIRE_IP_ALLOWLIST` 默认 `false`，启用后 `ADMIN_API_IP_ALLOWLIST` 支持精确 IP 和 IPv4 CIDR。`TRUST_PROXY=true` 仍要求直连来源显式列在 `TRUSTED_PROXIES` 后才信任 `X-Forwarded-For` 和 `X-Forwarded-Proto`。`NODE_ENV=production` 下明显默认的 `JWT_SECRET`、`GAME_ADMIN_TOKEN` 或 `ADMIN_PASSWORD` 会导致配置加载失败。`ADMIN_MONITORING_REQUIRE_AUTH`、`ADMIN_ENFORCE_ROLE_CHECK` 仍是部署或设计口径，其中监控接口和第一阶段权限矩阵代码侧已经默认启用。

`admin-api` 第一阶段权限矩阵仍复用 `admin_accounts.role`，不新增数据库迁移：`viewer` 拥有审计、安全日志、玩家、维护状态和监控只读权限；`operator` 在只读权限外可调整玩家非封禁状态，并执行 GM 广播、发道具和踢人；`admin` 与兼容角色 `super_admin` 拥有全部权限，包括玩家/GM 封禁、维护模式写入、监控归档和管理员 token 生命周期操作。权限拒绝统一返回 `403 INSUFFICIENT_PERMISSION` 并写结构化日志，但不向响应泄露完整权限矩阵。

### 9.6 `announce-service` / 公告读写边界

当前 `announce-service` 已读取：

```env
ANNOUNCE_ADMIN_TOKEN=dev-only-change-this-announce-admin-token
ANNOUNCE_READ_AUTH_REQUIRED=true
ANNOUNCE_READ_TOKEN=dev-only-change-this-announce-read-token
TICKET_SECRET=dev-only-change-this-ticket-secret
REDIS_KEY_PREFIX=
```

说明：

- `ANNOUNCE_ADMIN_TOKEN` 用于保护 `POST /api/v1/announcements`、`PUT /api/v1/announcements/:announceId` 和 `DELETE /api/v1/announcements/:announceId`。
- 当前支持 `Authorization: Bearer <token>` 和 `X-Admin-Token: <token>` 两种 header；不支持 query token，避免 token 进入访问日志。
- 缺 token 返回 `ANNOUNCE_ADMIN_TOKEN_REQUIRED`，token 错误返回 `ANNOUNCE_ADMIN_TOKEN_INVALID`。
- `GET /api/v1/announcements` 和 `GET /api/v1/announcements/:announceId` 默认要求只读凭证或玩家 game ticket：内部服务/网关可传 `Authorization: Bearer <ANNOUNCE_READ_TOKEN>`、`X-Read-Token` 或 `X-Service-Token`；玩家侧可传 `Authorization: Bearer <game_ticket>` 或 `X-Game-Ticket`。
- Bearer token 会先按 `ANNOUNCE_READ_TOKEN` 比对；不匹配时再按 game ticket 解析。game ticket 格式沿用 `auth-http` 签发的 `payloadB64.signatureB64`，校验 HMAC-SHA256 签名、`exp` 过期时间、Redis `${REDIS_KEY_PREFIX}ticket:<sha256(ticket)>` 归属和 `${REDIS_KEY_PREFIX}player-ticket-version:<playerId>` 版本；Redis 不可用、owner key 缺失、version key 缺失或版本不一致均拒绝。
- 只读接口不接受 query token；缺少只读凭证返回 `ANNOUNCE_READ_AUTH_REQUIRED`，显式 read token 错误返回 `ANNOUNCE_READ_TOKEN_INVALID`，ticket 错误返回对应 ticket 错误码。
- `ANNOUNCE_READ_AUTH_REQUIRED=false` 只允许本地/内网兼容调试；production 环境会拒绝该配置。
- `ANNOUNCE_READ_TOKEN` 只授予只读查询权限，不授予 `POST/PUT/DELETE` 写权限；生产环境若配置该 token，必须是非默认强值且不能与 `ANNOUNCE_ADMIN_TOKEN` 相同。
- `announce-service` 默认仍是内网能力服务，不是生产公网入口；生产公网入口仍只应是 `auth-http` 和 `game-proxy`。`NODE_ENV=production` 或 `APP_ENV=production` 时，`ANNOUNCE_ADMIN_TOKEN`、`TICKET_SECRET`、`ANNOUNCE_READ_AUTH_REQUIRED` 和可选 `ANNOUNCE_READ_TOKEN` 的弱配置会导致配置加载失败。
- 该阶段仍复用全局 game ticket，后续仍需网关化、announce 专用用途隔离/换票、公告读写 RBAC、审计查询和更完整的公网 TLS 策略。

### 9.7 `mail-service` / 邮件玩家入口与发信入口

当前 `mail-service` 已读取：

```env
TICKET_SECRET=dev-only-change-this-ticket-secret
MAIL_PLAYER_AUTH_REQUIRED=true
MAIL_SERVICE_TOKEN=dev-only-change-this-mail-service-token
GAME_ADMIN_ACTOR=mail-service
GAME_ADMIN_CONNECT_TIMEOUT_MS=3000
GAME_ADMIN_WRITE_TIMEOUT_MS=3000
GAME_ADMIN_READ_TIMEOUT_MS=3000
GAME_ADMIN_MAX_RESPONSE_BYTES=1048576
```

说明：

- `GET /api/v1/mails`、`GET /api/v1/mails/:mailId`、`PUT /api/v1/mails/:mailId/read` 和 `POST /api/v1/mails/:mailId/claim` 默认要求 `Authorization: Bearer <game_ticket>` 或 `X-Game-Ticket`。
- ticket 格式沿用 `auth-http` 签发的 `payloadB64.signatureB64`，校验 HMAC-SHA256 签名、`exp` 过期时间、Redis `${REDIS_KEY_PREFIX}ticket:<sha256(ticket)>` 归属和 `${REDIS_KEY_PREFIX}player-ticket-version:<playerId>` 版本；Redis 不可用、owner key 缺失、version key 缺失或版本不一致均拒绝。
- 玩家接口以认证出的 `playerId` 为准；query/body 中的 `player_id` 可省略，传入时必须一致。邮件详情、标记已读和领取附件都会校验 `mail.to_player_id`。
- `POST /api/v1/mails` 是内部/后台发系统邮件入口，要求 `MAIL_SERVICE_TOKEN`，支持 `Authorization: Bearer <token>`、`X-Service-Token` 和 `X-Admin-Token`，不接受 query/body token。
- `MAIL_SERVICE_TOKEN` 是 mail-service 上游调用凭证；`GAME_ADMIN_TOKEN` 仅用于 mail-service 调用下游 `game-server admin` 发奖，两个 token 不复用。
- 邮件领取发奖会先把邮件抢占为 `claiming`，再通过内网 `game-server admin TCP` 发奖；发奖请求使用稳定 `requestId = mail_claim:<mail_id>`，并在 `AdminAuthReq` 中携带合法 service actor（可用 `GAME_ADMIN_ACTOR` 指定，默认使用服务实例标识规范化结果或 `mail-service`），以兼容 `GAME_ADMIN_AUDIT_REQUIRE_ACTOR=true`。缺省或非法 actor 不会把非法字符发送给下游。
- `GAME_ADMIN_CONNECT_TIMEOUT_MS`、`GAME_ADMIN_WRITE_TIMEOUT_MS`、`GAME_ADMIN_READ_TIMEOUT_MS` 和 `GAME_ADMIN_MAX_RESPONSE_BYTES` 控制 mail-service 调用下游 admin TCP 的连接、auth envelope 写入、请求包写入、响应读取和响应大小上限；缺失、非法、0 或负数会回退默认值 `3000/3000/3000/1048576`。
- `MAIL_PLAYER_AUTH_REQUIRED=false` 只允许本地兼容调试；生产环境会拒绝该配置，并要求 `TICKET_SECRET` 与 `MAIL_SERVICE_TOKEN` 都替换为非默认强值。
- 该阶段仍复用全局 game ticket，后续仍需网关化、用途隔离/换票、发信 RBAC、发信审计查询和更完整的公网 TLS 策略。

---

## 10. 分阶段落地建议

### M0：立即补齐的高优先级项

1. 管理员 JWT session/jti、登出撤销、禁用后失效、基础 token version 校验、批量撤销和重置密码联动 bump version 管理接口已落地
2. 管理员登录失败限流、锁定和安全审计已落地；跨用户名/IP 的全局风控策略仍待补齐
3. 管理面、Redis、MySQL、admin 端口默认不暴露公网；`game-proxy` admin HTTP 口已有 token 鉴权和生产默认 token 拒绝，仍需部署侧网络隔离
4. `game-proxy` 与 `game-server` 鉴权前消息白名单已落地
5. 单连接消息频率限制和本实例内单玩家消息频率限制已在 `game-server` 落地，`chat-server` 已有有界出站写队列用于慢连接背压，并已在生产环境拒绝默认 `TICKET_SECRET`；单 IP 频率限制、`chat-server` 消息频率限制和跨实例全局玩家频率限制仍需继续补齐
6. `announce-service` 公告只读 read token / game ticket 鉴权和公告写接口 token 鉴权已落地，`mail-service` 玩家入口 game ticket 鉴权和发信 service token 已落地，仍需保持这些能力服务默认内网化；公告、邮件后续继续收敛到网关化、用途隔离/换票、RBAC、审计查询和公网 TLS 策略
7. 非法包计数、异常输入计数和安全审计统一；proxy admin 已有日志审计、JSONL 持久审计、操作人字段、读写 token 分离和第一阶段 scoped token 操作级 RBAC，仍缺数据库化/集中策略、审批流、审计查询、集中留存、按资源范围授权和统一 trace/request id

### M1：当前阶段最值得做的安全增强

1. 共享 ticket 缩短重放窗口，必要时升级为用途隔离或换票模型
2. 接入层 IP allowlist、Redis 动态 denylist 和封禁 TTL
3. 多 proxy 全局单 IP / 单玩家连接上限
4. 管理面 IP allowlist
5. 配置热更新、回滚、GM 操作审计补齐
6. 公网 HTTP 入口统一 HTTPS
7. 如果后续重新允许 `chat-server` 公网直连，再补 TLS 或安全隧道封装；默认生产模型应先走内网能力服务收敛

### M2：部署复杂度允许后推进

1. `game-proxy` 入口 TLS 化
2. 内部服务 service token 标准化
3. 内部 gRPC / 控制面逐步升级 mTLS
4. 更完整的封禁策略、设备指纹或更强风控手段

---

## 11. 验收标准

### 11.1 数据加密

- 生产环境对外 HTTP 接口不再允许明文
- `game-proxy` 公网长连接入口不再裸奔明文；`chat-server` 默认不作为生产公网长连接入口
- Bearer token / ticket 不在明文公网链路上传输
- 控制面和内部服务调用有明确的鉴权与网络隔离策略

### 11.2 反作弊基础

- 未鉴权连接无法发送业务消息
- 单 IP / 单连接 / 单玩家都有明确频率限制
- 超前帧、过期帧、异常时间戳都有统一处理与原因码
- 位移越界、碰撞、速度异常会触发拒绝和审计

### 11.3 敏感操作审计

- GM、维护模式、玩家状态修改、ticket 撤销、配置更新都能查到审计记录
- 审计可定位操作者、目标、来源 IP、结果和时间
- GM 广播审计会记录 NATS global broadcast 结果和 legacy 单实例 fallback 结果；GM 踢人/封禁审计会记录 NATS global kick 结果和 legacy 单实例 admin TCP 调用结果
- 日志不包含明文密码、完整 token 或完整 ticket

### 11.4 防火墙 / 黑白名单

- Redis、MySQL、管理端口默认不暴露到公网
- `mail-service`、`announce-service`、`chat-server` 默认不对公网开放；临时开放时有明确的网段、鉴权或反向代理约束
- `admin-api` 至少支持运营网段白名单
- `game-proxy` 至少支持总连接上限、静态 IP denylist 和本地单 IP / 单玩家连接上限；跨实例黑名单和全局连接限额仍需补齐
- 黑白名单和封禁策略可跨实例同步

---

## 12. 与 TodoList 的对应关系

- `数据加密（协议加密）`：对应第 5 章
- `反作弊基础（客户端校验）`：对应第 6 章
- `敏感操作审计`：对应第 7 章
- `防火墙/黑白名单`：对应第 8 章

如果后续开始实现上述能力，应同时更新：

1. `docs/rate-limit-and-security.md`
2. `docs/admin-panel.md`
3. 本文中的“当前现状”与“建议新增配置项”
