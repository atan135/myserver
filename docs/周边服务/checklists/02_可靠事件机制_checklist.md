# 邮件跨服务可靠交互 Checklist

## 目标

一步到位完成 `mail-service`、`game-server` 和 `chat-server` 的邮件跨服务交互闭环：

- `mail-service -> chat-server`：邮件创建后向在线玩家发送低风险即时通知；通知允许离线跳过或偶发丢失，PostgreSQL 中的邮件记录始终是权威事实。
- `mail-service -> game-server`：玩家领取附件时可靠、幂等地发放资产；任何超时、断线、进程崩溃和重试都不得造成重复发放、资产丢失或邮件永久卡在 `claiming`。
- 三个服务通过 service registry、稳定身份、版本化契约、指标、审计和故障恢复形成可部署、可回滚、可验证的完整链路。

当前已有基线包括邮件通知专用 outbox、chat-server 邮件 NATS 订阅、邮件领取 `claiming/claimed` 状态和 game-server 基于 `request_id` 的幂等发放。本清单在现有实现上补齐生产边界，不建设全仓库通用事件平台，不改造 metrics、session kick、GM 广播或其他服务。

## 基础原则

- [x] 邮件内容、状态和附件领取进度以 `mail-service` PostgreSQL 为权威，Core NATS 仅承担在线通知，不承诺离线持久投递。（验证：阶段 10 的 NATS/chat 故障不回滚邮件，离线期间无历史消息重放）
- [x] 游戏资产以 `game-server` 为权威，`mail-service` 不直接写背包数据库，也不根据超时推断发放失败。（验证：所有资产增量只由真实 game-server 写入 `character_inventory`，响应丢失后 mail-service 查询幂等结果再收敛）
- [x] 附件领取使用稳定 `request_id = mail_claim:<mail_id>`；相同请求重复执行返回首次确定结果，参数不一致必须拒绝。（验证：断线、崩溃、两 mail 实例并发与玩家重复请求均只有一条 grant；指纹冲突单元测试通过）
- [x] 正式环境由服务端解析目标 game-server，客户端不得选择或覆盖资产发放目标实例。（验证：strict registry + v2 route 指向权威实例，客户端目标覆盖返回 403）
- [x] 三服务协议支持滚动升级和明确回滚；每个阶段独立实现、验证和提交。（验证：滚动升级与回滚由阶段 11 验证；阶段 10 的 PostgreSQL 断连修复和故障演练分别提交为 `3a98149`、`e7e5e2f`，文档与清单随最终归档提交）
- [x] 执行需要启动 PostgreSQL、Redis、NATS 或三服务联调前，先列出依赖并等待用户确认。（验证：阶段 10 启动前已列明有状态操作，用户明确回复“确认执行”）

## 阶段 1：交互契约与故障语义收敛

- 开始时间：2026-07-13 16:38:16 +08:00
- 结束时间：2026-07-13 16:48:50 +08:00
- 开发总结：在正式聊天与邮件设计文档中固化三服务权威边界、当前与目标时序、通知和发放 v1 契约、确定性请求指纹、结果状态、错误分类、权威路由及非目标，作为后续实现阶段的稳定依据。
- 验证记录：`git diff --check` 通过（仅 CRLF 提示）；文档 38 个代码围栏成对；4.4.1 至 4.4.9 覆盖全部阶段条目，JSON 示例和必需契约标记由 worker 校验通过。

- [x] 记录邮件创建、通知 outbox、在线路由、chat push、附件预占、game-server 发放和邮件完成的当前时序。（验证：`docs/周边服务/聊天与邮件系统设计.md` 4.4.2、4.4.3 给出代码锚点和完整时序）
- [x] 明确通知链路语义：邮件落库成功即业务成功；chat-server 离线、玩家离线或 NATS 丢消息不回滚邮件。（验证：设计文档 4.4.1 定义邮件事务和 Core NATS 在线提示边界）
- [x] 明确领取链路语义：只有 game-server 返回可验证的首次成功或幂等成功后，邮件才能进入 `claimed`。（验证：设计文档 4.4.1、4.4.6 定义四类权威结果和邮件状态决策）
- [x] 定义通知事件字段：event_id、event_type、version、occurred_at、player_id、mail 摘要和 trace_id。（验证：设计文档 4.4.4 定义通知 v1 JSON、字段约束和兼容规则）
- [x] 定义附件发放请求字段：request_id、mail_id、character_id、附件快照、请求指纹、source、reason 和 trace_id。（验证：设计文档 4.4.5 定义发放 v1 字段及 canonical JSON + SHA-256 指纹算法）
- [x] 定义三服务稳定错误分类，区分无效请求、权限错误、路由不可用、超时、可重试失败、永久失败和结果未知。（验证：设计文档 4.4.7 定义 7 类错误、result_state、重试决策和 HTTP 映射）
- [x] 明确非目标：不引入 JetStream，不建设全服务 outbox/inbox SDK，不让 chat-server 保存邮件或参与附件状态。（验证：设计文档 4.4.9 明确非目标与滚动兼容边界）

## 阶段 2：邮件通知 Outbox 与事件版本

- 开始时间：2026-07-13 16:50:07 +08:00
- 结束时间：2026-07-13 17:30:59 +08:00
- 开发总结：完成 mail-service 通知 v1 信封、存量 payload 归一化、原子 outbox 写入、带 token 的并发租约、有限抖动重试、终止状态、保留清理、配置与指标；修复邮件落库后即时通知异常错误返回，并限制绝不启动超过最大次数的发布。
- 验证记录：`npm test --workspace mail-service` 51/51；5 个邮件定向文件 45/45；`npx tsc -p apps/mail-service/tsconfig.json --noEmit` 通过；`git diff --check` 通过（仅 CRLF 提示）。真实 PostgreSQL/NATS 竞争留待阶段 10 经确认后联调。

