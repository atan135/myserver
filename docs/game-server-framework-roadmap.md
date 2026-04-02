# game-server 底层框架建设路线图

这份文档的目标不是描述某一种具体玩法，而是为 `MyServer` 提供一条清晰的“底层框架化”演进路径。

当前仓库已经具备最小闭环：

- `auth-http` 已支持 guest/password 登录、access token、game ticket
- `game-server` 已支持 TCP 鉴权、心跳、房间加入/离开/准备/开始/结束
- `game-server` 已支持基础房间快照广播和玩家输入广播
- `auth-http` 已支持对 `game-server` 的内部状态查询与运行时配置代理
- `game-server` 已支持 CSV 配置表加载与热更
- 仓库已具备基础的 HTTP 测试和跨服务集成测试

但从“可联调最小闭环”到“可复用游戏底层框架”，还缺少一组更稳定的运行时抽象、治理能力和验收标准。

## 1. 路线图目标

这条路线图优先解决以下问题：

- 把当前偏“连接处理 + 房间逻辑混写”的实现，拆成可维护的框架分层
- 把当前“输入即时广播”的模型，演进为“房间级调度 + 按帧推进”的通用框架
- 把当前“房间状态全在内存、空房直接删”的实现，演进为可配置生命周期模型
- 补齐断线重连、慢连接治理、限流和管理面鉴权等底层能力
- 为后续玩法逻辑接入提供稳定扩展点，而不是继续把业务判断堆到 `server.rs`

## 2. 当前实现判断

当前项目更接近：

- 可运行的房间服
- 可联调的输入广播服
- 具备基础控制面的单机原型

当前项目还不算：

- 完整的房间生命周期框架
- 服务端权威帧同步框架
- 可长期承载多玩法的通用运行时底座

## 3. 建设原则

后续演进建议遵守这几个原则：

- 框架层只处理连接、调度、生命周期、输入聚合、广播、恢复和治理能力
- 业务层只处理具体玩法规则，不反向侵入框架调度结构
- 房间是否销毁、房间跑多少 fps、房间是否允许重连，必须配置化或策略化
- 管理面、风控、监控和测试必须视为框架组成部分，而不是后补工具
- 优先先完成单机内的抽象收敛，再做跨节点、多服和分布式扩展

## 4. 分阶段路线图

### P0：框架骨架收敛

目标：

- 建立清晰的 `ConnectionLayer + RoomManager + RoomLogic` 分层
- 把房间生命周期、调度和输入处理从 `server.rs` 中迁出
- 让 `game-server` 从“功能能跑”进入“结构可扩展”

建议交付：

- 新增 `RoomManager`
- 新增 `RoomRuntimePolicy`
- 新增 `RoomRuntime`
- 新增 `RoomLogic trait`
- 为房间引入 `current_frame`、`policy_id`、`pending_inputs`
- 把房间创建、加入、离开、开始、结束和输入校验迁移到独立模块

建议改动模块：

- `apps/game-server/src/server.rs`
- `apps/game-server/src/room.rs`
- 新增 `apps/game-server/src/room_manager.rs`
- 新增 `apps/game-server/src/room_policy.rs`
- 新增 `apps/game-server/src/room_logic.rs`
- `apps/game-server/src/main.rs`

验收标准：

- `server.rs` 只负责连接接入、鉴权、拆包、分发和调用管理器
- 房间生命周期不再依赖 `server.rs` 内部辅助函数拼接
- 房间逻辑具备明确扩展点，新增玩法不需要继续修改连接处理主流程
- 空房是否销毁不再硬编码为“成员为 0 立即删除”

依赖关系：

- 这是后续所有阶段的基础
- 如果不先做这一步，后续帧同步、恢复和持久化都会继续堆在连接层

### P1：房间级帧推进与生命周期策略

目标：

- 把房间从“输入即时广播”升级为“按帧聚合 + 按房间调度”
- 支持常驻房间、临时房间和空房低频运行
- 让框架具备真正的运行时生命周期模型

建议交付：

- 房间级 tick task
- `silent_room_fps / idle_room_fps / active_room_fps / busy_room_fps`
- `destroy_enabled / destroy_when_empty / empty_ttl_secs`
- `PlayerInputReq` 增加 `frame_id`
- 新增 `FrameBundlePush`
- 房间按帧聚合输入，不再“收到就广播”
- 房主离开、成员离开、空房保留和销毁逻辑统一交给 `RoomManager`

建议改动模块：

- `packages/proto/game.proto`
- `apps/game-server/src/protocol.rs`
- `apps/game-server/src/room.rs`
- `apps/game-server/src/room_manager.rs`
- `apps/game-server/src/room_logic.rs`
- `apps/game-server/src/server.rs`
- `tools/mock-client`
- `apps/simple-client/Assets/Scripts/MyServer`

验收标准：

- 房间有独立 `current_frame`
- 服务端能够按帧收集并广播输入集合
- 无人房间可以低频运行，而不是直接停掉
- 临时房间支持 TTL 销毁，常驻房间支持空房保留
- 当前设计文档 `docs/game-server-frame-sync-design.md` 中的核心抽象已落到代码

已知后续项：

- 第一版 `FrameBundlePush` 可以只广播输入集合
- 完整增量状态广播可以留到下一阶段

### P2：连接恢复、背压治理与安全边界

目标：

- 补齐长连接服务最容易出事故的基础能力
- 让框架能承载真实客户端，而不是只适合 happy path 联调

建议交付：

- 断线重连窗口
- 会话恢复和房间重入
- 顶号或重复登录策略
- 写队列有界化
- 慢连接检测、丢弃或断连策略
- 玩家消息频率限制
- 登录限流、IP 限流、非法包计数
- ticket 单次消费或防重放策略

