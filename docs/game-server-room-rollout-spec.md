# game-server 空房接管式灰度发布技术规范

这份文档用于约束后续 `game-proxy + game-server` 的灰度发布实现。

配套任务拆分见:

- [game-server-room-rollout-task-list.md](./game-server-room-rollout-task-list.md)

规范源声明:

- `docs/game-server-room-rollout-spec.md` 是“空房接管式灰度发布”的唯一规范源。
- `docs/game-server-room-rollout-task-list.md` 只是基于本规范的执行拆分，不得覆盖或改写本规范结论。
- 若任务清单、实现注释或其他设计稿与本文冲突，统一以本文为准。

当前冻结结论:

- room 接管判定在第一阶段固定使用“成员为空”，不使用“在线人数为 0”。
- 第一阶段客户端切服固定经过“旧服通知 -> 旧服断开 -> 客户端显式重连 -> `proxy` 重新路由”的链路。
- 第一阶段明确不做“同一连接内换 upstream”。

本文讨论的不是运行时 CSV 热更新，也不是在线有人房间的无感迁移，而是:

- 只有 `old_server` 和 `new_server` 两台 `game-server`
- `old_server` 上仍有玩家的 room 继续在旧服运行
- room 变成空房后，所有权再切到 `new_server`
- 客户端在需要切服时执行一次显式重连
- `new_server` 在接管 room 时，先从 `old_server` 拉取权威 room 状态

统一称为:

- `空房接管式灰度`

## 1. 目标

该方案要满足以下目标:

1. 灰度期间只存在一个旧服和一个新服。
2. `proxy` 能按 room 归属把玩家路由到旧服或新服。
3. 旧服上的 room 只有在没有玩家后，才能切换到新服。
4. 新服接管 room 时，必须从旧服拉取权威状态，而不是空建同名 room。
5. 旧服上所有 room 清空并完成接管后，旧服才能停止。

## 2. 非目标

第一阶段明确不做以下能力:

- 在线有人 room 的无感跨服迁移
- `proxy` 解析具体玩法逻辑或成为玩法状态权威节点
- 多旧服 / 多新服 / 分比例放量
- 同一个客户端连接内的无重连切服

## 3. 术语

- `old_server`: 灰度中的旧版本 `game-server`
- `new_server`: 灰度中的新版本 `game-server`
- `room owner`: 当前对某个 `room_id` 负责的 `game-server`
- `room route`: `room_id -> owner_server_id` 的路由记录
- `rollout_epoch`: 一次灰度发布的唯一编号
- `empty room takeover`: 空房后由新服接管 room
- `transfer payload`: room 导出给新服的完整迁移数据

## 4. 总体原则

### 4.1 路由粒度

`proxy` 的决策粒度必须提升到 `room_id`，而不是只按连接选一个默认 upstream。

### 4.2 客户端切服方式

第一阶段采用显式重连:

1. 旧服通知客户端需要重连
2. 旧服主动断开连接
3. 客户端重新连接 `game-proxy`
4. `proxy` 根据最新 room route 决定进入旧服或新服

该结论在第一阶段视为冻结约束:

- 必须经过客户端显式重连
- 不做同一连接内换 upstream
- 不要求 `proxy` 在既有业务连接内接力切服

### 4.3 一致性原则

room 的接管不能只依赖普通 `RoomSnapshot`。

必须使用以下原子流程:

1. 冻结旧 room
2. 导出权威状态
3. 新服导入状态
4. 更新 `proxy` 路由
5. 删除或封存旧 room

只要 room 还可能继续 tick，就不允许切换 owner。

## 5. 组件职责

### 5.1 game-proxy

`proxy` 负责:

- 维护灰度会话的 `rollout_epoch`
- 维护 room 级路由元数据
- 按 `room_id` / `player_id` 进行接入决策
- 区分旧房玩家和新房玩家应该进入哪台 server
- 在旧服彻底排空后结束灰度模式

`proxy` 不负责:

- NPC、怪物、行为树等玩法细节
- room 内部状态计算
- 权威游戏状态存储

### 5.2 old_server

`old_server` 负责:

- 继续托管已有 room
- 在业务层诱导玩家离开 room
- 在 room 满足条件时冻结 room
- 导出 room 的完整权威状态
- 在接管完成前拒绝该 room 的再次新建

### 5.3 new_server

`new_server` 负责:

- 注册为可用实例
- 接受 `proxy` 为新 room 分配的新流量
- 在接管旧 room 时向 `old_server` 拉取状态
- 成功导入后成为新的 room owner

## 6. proxy 需要维护的最小元数据

`proxy` 只应保存 room 路由元数据，不保存具体玩法状态。

推荐最小结构:

```text
RolloutSession {
  rollout_epoch,
  old_server_id,
  new_server_id,
  state,
}

RoomRouteRecord {
  room_id,
  owner_server_id,
  migration_state,
  member_count,
  online_member_count,
  empty_since_ms,
  room_version,
  rollout_epoch,
  last_transfer_checksum,
}

PlayerRouteRecord {
  player_id,
  current_room_id,
  preferred_server_id,
  rollout_epoch,
}
```

### 6.1 proxy 必须知道的信息

- room 当前归属哪台 server
- room 是否允许接管
- room 当前是否为空
- 某玩家当前是否仍属于旧 room
- 当前灰度是否已结束

### 6.2 proxy 不应知道的信息

- 怪物列表细节
- NPC 行为树节点
- buff、冷却、技能目标
- 仇恨、黑板、脚本变量

## 7. room 状态模型

推荐定义以下 room 状态:

```text
OwnedByOld
DrainingOnOld
FrozenForTransfer
ImportingToNew
OwnedByNew
TransferFailed
RetiredOnOld
```

状态说明:

- `OwnedByOld`: room 正常在旧服运行
- `DrainingOnOld`: 旧服仍持有 room，但处于排空阶段
- `FrozenForTransfer`: 旧服已冻结 room，等待导出
- `ImportingToNew`: 新服正在导入 payload
- `OwnedByNew`: room 已被新服接管
- `TransferFailed`: 导入失败，仍需回滚到旧服 owner
- `RetiredOnOld`: 旧服已确认不再持有该 room

## 8. 玩家切服模型

### 8.1 第一阶段必须使用显式重连

当前 `proxy` 还是连接级代理，第一阶段不做“同一连接内换 upstream”。

因此玩家从旧服切到新服时，必须经过:

1. 旧服下发重连通知
2. 客户端断线
3. 客户端重连 `proxy`
4. 客户端重新鉴权
5. 客户端再次发起 `RoomJoinReq` 或 `RoomReconnectReq`

### 8.2 谁通知客户端

第一阶段必须由 `old_server` 通知客户端，而不是由 `proxy` 中途改写业务会话。

原因:

- `proxy` 不需要在第一阶段理解完整业务协议
- 旧服最清楚某玩家何时必须离开旧 room
- 旧服可以在通知后主动断开连接，逼出明确的重连路径

### 8.3 客户端通知协议

建议新增正式协议:

```text
ServerRedirectPush {
  reason,
  room_id,
  rollout_epoch,
  reconnect_required,
  retry_after_ms,
}
```

字段要求:

- `reason`: 如 `room_migrated` / `server_rollover`
- `room_id`: 目标 room
- `rollout_epoch`: 当前灰度编号
- `reconnect_required`: 第一阶段必须为 `true`
- `retry_after_ms`: 可选的客户端退避时间

不建议长期复用 `ErrorRes` 表示切服。

## 9. room 接管条件

room 只有满足以下条件时，才允许从旧服切到新服:

1. `room.members.is_empty()` 为真，或业务层显式判定该 room 已无成员
2. room 当前没有进行中的导出任务
3. room 当前没有未提交的导入任务
4. room 已被旧服冻结

第一阶段建议使用严格条件:

- 以“成员为空”作为切换条件
- 不以“在线人数为 0”作为切换条件

该结论在第一阶段视为冻结约束，不再保留“两者都可”的实现口径。

原因:

- “在线人数为 0”仍可能允许断线重连
- “成员为空”才表示 room 生命周期已经走到真正可接管阶段

## 10. 冻结与接管流程

### 10.1 标准时序