- [x] 邮件记录与通知 outbox 在同一 PostgreSQL 事务内写入；任一写入失败时整体回滚。（验证：`db-store.js:createMailWithNotificationOutbox` 使用 BEGIN/COMMIT/ROLLBACK；`mail-store-claim.test.mjs` 覆盖 outbox 插入失败回滚）
- [x] 为通知 outbox 增加稳定 event_id、事件版本、trace_id、最大尝试次数和终止状态。（验证：`notification-outbox.js` 构造 v1 信封；两份 init.sql 与 `db-client.js` 增加持久字段和约束）
- [x] 对存量 outbox payload 保持兼容，升级期间能够读取旧格式并生成新事件信封。（验证：`normalizeMailNotificationEvent` 兼容旧嵌套 payload；定向测试验证稳定 event_id/trace_id）
- [x] 批量 claim 使用数据库租约和并发安全领取，多个 mail-service 实例不得同时持有有效租约。（验证：`reservePendingMailNotificationOutbox` 使用事务、`FOR UPDATE SKIP LOCKED` 和 lease token；状态转换只接受当前 sending token）
- [x] 发布失败采用有上限、带抖动的指数退避；进程崩溃后过期租约可由其他实例接管。（验证：`calculateOutboxBackoffMs` 可注入抖动；租约过期和最大次数测试确保不启动 max+1 次发布）
- [x] Core NATS publish 成功只表示通知已发布，不将其记录为 chat-server 或玩家已确认接收。（验证：`mails.service.ts` 仅把 outbox 标记 sent；设计文档 4.4.4 明确 sent 语义）
- [x] 对永久无效 payload 进入终止状态并保存截断错误摘要，避免无限重试。（验证：`PermanentOutboxPayloadError` 与 terminal 转换；测试覆盖未知事件类型不发布并终止）
- [x] 增加 outbox 积压、最老事件年龄、发布延迟、重试、终止和租约接管指标。（验证：`metrics.js` 暴露六类 `mail_outbox_*` 指标；指标测试通过）
- [x] 为已发送和终止记录定义保留与清理策略，不让 outbox 表无限增长。（验证：配置提供 sent/terminal 独立保留期和批次；`cleanupMailNotificationOutbox` 及双窗口测试通过）

## 阶段 3：Chat-server 在线通知消费闭环

- 开始时间：2026-07-13 17:32:02 +08:00
- 结束时间：2026-07-13 17:53:18 +08:00
- 开发总结：完成 chat-server 新旧邮件通知兼容、解析前大小限制、字段校验、双 subject 共享 TTL/容量去重、当前 session 定向推送、失败分类、有界重连、优雅停机和七类聚合指标，并修复旧连接退出误删新 session/在线路由的问题。
- 验证记录：`cargo fmt --manifest-path apps/chat-server/Cargo.toml --check` 通过；`cargo check`、`cargo clippy --all-targets` 由 worker 验证通过（仅既有 warning）；主审核复跑 `cargo test --manifest-path apps/chat-server/Cargo.toml` 51/51 通过。真实 NATS 断连留待阶段 10。

- [x] chat-server 同时兼容旧邮件 payload 与新版本事件信封，灰度期间不要求三服务同时发布。（验证：`mail_subscriber.rs` 的 `parse_notification` 分派 legacy/v1；兼容测试通过）
- [x] 校验 event_type、version、player_id、mail_id、字符串长度和 payload 大小，非法事件只记录受限日志。（验证：解析前 4096B 限制、UTF-8 字节校验与受限错误日志；边界测试通过）
- [x] 保持实例定向 subject 与 legacy player subject 的路由兼容，并防止同一事件经两条 subject 重复推送。（验证：`run_subscriber` 同时订阅两路并共享 deduplicator；双路测试只收到一次）
- [x] 实现按 event_id 的有界短期去重；缓存设置容量、过期和淘汰策略，不引入永久 inbox。（验证：`EventDeduplicator` 默认容量 10000、TTL 300 秒并按 FIFO 确定淘汰；容量/过期测试通过）
- [x] 玩家在线时仅向其当前有效 chat session 推送 `MailNotifyPush`，不得向其他玩家或旧 session 泄漏通知。（验证：`push_mail_to_player` 查当前 session；`unregister_session` 使用 same_channel 条件删除；并发回归测试通过）
- [x] 玩家离线、session 写队列已满或客户端断开时记录结果并安全跳过，不反向修改邮件状态。（验证：`PushOutcome` 分类 Offline/QueueFull/QueueClosed，测试覆盖三种分支）
- [x] 未知事件版本进入可观察的兼容失败路径，不导致订阅任务退出。（验证：UnsupportedVersion 独立指标与测试，handler 拒绝单条后继续循环）
- [x] NATS 连接中断后按退避重连并重新建立两个订阅，支持服务优雅停机。（验证：订阅循环使用有上限指数退避和 watch shutdown；main.rs 有界等待并超时 abort）
- [x] 增加接收、解析失败、版本拒绝、去重命中、在线推送、离线跳过和队列失败指标。（验证：`metrics.rs` 增加七项无高基数计数并覆盖 collector 测试）

## 阶段 4：Game-server 附件发放幂等契约

- 开始时间：2026-07-13 17:54:20 +08:00
- 结束时间：2026-07-13 18:58:09 +08:00
- 开发总结：game-server 完成邮件附件发放的稳定请求 ID、规范化指纹、事务幂等记录、结果查询、错误分类、当前角色推送及聚合指标；数据库读取失败和提交结果未知均不会污染内存背包，存量无结果记录进入明确的结果不可用路径。
- 验证记录：`cargo test --manifest-path apps/game-server/Cargo.toml`（422/422）、`cargo check --manifest-path apps/game-server/Cargo.toml`、`npm run check:proto`、`node tools/run-node-tests.js tests/characters/db-init-characters.test.mjs`（18/18）和 `git diff --check` 均通过；未启动 PostgreSQL、Redis、NATS 或服务进程。

