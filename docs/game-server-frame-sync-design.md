# game-server 帧同步与房间生命周期框架设计（草案）

这份文档面向 `apps/game-server` 的下一阶段重构设计，目标不是立即把所有代码写完，而是先把“框架应该长什么样”讲清楚，方便在开发前统一方案。

本文重点解决两个问题：

- 如何把当前“输入即时广播”的房间服演进为“按帧推进”的通用框架
- 如何同时支持不同生命周期的房间：
  - 长期存在的大世界房间
  - 可按配置销毁的对局 / 临时房间

## 1. 设计目标

新框架需要满足以下要求：

- 支持房间级帧推进，而不是全局一个固定 tick
- 支持按房间状态动态分配帧率，以节省 CPU
- 无人房间不能直接停掉，必须支持可配置的 `silent_room_fps`
- 房间生命周期不能写死，必须通过配置支持常驻房间和可销毁房间
- 框架层负责调度、生命周期、输入聚合
- 业务层负责具体游戏逻辑，不把某一种玩法写死在框架里

## 2. 当前项目现状

当前 `game-server` 已实现以下能力：

- TCP 长连接接入
- Protobuf 消息编解码
- 基础房间管理
- `InGame` 状态下允许玩家发送输入
- 房间级 `current_frame` 和固定帧循环
- 输入按帧聚合 (`pending_inputs`)
- 房间级动态帧率 (`silent_room_fps`, `idle_room_fps`, `active_room_fps`, `busy_room_fps`)
- 房间生命周期策略 (`RoomRuntimePolicy`)
- 房间运行时调度器 (`RoomManager`)
- `RoomLogic` trait 和 `TestRoomLogic` 实现
- `FrameBundlePush` 按帧广播
- 空房 TTL 销毁机制

当前实现已具备：

- 房间服
- 输入广播服
- 基础帧同步服

## 2.1 仍需完善的能力

以下能力尚未完全实现或需要优化：

- 多种 `RoomLogic` 实现（当前只有 `TestRoomLogic`）
- 完整增量状态广播
- 客户端帧率变化通知 (`RoomFrameRatePush`)
- 断线重连后的房间状态恢复
- 更细粒度的活跃度判定

## 3. 总体架构

建议把 `game-server` 拆成三层：

1. `Room`
2. `RoomManager`
3. `RoomLogic`

### 3.1 `Room`

`Room` 是房间运行时数据本体，负责保存：

- 房间成员
- 房间阶段
- 当前帧号
- 输入缓存
- 房间策略引用
- 业务状态容器

`Room` 不负责决定自己何时销毁，也不负责决定自己该跑多少 fps。

### 3.2 `RoomManager`

`RoomManager` 是框架层调度器，负责：

- 创建房间
- 删除房间
- 根据房间状态计算目标 fps
- 启停每个房间的 tick task
- 处理空房保活、延迟销毁、对局结束销毁等策略

### 3.3 `RoomLogic`

`RoomLogic` 是业务逻辑扩展点，负责：

- 每帧游戏规则更新
- 玩家加入离开后的业务处理
- 对局结束判定
- 房间是否允许销毁

这样框架层不会绑定某个具体玩法。

## 4. 房间生命周期模型

为了支持长期存在的房间和可销毁的临时房间，不能把“空房即删”写死在框架里，而要引入房间运行策略。

建议定义：

```rust
pub struct RoomRuntimePolicy {
    pub policy_id: String,

    pub silent_room_fps: u16,
    pub idle_room_fps: u16,
    pub active_room_fps: u16,
    pub busy_room_fps: u16,
    pub busy_room_player_threshold: usize,

    pub destroy_enabled: bool,
    pub destroy_when_empty: bool,
    pub empty_ttl_secs: u64,
    pub retain_state_when_empty: bool,
}
```

这组配置的含义：

- `silent_room_fps`：无人房间使用的低帧率
- `idle_room_fps`：有人但未开局时的帧率
- `active_room_fps`：运行中普通活跃房间的帧率
- `busy_room_fps`：运行中高负载房间的帧率
- `busy_room_player_threshold`：达到该人数后切到高档 fps
- `destroy_enabled`：该房间是否允许销毁
- `destroy_when_empty`：房间为空后是否触发销毁计时
- `empty_ttl_secs`：空房后保留多久再销毁
- `retain_state_when_empty`：空房时是否保留房间业务状态

