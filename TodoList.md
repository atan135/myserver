# Todo List

## 核心服务
- [x] 限流与风控（防刷、IP限速） ✅
- [x] 内部控制面（管理后台、GM命令） ✅
- [x] 开始游戏/结束游戏状态流转 ✅
- [x] 断线重连与掉线托管 ✅
- [x] 场景/关卡管理 ✅

## 多人游戏核心
- [x] 帧同步实现（lockstep） ✅ 第二阶段已完成
- [x] 状态同步实现（state sync） ✅ 框架就绪（game_state 字段已定义，业务层实现序列化逻辑）
- [ ] 延迟补偿算法 ⚠️ P0 已完成，P1 基础链路已完成，P2/P3 暂缓
- [x] 房间匹配系统（matchmaking） ✅
- [x] 观战/OB系统 ✅ 已集成到帧同步 Phase 2

## 运维支撑
- [x] 服务健康检查与监控告警 ✅
- [ ] 运行时热更新（配置项 / 可在线生效的 CSV）
- [ ] 滚动重启/灰度发布（游戏逻辑 / 启动期固化 CSV）
- [x] 性能指标采集（QPS、延迟、在线人数） ✅
- [x] 配置中心（动态下发配置）✅ 已具备 `game-server` 运行时配置项更新链路；复杂变更走滚动重启/灰度发布
- [x] 统一登录 SSO ✅ 已通过 ticket 机制实现，game-server/chat-server 共用同一套票据验证

## 安全
- [ ] 数据加密（协议加密）
- [ ] 反作弊基础（客户端校验）
- [ ] 敏感操作审计
- [ ] 防火墙/黑白名单

## 玩法底层开发
- [x] 移动同步 ✅
- [ ] 战斗基础
- [x] 背包系统 ✅

## 可选项
- [ ] 聊天系统增强（禁言、过滤）
- [ ] 好友系统
- [ ] 成就/排行榜
- [x] 邮件/公告系统 ✅
- [ ] SDK 对接（支付、统计）

---

**建议优先级**：限流风控 → 断线重连 → 帧/状态同步(已完成) → 房间匹配 → 配置中心

---

## 已完成
- [x] 服务健康检查与监控告警
  - admin-api 提供服务健康状态与监控数据查询接口
  - admin-web 新增服务监控总览与详情页面
  - auth-http 在线指标已优化为唯一玩家数与 5 分钟活跃会话
- [x] 性能指标采集（QPS、延迟、在线人数）
  - game-server 已接入真实 TCP 消息 QPS / 延迟、已认证在线玩家数与 room_count
  - chat-server 已接入真实聊天消息 QPS / 延迟与会话表驱动的在线玩家数
  - game-proxy 已接入真实代理建链延迟与连接数
  - match-service 已接入真实 gRPC QPS / 延迟与匹配池 pool_size
  - 已通过编译和联调验证，监控历史窗口中可看到非 0 指标随业务变化
- [x] 房间匹配系统（matchmaking）
  - MatchService 与 GameServer 的 gRPC 通信
  - 支持创建房间并加入、加入已有房间等匹配场景
  - mock-client 集成测试场景
- [x] game-server 帧同步第三阶段：支持观战者和断线重连
  - 新增 `MemberRole` 枚举区分 Player/Observer
  - 新增 `RoomJoinAsObserverReq/Res` 观战者加入协议
  - 重连时返回 `snapshot + current_frame_id + recent_inputs`
  - `FrameBundlePush` 每 N 帧携带完整快照
  - `Room.input_history` 保存最近 300 帧输入
- [x] game-server 帧同步第二阶段：为 `FrameBundlePush` 或后续新消息设计并实现"广播完整增量状态"能力。当前第一版仅广播输入集合。
- [x] 状态同步框架：RoomLogic 新增 `get_serialized_state()` 和 `restore_from_serialized_state()` 方法，框架层已就绪，业务层实现具体序列化
- [x] 场景/关卡管理：RoomLogic 模块化，新增 persistent_world/disposable_match/sandbox 三种场景模板，支持策略化房间生命周期管理
- [x] retain_state_when_empty 逻辑：RoomManager 统一处理空房清理任务，根据策略（destroy_enabled/destroy_when_empty/retain_state_when_empty/empty_ttl_secs）决定销毁时机
- [x] 邮件通知系统
  - mail-service (Node.js): HTTP REST API 管理邮件，支持创建/读取/标记已读
  - Redis Pub/Sub 实现跨服务通知
  - chat-server 订阅 Redis 频道，收到通知后推送给在线玩家
  - MailNotifyPush (1501) 协议及 mock-client 测试支持
- [x] 移动同步
  - MovementSystem 支持 Entity 移动状态管理
  - MoveInputReq 支持 MOVE_DIR/MOVE_STOP/FACE_TO
  - MovementSnapshotPush 广播位置/朝向快照
  - MovementRejectPush 处理非法移动拒绝
  - mock-client 双客户端移动同步测试
- [x] 背包系统
  - inventory 模块：Item、ItemContainer、EquipmentSlots、AttrPanel、Buff、PlayerData
  - player 模块：PlayerManager、MySqlPlayerStore
  - 协议：ItemEquipReq/Res、ItemUseReq/Res、ItemDiscardReq/Res、WarehouseAccessReq/Res、ItemAddReq/Res、GetInventoryReq/Res
  - 推送：InventoryUpdatePush、AttrChangePush、VisualChangePush
  - mock-client 完整背包测试流程支持