- [x] 保持 `mail_claim:<mail_id>` 为跨重试稳定 request_id，并校验长度、格式和 source。（验证：`admin_server/gm.rs:920-938` 要求 `source=mail-claim`、request_id 与 mail_id 精确对应，并校验 ID、指纹和 trace_id）
- [x] 对 character_id、标准化附件列表、绑定状态和 source 计算确定性请求指纹。（验证：`grant_contract.rs:28-70` 按 item_id/binded 排序合并后，对 mail_id、character_id、source、items 的 canonical JSON 计算 `sha256:` 指纹；固定向量测试通过）
- [x] 扩展幂等记录，保存 request_id、character_id、请求指纹、发放结果摘要和创建时间。（验证：`db_player_store.rs:54-59,236-299` 和 `db/init.sql:647-659` 持久化全部字段并约束 request_id 唯一）
- [x] 相同 request_id 与相同指纹返回首次成功结果，不再次创建物品 UID 或增加数量。（验证：`player_manager.rs:208-220` 在构造物品前查询首次记录，`grant_same_request_replays_first_result_without_rebuilding_items` 回归测试通过）
- [x] 相同 request_id 与不同 character_id、附件或绑定状态返回明确冲突，不复用首次结果。（验证：`player_manager.rs:378-390` 同时比较角色与指纹并返回 `REQUEST_FINGERPRINT_CONFLICT`；角色/指纹冲突测试通过）
- [x] 背包变更与幂等记录在同一数据库事务提交；任一失败时均不得留下部分结果。（验证：`db_player_store.rs:231-300` 在单一事务插入 grant 记录并 upsert 背包；`player_manager.rs:275-294` 仅在确定提交成功后更新内存，提交未知返回 `INVENTORY_COMMIT_RESULT_UNKNOWN`）
- [x] 对容量不足、非法物品、配置缺失和数据库失败返回稳定且可分类的错误。（验证：`admin_server/gm.rs:108-174,249-278` 与 `player_manager.rs` 的 `GrantItemsError` 映射 INVALID_REQUEST、PERMANENT_FAILURE、RETRYABLE_FAILURE、RESULT_UNKNOWN；game-server 422 项测试通过）
- [x] 发放成功后仅向当前权威在线角色推送背包变化；推送失败不回滚已经提交的资产。（验证：`admin_server/gm.rs:176-247` 仅在首次提交后按 character_id 调用 `RoomManager::send_to_character`，推送失败仍返回已提交结果并记录失败）
- [x] 提供受保护的内部结果查询能力，可按 request_id 返回未见请求、成功、冲突或结果不可用。（验证：消息 3009/3010 仅在 `admin_server.rs:115-149` admin token 鉴权后的循环分派；`gm.rs:282-394` 覆盖 not_seen、succeeded、conflict、result_unavailable）
- [x] 增加幂等首次成功、重复命中、指纹冲突、事务失败和在线推送失败指标与审计。（验证：`metrics.rs` 汇总五类 `inventory_grant_*` 指标；`gm.rs:176-278` 记录指标并通过 admin audit 返回稳定结果，指标单测通过）

## 阶段 5：Game-server 权威路由与调用安全

- 开始时间：2026-07-13 18:59:43 +08:00
- 结束时间：2026-07-13 20:56:17 +08:00
- 开发总结：完成 game-server Redis v2 在线资产路由、跨实例 generation/token fencing、失权 fail-closed 断开和领取前权威复核；mail-service 依据在线路由与健康 registry endpoint 选择实例，携带同一请求身份和 route fence，严格环境拒绝客户端目标，本地固定 fallback 仍以真实 route owner 为身份。离线角色因暂无可靠单写 owner 明确返回可重试路由失败，资产成功后直接向当前 PlayerRegistry session 推送。
- 验证记录：`cargo test --manifest-path apps/game-server/Cargo.toml` 435/435；`cargo check --manifest-path apps/game-server/Cargo.toml` 通过；`npm test --workspace mail-service` 69/69；5 个非联调邮件文件 45/45；`npx tsc --noEmit -p apps/mail-service/tsconfig.json`、`npm run check:proto`、阶段 Rust 文件 `rustfmt --check`、Node `--check` 和 `git diff --check` 均通过。真实 Redis Lua 并发与多实例切换留待阶段 10 经确认后联调。

- [x] 梳理在线角色权威实例、离线角色和多 game-server endpoint 下的资产写入规则。（验证：`core/online_route.rs` 定义 v2 route、owner、generation 和 fence；`聊天与邮件系统设计.md` 4.4.6 固化在线单写、离线拒绝与多 endpoint 规则）
- [x] 正式环境由 mail-service 根据服务发现与权威路由选择 game-server admin endpoint，不信任客户端 `targetInstanceId`。（验证：`game-admin-client.js:552-647` 读取 v2 route 并与发现 endpoint 交叉验证；严格路由 socket 测试通过）
- [x] `targetInstanceId` 仅保留为 local/development 调试能力，生产或严格发现环境收到该字段时忽略或拒绝。（验证：`mails.service.ts:465-478` 在严格发现下预占前返回 `CLIENT_TARGET_INSTANCE_FORBIDDEN`；单测覆盖 403 和零下游调用）
- [x] 在线角色优先路由到权威实例，保证成功发放后能够向正确连接推送背包更新。（验证：请求携带 generation/token，`admin_server/gm.rs:598-649` 校验本地 handle 与 Redis route/owner/fence；`mail_grant_push_uses_the_current_registry_session` 证明只推当前 session）
- [x] 离线角色选择明确且稳定的可写实例；如果架构暂不支持安全离线写入，则返回可重试路由错误而不是随机调用。（验证：`game-admin-client.js:621-647` 缺 route 返回 `MAIL_CLAIM_ROUTE_UNAVAILABLE`，两 endpoint 缺路由测试确认零请求）
- [x] 目标实例切换、摘流或 Room 迁移期间重新解析路由，并保持同一 request_id。（验证：`game-admin-client.js:668-751` 仅对 `ROUTE_UNAVAILABLE/not_applied` 重新解析；切换测试确认两次调用的 request_id、fingerprint、trace_id 相同而 fence 更新）
- [x] game-server admin 调用使用服务身份认证、连接/读写超时、响应大小限制和错误码校验。（验证：`game-admin-client.js:256-331` 使用 admin token/service actor 和三类超时；测试覆盖大小、读超时、sequence、flags、消息类型与业务错误码）
- [x] 防止客户端伪造角色、邮件 ID、附件列表、source、request_id 或内部目标实例。（验证：`mails.service.test.ts` 覆盖攻击者字段被忽略；`admin_server/gm.rs:1021-1089` 重算并校验 mail ID、request ID、source、附件指纹、trace 和 route fence）
- [x] 路由不可用与业务发放失败分别记录指标，日志包含实例、request_id 和 trace_id 但不输出完整附件隐私数据。（验证：`metrics.js` 提供 `mail_claim_route_unavailable`/`mail_claim_grant_failures`；安全日志仅记录实例、requestId、traceId、errorCode 和阶段，指标回归测试通过）

