# 文档校准状态汇总

## 当前结论

截至当前仓库状态，`summary.md` 之前记录的多项“文档与代码不一致”已经通过后续文档校准处理，不应继续作为待修复清单使用。

当前正式阅读入口应以这些文件为准：

- `CLAUDE.md`：AI 与协作者入口，只保留整体理念、架构边界、基础设定和文档导航
- `docs/architecture.md`：当前整体架构主说明
- `docs/protocol.md`：当前协议设计主说明
- 各专题文档：对应模块的当前实现、目标设计和未完成项

`docs/prompts/*` 只保留初始设计阶段历史提示词，不再作为当前设计依据。

## 已完成校准的范围

### 整体与协议

- `CLAUDE.md` 已调整为入口文档，不再承载大量细节实现。
- `docs/architecture.md` 已作为当前整体架构主说明。
- `docs/protocol.md` 已按当前协议与服务边界重新校准。

### 游戏服与接入层

- `docs/game-server-rust-guide.md`、`docs/game-server-framework-roadmap.md`、`docs/game-server-frame-sync-design.md`、`docs/game-proxy-hot-update-design.md` 等已按当前 `game-server` / `game-proxy` 代码口径做过校准。
- 文档已区分当前已实现能力、目标设计和仍未落地的能力。

### 配置、场景与具体游戏逻辑

- CSV 配置、CSV 热更、场景地图文档已按当前实现状态补充说明。
- 背包和战斗已从“配置与场景”中拆为“具体游戏逻辑”文档分类。
- `docs/game-server-scene-map-format-design.md` 已明确当前 `SceneQuery` 只提供 `scene`、`spawn_point`、`is_walkable`、`clamp_position`，`resolve_aoi_block` 仍是后续能力。

### 周边服务与后台

- `docs/service-registry-design.md` 已补充当前 Redis 注册中心实现状态，明确没有独立 HTTP Registry API。
- `docs/game-server-chat-design.md` 已修正聊天、邮件、公告的当前真实链路：`chat-server`、`mail-service`、`announce-service` 独立部署，未共用 `message-store`，`game-proxy` 不负责聊天/邮件/公告转发。
- `docs/admin-panel.md` 已补充监控服务清单和当前权限边界。

### 安全

- `docs/security-design.md` 已补充 `auth-http`、`game-proxy`、`game-server`、`chat-server` 的当前安全边界。
- `docs/rate-limit-and-security.md` 已明确当前 ticket 校验、限流和风控现状：
  - `auth-http` 负责 ticket 签发、存储、撤销和登录侧限流
  - `game-proxy` 会校验 `AuthReq` ticket 签名与 Redis ticket 记录
  - `game-server` 会校验 ticket 签名与 Redis 归属
  - `chat-server` 当前只校验 ticket 签名和过期时间，不查询 Redis ticket 记录

## 仍需注意的真实缺口

以下内容不是“文档没写清楚”，而是当前代码能力本身仍未完整落地，相关文档已经或应该明确标注为当前缺口：

- `admin-api` 后端接口当前只有 JWT 登录校验，尚未真正执行基于角色的接口授权。
- `/api/admin/monitoring/*` 监控接口当前没有鉴权，不应直接暴露到公网。
- `game-server` admin 侧 GM 广播、踢人、封禁仍未形成完整端到端闭环。
- `game-proxy` 当前没有 IP 黑名单、单 IP / 单账号连接上限和成熟公网加密方案。
- `game-server` 当前没有统一消息频率限制、时间戳窗口、反重放和通用作弊计数。
- `chat-server` 当前没有 Redis ticket 存在性校验、统一消息频率限制和公网 TLS 策略。
- `mail-service` / `announce-service` 当前缺少统一玩家鉴权、后台鉴权或角色约束。
- `SceneCatalog` 当前不会随 CSV reload 自动重建，场景元数据热更不等于运行中查询立即生效。
- AOI 分块查询、兴趣管理接入、更多场景业务查询仍是后续能力。

## 后续维护口径

1. 如果继续发现文档与代码不一致，优先修正文档中的“当前实现”描述，不要把目标设计写成已完成能力。
2. 如果代码补齐了上述真实缺口，应同步更新对应专题文档和本汇总。
3. 新增功能文档时应放入 `docs/` 的对应分类，不要再依赖 `docs/prompts/`。