---

## 延迟补偿专项任务清单

参考文档：`docs/network-lag-compensation-design.md`

当前现状：
- 已有按帧输入广播主链路
- 已完成 P0 房间帧等待策略
- 已完成 P1 位移权威校正基础链路，并已通过 `mock-client` 联调
- P2 待确认真实客户端技术栈（是否 Unity）后再启动
- P3 暂按框架层后置，不在当前阶段细化战斗命中回溯

### P0 房间帧等待策略（最高优先级）
- [x] 重构房间输入缓存：从 `Vec<PlayerInputRecord>` 改为按 `frame_id -> player_id` 聚合，能判断“本帧是否收齐输入”
- [x] 为 `RoomRuntimePolicy` 增加延迟补偿参数：`input_delay_frames`、`wait_timeout_ms`、`wait_strategy`、`missing_input_strategy`
- [x] 在 `RoomManager` tick 中加入 `wait_deadline` 逻辑，支持“严格等待全部输入”和“乐观推进”
- [x] 实现缺失输入补偿策略：空输入、复用上一帧输入、长时间缺失踢出
- [x] 明确重复输入、迟到输入、过期输入、同帧覆盖输入的处理规则
- [x] 为观战/重连补充当前等待帧和最近输入窗口的恢复语义，避免客户端恢复后继续错帧
- [x] 增加联调/测试场景：future input、超时补帧、双人不同延迟、重复输入、迟到输入

### P1 位移权威校正通用化（高优先级，基础实现已完成）
- [x] 将 `movement_demo` 中的位移校正逻辑抽成通用模块，不再只在 demo 房间生效
- [x] 统一位移校正触发条件：固定 N 帧校正、误差超阈值立即校正、关键事件强校正
- [x] 为误差判定补齐“客户端预测位置 vs 服务端权威位置”的比较依据，而不是仅靠“有变化/有 reject”触发
- [x] 统一 `MovementSnapshotPush` / `MovementRejectPush` 语义，区分全量校正、增量校正、强校正和原因码
- [x] 将重连恢复中的权威位移恢复纳入正式链路，而不是只靠 `RoomSnapshot.game_state` 文本兜底
- [x] 为大房间预留 AOI / 兴趣管理，只向相关玩家下发附近实体或战斗相关实体的校正
- [ ] 增加 mock-client 验证场景：已完成阈值校正、重连恢复与双客户端一致性回归；阻挡修正/关键事件强校正专项场景按需再补

### P2 客户端预测与回滚重演（暂缓，待确认客户端技术栈）
当前判断：
暂未确认真实客户端是否采用 Unity，因此先不推进客户端协议接入、预测回滚、表现层插值和重连后本地重建，避免围绕错误的客户端实现模型投入。
- [ ] `simple-client` / 真实客户端补齐 `MovementSnapshotPush`、`MovementRejectPush` 的协议定义、解码和事件分发
- [ ] 客户端维护最近输入 ring buffer，并保存最近若干帧预测状态
- [ ] 收到服务端权威状态后实现 `rollback_to(server_state) + replay_recent_inputs()`
- [ ] 为位移校正增加表现层策略：小误差插值、中误差追赶、大误差硬修正
- [ ] 补齐客户端对 `RoomSnapshot.current_frame_id`、`RoomSnapshot.game_state`、成员 `offline/role` 等字段的解码
- [ ] 重连恢复时先基于 `snapshot + current_frame_id + recent_inputs` 重建本地状态，再恢复预测
- [ ] 增加联调场景：高延迟、抖动、丢包、重连后继续移动

### P3 战斗命中回溯（延后，框架层暂不细化战斗逻辑）
当前判断：
项目当前定位为通用后端框架，先提供房间时序、位移权威校正和恢复能力，不在这一阶段深入实现 FPS/TPS 式命中回溯、raycast 与细粒度战斗判定。
- [ ] 先确认是否真的需要 FPS/TPS 式命中回溯；如果当前主要是 MMO/RPG 技能战斗，可继续后置
- [ ] 如需实现，为战斗/射击请求补 `client_timestamp` 或等价的客户端开火时间字段
- [ ] 为战斗实体维护 `PositionHistory` / 历史帧快照环形缓冲区
- [ ] 基于历史帧位置实现 rewind 查询，支持“回到开火时刻”的命中判定
- [ ] 配置命中框 / 判定半径，并实现 raycast 或技能命中判定接口
- [ ] 返回 `hit_frame`、`hit_position` 等结果字段，供客户端在回溯命中点播放特效
- [ ] 增加安全边界：最大可回溯窗口、时间戳校验、异常延迟裁剪
- [ ] 增加专项测试：移动目标、遮挡边缘、延迟尖峰、擦边判定

### 建议推进顺序
1. 维持 P0 / P1 的回归验证，按需要补齐剩余 `mock-client` 场景和更完整的 AOI 能力
2. 等确认真实客户端技术栈后，再决定是否正式启动 P2 的客户端预测与回滚重演
3. 框架层先聚焦通用同步与恢复能力，不提前展开过细的客户端表现层实现
4. P3 继续后置，仅在明确需要 FPS/TPS 式命中回溯时再启动专项开发