## 阶段 6：Mail-service 附件领取持久状态机

- 开始时间：2026-07-13 20:58:30 +08:00
- 结束时间：2026-07-13 21:53:35 +08:00
- 开发总结：新增独立 `mail_claim_workflows` 持久状态机，冻结 request_id、角色、规范化附件与指纹，使用 lease token 和事务条件更新串行领取。明确未执行错误保留同一工作流重试，响应超时、损坏或契约无法验证进入待对账且玩家重试不再发放；成功后原子完成 workflow 与存量邮件，邮件硬删除后仍保留证据并可按冻结快照收敛。玩家 API 使用稳定状态、HTTP 语义和安全文案，不回显内部 endpoint、Redis 或 token 诊断。
- 验证记录：`npm test --workspace mail-service` 75/75；5 个非联调邮件文件 50/50；`npx tsc --noEmit -p apps/mail-service/tsconfig.json`、`npm run check:proto`、相关 Node `--check`、`git diff --check` 均通过；根数据库初始化静态回归 18/18。未启动真实 PostgreSQL/Redis/NATS，真实行锁竞争留待阶段 10。

- [x] 将领取状态明确为可持久恢复的 `unread/read -> claiming -> claimed`，并定义可重试失败与永久失败的记录方式。（验证：`db/init.sql:788-824` 约束 processing/retryable_failure/permanent_failure/reconciliation_pending/claimed；`mails.service.ts:130-153` 稳定分类下游错误）
- [x] 领取预占时持久化 claim_request_id、character_id、附件快照指纹、attempts、lease/更新时间和最后错误。（验证：`mail_claim_workflows` 持久化全部字段；`db-store.js:574-714` 首次预占冻结请求并写 attempts、lease 和 trace）
- [x] 一封邮件只有一个有效领取工作流；并发请求返回首次结果或明确的处理中状态。（验证：mail_id/request_id 双唯一约束；首次 PostgreSQL 领取锁 mail 后二次查询 workflow，active lease 测试返回 processing 且零并发 grant）
- [x] 首次请求预占成功后调用 game-server；请求超时或连接断开时将结果标记为未知，不立即释放成可重新生成新请求的状态。（验证：`db-store.js:810-873` 持久化 reconciliation_pending；真实 TCP 损坏响应/非法 protobuf 与读超时测试均为 202，二次请求未新增 grant）
- [x] game-server 明确成功或幂等成功后，mail-service 条件更新为 `claimed` 并记录完成时间。（验证：`db-store.js:875-971` 仅匹配 processing + lease_token 时在事务内完成 workflow/mails 并写 completed_at/result_summary）
- [x] game-server 明确表示请求从未执行且错误可重试时，保留同一 request_id 安排重试。（验证：retryable route/connect 测试确认 mail 保持 claiming，重试沿用 claim_request_id、角色、冻结快照和指纹并递增 attempts）
- [x] 永久业务错误保存可解释原因并允许玩家修正条件后以同一领取工作流重试，不能静默吞掉附件。（验证：永久业务失败测试保存内部错误分类和安全公开码，下一次仍使用原 workflow/frozen snapshot；未知永久码不向玩家泄漏原始异常）
- [x] 邮件过期、删除或内容修改不得破坏已开始的领取工作流；claiming 后使用已冻结附件快照。（验证：workflow 无 mails 级联外键；`started workflow ... after hard mail deletion` 与 store 硬删除测试均按冻结快照完成 claimed）
- [x] mail-service 响应明确区分已领取、处理中、可重试失败、永久失败和状态待对账。（验证：`mails.service.ts:156-230` 映射 claimed/processing/retryable_failure/permanent_failure/reconciliation_pending 到 200/202/503/422/202；controller 去除内部 `_http_status`）
- [x] 所有状态转换使用条件更新或事务锁，防止 worker、玩家请求和管理操作并发覆盖。（验证：reserve 使用 `FOR UPDATE` 和首次并发二次检查；failure/complete 要求当前 lease_token，过期接管测试证明旧 token 的晚到成功与失败均不能覆盖）

## 阶段 7：Claiming 自动恢复与对账

- 开始时间：2026-07-13 21:56:04 +08:00
- 结束时间：2026-07-13 23:02:06 +08:00
- 开发总结：新增独立 ClaimRecoveryWorker 和持久 recovery lease，以启动扫描、周期扫描、`FOR UPDATE SKIP LOCKED`、租约 token fencing 和有界停机恢复超时领取。结果未知时并发查询全部健康 game-server admin endpoint；只有一致 `not_seen` 才按冻结请求重试，一致成功则零发放补写 claimed，响应损坏、实例不可用或结果不一致均 fail closed。恢复达到上限、明确指纹冲突或永久错误进入 manual_review，并保留冻结请求、查询证据、错误和时间指标。
- 验证记录：主审核复跑 `npm test --workspace mail-service` 95/95；5 个非联调邮件文件 50/50；根数据库初始化静态回归 18/18；`npx tsc --noEmit -p apps/mail-service/tsconfig.json`、`npm run check:proto`、相关 Node `--check` 和 `git diff --check` 均通过。真实 PostgreSQL 行锁、Redis service registry、多进程 lease 接管及三服务故障注入留待阶段 10 经确认后联调。

