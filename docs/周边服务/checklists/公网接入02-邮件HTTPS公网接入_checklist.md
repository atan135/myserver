# 邮件服务内网功能与测试 Checklist

## 目标

验证 `mail-service` 作为内网邮件权威服务的功能、数据一致性和本地联调能力，不依赖 Caddy、DNS、TLS 或正式公网域名：

- 在 development/local 或隔离测试网络中，通过显式 `http://127.0.0.1:9003`（或等价内网 endpoint）验证邮件查询、已读、领取和内部发信。
- 保持 PostgreSQL 邮件权威、Redis ticket ownership/version 校验、NATS 通知 outbox 和 `game-server.admin` 附件资产发放的既有职责。
- 验证邮件读取、领取、重复领取、恢复和下游故障时的幂等与可恢复语义。
- 保留 `mock-client` 的显式内网调试入口，系统发信与玩家读/领使用不同命令和凭证。
- 不执行或验收 Caddy 路由、HTTPS、DNS、证书、防火墙、`services.mail` 公网地址下发、正式客户端接入、灰度或公网回滚；这些事项统一由[公网接入03：周边服务客户端公网接入总体验收](../../../summary/公网接入03-周边服务客户端公网接入总体验收_checklist.md)跟踪。

## 基础原则

- [x] `mail-service`、PostgreSQL 和邮件状态机是邮件事实来源；NATS 推送只用于在线提示，不替代主动查询。（验证：核心流从显式 mail endpoint 拉取 PostgreSQL 权威邮件；可靠性演练验证 NATS/chat 中断不影响邮件落库，恢复后通知指向同一 `mailId`。）
- [x] 内网测试可以显式访问 `9003`，但不得把该 endpoint、service token 或 registry endpoint 当作正式客户端配置。（验证：`tools/mock-client/README.md`、`help.txt` 和 `聊天与邮件系统设计.md` 将 loopback endpoint 限定为内网调试，公网验收继续由 03 清单跟踪。）
- [x] 玩家读取与领取仍以 game ticket 的 player/character claim 为准；内部服务操作必须使用独立服务凭证。（验证：核心流使用真实 character-bound `X-Game-Ticket` 读邮件，内部发信使用独立 `X-Service-Token`；mail-service 140/140 通过。）
- [x] 附件资产只经 `game-server.admin` 权威发放，客户端或测试工具不能覆盖目标角色、附件、来源或实例。（验证：可靠性演练拒绝附件与目标实例覆盖，并由 registry 发现的权威 `game-server.admin` 完成发放；grant 始终为 1。）
- [x] 单元/静态测试与需要 PostgreSQL、Redis、NATS、game-server 的内网联调分开记录。（验证：阶段 5 记录无外部依赖回归；阶段 6 单独记录隔离服务联调、故障注入和清理证据。）
- [x] 启动 PostgreSQL、Redis、NATS、game-server、mail-service 或执行真实领取、发信、故障注入前，先列出依赖并等待用户确认。（验证：Stage 6 在列明隔离依赖与影响后取得用户“确认授权”，随后才启动随机端口服务与故障演练。）

## 阶段 1：内网邮件契约与职责盘点

- 开始时间：2026-07-31 16:38:55 +08:00
- 结束时间：2026-07-31 16:48:17 +08:00
- 开发总结：完成 mail-service HTTP 路由、鉴权和下游副作用盘点，明确玩家读取、内部发信、奖励投递和运维控制面在内网测试中的职责与凭证边界。
- 验证记录：对照 `mails.controller.ts`、`mail-operations.controller.ts`、`health.controller.ts`、`mail-auth.js` 和领取状态机审阅；`git diff --check` 通过，未启动外部依赖或服务。

- [x] 盘点 `mail-service` 全部 HTTP 路由、method、鉴权方式、请求体和下游副作用，形成玩家、服务和运维三类入口清单。（验证：`docs/周边服务/聊天与邮件系统设计.md:395` 的路由表逐项对应 `apps/mail-service/src/mails/mails.controller.ts`、`mail-operations/mail-operations.controller.ts` 和 `health.controller.ts`。）
- [x] 明确内网玩家测试包含邮件列表、详情、标记已读和领取附件，内部服务测试包含发信、奖励投递和恢复控制面。（验证：`聊天与邮件系统设计.md:395` 的路由表与各 controller 当前实现一致。）
- [x] 明确 `9003` 只作为 development/local 的显式测试入口，正式公网入口和客户端地址下发不属于本清单。（验证：`聊天与邮件系统设计.md:400` 将 `9003` 定义为 development/local 内部联调；公网事项已转入 `公网接入03`。）