```text
1. proxy / control plane 发现 room 满足接管条件
2. old_server 将 room 标记为 DrainingOnOld
3. old_server 冻结 room，状态变为 FrozenForTransfer
4. old_server 导出 RoomTransferPayload
5. new_server 拉取或接收 payload
6. new_server 导入 payload 并创建同 room_id 的 room
7. new_server 返回导入成功和 checksum
8. proxy 更新 room route -> new_server
9. old_server 将 room 标记为 RetiredOnOld 并删除本地实例
```

### 10.2 冻结必须做到的事

冻结 room 时，`old_server` 必须同时做到:

- 拒绝该 room 的新加入
- 拒绝该 room 的新输入
- 停止该 room 的 tick 推进
- 停止新的 NPC/怪物行为推进
- 停止新的随机事件、定时器、脚本调度推进

只有完成冻结后，导出的状态才是可迁移的权威状态。

## 11. RoomTransferPayload 规范

### 11.1 设计原则

`RoomTransferPayload` 必须是“可恢复出同一 room 运行态”的完整数据，而不是展示用快照。

### 11.2 最小字段

建议最小字段如下:

```text
RoomTransferPayload {
  rollout_epoch,
  room_id,
  room_version,
  policy_id,
  owner_player_id,
  room_phase,
  current_frame_id,
  last_applied_frame_id,
  snapshot,
  recent_inputs,
  waiting_frame_id,
  waiting_inputs,
  movement_state,
  logic_state,
  runtime_timers,
  match_id,
  checksum,
}
```

### 11.3 必须包含的运行态

如果 room 中存在 NPC、怪物、召唤物、行为树、场景对象，则 payload 必须包含:

- 实体列表
- 实体位置、朝向、血量、阵营
- 技能冷却
- buff 剩余时间
- 仇恨与目标
- AI/行为树当前节点
- AI 黑板或上下文变量
- 定时器和调度器剩余时间
- 随机数种子或 RNG 状态

如果缺失上述任一项，就不能宣称“旧 room 行为已被完整接管”。

## 12. NPC / 怪物 / 行为树的一致性要求

### 12.1 必须满足的保证

当旧 room 已经没有玩家时，旧 room 下的 NPC、怪物、行为树等状态同步到新服，必须满足:

1. 导出时旧 room 已冻结
2. 导入时新 room 使用同一个 `room_id`
3. 导入后实体数量和关键状态与导出时一致
4. 导入后行为树从导出点继续运行，而不是重新从根节点冷启动

### 12.2 为什么普通 snapshot 不够

普通 `RoomSnapshot` 只适合:

- 客户端显示
- 断线恢复辅助
- 轻量调试

普通 snapshot 不足以恢复:

- AI 运行点
- 行为树黑板
- 冷却计时
- 定时器
- 内部 ECS 稠密存储状态

因此 NPC/怪物接管必须走专门的 transfer payload，而不是直接复用客户端同步快照。

### 12.3 行为树恢复要求

若项目后续引入行为树系统，每个可迁移 room logic 必须实现:

- 导出当前激活节点
- 导出黑板数据
- 导出正在等待的条件、计时器、目标引用
- 导入后恢复到同一执行点

否则该玩法不允许纳入“空房接管式灰度”的支持范围。

## 13. game-server 需要新增的接口

### 13.1 对 proxy / 控制面暴露的 room 路由信息

建议新增只读接口:

- `ListRoomRoutes`
- `GetRoomRoute(room_id)`
- `GetDrainStatus`

至少返回:

- `room_id`
- `owner_server_id`
- `member_count`
- `online_member_count`
- `migration_state`
- `empty_since`

### 13.2 old_server 需要新增的内部接口

建议新增内部协议:

- `FreezeRoomForTransferReq`
- `FreezeRoomForTransferRes`
- `ExportRoomTransferReq`
- `ExportRoomTransferRes`
- `RetireTransferredRoomReq`
- `RetireTransferredRoomRes`

### 13.3 new_server 需要新增的内部接口

建议新增内部协议:

- `ImportRoomTransferReq`
- `ImportRoomTransferRes`
- `ConfirmRoomOwnershipReq`
- `ConfirmRoomOwnershipRes`

