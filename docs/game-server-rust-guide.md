# game-server 项目说明（面向熟悉 C++ 的 Rust 初学者）

这份文档只讲 `apps/game-server` 这个 Rust TCP 游戏服，并且假设你：

- 对服务端、网络、状态机、C++ 比较熟
- 对 Rust 语法和生态还不熟

目标不是把 Rust 教科书重讲一遍，而是让你能顺着这个项目，把“代码怎么跑起来”和“Rust 为什么这么写”对应起来。

## 1. 先看整体职责

`game-server` 是一个基于 `Tokio` 的 TCP 长连接服务，负责：

- 接受客户端 TCP 连接
- 解析自定义包头
- 用 Protobuf 解析消息体
- 校验 `auth-http` 签发的 ticket
- 维护房间状态
- 向房间内玩家广播状态和游戏消息
- 把连接事件、房间事件写入 MySQL 审计表

它不负责账号登录本身。登录和 ticket 签发在 `apps/auth-http`。

你可以把当前系统理解成：

1. `auth-http` 像一个登录网关
2. `game-server` 像一个轻量房间服 / 对局接入服
3. Redis 用来放短生命周期状态
4. MySQL 用来做审计落库

## 2. 代码入口是怎么跑起来的

入口在 [main.rs](/c:/project/MyServer/apps/game-server/src/main.rs)。

启动流程非常直接：

1. `dotenvy::dotenv()` 读取环境变量
2. `Config::from_env()` 组装配置
3. `init_logging(&config)` 初始化 `tracing`
4. `MySqlAuditStore::new(&config).await?` 初始化可选 MySQL 连接池
5. `server::run(&config, mysql_store.clone()).await` 启动 TCP 监听
6. 退出前 `mysql_store.close().await`

如果你用 C++ 来类比，`main.rs` 就像：

- 先加载配置
- 初始化日志系统
- 初始化数据库连接池
- 启动一个 accept loop
- 每个连接交给一个异步任务处理

这里最重要的 Rust 语法点是：

- `#[tokio::main]`：相当于“帮你生成异步运行时 main”
- `async fn main() -> Result<...>`：主函数本身可以 `await`
- `?`：如果出错就立刻返回，类似你在 C++ 里一路向上传播错误，只是写法更短

## 3. 模块分工

### 3.1 `config.rs`

文件：[config.rs](/c:/project/MyServer/apps/game-server/src/config.rs)

职责：

- 从环境变量读取配置
- 提供默认值
- 提供 `bind_addr()`

这个 `Config` 是普通结构体：

```rust
#[derive(Clone)]
pub struct Config { ... }
```

这里的 `Clone` 很重要。它表示这个配置对象可以显式复制。在本项目里，配置会被传给多个连接任务，所以需要可复制。

对 C++ 的映射可以理解为：

- `struct Config`
- 手动实现一个“值语义拷贝能力”
- 每个连接任务拿到一份独立配置副本

### 3.2 `protocol.rs`

文件：[protocol.rs](/c:/project/MyServer/apps/game-server/src/protocol.rs)

职责：

- 定义 TCP 固定包头格式
- 定义消息号 `MessageType`
- 实现包头解析和封包

当前包头格式：

```text
| magic(2) | version(1) | flags(1) | msgType(2) | seq(4) | bodyLen(4) | body(N) |
```

这和很多 C++ 游戏服常见的二进制协议头完全同类，只是这里用 Rust 标准库的大小端转换：

- `u16::from_be_bytes(...)`
- `u32::from_be_bytes(...)`
- `to_be_bytes()`

`MessageType::from_u16` 相当于手写一个安全版的消息号反序列化。不是所有整数都能转成枚举，非法值返回 `None`。

这就是 Rust 常见风格：

- 不相信外部输入
- 不做隐式未定义行为
- 用 `Option` 表达“有/没有这个值”

### 3.3 `build.rs` + `pb`

文件：[build.rs](/c:/project/MyServer/apps/game-server/build.rs)

`build.rs` 会在编译时运行，用 `prost-build` 把：

- `packages/proto/game.proto`
- `packages/proto/admin.proto`

生成 Rust 代码。

在 [main.rs](/c:/project/MyServer/apps/game-server/src/main.rs) 里：

```rust
mod pb {
    include!(concat!(env!("OUT_DIR"), "/myserver.game.rs"));
}
```

这段可以类比成：

- CMake / codegen 先生成一份 `.pb.cc/.pb.h`
- 再把生成结果编进当前工程

区别是 Rust 常把生成代码放在编译输出目录，再通过 `include!` 引入。

### 3.4 `session.rs`