## 阶段 2：内网请求鉴权与业务安全回归

- 开始时间：2026-07-31 17:15:09 +08:00
- 结束时间：2026-07-31 17:44:16 +08:00
- 开发总结：玩家读取路由统一使用 game ticket 的 player/character claim，拒绝客户端身份和领取目标覆盖；请求校验、错误分类、分层限流、日志与指标均保持服务端权威边界。
- 验证记录：`npx tsc --noEmit -p apps/mail-service/tsconfig.json` 通过；config、registry、请求处理和邮件服务定向测试 69/69 通过；`git diff --check` 通过，未启动外部依赖。

- [x] 玩家读取、详情、已读和领取从 game ticket 取得账号玩家 ID，不信任 query/body 的 `player_id` 覆盖。（验证：`apps/mail-service/src/mails/mails.controller.ts:80`-`:89` 在 action 前校验 ticket；`public-mail-request.js:270` 拒绝非白名单 query。）
- [x] 领取角色 ID 只来自 character-bound ticket 或服务端权威上下文，客户端不得覆盖附件、来源、request ID 或目标实例。（验证：`mails.controller.ts:211` 对领取 body 强制为空；`mails.service.ts:763`-`:766` 使用 authenticated character；`mails.service.test.ts:340` 覆盖 body override 无效。）
- [x] mail ID、分页、状态、locale、body 和 header 具备明确格式与大小校验，并使用稳定的错误分类。（验证：`public-mail-request.js:13`-`:290` 的输入限制和 `http-exception.filter.ts:16`-`:63` 的错误映射；`public-mail-request.test.js` 覆盖非法输入与错误脱敏。）
- [x] 邮件读取、领取、并发领取和单账号邮箱扫描具备分层限流，日志和指标不记录邮件正文或高基数身份标签。（验证：`mail-player-rate-limiter.js:57`-`:78`、`mails.controller.ts:87`-`:123` 和 `metrics.js:369`-`:379`；`public-mail-request.test.js` 覆盖限流与指标标签。）

## 阶段 3：内部附件领取与服务协作回归

- 开始时间：2026-08-02 11:40:22 +08:00
- 结束时间：2026-08-02 12:03:02 +08:00
- 开发总结：完成邮件业务、Redis ticket ownership/version 和附件领取状态机回归；索引化 service registry 迁移导致的测试 mock 漂移已修复，未改变 PostgreSQL 邮件权威或 game-server admin 幂等请求。
- 验证记录：`npm test --workspace mail-service` 140/140、`npx tsc --noEmit -p apps/mail-service/tsconfig.json`、`npm run check:discovery-config` 0 violations、`node --test tests/registry/deployment-discovery-dry-run.test.mjs` 2/2 均通过；未启动外部依赖。

- [x] 单元测试覆盖 ticket 篡改、Redis owner/version、跨玩家 ownership、附件/角色/目标实例覆盖和领取状态恢复。（验证：`src/mail-auth.test.js`、`src/mails/public-mail-request.test.js` 与 `mails.service.test.ts`；`npm test --workspace mail-service` 140/140 通过。）
- [x] 附件领取继续通过 `game-server.admin` 的幂等请求完成资产发放，重复领取不重新发放附件。（验证：`mails.service.ts:763`-`:766`、`:915` 保持服务端权威参数；`mails.service.test.ts` 覆盖领取请求参数与状态机。）
- [x] service registry 内部发现使用索引化 `ZRANGEBYSCORE` 与 pipeline 契约，fixture 覆盖完整 endpoint 和缺失 endpoint 的失败路径。（验证：`npm run check:discovery-config` 0 violations；`deployment-discovery-dry-run.test.mjs` 2/2 通过。）

## 阶段 4：本地工具与内网调试兼容

- 开始时间：2026-07-31 18:48:38 +08:00
- 结束时间：2026-08-02 11:30:19 +08:00
- 开发总结：完成 mock-client 邮件测试命令的身份与凭证边界整理，保留显式 `--mail-base-url` 内网调试方式，并将系统发信与玩家读/领隔离。
- 验证记录：`npm test --workspace mock-client` 7/7 通过；未启动 mail-service、Redis、PostgreSQL 或 game-server。

