# Todo List

## 核心服务
- [x] 限流与风控（防刷、IP限速） ✅
- [x] 内部控制面（管理后台、GM命令） ✅
- [x] 开始游戏/结束游戏状态流转 ✅
- [x] 断线重连与掉线托管 ✅
- [x] 场景/关卡管理 ✅

## 多人游戏核心
- [~] 帧同步实现（lockstep） ⚙️ 第二阶段已完成
- [ ] 状态同步实现（state sync）
- [ ] 延迟补偿算法
- [ ] 房间匹配系统（matchmaking）
- [ ] 观战/OB系统

## 运维支撑
- [ ] 服务健康检查与监控告警
- [ ] 灰度发布/热更新
- [ ] 性能指标采集（QPS、延迟、在线人数）
- [ ] 配置中心（动态下发配置）
- [ ] 统一登录 SSO

## 安全
- [ ] 数据加密（协议加密）
- [ ] 反作弊基础（客户端校验）
- [ ] 敏感操作审计
- [ ] 防火墙/黑白名单

## 可选项
- [ ] 聊天系统增强（禁言、过滤）
- [ ] 好友系统
- [ ] 成就/排行榜
- [ ] 邮件/公告系统
- [ ] SDK 对接（支付、统计）

---

**建议优先级**：限流风控 → 断线重连 → 帧/状态同步 → 房间匹配 → 配置中心

---

## 已完成
- [x] game-server 帧同步第三阶段：支持观战者和断线重连
  - 新增 `MemberRole` 枚举区分 Player/Observer
  - 新增 `RoomJoinAsObserverReq/Res` 观战者加入协议
  - 重连时返回 `snapshot + current_frame_id + recent_inputs`
  - `FrameBundlePush` 每 N 帧携带完整快照
  - `Room.input_history` 保存最近 300 帧输入
- [x] game-server 帧同步第二阶段：为 `FrameBundlePush` 或后续新消息设计并实现"广播完整增量状态"能力。当前第一版仅广播输入集合。
- [x] 场景/关卡管理：RoomLogic 模块化，新增 persistent_world/disposable_match/sandbox 三种场景模板，支持策略化房间生命周期管理
- [x] retain_state_when_empty 逻辑：RoomManager 统一处理空房清理任务，根据策略（destroy_enabled/destroy_when_empty/retain_state_when_empty/empty_ttl_secs）决定销毁时机