文件：[session.rs](/c:/project/MyServer/apps/game-server/src/session.rs)

它定义的是“单个 TCP 连接的会话状态”，不是账号系统意义上的 session。

包含：

- `id`
- `state`
- `player_id`
- `room_id`

这里要注意：

- `player_id: Option<String>`
- `room_id: Option<String>`

意思是这两个字段在某些阶段可能不存在。

对 C++ 的直觉映射：

- `Option<String>` 类似 `std::optional<std::string>`
- `SessionState` 类似简单枚举状态机

### 3.5 `room.rs`

文件：[room.rs](/c:/project/MyServer/apps/game-server/src/room.rs)

这个模块定义房间内存模型：

- `Room`
- `RoomMemberState`
- `RoomPhase`
- `OutboundMessage`

核心点：

- 房间里保存成员列表 `HashMap<String, RoomMemberState>`
- 每个成员带一个 `sender`
- `sender` 是给该连接写回消息的通道句柄

这意味着：房间广播时并不直接操作 socket，而是把消息推给每个连接自己的写队列。

这是一个很典型的解耦方式：

- 连接读取逻辑在连接任务里
- 房间逻辑不碰底层 socket
- 广播只管投递消息

如果用 C++ 类比，可以把它想成：

- 每个连接有一个线程安全发送队列
- 房间对象里只保存这些发送队列的句柄

### 3.6 `ticket.rs`

文件：[ticket.rs](/c:/project/MyServer/apps/game-server/src/ticket.rs)

职责：

- 校验 ticket 格式
- 用 HMAC-SHA256 校验签名
- 解析 payload
- 检查过期时间
- 返回 `player_id`

ticket 格式是：

```text
base64url(payload_json).base64url(signature)
```

但这里有个关键点：服务端不仅校验签名，还会去 Redis 检查这个 ticket 是否存在。

所以鉴权是两层：

1. ticket 本身合法且未过期
2. Redis 里存在对应 ticket 的记录

这能防止“拿到旧 ticket 无限重放”的一部分问题，至少要求这个 ticket 必须是登录服实际签发过、且尚未过期的。

### 3.7 `mysql_store.rs`

文件：[mysql_store.rs](/c:/project/MyServer/apps/game-server/src/mysql_store.rs)

这是一个“可选启用”的审计存储层。

如果 `MYSQL_ENABLED=false`，内部 `pool` 就是 `None`，后续所有写库调用直接返回，不影响主链路。

这就是 Rust 里很常见的“显式可选资源”写法：

- `Option<Pool>`
- `Some(pool)` 表示启用
- `None` 表示关闭

比起 C++ 里用空指针或者特殊状态，这种表达更直接，而且编译器会强迫你处理“可能不存在”的分支。

## 4. 整个连接生命周期

真正的主逻辑在 [server.rs](/c:/project/MyServer/apps/game-server/src/server.rs)。

建议你把它当成全项目第一阅读重点。

### 4.1 监听与 accept loop

`run()` 里先做三件事：

- `TcpListener::bind(...)`
- 创建 Redis client
- 创建共享房间表 `Arc<Mutex<HashMap<String, Room>>>`

这里最关键的一行是：

```rust
type SharedRooms = Arc<Mutex<HashMap<String, Room>>>;
```

如果翻译成 C++ 心智模型，大致就是：

```cpp
using SharedRooms =
    std::shared_ptr<std::mutex + std::unordered_map<std::string, Room>>;
```

当然 C++ 没这种直接语法，但意思差不多：

- `Arc` 类似线程安全引用计数智能指针
- `Mutex` 类似互斥锁
- `HashMap` 类似 `std::unordered_map`

为什么要这样包？

- 多个连接任务会并发访问房间表
- 房间表需要共享所有权
- 访问时要串行保护

### 4.2 每个连接一个 Tokio 任务

accept 到连接之后：

- 分配 `session_id`
- 记录连接审计日志
- `tokio::spawn(async move { ... })`

这等价于“把这个连接交给一个并发执行单元处理”，但不是传统 OS 线程一连接一线程，而是 Tokio 的轻量任务。

对 C++ 的直觉类比：

- 不像 `std::thread` 那么重
- 更像协程调度器里的 task
- 遇到 `await` 时让出执行权

### 4.3 读写拆分

在 `handle_connection()` 里：

```rust
let (mut reader, mut writer) = socket.into_split();
let (tx, mut rx) = mpsc::unbounded_channel::<OutboundMessage>();
```

这里做了两件关键事：

1. 把一个 TCP socket 拆成读半边和写半边
2. 建一个无界 channel，当作这个连接的发送队列