- [x] mock-client 玩家邮件请求自动携带真实 game ticket，不通过 `player_id` 冒充身份。（验证：`tools/mock-client/src/scenarios/mail.js` 的 list/get/read/claim 构造 `X-Game-Ticket`；`mail-https.test.js` 断言 query/body 无 player ID。）
- [x] 保留 `--mail-base-url` 作为显式内网调试参数，未配置公开 descriptor 时不隐式猜测内部 endpoint。（验证：`auth.js` 与 `mail-https.test.js` 覆盖 explicit override 和缺配置分支。）
- [x] 系统发信与玩家读/领使用不同命令和凭证，玩家场景不自动读取 `MAIL_SERVICE_TOKEN`。（验证：`mail.js` 拒绝玩家 service token；`mail-send`/`mail-send-and-notify` 要求显式 HTTP `--mail-base-url` 与 `--service-token`。）
- [x] mock-client 文档将 `9003` 标为仅限内部联调，禁止将系统发信接口用于玩家测试路径。（验证：`tools/mock-client/README.md` 与 `help.txt` 已区分玩家读/领和内部发信。）

## 阶段 5：无外部依赖自动化回归

- 开始时间：2026-08-02 11:40:22 +08:00
- 结束时间：2026-08-02 12:03:02 +08:00
- 开发总结：完成 mail-service、服务发现配置和 mock-client 的无外部依赖回归，内网业务功能保持稳定。
- 验证记录：`npm test --workspace mail-service` 140/140、`npx tsc --noEmit -p apps/mail-service/tsconfig.json`、`npm run check:discovery-config` 0 violations、`node --test tests/registry/deployment-discovery-dry-run.test.mjs` 2/2、`npm test --workspace mock-client` 7/7 均通过。

- [x] 使用 `npm test --workspace mail-service` 验证邮件业务、领取状态机、鉴权和错误处理不回归。（验证：140/140 通过。）
- [x] 使用 `npx tsc --noEmit -p apps/mail-service/tsconfig.json` 验证 TypeScript。（验证：通过。）
- [x] 使用 `npm run check:discovery-config` 和 deployment-discovery fixture 验证内部服务发现配置。（验证：0 violations；fixture 2/2 通过。）
- [x] 使用 `npm test --workspace mock-client` 验证内网邮件工具参数、身份边界和回归场景。（验证：7/7 通过。）

## 阶段 6：隔离内网服务联调

- 开始时间：2026-08-03 10:16:33 +08:00
- 结束时间：2026-08-03 11:58:01 +08:00
- 开发总结：完成随机端口、专用临时数据库和独立 Redis prefix 下的邮件核心链路与可靠性故障演练；补齐邮件断言重试语义、冻结附件公开映射、真实背包容量阻塞及进程启动/清理门禁，未访问 Caddy、DNS、TLS 或公网入口。
- 验证记录：主 agent 复跑核心联调 1/1、清理测试 4/4、可靠性演练 12/12、managed-process 2/2、mail-service 140/140、邮件断言 3/3、TypeScript 和隔离 game-server 构建均通过；临时数据库和本轮测试进程残留均为 0。game-server 全量 506 项中邮件相关通过，但 33 个既有 `config::tests` 因生产断言公钥必填校验未同步 fixture 失败，不属于本轮改动。

- [x] 经用户确认后准备隔离 PostgreSQL、Redis、NATS、auth-http、mail-service、game-server、chat-server 和测试账号/角色；Caddy、外网 DNS 与证书不在本阶段范围内。（验证：用户于本轮明确授权；`mail-internal-core-flow.test.mjs` 和 `mail-reliability-fault-drill.test.mjs` 使用随机端口、`myserver_mail_acceptance_<run-id>` 专用数据库与独立 Redis prefix，未启动或访问 Caddy、DNS、证书配置。）
- [x] 经显式内网 `mail-service` endpoint 验证真实登录、选角、签票后的列表、详情、已读和无附件邮件操作。（验证：`tests/mail/mail-internal-core-flow.test.mjs` 使用专用 PostgreSQL、随机 Redis/NATS/auth/mail 端口完成真实注册、再次登录、建角、选角签票、内网发信、列表、详情、已读和无附件领取拒绝，主 agent 复跑 1/1 通过；`tests/mail/mail-runtime-cleanup.test.mjs` 4/4 证明主流程与清理双重失败不丢失；专用数据库残留 0。）
- [x] 验证在线角色领取附件后只发放一次，重复点击、并发请求和响应丢失保持幂等。（验证：`mail-reliability-fault-drill.test.mjs` 覆盖正常领取、双 mail-service 并发领取、响应丢失与 mail-service 提交后崩溃恢复，所有场景的 `grantCount(mailId)` 均为 1；主 agent 完整演练 12/12 通过。）
- [x] 验证离线/路由缺失、背包容量不足、game-server 切换、mail-service 重启和 PostgreSQL/NATS 短暂故障语义不回归。（验证：真实演练覆盖 grant 前 game-server 离线、registry 路由丢失/恢复、48 格背包容量原子阻塞与释放空间后恢复、双 game-server 权威切换、mail-service 重启、Redis/PostgreSQL/NATS 故障恢复；12/12 通过。）
- [x] 验证新邮件经 NATS/chat 通知后，测试工具能够通过内网 endpoint 拉取同一权威邮件。（验证：真实演练的正常、NATS 中断恢复和 chat 重连场景校验通知 `mailId` 与 PostgreSQL/mail-service 权威邮件一致，chat 离线不影响邮件落库且不回放历史通知；12/12 通过。）
- [x] 联调结束后清理测试进程、临时数据库、Redis prefix、测试邮件和账号，并记录清理证据。（验证：测试清理钩子终止全部托管进程、删除并反查 Redis prefix、关闭连接并删除专用数据库；主 agent 复验 `myserver_mail_acceptance_%` 数据库 0、本轮近 5 分钟测试进程 0，默认端口既有服务未停止。）