## 14. RoomLogic 能力约束

所有希望支持空房接管式灰度的 `RoomLogic` 必须实现:

- `export_transfer_state()`
- `import_transfer_state(payload)`
- `checksum_transfer_state()`

建议不要只使用当前的:

- `get_serialized_state()`
- `restore_from_serialized_state()`

因为前者更像轻量快照钩子，不足以承载完整 room 迁移语义。

## 15. proxy 的路由规则

### 15.1 新 room

如果某 `room_id` 尚未存在 route record，则:

- 默认分配给 `new_server`

### 15.2 旧 room

如果某 `room_id` 的 route record 仍属于 `old_server`，则:

- 进入该 room 的请求必须继续进入 `old_server`

### 15.3 已接管 room

如果某 `room_id` 的 route record 已切到 `new_server`，则:

- 新 join 必须进入 `new_server`
- 断线重连也必须进入 `new_server`

### 15.4 灰度结束

当 `old_server` 满足以下条件时，`proxy` 可以结束灰度模式:

1. `owned_room_count == 0`
2. `migrating_room_count == 0`
3. `connection_count == 0`

结束后:

- 清理 `rollout_epoch`
- 清理 `old_server` 相关 route metadata
- 后续流量全部按普通新服模式处理

## 16. 失败与回滚

### 16.1 导入失败

如果 `new_server` 导入 payload 失败:

- `proxy` 不得切换 room route
- `old_server` 继续保留 room owner
- room 状态进入 `TransferFailed`
- 记录失败原因与 checksum

### 16.2 导入成功但确认失败

如果新服导入成功，但 route 切换失败:

- 不得同时让新旧服都对外宣称自己是 owner
- 必须有唯一 owner
- 默认回滚到 `old_server` owner

### 16.3 客户端重连到错误 server

如果玩家在灰度期间重连到了错误 server:

- server 必须返回明确错误码
- `proxy` 或客户端必须根据 room route 重新发起正确连接

## 17. 日志与审计

以下事件必须落日志并建议落审计:

- 灰度会话开始
- room 进入 `DrainingOnOld`
- room 冻结
- payload 导出成功或失败
- payload 导入成功或失败
- room route 切换
- 客户端收到 `ServerRedirectPush`
- 旧服排空完成
- 灰度会话结束

## 18. 验收标准

### 18.1 简化版切服

必须验证:

1. 旧服通知客户端重连后，客户端会重新连接 `proxy`
2. 重连后 `proxy` 可将玩家送入新服
3. 旧连接不会被继续保留在错误 room owner 上

### 18.2 空房接管

必须验证:

1. 旧 room 只有在成员为空后才触发接管
2. 接管时旧 room 已冻结，不再继续推进
3. 新 room 使用相同 `room_id`
4. route 切换后新请求全部进新服

### 18.3 NPC / 怪物 / 行为树

必须验证:

1. 导出前后实体数量一致
2. 关键属性如坐标、血量、朝向一致
3. 冷却、buff、定时器保持连续
4. AI/行为树从导出点继续，而不是重置

### 18.4 旧服停止条件

必须验证:

1. 旧服 `owned_room_count == 0`
2. 旧服 `migrating_room_count == 0`
3. 旧服 `connection_count == 0`
4. `proxy` 已退出灰度模式

## 19. 实施建议

建议按三个阶段落地:

### 阶段一

- `proxy` 增加 room route metadata
- `old_server` 增加显式重连通知
- `proxy` 支持灰度期间按 room 路由

### 阶段二

- `old_server/new_server` 增加 freeze/export/import/retire 内部协议
- 先支持最简单玩法 room 的空房接管

### 阶段三

- 为 combat、movement、NPC、怪物、行为树补齐完整 transfer payload
- 补齐 checksum、失败回滚、自动化验收

## 20. 当前项目里的推荐结论

后续开发中，统一采用下面的结论:

- 第一阶段切服依赖客户端重连
- `proxy` 只保存 room 路由元数据，不理解玩法
- 旧 room 下 NPC、怪物、行为树的一致性，只能通过“冻结导出-导入接管”保证
- 未实现完整 transfer payload 的玩法，不允许宣称支持空房接管式灰度