随后又启动一个 `writer_task`：

- 它只负责从 `rx.recv().await` 取消息
- 编码成 packet
- `writer.write_all(&packet).await`

这是一种很常见的网络层设计：

- 读循环只管读和处理
- 写循环只管串行写 socket
- 避免多个地方同时写一个连接

如果你来自 C++，可以把它理解成“单写线程模型”的异步版。

### 4.4 心跳和读包

主读循环每次先读固定 14 字节包头：

```rust
timeout(Duration::from_secs(config.heartbeat_timeout_secs), reader.read_exact(&mut header_buf)).await
```

这个写法很关键。

它不是“专门处理 ping 包的超时线程”，而是：

- 对下一次读包头设置超时
- 超时就断连

只要客户端持续发消息，不管是 `PING_REQ` 还是别的请求，都能刷新这个超时窗口。

也就是说当前项目的“心跳超时”本质上是：

- 连接在一段时间内没有任何入站流量，就断开

### 4.5 包头校验与 body 限长

读完 header 后会依次检查：

- `magic`
- `version`
- `flags`
- `body_len <= max_body_len`
- `msg_type` 是否已定义

这部分是典型的协议防御式编程，和 C++ 服务端思路一致，只是 Rust 用 `Result`/`Option` 把错误分支写得更显式。

### 4.6 消息分发

拿到 `MessageType` 之后，进入一个大 `match`：

- `AuthReq`
- `PingReq`
- `RoomJoinReq`
- `RoomLeaveReq`
- `RoomReadyReq`
- `RoomStartReq`
- `PlayerInputReq`
- `RoomEndReq`

如果你习惯 C++，这可以理解成：

- 一个大号消息分发表
- 类似 `switch(msg_type)`
- 每个分支里做 decode、状态校验、业务处理、响应

Rust 的 `match` 比 `switch` 更强，因为它要求分支更完整、类型更一致。

## 5. 鉴权链路是怎么闭环的

这一段很重要，因为它跨了 Node 和 Rust 两个服务。

### 5.1 登录服发 ticket

在 [auth-store.js](/c:/project/MyServer/apps/auth-http/src/auth-store.js) 中：

- `issueGameTicket(playerId, ...)`
- 构造 payload：`playerId + nonce + exp`
- `HMAC-SHA256` 签名
- 写 Redis：`ticket:<sha256(ticket)> -> playerId`

### 5.2 游戏服验 ticket

在 [server.rs](/c:/project/MyServer/apps/game-server/src/server.rs) 的 `AuthReq` 分支里：

1. Protobuf decode `AuthReq`
2. `verify_ticket(...)` 校验签名和过期时间
3. 计算 `hash_ticket(ticket)`
4. 去 Redis 查 `ticket:<sha256(ticket)>`
5. 校验 Redis 返回的 `playerId` 是否和 ticket payload 中一致
6. 成功后把 `session.state` 置为 `Authenticated`

因此当前鉴权不是单纯 JWT 那种“自包含即可”，而是：

- 自包含签名校验
- 再加 Redis 存在性校验

这对初期项目是合理的，因为服务端可以保留吊销能力和更强的控制力。

## 6. 房间状态机怎么工作

### 6.1 房间表

全局房间表是：

```rust
HashMap<String, Room>
```

key 是 `room_id`，value 是房间对象。

### 6.2 加入房间

`join_room(...)` 的规则：

- 如果房间不存在，就创建
- 第一个进入的玩家自动成为 `owner`
- 如果房间已在游戏中，且该玩家不在房间里，拒绝加入
- 房间最多 10 人
- 成员加入后默认 `ready=false`

注意这里的行为很“游戏房间服”：

- 同一个连接只能在一个房间里
- 如果已经在当前房间，再发 join，不会重复加入，而是直接返回成功
- 如果在别的房间，返回 `ALREADY_IN_OTHER_ROOM`

### 6.3 离开房间

`leave_room(...)` 的规则：

- 删除当前成员
- 如果房间空了，直接删除整个房间
- 如果离开的是房主，转移 `owner` 给下一个成员
- 无论是否在游戏中，只要有人离开，房间都会 `reset_to_waiting()`

这个 reset 非常关键。它说明当前设计是：

- 对局过程中掉人，不继续维持对局态
- 直接回退到等待态，要求剩余玩家重新准备

### 6.4 准备 / 开始 / 输入 / 结束

`Room` 上有几个状态检查函数：

- `can_start_game`
- `can_send_input`
- `can_end_game`

这些函数相当于把状态机约束集中放在房间对象内部。

这点是好设计，因为你不希望业务规则散落在各个消息分支里。