- [x] 实现领取恢复 worker，按租约领取超时或待重试记录，支持多 mail-service 实例并发运行。（验证：`ClaimRecoveryWorker.processRecoveries` 批量恢复且单实例不重入；`DbMailStore.reserveMailClaimRecoveries` 使用独立 recovery token 与 PostgreSQL `FOR UPDATE SKIP LOCKED`；95/95 测试覆盖双 worker 竞争）
- [x] 对结果未知记录先查询 game-server 的 request_id 结果，再决定完成邮件或继续原请求。（验证：`recoverOne` 对过期 processing/reconciliation_pending 的 query 模式先调用 `queryMailAttachmentGrant`；真实 TCP 测试校验 3009/3010 的 request_id、指纹、trace 和结果证据）
- [x] 查询确认已发放时将邮件补写为 `claimed`，不得再次发放。（验证：`completeMailClaimRecovery` 在同一事务条件更新 workflow 与存量 mail；`succeeded reconciliation completes mail with zero grant` 证明查询成功后 grant 调用数为 0）
- [x] 查询确认从未处理时使用原 request_id、原 character_id 和冻结附件快照重试发放。（验证：`retryGrant` 只读取持久 workflow 的 `claim_request_id`、`character_id`、`attachments_snapshot` 和 `attachments_fingerprint`；`not_seen retries the original frozen request` 逐字段断言通过）
- [x] 查询暂时不可用时退避重试，不把邮件恢复成普通未领取状态。（验证：查询任一 endpoint 异常、响应损坏或多实例结果不一致统一返回 result_unavailable；worker 写回 reconciliation_pending 与 next_recovery_at，测试证明不 grant 且 mail 保持 claiming）
- [x] 达到自动重试上限后进入待人工处理状态，保留资产侧证据和最后错误。（验证：reserve/defer 两处按 `recovery_attempts >= maxAttempts` 转 manual_review；上限测试保留最后 `GAME_ADMIN_READ_TIMEOUT`、冻结附件和 claiming 邮件状态）
- [x] worker 启停、崩溃、租约过期和实例切换不会遗失或并发处理同一领取。（验证：玩家 lease 与 recovery lease 双向清理并分别按当前 token 和未过期时间条件写入；测试覆盖过期 recovery lease 接管、旧 owner 写回失败、不重入与有界停机）
- [x] 提供启动时恢复扫描及周期扫描，避免重启后 `claiming` 永久卡住。（验证：`onModuleInit` 先执行 startup scan，再启动不重叠的 interval；测试覆盖启动恢复过期 processing、慢查询期间周期扫描不重入及 `onModuleDestroy` 清理 timer）
- [x] 记录恢复数量、未知结果年龄、查询结果、重试次数、人工待处理和最终恢复耗时指标。（验证：`metrics.js` 发布 `mail_claim_recovery_*` 聚合指标；时间锚点保留 lease 前 updated_at，首次扫描 120 秒旧记录的年龄/恢复耗时测试与 metrics payload 回归通过）

## 阶段 8：查询、人工恢复与审计

- 开始时间：2026-07-13 23:03:56 +08:00
- 结束时间：2026-07-13 23:39:35 +08:00
- 开发总结：在 mail-service 增加独立内网邮件运维控制面，以 operations token 保护精确过滤查询、query-first 对账/原请求重试和 terminal outbox 重放，manual_review 恢复还必须叠加独立 high-risk token。管理操作强制 operation request id、actor 和 reason，以 PostgreSQL 事务原子写入状态变化与 append-only 审计；所有领取恢复只排入现有 worker，不能直接发奖或标记 claimed。同步完成字段脱敏、游标分页、硬删除 workflow 查询/恢复、保留期约束、聚合指标和告警处置说明。
- 验证记录：主审核复跑 `npm test --workspace mail-service` 109/109；5 个非联调邮件文件 50/50；根数据库初始化静态回归 18/18；`npx tsc --noEmit -p apps/mail-service/tsconfig.json`、`npm run check:proto`、相关 Node `--check` 和 `git diff --check` 均通过。真实 PostgreSQL 审计 trigger、行锁竞争、Redis registry 和 game-server 查询留待阶段 10 经确认后联调。

- [x] 提供受保护的内部查询，按 mail_id、request_id、player_id、character_id 和状态定位领取工作流。（验证：`MailOperationsController` 的 claims 接口要求独立 `MAIL_OPERATIONS_TOKEN`；`queryClaims/queryMailClaimWorkflows` 支持五类精确过滤、limit 1..50 和 before_id 游标，测试覆盖全部 locator 与无过滤拒绝）
- [x] 查询同时展示 mail-service 状态、game-server 幂等结果、附件指纹和最近错误，不返回不必要敏感内容。（验证：服务组合 mail/workflow/outbox/最近审计与全健康 game 查询结果，只输出安全错误字段和摘要计数；测试证明不返回正文、附件快照和下游自由错误，硬删除 mail 后 workflow 仍可查询）
- [x] 提供“重新对账/按原请求重试”操作，不提供跳过 game-server 直接标记成功的普通入口。（验证：reconcile/retry-original 仅事务性切换到 reconciliation_pending；worker 必须先 query，只有一致 not_seen 才使用冻结请求 grant；控制面没有直接 grant 或 claimed 更新代码）
- [x] 确需人工纠正时使用独立高风险权限、操作理由、前后快照和不可删除审计。（验证：manual-recover 同时要求 operations/high-risk 两个生产强制独立凭证，并强制 actor/reason；`mail_admin_operation_audit` 与 workflow 同事务写入安全前后快照，trigger 拒绝 UPDATE/DELETE/TRUNCATE）
- [x] 通知 outbox 终止事件支持受限重放；重放只影响在线提示，不改变邮件和资产状态。（验证：outbox replay 要求 operations token 且仅接受 terminal；保留原 event_id/payload，重置投递租约和 attempts；回归测试前后比对 mail/workflow 完全不变）
- [x] 管理操作具备 request_id 和幂等语义，重复点击不会产生第二次发放。（验证：所有写操作强制 `operation_request_id`，审计表唯一约束加 transaction advisory lock 串行化；同 action/target/actor/reason 返回首次 result，不同上下文复用 ID 返回 ADMIN_OPERATION_CONFLICT）
- [x] 定义邮件、通知 outbox、领取记录和 game-server 幂等记录的保留期限与关联查询。（验证：配置与响应定义 mail/workflow/game grant 默认 400 天、sent outbox 7 天、terminal 30 天、审计不自动删除；启动校验 mail/game 窗口不得短于 workflow，按 mail_id/request_id/event_id/target 可关联）
- [x] 为异常领取率、长期 claiming、指纹冲突和人工恢复建立告警条件与处理说明。（验证：metrics 发布 attempts/succeeded、四状态长期 claiming、fingerprint conflicts 和 manual backlog 聚合字段且无高基数标签；正式文档定义窗口、失败率/时长/数量阈值及禁止盲目重发的处置步骤）