这里不再把房间硬编码成“world”或“moba”类型，而是统一用策略模板表达。

### 4.1 常驻房间策略示例

```text
policy_id = persistent_world
silent_room_fps = 1
idle_room_fps = 2
active_room_fps = 10
busy_room_fps = 20
busy_room_player_threshold = 20
destroy_enabled = false
destroy_when_empty = false
empty_ttl_secs = 0
retain_state_when_empty = true
```

特点：

- 房间长期存在
- 无人也继续低频运行
- 房间状态不会因为空房而丢失

### 4.2 临时房间策略示例

```text
policy_id = disposable_match
silent_room_fps = 1
idle_room_fps = 5
active_room_fps = 15
busy_room_fps = 30
busy_room_player_threshold = 2
destroy_enabled = true
destroy_when_empty = true
empty_ttl_secs = 60
retain_state_when_empty = false
```

特点：

- 房间允许被销毁
- 空房后进入保留计时窗口
- 超过 TTL 后删除

## 5. 帧率调度模型

### 5.1 设计原则

帧率调度由 `RoomManager` 统一负责，不由 `Room` 自己决策。

因为“房间应该跑多少 fps”本质上是运行时资源调度问题，不是房间业务规则问题。

### 5.2 帧率计算函数

建议基础版本按房间成员数量和房间阶段计算：

```rust
fn compute_room_fps(room: &Room, policy: &RoomRuntimePolicy) -> u16 {
    let member_count = room.members.len();

    if member_count == 0 {
        return policy.silent_room_fps;
    }

    match room.phase {
        RoomPhase::Waiting => policy.idle_room_fps,
        RoomPhase::InGame => {
            if member_count >= policy.busy_room_player_threshold {
                policy.busy_room_fps
            } else {
                policy.active_room_fps
            }
        }
    }
}
```

后续如果要更细化，可以再引入：

- 最近输入频率
- 最近广播频率
- 房间内 AI/NPC 数量
- 房间业务逻辑负载

但第一版先不要做复杂化。

## 6. 无人房间机制

这是本次设计里的一个关键点。

无人房间不能直接设为 `0 fps`，因为某些房间即使没有玩家，也仍然需要继续推进：

- 大世界环境逻辑
- AI / NPC 巡逻
- 资源刷新
- 定时器
- 状态清理
- 战斗结算尾处理

因此需要引入统一的 `silent_room_fps`。

### 6.1 设计要求

- `silent_room_fps` 必须可配置
- `silent_room_fps` 必须大于 0
- 无人房间可以继续 tick，但不应做无意义广播

### 6.2 建议约束

建议配置校验时保证：

- `silent_room_fps >= 1`
- `idle_room_fps >= silent_room_fps`
- `active_room_fps >= idle_room_fps`
- `busy_room_fps >= active_room_fps`

## 7. Room 运行时数据结构设计

当前 `Room` 结构已实现为”统一运行时容器”。

```rust
pub struct Room {
    pub room_id: String,
    pub owner_player_id: String,
    pub phase: RoomPhase,
    pub members: HashMap<String, RoomMemberState>,

    pub current_frame: u32,
    pub created_at: Instant,
    pub last_active_at: Instant,
    pub empty_since: Option<Instant>,

    pub policy_id: String,
    pub pending_inputs: Vec<PlayerInputRecord>,

    pub logic: Box<dyn RoomLogic>,
}
```

其中：

- `current_frame`：当前房间帧号
- `created_at`：房间创建时间（用于活跃度判定）
- `last_active_at`：最后活跃时间（每次玩家操作时更新）
- `empty_since`：房间变空的时间点（用于 TTL 销毁计时）
- `policy_id`：指向房间策略模板
- `pending_inputs`：输入缓冲区，按接收顺序存储
- `logic`：业务逻辑实例

### 7.1 输入结构

```rust
pub struct PlayerInputRecord {
    pub frame_id: u32,
    pub player_id: String,
    pub action: String,
    pub payload_json: String,
    pub received_at: Instant,
}
```

### 7.2 Room 辅助方法

```rust
impl Room {
    pub fn update_activity(&mut self);        // 更新最后活跃时间
    pub fn mark_empty(&mut self);           // 标记房间为空
    pub fn clear_empty(&mut self);          // 清除空房标记
    pub fn is_empty(&self) -> bool;         // 检查房间是否为空
    pub fn should_destroy(&self, policy: &RoomRuntimePolicy) -> bool;  // 根据策略判断是否销毁
}
```