建议改动模块：

- `apps/game-server/src/session.rs`
- `apps/game-server/src/server.rs`
- `apps/game-server/src/room_manager.rs`
- `apps/game-server/src/ticket.rs`
- `apps/auth-http/src/routes.js`
- `apps/auth-http/src/auth-store.js`
- `apps/auth-http/src/redis-client.js`

验收标准：

- 玩家断线后可在窗口期内恢复会话
- 慢连接不会导致广播链路无限堆积
- 单连接、单玩家、单 IP 都有明确限流边界
- 不合法连接和异常输入有统一惩罚与审计

### P3：控制面、观测性和状态持久化

目标：

- 把“能跑”提升到“可维护、可排障、可运营”
- 把控制能力从 demo 接口提升为正式框架能力

建议交付：

- admin 鉴权和权限模型
- 管理命令审计
- 指标采集：连接数、房间数、广播量、错误码、慢连接数、tick 耗时
- 房间快照持久化接口
- 对局事件流或回放基础结构
- 配置版本号和变更来源追踪

建议改动模块：

- `packages/proto/admin.proto`
- `apps/game-server/src/admin_server.rs`
- `apps/auth-http/src/routes.js`
- `apps/game-server/src/mysql_store.rs`
- 新增 metrics 相关模块

验收标准：

- 管理面不再是裸暴露接口
- 服务问题可以通过日志和指标快速判断位置
- 房间状态和关键事件具备最小恢复与追踪能力

### P4：玩法接入层与多房间模板能力

目标：

- 让“框架”和“玩法实现”真正解耦
- 让后续不同房间类型复用同一套运行时

建议交付：

- 多种 `RoomLogic` 实现注册机制
- 房间模板或策略模板
- 自动建房、匹配分配、常驻房间预创建
- 配置驱动的房间类型装配

建议改动模块：

- `apps/game-server/src/room_logic.rs`
- `apps/game-server/src/room_policy.rs`
- `apps/game-server/src/room_manager.rs`
- `apps/auth-http`
- `tools/mock-client`

验收标准：

- 新增一种玩法逻辑时，不需要改连接层和通用调度层
- 房间是否可销毁、目标 fps、是否保留状态由模板控制
- 自动建房与手动指定房间两种模式可以共存

### P5：分布式与容量扩展

目标：

- 在单机框架稳定后，再解决多节点和扩容问题

建议交付：

- 网关或接入层拆分
- 房间分片与服务发现
- 节点间房间路由
- 跨节点状态同步或迁移机制

说明：

- 这一阶段不建议过早进入
- 在 P0-P4 未稳定前，分布式只会放大当前抽象问题

## 5. 推荐落地顺序

建议实际开发顺序如下：

1. 先完成 P0，收敛结构分层
2. 再完成 P1，拿到真正的房间级运行时框架
3. 之后完成 P2，补齐长连接服务的稳定性底线
4. 再完成 P3，让控制、观测和排障能力成型
5. 最后进入 P4 和 P5，扩展玩法装配和多节点能力

## 5.1 当前阶段调整

当前项目已完成 P0 与 P1 的主体收敛。

从 P2 往后的路线暂时不继续推进，当前优先级切换为一条新的接入层支线：

- 新增 `apps/game-proxy`
- 在客户端与 `game-server` 之间增加一层 proxy
- 把 `game-server` 的热切换、摘流和连接代理能力前置到 proxy
- 客户端与 proxy 使用 `KCP`
- proxy 与 `game-server` 使用 `UDS`

当前设计文档见：

- `docs/game-proxy-hot-update-design.md`

这条支线的目标不是替代 `game-server`，而是补一层独立的连接代理与热切换层。

后续建议开发顺序调整为：

1. 先确认 `game-proxy` 文档细节
2. 再实现 `apps/game-proxy` 最小骨架
3. 再让 `mock-client` 和 `simple-client` 改为优先连接 `game-proxy`
4. 最后再决定是否恢复 P2-P5 的继续开发

## 6. 当前不建议优先做的事

这些方向有价值，但不建议现在优先：

- 过早上多服、多节点和复杂服务发现
- 在没有房间调度框架前就直接做完整状态同步
- 在没有恢复机制前就把客户端做得很复杂
- 在没有统一观测指标前就做大规模压测结论

## 7. 文档对应关系

当前仓库内和本路线图直接相关的文档有：

- `docs/game-server-frame-sync-design.md`
- `docs/game-server-csv-config-design.md`
- `docs/protocol.md`

建议使用方式：

- 本文负责说明“先做什么、后做什么”
- `game-server-frame-sync-design.md` 负责说明 P1 的核心运行时设计
- `protocol.md` 负责说明协议层的当前约束与演进点

## 8. 第一阶段建议拆单

如果按最近一次开发迭代拆任务，建议直接拆成这几项：

1. 拆出 `RoomManager`，把 `join_room / leave_room / start_game / end_game` 从 `server.rs` 迁出
2. 为 `Room` 增加运行时字段和策略引用
3. 定义 `RoomLogic trait` 和最小 `TestRoomLogic`
4. 为 `PlayerInputReq` 增加 `frame_id`，补 `FrameBundlePush`
5. 引入房间 tick task 和基础 fps 策略
6. 调整 `mock-client` 和测试，让新链路可验证

## 9. 路线图完成标志

当满足以下条件时，可以认为“底层框架第一阶段完成”：

- 连接层、房间调度层、玩法逻辑层已明确分离
- 房间级帧推进已可运行
- 房间生命周期已策略化
- 断线恢复和慢连接治理已具备最小能力
- 管理面与观测面已达到基础可运维水平
- 玩法开发可以主要围绕 `RoomLogic` 扩展，而不是改底层接入主干