## 阶段 9：单元与契约测试

- 开始时间：2026-07-13 23:41:35 +08:00
- 结束时间：2026-07-14 00:21:12 +08:00
- 开发总结：补齐邮件跨服务可靠性单元与契约验证：Node.js、chat-server 和 game-server 共同读取同一 v1 fixture，锁定通知信封、附件规范化、编码和 SHA-256 指纹；game-server 增加仅测试可用的持久化失败注入，证明事务失败不会污染内存背包或幂等记录；mail-service 日志最终格式化层无条件隐藏自由错误详情、凭证、正文、附件和 endpoint，运维查询只保留白名单 game instance ID。阶段 1-8 已有 outbox、领取状态机、恢复 worker 和并发租约测试一并纳入全量回归。
- 验证记录：主审核复跑 `npm test --workspace mail-service` 115/115、五个非联调邮件文件 50/50、`cargo test --manifest-path apps/chat-server/Cargo.toml` 52/52、`cargo test --manifest-path apps/game-server/Cargo.toml` 438/438；chat-server `cargo check`、普通 `cargo clippy --all-targets`、`cargo fmt --check` 通过，game-server `cargo check` 与本轮四个 Rust 文件 `rustfmt --check` 通过；`npx tsc --noEmit -p apps/mail-service/tsconfig.json`、`npm run check:proto`、根数据库初始化静态回归 18/18、相关 Node `--check` 和 `git diff --check` 均通过。附加的 chat-server `clippy -D warnings` 被既有 dead-code/风格告警阻塞；game-server 全仓 `cargo fmt --check` 仍只命中本轮未修改的 `tools/csv_codegen.rs`、room logic/tick 和 lockstep demo 文件。

- [x] mail-service 覆盖邮件与 outbox 同事务、租约领取、退避、终止和清理测试。（验证：五个非联调邮件文件 50/50；`PostgreSQL mail and notification outbox rollback as one transaction`、lease fencing、deterministic backoff、maximum attempts 和双保留期清理用例均通过）
- [x] chat-server 覆盖新旧 payload、未知版本、非法字段、双 subject 去重、玩家离线和队列失败测试。（验证：chat-server 52/52；`accepts_v1_envelope_and_legacy_payload`、unknown version/invalid fields、`both_subject_routes_share_event_id_deduplication`、offline/full/closed session 与 bounded channel full 用例通过）
- [x] game-server 覆盖首次发放、相同指纹重试、不同指纹冲突和事务回滚测试。（验证：game-server 438/438；`grant_replay_returns_first_result_without_building_new_items`、`grant_request_id_conflicts_across_fingerprint_or_character`、并发首次发放和 `grant_transaction_failure_does_not_publish_partial_inventory_or_record` 通过）
- [x] mail-service 覆盖成功领取、并发领取、明确失败、超时未知、恢复查询和原请求重试测试。（验证：mail-service 115/115；成功完成、active lease、not_applied/permanent failure、response timeout、succeeded reconciliation 与 `not_seen retries the original frozen request` 用例通过）
- [x] 覆盖服务重启后恢复 claiming、多个 worker 竞争租约和达到重试上限测试。（验证：`startup scan recovers an expired processing workflow after querying`、`multiple workers compete for one recovery lease`、过期 lease fencing 和 `recovery attempt limit moves workflow to manual review` 均通过）
- [x] 使用固定 fixture 验证 Node.js 事件/请求与 Rust 解析结果一致。（验证：`tests/fixtures/mail-cross-service-v1.json` 被 Node builder/encoder、chat `parse_notification`、game grant decoder 与 Rust 指纹实现直接读取；Node 2、chat 1、game 2 个 fixture 用例通过）
- [x] 测试错误日志和审计不包含 ticket、admin token、完整邮件正文或无界附件 payload。（验证：`logger.test.js` 覆盖 opaque Error 文本、凭证、正文、200 项附件与 Redis/NATS endpoint；operations 回归证明审计/查询仅保留有界摘要和白名单实例 ID）
- [x] 将相关 Node.js、Rust 和协议检查命令记录到阶段验证结果。（验证：本阶段验证记录已列出 Node 115/115 + 50/50、Rust 52/52 + 438/438、check/clippy/fmt、TypeScript、协议、DB 18/18、语法与 diff 检查结果及既有格式告警）

## 阶段 10：三服务故障注入与联调验收

