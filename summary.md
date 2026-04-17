# 文档与代码不一致汇总

## 对比范围

- 文档：`CLAUDE.md`、`README.md`、`docs/*.md`
- 未纳入本次对比：`docs/prompts/*`
- 代码：按文档涉及的核心服务、协议、配置与启动链路进行核对，重点覆盖 `auth-http`、`admin-api`、`admin-web`、`mail-service`、`chat-server`、`match-service`、`game-proxy`、`game-server`、`packages/proto`

## 分类口径

- `文档未写明`：代码里已经存在能力，或者当前实现状态已经变化，但文档缺失、过时或描述错误
- `代码未实现`：文档把能力写成当前方案/既定能力/已完成设计，但代码未实现、只实现了一部分，或者仍是 stub
- 说明：对于设计类文档，如果文档已经明确写成“待完成/后续阶段/开放问题”，这类内容不计入本次不一致

### 7. `docs/game-server-chat-design.md` 描述的聊天/邮件整体方案只实现了一部分

- 判定：`代码未实现`
- 文档侧：文档画出了 `chat-service + mail-service + announce-svc` 的整体结构，并描述了附件领取、邮件/聊天关系、离线消息体验等。
- 代码侧：
- `announce-service` 已实现，并支持公告 CRUD、有效公告查询、Redis 注册、metrics 上报；`mock-client` 已有 `announce-list/get/create/update/delete` 调试场景
- 没有看到“聊天与邮件共用存储层”的实现
- 邮件附件领取已经实现：`mail-service` 提供了 `POST /api/v1/mails/:mailId/claim`，会先调用 `game-server admin` 发奖，再将邮件状态更新为 `claimed`
- 离线聊天当前更接近“历史可查询”，代码中未看到“登录后自动补发离线消息”的闭环
- 相关文件：`docs/game-server-chat-design.md`，`apps/chat-server/src/chat_store.rs`，`apps/chat-server/src/chat_service.rs`，`apps/chat-server/src/mail_subscriber.rs`，`apps/mail-service/src/routes.js`，`apps/announce-service/src/routes.js`，`tools/mock-client/src/scenarios/announce.js`

### 8. `docs/rate-limit-and-security.md` 描述的风控分层只实现了 `auth-http` 的一部分

- 判定：`代码未实现`
- 文档侧：文档描述了 `auth-http`、`game-proxy`、`game-server` 三层风控。
- 代码侧：
- ticket 不是一次性消费，`game-server` 验证通过后没有删除 Redis ticket
- ticket 默认 TTL 是 24 小时，不是文档中的 5 分钟
- `game-proxy` 的 IP 限速、单 IP 连接数限制、黑名单未实现
- `game-server` 的消息频率限制、操作冷却未看到实现
- 相关文件：`docs/rate-limit-and-security.md`，`apps/auth-http/src/auth-store.js`，`apps/auth-http/src/config.js`，`apps/game-server/src/core/service/core_service.rs`，`apps/game-proxy/src/*`，`apps/game-server/src/*`

### 9. `docs/game-server-scene-map-format-design.md` 里的第一阶段查询接口未完整实现

- 判定：`代码未实现`
- 文档侧：文档列出了 `is_walkable / is_blocked / resolve_aoi_block / clamp_position` 一组查询能力。
- 代码侧：当前 `SceneQuery` 只实现了 `is_walkable` 和 `clamp_position`，没有 `is_blocked`、`resolve_aoi_block`。
- 相关文件：`docs/game-server-scene-map-format-design.md`，`apps/game-server/src/core/system/scene/query.rs`

### 10. `docs/admin-panel.md` 的“所有管理接口都需要 Bearer token”与代码实现不符

- 判定：`代码未实现`
- 文档侧：安全说明写的是“所有管理接口需要 `Authorization: Bearer <token>`”。
- 代码侧：监控接口在认证中间件之前挂载，当前是匿名可访问。
- 相关文件：`docs/admin-panel.md`，`apps/admin-api/src/routes.js`

---

## 三、建议优先修正顺序

### 高优先级

- 修正文档中的端口、协议和实现状态，避免开发/联调时直接连错端口或误判功能是否可用
- 修复 ticket 一次性消费、监控接口鉴权这两项“文档承诺已存在但代码未兑现”的问题，并修正文档/汇总里对 `mail-service` 附件领取实现状态的误判
- 补齐 `service-registry` 的登录响应和实例级心跳口径，避免文档、注册中心和监控系统各自维护不同事实

### 中优先级

- 更新 `docs/admin-panel.md`、`docs/monitoring-design.md`、`docs/protocol.md`
- 把 `match-service`、背包、聊天/邮件、场景查询这些“部分实现”明确标注为当前进度，避免文档看起来像已经全部可用
