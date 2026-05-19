# game-server 底层框架路线图

## 1. 文档定位

本文记录 `game-server` 从最小房间服演进为通用游戏运行时框架的路线图和当前状态。

本文不是当前架构主说明：

- 当前服务边界以 [architecture.md](./architecture.md) 为准
- 当前协议号和字段以 [protocol.md](./protocol.md) 为准
- 已落地的房间运行时细节以 [game-server-frame-sync-design.md](./game-server-frame-sync-design.md) 为准

路线图可以保留为独立文档，因为它回答的是“接下来按什么顺序补底层能力”，不适合并入架构或协议文档。

## 2. 当前状态概览

当前项目已经不再是早期“连接处理 + 简单房间逻辑混写”的版本，`apps/game-server/src` 已经完成第一轮框架化拆分：

- `core/`
  - 通用运行时与抽象：`context`、`logic`、`runtime`、`room`、`service`、`system`、`config_table`、`player`、`inventory`
- `gameroom/`
  - 具体房间逻辑：`test_room`、`persistent_world`、`disposable_match`、`sandbox`、`movement_demo`、`combat_demo`
- `gameservice/`
  - 游戏侧业务消息入口，如 `room_query`、`config`、`debug`
- `gameconfig/`
  - 具体 CSV 表注册和装配

当前稳定能力：

- `RoomManager + RoomRuntimePolicy + RoomLogic` 分层已形成
- 房间级 tick、帧输入聚合、定时快照、输入历史已落地
- 常驻房、临时房、sandbox、移动 demo、战斗 demo 等策略/逻辑已经接入
- 观战和断线重连可返回快照、最近输入、等待输入和移动恢复状态
- CSV 表支持启动加载和运行时热更新
- `game-proxy` 已作为 KCP 接入层落地，并支持 TCP fallback、静态上游和注册中心发现
- `game-server` 已有 admin 口、internal socket、metrics、MySQL 审计、服务注册和 session kick
- drain mode 已能阻止新建房，并允许已存在房间的加入、重连和观战

当前需要继续注意：

- 发送队列仍是无界 `mpsc::UnboundedSender`，慢连接治理还不完整
- 玩家消息频率限制、非法包惩罚和更完整的风控边界仍需补齐
- `game-server` admin 口已支持状态、配置、drain 和 GM 发物品；广播、踢人、封禁仍是后台入口和消息号预留，服务端 handler 未完整实现
- room transfer 的 proto 和消息号已定义，proxy 侧也有 rollout 状态管理，但完整冻结、导出、导入、迁移退休链路仍未闭环
- 服务发现仍是部分服务接入，尚未统一到所有服务同一实现和同一健康判定模型

## 3. 建设原则

后续演进继续遵守这些原则：

- 框架层只处理连接、调度、生命周期、输入聚合、广播、恢复和治理能力
- 业务层只处理具体玩法规则，不反向侵入框架调度结构
- 房间是否销毁、跑多少 fps、是否允许重连、缺帧怎么处理，必须策略化
- 管理面、风控、监控和测试视为框架组成部分，而不是后补工具
- 优先完成单机内抽象收敛，再推进跨节点、多服和分布式扩展

## 4. 阶段状态

### P0：框架骨架收敛

状态：已基本完成。

已落地：

- `ConnectionLayer + RoomManager + RoomLogic` 分层
- `RoomRuntimePolicy`
- `RoomLogic trait` 与 `RoomLogicFactory`
- 房间生命周期迁出 `server.rs`
- `core/` 与 `gameroom/` 依赖方向基本清晰

剩余关注：

- 继续减少 `server.rs` 对具体业务消息的认知
- 对跨模块公共错误码和日志字段做进一步统一

### P1：房间级帧推进与生命周期策略

状态：已基本完成。

已落地：

- 房间级 tick task
- `FrameBundlePush`
- 输入按帧聚合
- 未来帧输入缓存、过期帧拒绝、缺帧策略
- `silent / idle / active / busy` fps 策略
- 空房保留、离线 TTL、cleanup task
- 观战与重连恢复数据

剩余关注：

- 更完整的增量状态同步和回放结构
- 不同玩法对快照频率、AOI 和恢复数据的定制化

### P2：连接恢复、背压治理与安全边界

状态：部分完成。

已落地：

- 断线重连窗口和离线成员保留
- 同账号并发登录踢旧连接
- header 校验、body 长度限制、心跳超时
- drain mode 下新建房拦截
- `auth-http` IP 限流、账号锁定和安全审计

仍需补齐：

- 有界写队列、慢连接检测和丢弃策略
- 单连接 / 单玩家消息频率限制
- 非法包计数、封禁或短期惩罚策略
- ticket 单次消费、刷新、吊销与 proxy/game-server/chat-server 的一致校验模型

### P3：控制面、观测性和状态持久化

状态：部分完成。

已落地：

- `game-server` admin 状态查询、运行时配置更新和 drain mode
- Redis metrics 与 heartbeat
- `admin-api + admin-web` 监控页面
- MySQL 账号、审计、连接事件、房间事件、玩家背包数据等持久化
- metrics 归档服务

仍需补齐：

- `game-server` admin 口自身的认证和权限边界
- GM 广播、踢人、封禁的服务端闭环
- 房间快照持久化接口
- 对局事件流、回放和可恢复状态版本
- 配置版本号、变更来源和灰度追踪

### P4：玩法接入层与多房间模板能力

状态：部分完成。

已落地：

- 多个 `RoomLogic` 实现
- `GameRoomLogicFactory`
- 多种 `RoomRuntimePolicy`
- `movement_demo`、`combat_demo` 等玩法样例
- CSV 表与场景、移动、战斗、背包系统的初步装配

仍需补齐：

- 配置驱动的房间模板注册
- 自动建房、匹配分配和常驻房预创建
- 更清晰的玩法模块目录规范、状态导出接口和测试模板

### P5：分布式与容量扩展

状态：未完成，不建议过早推进。

当前已具备基础：

- `game-proxy` 路由层
- Redis service registry
- `game-server` 注册实例和 internal socket metadata
- proxy 按注册中心发现上游
- rollout session 与 room route store 的初步结构

仍需补齐：

- 房间 owner 路由的统一控制面
- 完整 room transfer 协议处理
- 跨节点状态迁移和失败回滚
- 多 proxy、多 game-server 下的一致路由与健康判定

## 5. 推荐后续顺序

建议按以下顺序继续：

1. 先补齐 P2 的背压和消息频率限制，避免长连接服务在真实客户端下出现内存和刷包风险。
2. 补齐 admin / GM 控制面的真实闭环，尤其是后台已有入口但服务端尚未处理的广播、踢人、封禁。
3. 完成 room transfer 的最小可验证链路，把 `1601-1610` 从协议预留推进到可联调能力。
4. 统一 service registry、metrics heartbeat 和服务健康判定口径。
5. 再推进配置驱动房间模板和更多玩法接入规范。
6. 最后再考虑多节点迁移、分片和跨服扩容。

## 6. 相关文档

- [整体架构](./architecture.md)
- [协议设计](./protocol.md)
- [帧同步与房间生命周期设计](./game-server-frame-sync-design.md)
- [更新策略拆分](./game-server-update-strategy.md)
- [game-proxy 热切换代理设计](./game-proxy-hot-update-design.md)
- [空房接管式灰度规范](./game-server-room-rollout-spec.md)
- [CSV 配置表设计](./game-server-csv-config-design.md)
- [场景地图格式设计](./game-server-scene-map-format-design.md)
- [战斗 ECS 设计](./game-server-combat-ecs-design.md)