- 开始时间：2026-07-15 11:38:19 +08:00
- 结束时间：2026-07-15 13:30:21 +08:00
- 开发总结：新增真实三服务故障注入脚本，以专用 PostgreSQL 验收数据库、隔离 Redis/NATS、两个真实 game-server、两个 mail-service、真实 chat-server 和持久在线玩家连接验证完整链路；通过 TCP 代理精确注入发放前断线、成功响应丢失、mail-service 提交后崩溃和 PostgreSQL 传输中断。验收中修复存量 drill 对 v1 信封、权威路由和 3004 protobuf 成功结果的契约漂移，并修复 mail-service 未监听 PostgreSQL Pool/已借出 Client 错误而在短暂断连时退出的问题；恢复开发后进一步保证日志器尚未初始化或报告器异常时，数据库错误监听器也不会向事件循环抛错。所有测试进程、动态端口、Redis 前缀和专用数据库均已清理，未停止或清理本机原有 PostgreSQL、Redis、NATS。
- 验证记录：`node --test tests/mail/mail-reliability-fault-drill.test.mjs` 11/11，通过正常闭环及 NATS、chat、game、mail、registry、Redis、PostgreSQL、多实例、路由切换、并发与恶意参数十类真实场景；`node --test tests/mail/mail-notify-claim-drill.test.mjs` 1/1；`npm test --workspace mail-service` 125/125；`cargo test --manifest-path apps/chat-server/Cargo.toml` 57/57；game-server 首轮全量测试因既有 GM 审计文件瞬时读取断言出现 437/438，失败用例定向复跑 1/1 后按正常并发模式全量复跑 438/438；`npx tsc --noEmit -p apps/mail-service/tsconfig.json`、`npm run check:proto`、数据库初始化静态回归 18/18、相关 `node --check` 和 `git diff --check` 通过。Rust 仅输出既有 warning；验收数据库已删除且 `myserver_mail_acceptance%` 残留查询为 0，隔离测试进程均已退出。

- [x] 验证 NATS 不可用时邮件仍成功落库、通知 outbox 保留并在恢复后重发。（验证：隔离 NATS 停止期间创建邮件返回成功且 outbox 保持 pending/sending；同端口恢复后 outbox 进入 sent，在线 chat 客户端收到对应 v1 推送）
- [x] 验证 chat-server 离线时邮件业务不受影响，恢复后不错误宣称历史通知已送达。（验证：强制停止真实 chat-server 后邮件和 outbox 正常完成；重启并重新认证后没有历史推送，新邮件才产生实时 `MailNotifyPush`）
- [x] 验证 game-server 发放前断线时使用原 request_id 恢复发放一次。（验证：停止权威 game-server 并在 admin 请求转发前断线，零 grant；同实例重启和 route 恢复后重复原领取请求，workflow 保持 `mail_claim:<mail_id>` 并最终仅一条 grant、资产只增加目标数量）
- [x] 验证 game-server 发放成功但响应丢失时，对账完成邮件且资产只增加一次。（验证：admin 故障代理确认截获真实 game-server 成功响应，mail-service 查询共享 PostgreSQL 幂等结果后补写 claimed；grant 记录和背包增量各一次）
- [x] 验证 game-server 成功后 mail-service 在写 `claimed` 前崩溃，重启可自动收敛。（验证：故障代理在 game 成功响应到达时强制终止 mail-service；确认 grant 已提交后重启 mail-service，startup recovery 查询结果并自动完成 claimed，未再次发奖）
- [x] 验证 mail-service 多实例、game-server 多实例和权威实例切换时路由及幂等正确。（验证：两 mail 实例并发领取，玩家 route 从 game A 切换到 game B；workflow 记录 game B 且共享数据库中仅一条 grant）
- [x] 验证 PostgreSQL、Redis、service registry 短暂不可用时的错误分类、退避和恢复。（验证：空 registry namespace 返回 503 且零 grant，恢复正式 namespace 和稳定 route 后原 workflow 收敛；Redis 停止返回 `AUTH_BACKEND_UNAVAILABLE`，重启五个服务后原邮件领取一次；PostgreSQL TCP 中断期间创建/领取失败且零写入零 grant，恢复后成功收敛。Pool 与已借出 Client 错误监听回归通过）
- [x] 验证同一邮件重复点击、并发请求和恶意篡改目标实例/附件均不会重复发奖。（验证：严格发现环境拒绝客户端 `target_instance_id` 为 403；客户端附件覆盖不进入发奖请求；两 mail 实例并发及恢复重试始终只有同一 request_id 的一条 grant）
- [x] 执行现有 mail 通知领取 drill、mock-client 或新增三服务联调脚本并记录结果。（验证：更新后的存量 strict-registry drill 1/1；新增真实故障 drill 11/11，并直接使用 mock-client TCP 协议客户端完成 game/chat 认证和通知接收）
- [x] 根据仓库约定，启动 PostgreSQL、Redis、NATS、mail-service、chat-server、game-server 及相关入口前先获得用户确认。（验证：2026-07-15 用户明确回复“确认执行”后才创建专用数据库、启动隔离依赖与三服务）

## 阶段 11：灰度、回滚与文档收口

- 开始时间：2026-07-14 00:23:51 +08:00
- 结束时间：2026-07-14 01:07:55 +08:00
- 开发总结：完成邮件三服务链路的灰度与回滚保护。chat-server 增加旧扁平 payload 独立开关和运行期截止时间，strict/test/staging/production 或显式 required discovery 默认拒绝旧格式，但继续保留两条 subject 的 v1 去重；mail-service 将新领取 intake 与存量 recovery 拆分控制，关闭 intake 后不创建/重领 lease 或调用 game-server，关闭 recovery 前必须同时关闭 intake，并在启动时通过 PostgreSQL/内存 backlog 检查拒绝遗弃任何非 claimed 工作流。健康接口和 registry metadata 可逐实例核验开关/契约状态，正式文档补齐部署顺序、灰度指标、回滚 SQL、兼容删除门槛、变量与联调命令；未修改 game-server 既有幂等查询实现，也未扩展到其他事件平台。
- 验证记录：主审核复跑 `npm test --workspace mail-service` 123/123、五个非联调邮件文件 50/50、`cargo test --manifest-path apps/chat-server/Cargo.toml` 57/57、`cargo test --manifest-path apps/game-server/Cargo.toml` 438/438；chat-server `cargo check`、普通 `cargo clippy --all-targets`、`cargo fmt --check` 通过，game-server `cargo check` 通过；`npx tsc --noEmit -p apps/mail-service/tsconfig.json`、`npm run check:proto`、根数据库静态回归 18/18、相关 Node `--check`、修改文档相对链接/代码围栏和 `git diff --check` 均通过。Rust 检查仅输出既有 warnings；未启动 PostgreSQL、Redis、NATS 或服务进程，真实 backlog、多实例滚动和故障注入仍属于阶段 10。