## 8. RoomManager 运行时结构

建议新增房间运行时表，而不是只保留 `SharedRooms`。

```rust
pub struct RoomRuntime {
    pub current_fps: u16,
    pub target_fps: u16,
    pub tick_running: bool,
}

type SharedRooms = Arc<Mutex<HashMap<String, Room>>>;
type SharedRoomRuntimes = Arc<Mutex<HashMap<String, RoomRuntime>>>;
```

说明：

- `Room` 保存业务和状态
- `RoomRuntime` 保存调度信息

两者职责分离，避免把运行时调度字段塞得过满。

## 9. 帧推进模型

### 9.1 基本流程

每个房间在 `InGame` 或需要后台运行时，由 `RoomManager` 确保有一个 tick task 存在。

每次 tick：

1. 根据房间当前 `target_fps` 计算下一帧间隔
2. 推进 `current_frame`
3. 取出该帧输入集合
4. 调用业务逻辑 `on_tick(frame_id, inputs)`
5. 如有在线成员，则广播该帧数据或状态变更
6. 检查房间生命周期状态，决定是否继续运行

### 9.2 注意事项

- 不要直接 `sleep(fixed_duration)` 无限循环，应使用 deadline 方式减少漂移
- 帧率变化时，下个周期自动使用新的 interval
- 无人房间仍会 tick，但默认不广播网络消息

## 10. 协议演进建议

当前 `PlayerInputReq` 已实现 `frame_id` 字段：

```proto
message PlayerInputReq {
  uint32 frame_id = 1;
  string action = 2;
  string payload_json = 3;
}
```

已实现的按帧广播消息：

```proto
message FrameInput {
  string player_id = 1;
  string action = 2;
  string payload_json = 3;
}

message FrameBundlePush {
  string room_id = 1;
  uint32 frame_id = 2;
  uint32 fps = 3;
  repeated FrameInput inputs = 4;
  bool is_silent_frame = 5;
}

message RoomFrameRatePush {
  string room_id = 1;
  uint32 fps = 2;
  string reason = 3;
}
```

说明：

- `FrameBundlePush` 是第一版真正的按帧广播载体
- 第一版只广播输入集合，不广播完整增量状态
- `RoomFrameRatePush` 用于通知客户端当前房间帧率变化（待客户端对接）
- `is_silent_frame` 表示这是无人房间或无输入帧，便于客户端做差异化处理

## 11. RoomLogic 抽象建议

当前已实现房间业务逻辑接口 `RoomLogic trait`。

```rust
pub trait RoomLogic: Send {
    fn on_room_created(&mut self, room_id: &str) {}
    fn on_player_join(&mut self, player_id: &str) {}
    fn on_player_leave(&mut self, player_id: &str) {}
    fn on_game_started(&mut self) {}
    fn on_game_ended(&mut self) {}
    fn on_player_input(&mut self, player_id: &str, action: &str, payload_json: &str) {}
    fn on_tick(&mut self, frame_id: u32, inputs: &[PlayerInputRecord]) {}
    fn should_destroy(&self) -> bool { false }
}
```

当前提供最简样例实现 `TestRoomLogic`，用于验证：

- 房间 tick 流程可正常运行
- 输入聚合可正常进入逻辑层
- 无人房间在 `silent_room_fps` 下仍会继续推进

示例：

```rust
pub struct TestRoomLogic {
    pub tick_count: u64,
}

impl RoomLogic for TestRoomLogic {
    fn on_room_created(&mut self, _room_id: &str) {}
    fn on_player_join(&mut self, _player_id: &str) {}
    fn on_player_leave(&mut self, _player_id: &str) {}
    fn on_game_started(&mut self) {}
    fn on_game_ended(&mut self) {}
    fn on_player_input(&mut self, _player_id: &str, _action: &str, _payload_json: &str) {}

    fn on_tick(&mut self, _frame_id: u32, _inputs: &[PlayerInputRecord]) {
        self.tick_count += 1;
    }

    fn should_destroy(&self) -> bool {
        false
    }
}
```

最终销毁决策应为：

```text
destroy_decision = runtime_policy + room_state + room_logic
```

而不是简单地：

```text
member_count == 0 => destroy
```

## 12. 模块划分建议

建议新增或调整这些模块：

- `room.rs`
  - 保留房间结构和基础状态函数