## 阶段 7：内网测试文档与归档

- 开始时间：2026-08-03 11:59:28 +08:00
- 结束时间：2026-08-03 12:07:49 +08:00
- 开发总结：完成邮件设计与 mock-client 文档的内网验收说明，明确隔离入口、随机端口、专用数据库、服务发现、凭证边界、定向命令和失败清理；清单已归档到周边服务 checklists。
- 验证记录：主 agent 对照当前测试 harness、参数解析和服务启动配置逐项复核；`git diff --check` 与新增文档敏感信息扫描通过，未启动额外服务。

- [x] 同步邮件设计与 mock-client 文档，明确本清单只覆盖内网测试入口、依赖和验证命令。（验证：`聊天与邮件系统设计.md` 新增 4.5 内网隔离测试与验收；`tools/mock-client/README.md` 和 `help.txt` 同步 loopback 示例与六条定向命令。）
- [x] 核对内网测试配置、服务发现、测试凭证、日志脱敏和清理步骤，不记录真实 token 或生产地址。（验证：文档准确区分 auth descriptor 关闭与 mail registry 独立注册，说明 ticket/service token 隔离、环境注入、`runWithCleanup` 和残留反查；新增内容敏感信息扫描无命中。）
- [x] 内网测试全部完成后，将本清单归档到 `docs/周边服务/checklists/`。（验证：清单已移动为 `docs/周边服务/checklists/公网接入02-邮件HTTPS公网接入_checklist.md`，归档前确认目标路径不存在。）

## 最终完成定义

以下项目作为邮件服务内网功能与测试的整体完成标准，由全部相关阶段完成后统一验收。

- 开始时间：2026-08-03 12:07:49 +08:00
- 结束时间：2026-08-03 12:07:49 +08:00
- 验收总结：邮件服务内网功能与测试验收完成。真实注册/登录/选角/签票、邮件读写和附件领取闭环可重复执行，故障恢复与 exactly-once 发放通过隔离演练，资源清理无残留；公网入口事项未纳入本清单。

- [x] 隔离内网环境中，邮件列表、详情、已读、内部发信和附件领取形成完整可重复的验证闭环。（验证：核心流 1/1、可靠性演练 12/12；专用数据库与随机端口运行后自然退出。）
- [x] 邮件 ownership、角色绑定、附件参数、领取幂等、恢复和下游故障语义均保持服务端权威。（验证：mail-service 140/140、game-server 邮件断言 3/3；并发、响应丢失、容量阻塞和服务切换场景 grant 均为 1。）
- [x] PostgreSQL 邮件权威、Redis ticket ownership/version、NATS 通知和 game-server admin 发放职责保持清晰且无回归。（验证：核心流和故障演练分别验证 PostgreSQL 拉取、Redis ticket/route、NATS/chat 通知和 registry-discovered admin 发放。）
- [x] mock-client 的显式 `9003` 内网调试与服务凭证边界可用，测试清理完整。（验证：mock-client 7/7；文档和 help 使用显式 loopback endpoint，玩家 ticket 与内部 service token 分离；临时数据库、Redis prefix 和本轮测试进程残留为 0。）
- [x] 不把 Caddy、HTTPS、DNS、TLS、公开 endpoint 或正式客户端公网验收记入本清单。（验证：清单目标、Stage 7 文档和实际测试均限定于 loopback/隔离内网；公网事项链接到 03 清单。）