开始游戏的条件：

- 当前不在 `InGame`
- 发送者必须是房主
- 至少 2 人
- 全员 `ready=true`

发送玩家输入的条件：

- 房间必须在 `InGame`
- 该玩家必须是房间成员

结束游戏的条件：

- 房间必须在 `InGame`
- 只能房主结束

结束后会：

- `phase = Waiting`
- 所有人 `ready = false`

## 7. 广播机制怎么做的

广播不是直接遍历 socket 写数据，而是：

1. 先从房间里收集所有成员的 `sender`
2. 组装好 protobuf body
3. 对每个 sender 执行 `send(...)`

对应两个广播函数：

- `broadcast_snapshot(...)`
- `broadcast_game_message(...)`

这样设计的好处：

- 房间逻辑和网络写出解耦
- 一个连接的慢写不会直接把房间逻辑卡住
- socket 写操作始终集中在 `writer_task`

当然当前实现也有一个工程上要知道的取舍：

- `mpsc::unbounded_channel` 是无界队列
- 如果客户端极慢，而服务端持续广播，理论上可能积压内存

对最小版本项目这可以接受，但后面如果要上真实环境，通常会改成：

- 有界队列
- 背压
- 慢连接踢出策略

## 8. 为什么这份代码看起来不像传统 OOP

如果你来自 C++，会感觉这里“类很少，函数很多”。这是正常的。

Rust 项目常见风格是：

- 数据结构放在 `struct`
- 规则检查放在 `impl`
- 编排逻辑写在模块级函数

例如：

- `Room` 负责保存房间状态和核心规则函数
- `server.rs` 负责消息编排和连接处理
- `Session` 只是一个连接上下文结构体

这比“所有东西都塞进一个 Server 类”更扁平。

你可以把它理解成：

- Rust 并不鼓励把面向对象层次做得很深
- 它更强调“数据 + 明确所有权 + 模块边界”

## 9. 这个项目里最值得你掌握的 Rust 概念

下面这些不是抽象知识，而是你读这个项目时会反复遇到的。

### 9.1 `Option<T>`

表示“这个值可能不存在”。

例如：

```rust
pub player_id: Option<String>
```

比起 C++ 的空指针，这更安全；比起约定空字符串，这更明确。

常见用法：

- `let Some(x) = ... else { ... };`
- `as_deref()`
- `clone()`

例如：

```rust
let Some(player_id) = session.player_id.clone() else {
    ...
};
```

意思就是：

- 如果有值，取出来
- 如果没有，走 `else`

你可以把它看成模式匹配版 `std::optional`.

### 9.2 `Result<T, E>`

表示“成功得到 T，失败得到 E”。

例如：

```rust
fn parse_header(...) -> Result<PacketHeader, &'static str>
```

这比返回错误码再配合输出参数更直接。

而 `?` 的作用是：

- 如果是 `Ok(v)`，继续执行
- 如果是 `Err(e)`，立刻返回这个错误

### 9.3 所有权和借用

这是 Rust 最核心，但在这个项目里并没有难到不可读。

你只需要先抓住三条：

1. 默认情况下，一个值同一时刻只有一个所有者
2. 不想转移所有权时，就借用 `&T`
3. 多任务共享时，通常用 `Arc<T>`

比如：

```rust
pub async fn run(config: &Config, mysql_store: MySqlAuditStore) -> Result<...>
```

这里：

- `config: &Config` 是借用，不拿走配置所有权
- `mysql_store: MySqlAuditStore` 是拿到一个可克隆的拥有者

后面每个连接任务里又会：

- `let connection_config = config.clone();`
- `let mysql_store = mysql_store.clone();`

这代表任务要拿一份自己能长期持有的数据。

### 9.4 `Arc<Mutex<T>>`

这是异步 Rust 里最常见的共享可变状态容器之一。

当前项目里用它包房间表。

常见用法：

```rust
let mut rooms_guard = rooms.lock().await;
```

意思是：

- 异步地拿锁
- 得到一个 guard
- guard 活着期间可以修改里面的 `HashMap`

这和 C++ 的 `std::lock_guard<std::mutex>` 很像，只不过这里的锁是异步版本。

### 9.5 `tokio::spawn`

它会启动一个并发任务。

注意这里不是简单“开线程”，而是把 future 交给 Tokio 调度。

你可以先把它粗略理解成：

- 轻量协程任务
- 能并发处理很多连接
- 遇到 I/O `await` 时不会阻塞整个线程

### 9.6 `mpsc`

这里是多生产者、单消费者 channel。

本项目里：