- `room_manager.rs`
  - 负责房间创建、销毁、调度、fps 计算、tick task 管理
- `room_policy.rs`
  - 定义房间运行策略和内置策略模板
- `room_logic.rs`
  - 定义 `RoomLogic` trait、逻辑工厂和 `TestRoomLogic`
- `protocol.rs`
  - 扩展新的帧同步消息号和 packet 编解码支持
- `server.rs`
  - 继续承担连接接入和消息分发，但不再直接承载房间调度主逻辑

为了后续框架化，允许在 `apps/game-server/src` 下新增目录和模块，不要求继续把所有逻辑堆在单层文件中。

## 13. 配置设计建议

第一版直接采用内置策略模板，不引入外部策略配置文件。

全局环境变量仍可保留为默认值来源，但房间实际创建时以代码内置模板为准。

### 13.1 全局默认配置

在 `Config` 中新增：

```rust
pub default_silent_room_fps: u16,
pub default_idle_room_fps: u16,
pub default_active_room_fps: u16,
pub default_busy_room_fps: u16,
pub default_busy_room_player_threshold: usize,
pub default_empty_ttl_secs: u64,
```

### 13.2 房间策略模板

实际创建房间时优先从策略模板取值，而不是一律读全局默认值。

例如：

- `persistent_world`
- `disposable_match`
- `sandbox`

第一版直接在代码里内置模板构造函数。

## 14. 第一阶段开发范围建议

建议不要一次把所有高级能力都做进去，第一阶段只做最小可运行版本。

### Phase 1

- 为 `Room` 增加 `current_frame`、`policy_id`、`pending_inputs`
- 引入 `RoomRuntimePolicy`
- 引入 `RoomManager`
- 直接引入 `RoomLogic trait`
- 提供 `TestRoomLogic` 最简运行样例
- 支持 `silent_room_fps`
- 支持 `destroy_enabled`、`destroy_when_empty` 和 `empty_ttl_secs`
- `PlayerInputReq` 增加 `frame_id`
- 服务端按帧聚合输入并广播 `FrameBundlePush`
- 无人房间与有人房间走同一套逻辑更新链路，只是每秒 tick 次数不同

### Phase 2

- 客户端帧率变化通知
- 房间 tick task 平滑调速
- 更细的活跃度判定
- 空帧压缩 / 合并

### Phase 3

- 多种 `RoomLogic` 实现
- 常驻房间预创建
- 临时房间自动销毁策略完善
- 更完整的监控与审计日志

## 15. 当前代码需要特别修改的点

### 15.1 `leave_room()` 不能再直接删除空房

当前实现里，空房会立刻从 `HashMap` 移除。这不适合常驻房间。

后续应改成：

- 标记 `empty_since_ms`
- 由 `RoomManager` 决定是否销毁

### 15.2 `PlayerInputReq` 不能再收到即广播

当前实现是收到输入后直接 `broadcast_game_message(...)`。

后续应改成：

- 校验输入合法性
- 放入 `pending_inputs`
- 等房间 tick 时统一广播本帧输入集合

### 15.3 `server.rs` 不应该继续承载全部房间控制逻辑

随着帧同步和生命周期复杂度上升，`server.rs` 只应负责：

- 连接读写
- 鉴权
- 消息分发
- 调用 `RoomManager`

房间运行时调度要迁走。

## 16. 关键设计结论

这套框架的核心结论有三条：

1. 房间生命周期必须策略化，而不是写死
2. 无人房间仍然需要支持低频运行，因此必须引入 `silent_room_fps`
3. 帧率决策应该由 `RoomManager` 统一调度，不应该分散在 `Room` 或连接逻辑中

如果这三点定下来，后面的代码演进路径会比较清晰。

## 17. 本轮确认结果

本轮设计确认如下：

1. 第一版直接引入 `RoomLogic trait`，并提供 `TestRoomLogic` 最简样例
2. 房间策略模板第一版内置在代码中
3. 第一版 `FrameBundlePush` 只广播输入集合，不广播完整增量状态
4. 无人房间和有人房间走相同逻辑链路，只是 tick 次数不同
5. 不按“是否 MOBA”区分房间，而是统一通过房间销毁配置控制：
   - `destroy_enabled`
   - `destroy_when_empty`
   - `empty_ttl_secs`

完整增量状态广播不进入第一版范围，已单独记录到待办清单。