- [x] 为通知新事件信封提供旧格式兼容窗口和独立开关。（验证：`CHAT_MAIL_ACCEPT_LEGACY_PAYLOAD` 与 `CHAT_MAIL_LEGACY_COMPAT_UNTIL_EPOCH_SECONDS` 接入 `Config::from_env` 和订阅解析；strict/required discovery wiring、截止到期拒绝及 v1 不受影响测试随 chat 57/57 通过）
- [x] 为附件领取新状态机和恢复 worker 提供启停开关，不允许回滚时遗弃已进入新状态的记录。（验证：`MAIL_CLAIM_NEW_REQUESTS_ENABLED=false` 只返回已有状态且零 reserve/grant；recovery=false 配置要求 intake=false，worker 通过 `getMailClaimWorkflowBacklogSummary` 在非 claimed backlog 下抛 `MAIL_CLAIM_RECOVERY_DISABLE_BLOCKED`，内存与 PostgreSQL 路径测试通过）
- [x] 明确部署顺序：先部署兼容消费者和 game-server 幂等查询，再部署 mail-service 生产者与恢复 worker。（验证：`邮件可靠链路灰度与回滚手册.md` 固化数据库 -> game 3009/3010 -> 有期限 chat 兼容 -> intake 关闭的 mail -> 开启新领取顺序）
- [x] 灰度期间对比通知成功率、claiming 数量、幂等命中、指纹冲突和恢复耗时。（验证：灰度手册与 `监控设计.md` 使用真实完整 metrics key，规定基线/灰度同窗口比较、实例明细、指纹冲突暂停和 recovery age/duration 观察）
- [x] 回滚前停止新领取、等待或接管有效租约，并验证存量工作流仍可由兼容版本恢复。（验证：回滚 Runbook 先滚动 intake=false，再查询两类有效 lease，保留 recovery worker、game 幂等查询和冻结请求，禁止清 lease、退回 unread/read 或生成新 request ID）
- [x] 删除旧兼容代码前确认所有实例升级、旧 payload 清空且无旧状态记录。（验证：手册要求 registry build/能力 metadata、active outbox SQL、最后旧 producer 后完整业务高峰 + metrics 窗口 + dedup TTL 零拒绝，以及无非 claimed/旧状态记录）
- [x] 同步聊天与邮件系统设计、协议、数据库、服务发现、监控和故障处理文档。（验证：同步 7 份正式专题文档并新增灰度回滚手册；修改文档相对链接与代码围栏检查通过）
- [x] 记录正式运行所需环境变量、默认值、严格环境限制和本地联调命令。（验证：chat/mail env 示例与手册表覆盖兼容、outbox、intake/recovery、清理、告警、保留期、运维凭证和 game admin 参数；明确 strict 限制、静态命令、外部依赖和 dev-stack 不启动 PostgreSQL/mail-service）
- [x] 完成代码、测试、配置、协议和文档的最终范围核对，确保未扩展到其他服务事件平台。（验证：本阶段代码只改 chat-server/mail-service，game-server 仅复用既有 3009/3010；手册明确不引入 JetStream，不改 session kick、metrics、GM 广播或其他事件，协议检查通过）

## 最终完成定义

以下项目作为邮件跨服务交互的整体完成标准，由全部相关阶段完成后统一验收。

- 开始时间：2026-07-15 11:38:19 +08:00
- 结束时间：2026-07-15 13:30:21 +08:00
- 验收总结：邮件跨服务可靠交互已完成代码、单元、契约和真实故障注入验收。PostgreSQL 邮件/outbox 与 game-server 资产/幂等记录的权威边界成立；通知允许离线跳过，附件领取在断线、响应丢失、进程崩溃、Redis/registry/PostgreSQL 短暂故障、多实例竞争和权威路由切换下均保持稳定请求身份并最终收敛。验收发现的 PostgreSQL Pool/Client 未处理断连错误已修复并纳入回归，启动期日志器尚未配置时错误监听也保持不抛异常；未扩展到其他业务事件平台。

- [x] 邮件创建与通知 outbox 原子提交，NATS 或 chat-server 故障不影响邮件权威数据。（验证：阶段 2 单元事务回滚加阶段 10 NATS/chat 真实故障共同通过）
- [x] chat-server 能兼容消费版本化通知、避免双 subject 重复推送，并安全处理离线和未知版本。（验证：chat 57/57、共享 fixture、strict-registry drill 和真实在线/离线推送通过）
- [x] game-server 对附件发放实施 request_id + 请求指纹幂等，相同请求只产生一次资产结果。（验证：game 438/438，真实断线/丢响应/并发场景均只有一条 grant 和一次资产增量）
- [x] 正式环境由服务端确定 game-server 权威目标，客户端无法控制发奖实例或附件参数。（验证：严格 registry + v2 route 选择权威实例，客户端目标覆盖返回 403，附件覆盖被忽略）
- [x] 任意服务在领取时序任一点崩溃或超时，邮件与资产最终都能自动收敛到一致状态。（验证：发放前断线、成功响应丢失、game 提交后 mail 崩溃、Redis 和 PostgreSQL 断连恢复通过）
- [x] 长期 claiming、未知结果、指纹冲突和终止通知可查询、可告警、可审计并受控恢复。（验证：阶段 7/8/9 的恢复、运维、审计与指标回归随 mail 125/125 通过）
- [x] 三服务的单元、契约、故障注入和端到端联调验证全部通过。（验证：mail 125/125、chat 57/57、game 最终全量复跑 438/438、真实故障 drill 11/11、strict-registry drill 1/1、TypeScript、协议与数据库静态检查通过）
- [x] 灰度部署、兼容窗口、回滚和运行文档完整，不依赖其他业务服务改造。（验证：阶段 11 文档范围核对通过，阶段 10 仅补充邮件链路验收与 PostgreSQL 连接恢复修复）