- 房间逻辑、错误处理、消息处理都可以往 `tx` 发消息
- `writer_task` 是唯一消费者，从 `rx` 读并写 socket

这相当于把“连接写出”收束成单点。

## 10. 对 C++ 开发者最容易误解的几个点

### 10.1 `clone()` 不一定很贵

在 C++ 里你看到 copy 可能会紧张，但在 Rust 里要分情况。

当前项目里的 `clone()` 有几类：

- `String` clone：会复制内容，确实有成本
- `redis_client.clone()`：通常是轻量句柄复制
- `mysql_store.clone()`：复制的是内部 pool 句柄，不是复制数据库连接池全部资源
- `mpsc::UnboundedSender::clone()`：复制发送端句柄

所以不要看到 `clone()` 就直接等同于“深拷贝大对象”。

### 10.2 `match` 不只是 `switch`

Rust 的 `match` 经常同时做：

- 分支判断
- 解包
- 绑定变量

比如：

```rust
let Some(message_type) = MessageType::from_u16(header.msg_type) else {
    ...
};
```

这不是简单的条件判断，而是在同时做“校验 + 取值”。

### 10.3 `if let` / `let ... else`

这是读 Rust 业务代码必须适应的语法。

它本质上是：

- 如果模式匹配成功，就继续
- 否则走失败分支

比先判空再取值更紧凑。

## 11. 当前项目的工程取舍和局限

这份代码是“第一版可跑闭环”，不是最终形态。你读代码时要区分“机制正确”和“工程成熟度”。

当前比较明显的取舍有：

- 房间表只在内存里，没有分布式扩展能力
- 没有限流
- 没有消息频率控制
- 发送队列是无界的
- 断线重连、重放保护、幂等策略都还比较基础
- 房间状态机还比较简单，更多像 Demo 级别房间服

这些不是写法错误，而是当前阶段故意没做复杂化。

## 12. 推荐阅读顺序

如果你要真正吃透这个项目，我建议按这个顺序看：

1. [protocol.rs](/c:/project/MyServer/apps/game-server/src/protocol.rs)
2. [session.rs](/c:/project/MyServer/apps/game-server/src/session.rs)
3. [room.rs](/c:/project/MyServer/apps/game-server/src/room.rs)
4. [ticket.rs](/c:/project/MyServer/apps/game-server/src/ticket.rs)
5. [server.rs](/c:/project/MyServer/apps/game-server/src/server.rs)
6. [main.rs](/c:/project/MyServer/apps/game-server/src/main.rs)
7. [game.proto](/c:/project/MyServer/packages/proto/game.proto)
8. [auth-store.js](/c:/project/MyServer/apps/auth-http/src/auth-store.js)

这么看有两个好处：

- 先建立数据结构和协议认知，再看总流程
- 看到 `server.rs` 时不容易被大量 `match` 分支淹没

## 13. 你可以先抓住的“最小理解闭环”

如果你现在只想快速建立认知，可以先记住下面这个闭环：

1. 客户端通过 HTTP 登录拿到 ticket
2. TCP 连上 `game-server`
3. 先发 `AUTH_REQ`
4. 服务端校验 ticket 签名、过期时间、Redis 记录
5. 认证成功后，连接进入 `Authenticated`
6. 玩家加入房间
7. 房间成员准备
8. 房主开始游戏
9. 游戏中允许发送 `PLAYER_INPUT_REQ`
10. 房间内广播 `GAME_MESSAGE_PUSH`
11. 房主结束游戏，房间回到 `waiting`

只要这个闭环能在你脑子里跑通，你再去读具体 Rust 语法就不会乱。

## 14. 如果你接下来继续学这个项目，建议重点补哪几块 Rust

只针对这个项目，我建议你按下面顺序补 Rust：

1. `Option` / `Result`
2. `String` / `&str`
3. 所有权、借用、生命周期的基本规则
4. `async/await`
5. `Arc`、`Mutex`
6. `tokio::spawn`、channel
7. trait 和泛型的基础用法

这里先不用急着深挖高级生命周期技巧，因为这个项目本身已经尽量避开了最难读的写法。

## 15. 一句话总结

这个 `game-server` 的核心并不复杂：

- 一个 Tokio TCP 服务
- 一个基于 Redis ticket 的连接鉴权
- 一个内存房间表
- 一个简单清晰的房间状态机
- 一套通过 channel 解耦的广播写出机制

如果你熟悉 C++ 服务端，那么真正需要适应的不是业务本身，而是 Rust 用 `Option`、`Result`、借用、`Arc<Mutex<_>>`、`async/await` 把这些老问题表达得更显式。
