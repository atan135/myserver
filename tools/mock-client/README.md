# Mock Client 技术文档

## 概述

Mock Client 是一个用于测试 MyServer 游戏后端框架的联调工具。默认模拟玩家客户端只接触 `auth-http` 和 `game-proxy` 玩家入口；聊天场景默认保持本地 `9001/TCP` 内部联调方式，也可显式使用本地 `ws://127.0.0.1:9011/` 或从登录响应 `services.chat` 读取正式 `wss://chat.bevy.zergzerg.cn/`。玩家邮件场景从登录、选角或补签票响应的 `services.mail` 读取正式 HTTPS 地址；`mail-send` 和 `mail-send-and-notify` 是 service-token-only 的内部发信场景，必须显式传入本地内部地址。公告场景仍是内部联调路径。

## 项目结构

```
tools/mock-client/
├── src/
│   ├── index.js         # 主入口
│   ├── constants.js     # 协议常量 (MESSAGE_TYPE, SCENARIO, MAGIC)
│   ├── args.js          # 命令行参数解析
│   ├── protocol.js      # Protobuf 编解码工具
│   ├── messages.js      # 消息编码/解码函数
│   ├── packet.js        # 数据包封包/解包
│   ├── client.js        # TCP 协议客户端类
│   ├── websocket-client.js # WebSocket/WSS 协议客户端类
│   ├── auth.js          # 认证相关函数
│   └── scenarios/       # 场景测试模块
│       ├── index.js     # 场景统一导出
│       ├── room.js      # 房间相关场景
│       ├── chat.js      # 聊天相关场景
│       ├── mail.js      # 邮件相关场景 (HTTP + 通知联调)
│       ├── announce.js  # 公告相关场景 (HTTP CRUD 调试)
│       ├── character.js # 角色体系接口与选角后入服调试
│       ├── activity.js  # 活动领取、抽奖、幂等重放和结果查询
│       ├── game.js      # 游戏相关场景
│       ├── robot-sync.js # MyBevy Robot Sync 房间场景
│       ├── movement.js  # 移动相关场景
│       ├── movement-interactive.js # 交互式双客户端移动
│       ├── inventory.js # 背包系统测试
│       └── interactive.js # 交互式聊天
├── package.json
└── help.txt             # 命令行帮助
```

## 核心模块

### constants.js
定义协议常量：
- `MAGIC`: 0xCAFE - 协议头魔数
- `VERSION`: 协议版本
- `HEADER_LEN`: 协议头长度 (14 bytes)
- `MESSAGE_TYPE`: 所有消息类型枚举
- `SCENARIO`: 测试场景枚举

### protocol.js
Protobuf 风格的编解码工具：
- `encodeVarint()` / `decodeVarint()` - Varint 编解码
- `encodeStringField()` / `readString()` - 字符串字段
- `encodeBoolField()` / `readBool()` - 布尔字段
- `encodeInt64Field()` / `readInt64()` - 64位整数
- `encodeUInt32Field()` / `readUInt32()` - 32位无符号整数
- `decodeFieldsWithRepeated()` - 支持重复字段的解码

### messages.js
消息编解码函数：
- **编码器**: `encodeAuthReq`, `encodeRoomJoinReq`, `encodeRoomLeaveReq`, `encodeChatPrivateReq` 等
- `encodeMoveInputReq()` 支持附带客户端预测状态：`{ x, y, frameId }`
- **解码器**: `decodeByMessageType()` - 根据消息类型自动解码响应
  - 已支持解码 `MovementSnapshotPush` / `MovementRejectPush` 的校正字段
  - 已支持解码 `RoomReconnectRes` / `RoomJoinAsObserverRes` 的 `movementRecovery`
  - 已支持解码角色状态 push：`CharacterElementsChangePush(1505)`、`CharacterTitleChangePush(1506)`、`CharacterDisciplineChangePush(1507)`
  - `encodeRoomReconnectReq(lastCharacterPushSequence)` 可携带当前角色已应用的 push sequence；默认不传仍编码为空 body

### client.js
`TcpProtocolClient` 类：
- `connect()` - 建立 TCP 连接
- `send(messageType, seq, body)` - 发送数据包
- `readNextPacket(timeoutMs)` - 读取下一个数据包
- `readUntil(timeoutMs, predicate)` - 读取直到满足条件
- `close()` - 关闭连接

### auth.js
认证辅助函数：
- `fetchTicket(options, overrides)` - 从 HTTP 认证服务获取 ticket
- `fetchLoginSession(options, overrides)` - 只登录并获取 access token，不自动选角
- `listCharacters()` / `createCharacter()` / `selectCharacter()` - 调用角色列表、创建、选择接口
- `refreshTicketIfNeeded(options, login)` - ticket 快过期时通过 access token 重新签发
- `applyDiscoveredServices(options, login)` - 使用 auth-http 返回的 `services` 自动更新测试目标地址
- `resolveAccountCredentials()` - 解析账号密码
- `formatLoginSummary()` - 格式化登录信息，清晰输出账号 `accountPlayerId`、游戏内 `characterId`、`worldId`、`exp`

当前登录后必须先选择角色才能拿到可用于游戏入口的 ticket。`fetchTicket()` 会在登录后按 `--character-id`、已有角色列表、`--auto-create-character` / `--create-character-if-missing` 选择或创建角色；无角色且未请求自动创建时会提示先创建或选择角色，不会直接进入游戏。

角色接口和 ticket payload 以 `docs/协议与客户端/协议设计.md` 为准；外部客户端接入流程见 `docs/协议与客户端/外部客户端接入说明.md`。

### scenarios/mail.js
邮件辅助场景：
- 玩家列表、详情、已读和领取使用当前 character-bound game ticket 的 `X-Game-Ticket` header，身份不从 query 或 body 传入
- 优先从 `services.mail` 读取 `https://api.bevy.zergzerg.cn/api/v1/mails`；只有 descriptor 缺失或 `--no-service-discovery` 时，玩家场景才使用显式 `--mail-base-url` 本地调试地址
- 系统发信仅允许 `mail-send` / `mail-send-and-notify` 携带显式 `--service-token` 直连内部地址，不能经 Caddy 玩家入口
- 领取收到 `202` 结果未知时只查询同一邮件状态，不重发 claim
- 支持联调 `chat-server` 的 `MAIL_NOTIFY_PUSH`

邮件内网清单只验收本地或隔离测试入口，不包含 Caddy、DNS、TLS、公开 descriptor 或公网可达性。手工验证玩家场景时同时传 `--no-service-discovery` 和 loopback `--mail-base-url`，确保目标不会被登录响应改写；内部发信另传测试环境提供的 `--service-token`。不要把 game ticket、service token、数据库连接串或真实服务地址写入命令脚本、文档和日志。

完整隔离验收优先使用仓库测试 harness，不需要占用 `3000`、`7000`、`9001`、`9003` 等默认端口，也不会停止已经运行的开发服务。依赖和清理边界见 `docs/周边服务/聊天与邮件系统设计.md` 的“内网隔离测试与验收”。在仓库根目录使用以下定向命令：

```powershell
npm test --workspace mail-service
npx tsc --noEmit -p apps/mail-service/tsconfig.json
npm test --workspace mock-client
node --test tests/mail/mail-runtime-cleanup.test.mjs tests/mail/mail-managed-process.test.mjs
node --test tests/mail/mail-internal-core-flow.test.mjs
node --test tests/mail/mail-reliability-fault-drill.test.mjs
npm run test:mail:server
```

`mail-internal-core-flow` 和 `mail-reliability-fault-drill` 两个真实联调用例要求本地 PostgreSQL 管理凭证；可靠性演练还要求 Redis 和 Core NATS 的可执行文件可被解析，以及已编译的 `game-server` / `chat-server`。`test:mail:server` 是完整服务端准入入口，会在 `.tmp/cargo-target/mail-server-self-test` 构建隔离的 Rust 制品，不覆盖或停止正在运行的默认目录服务。Redis、Core NATS 和应用服务都由测试在随机 loopback 端口启动，不要求也不复用预先运行的默认端口服务。测试自行创建并删除 `myserver_mail_acceptance_<run-id>` 数据库，并使用独立 Redis prefix。单独运行可靠性演练时，可通过 `TEST_GAME_SERVER_BIN` / `TEST_CHAT_SERVER_BIN` 指向相对项目根目录的隔离制品；文档和验收记录不保存这些环境中的凭证值。

### scenarios/announce.js
公告辅助场景：
- 通过 HTTP 调用 `announce-service` 的公告 CRUD 接口
- 支持列表筛选：`locale`、`priority`、`target_group`、`active_only`
- 支持时间窗口调试：`start_time`、`end_time`、`duration_seconds`

## 协议格式

### 数据包结构 (14字节头 + body)
```
+--------+--------+--------+--------+--------+--------+
| MAGIC (2B) | Ver | Flag | MsgType (2B) | Seq (4B) |
+--------+--------+--------+--------+--------+--------+
|              Body Length (4B)           |  Body... |
+--------+--------+--------+--------+--------+--------+
```

- **MAGIC**: 0xCAFE (big-endian)
- **Version**: 1
- **Flag**: 0
- **MessageType**: 消息类型 ID
- **Seq**: 序列号
- **BodyLength**: body 长度 (big-endian)

## 测试场景

### 房间场景 (room.js)
| 场景 | 说明 |
|------|------|
| `happy` | 正常流程：登录→入房→准备→离房 |
| `get-room-data` | 获取房间数据 |
| `get-room-data-in-room` | 在房间内获取数据 |
| `two-client-room` | 双客户端：入房→离房→房主转移 |
| `start-game-single-client` | 单客户端开始游戏 (应失败) |
| `start-game-ready-room` | 双客户端准备后开始游戏 |
| `invalid-ticket` | 非法 ticket 认证 |
| `unauth-room-join` | 未认证入房 |
| `unknown-message` | 未知消息类型 |
| `oversized-room-join` | 超大 RoomId |
| `reconnect` | 断线重连 |
| `reconnect-all-disconnected` | 全员掉线后 TTL 内双重连 |

### 匹配场景 (room.js)
| 场景 | 说明 |
|------|------|
| `create-matched-room` | 创建匹配房间并通知 MatchService |
| `create-matched-room-and-join` | 创建匹配房间并让所有角色加入，验证完整回调 |

### 游戏场景 (game.js)
| 场景 | 说明 |
|------|------|
| `gameplay-roundtrip` | 完整游戏流程：入房→准备→开始→输入→结束 |
| `combat-dual-client` | 双客户端 `combat_demo` 联调：A 施法，B 掉血并验证快照 |
| `movement-demo` | movement_demo 单客户端位移联调 |
| `robot-sync-room` | 双客户端 `robot_sync_room` 联调：验证 `robot_move` 帧转发和非法输入拒绝 |

### 聊天场景 (chat.js, interactive.js)

聊天传输通过 `--chat-transport` 选择，默认 `tcp` 不改变既有流程：

| transport | 使用场景 | 地址来源 |
|-----------|----------|----------|
| `tcp` | 本地内部联调 | 显式 `--chat-port 9001`，可选 `--chat-host` |
| `ws` | 本地可选 WebSocket listener | 显式 `--chat-ws-url ws://127.0.0.1:9011/` |
| `wss` | 正式公网入口 | 优先读取登录/签票响应 `services.chat`；测试可显式传入 `--chat-ws-url wss://.../` |

每个 `ws`/`wss` logical binary message 承载且只承载一个既有 14 字节聊天协议包。传输层复用现有包头与 Protobuf 编解码，不接受文本消息、半包或多包拼接。

| 场景 | 说明 |
|------|------|
| `chat-private` | 私聊消息 |
| `chat-group` | 群聊消息 |
| `group-create` | 创建群组 |
| `group-join` | 加入群组 |
| `group-leave` | 离开群组 |
| `group-dismiss` | 解散群组 |
| `group-list` | 群组列表 |
| `chat-history` | 聊天历史 |
| `chat-two-client` | 双客户端群聊 |
| `chat-private-two-client` | 双客户端私聊 |
| `chat-interactive` | 交互式聊天 (终端输入) |

### 邮件场景 (mail.js)
| 场景 | 说明 |
|------|------|
| `mail-send` | 通过内部 service token 发送邮件到指定玩家或当前登录玩家 |
| `mail-list` | 获取当前 ticket 绑定玩家的邮件列表 |
| `mail-get` | 获取邮件详情 |
| `mail-read` | 标记邮件已读 |
| `mail-claim` | 领取邮件附件（重复领取会返回幂等结果） |
| `mail-send-and-notify` | 发邮件并等待聊天服 `MAIL_NOTIFY_PUSH` |

### 公告场景 (announce.js)
| 场景 | 说明 |
|------|------|
| `announce-list` | 获取公告列表，支持按语言、优先级、目标组、是否仅激活中筛选 |
| `announce-get` | 获取单条公告详情 |
| `announce-create` | 创建公告，需提供标题、内容和结束时间或持续时长 |
| `announce-update` | 更新公告标题、正文、时间窗口、优先级等字段 |
| `announce-delete` | 删除公告 |

### 移动同步场景 (movement.js, movement-interactive.js)
| 场景 | 说明 |
|------|------|
| `movement-demo` | movement_demo 单客户端位移联调 |
| `movement-sync-validation` | 移动同步验证：MoveDir/MoveStop/FaceTo |
| `movement-dual-client-sync` | 双客户端移动同步验证 |
| `movement-snapshot-throttle` | 快照节流验证（每3帧） |
| `movement-face-to` | FaceTo 转向与 last input wins |
| `movement-authoritative-correction` | 客户端预测漂移后，验证服务端下发强校正 |
| `movement-reconnect-recovery` | movement_demo 断线重连，验证 `movement_recovery` 恢复数据 |
| `movement-interactive` | 交互式双客户端移动同步（键盘控制） |

### 背包系统场景 (inventory.js)
背包场景会先通过账号登录和选角获取 character-bound game ticket，随后所有背包、仓库、装备和道具操作都作用于该 `characterId`。场景启动时会打印 `inventory.target`，用于确认当前账号和角色目标。

| 场景 | 说明 |
|------|------|
| `inventory-equip` | 装备穿戴到指定槽位 |
| `inventory-use` | 使用背包中的消耗品 |
| `inventory-discard` | 丢弃背包中的物品 |
| `inventory-warehouse` | 仓库存取操作 |
| `inventory-get` | 获取当前背包和仓库状态 |
| `inventory-full` | 完整背包流程测试 |

### 活动系统场景 (activity.js)

`activity` 默认是 live 场景：先通过 `auth-http` 登录和选角，再使用配置的 TCP endpoint 连接 `game-proxy` 本地 TCP fallback，完成 `AuthReq`（同时触发服务端可信 `game_entry` 登录事件）后依次执行 `list -> detail -> progress -> claim/draw -> 同 request id 重放 -> detail -> progress`。本地默认 fallback 是 `127.0.0.1:14000`（默认 KCP `PROXY_PORT=4000` 加 10000，实际值可由 game-proxy `.env` 覆盖后再通过 `--host/--port` 指定）。当前 auth-http 的 `services.game` 是 KCP descriptor，TCP mock-client 会有意忽略；只有登录响应实际提供 `protocol: tcp` 时，现有 discovery helper 才覆盖 TCP endpoint。

每个响应都按 `messageType + seq` 匹配，并核对活动、版本、阶段、动作和 request id；首次失败会带服务端 `error_code` 退出，重放必须返回 `duplicate=true` 且保持相同 `processing` 状态。live flow 默认在非重放请求之间等待 150ms，避开 detail/progress 共享的 100ms `read:detail` 限流窗口；`--activity-pacing-ms` 可在 `0..5000` 内调整，`0` 只适合 fake-client/专项限流测试。首次动作与同 request id 重放之间不会插入该等待。

| 场景 | 说明 |
|------|------|
| `activity --activity-action claim` | 领取服务端阶段奖励并验证重复领取 |
| `activity --activity-action draw` | 执行免费或道具券抽奖并验证重复请求；具体消耗由目标活动的服务端配置和玩家状态决定 |
| `activity --activity-dry-run` | 仅离线构造同一请求序列，不登录、不连接服务 |

活动协议 body 只接受服务端已下发的 `activity id`、`version`、可选/必需的 `stage id`、`claim|draw` 和不透明 `client request id`；request id 按服务端规则最多 128 UTF-8 bytes。CLI 的 `--character-id` 仅供 `auth-http` 选角和签发 character-bound ticket，不会写入任何活动请求；活动协议不接受角色身份、奖励内容、奖池权重、概率、进度、道具券 UID/数量或其他服务端权威字段，也不会输出 game ticket、access token 或密码。当前协议没有独立的“抽奖结果查询”请求；场景在动作后重新读取 `ActivityDetailRes.progress_json` 和 `ActivityProgressRes.progress_json`，仅断言它们是对应活动版本的合法 JSON，不伪造额外结果接口。

### 角色体系场景 (character.js)
| 场景 | 说明 |
|------|------|
| `character-list` | 查询当前账号角色列表，可输出 JSON |
| `character-create` | 创建角色，支持角色名和外观 JSON |
| `character-profile` | 查询当前账号指定或首个角色的 auth-http 基础资料、四属性、称号 / 职业空态和同名区分信息 |
| `character-delete` | 软删除当前账号指定或首个角色，输出恢复窗口和硬删冷却信息 |
| `character-restore` | 恢复当前账号指定软删除角色，必须传 `--character-id` |
| `character-select` | 选择角色并展示 `characterId` 和 ticket payload 摘要 |
| `character-login-auth` | 登录、选角、连接 `game-proxy` 并完成 `AuthReq` |
| `character-room-join` | 登录、选角、连接游戏入口并加入房间 |
| `character-elements-debug` | 登录、选角、查询四属性、执行受控 debug 变更、监听 `CharacterElementsChangePush`、再次查询并输出 before/change/after |
| `character-titles-debug` | 登录、选角、查询称号、debug 授予称号、装备称号、监听称号 push、再次查询并输出 JSON 摘要 |
| `character-disciplines-debug` | 登录、选角、设置职业阶位、触发称号解锁检查、监听职业 push、确认职业阶位称号授予 |
| `character-discipline-learn` | 登录、选角、调用正式职业学习协议、监听职业 push、再次查询职业列表并输出定义和消耗摘要 |
| `character-discipline-activate` | 登录、选角、激活已学习职业 / 流派、监听职业 push，并输出 activeSkillPool 和称号解锁摘要 |
| `character-discipline-deactivate` | 登录、选角、停用已激活职业 / 流派、监听职业 push，并输出 activeSkillPool |
| `character-discipline-switch` | 登录、选角、切换当前激活职业 / 流派、监听职业 push，并输出切换后的 activeSkillPool |
| `character-discipline-points` | 登录、选角、给已学习职业 / 流派增加 points、监听职业 push，并观察自动阶位推进 |
| `character-progress-apply` | 登录、选角、触发正式任务 / 成就 / 活动 / 排行 / 世界事件进度奖励，并监听首个角色状态 push |
| `character-role-system-check` | 聚合验收角色创建 / profile / 删除 / 恢复、正式职业学习 / 激活、任务称号触发、push 监听和可选后台只读校验 |
| `admin-character-readonly-check` | 仅调用 `admin-api` 角色 profile / titles 只读端点并输出摘要，不执行 GM 写操作 |
| `character-duplicate-name` | 创建两个同名角色，验证同名角色允许创建 |
| `character-limit` | 连续创建角色，验证普通账号第 7 个角色返回 `CHARACTER_LIMIT_EXCEEDED` |

### 认证与安全场景 (auth.js)
| 场景 | 说明 |
|------|------|
| `logout` | 登录、校验 `/me`、退出登录并确认 session 失效 |
| `kick-session` | 同账号重复登录踢旧 session，并验证 TCP kick push |
| `password-ticket-revoke` | 改密后旧 game ticket 应被拒绝，新密码登录后的新 ticket 可用 |

## 使用方法

### ID 格式

当前服务端使用全局唯一 ID 机制。登录返回的玩家 ID 为 `plr_<base32>`；邮件、公告、聊天消息和聊天群分别使用 `mail_`、`ann_`、`msg_`、`grp_` 前缀；物品实例 `uid` 为可解码的 `uint64` 数字 ID。

角色状态 push 的 `push` 摘要包含 `characterId`、`sequence`、`revision`、`sourceType/sourceId`、`action` 和 `summary`。场景会断言 push 的 `characterId` 等于当前登录 ticket 绑定角色；真实客户端应按 `characterId + revision` 去重，并在断线重连后重新查询四属性、称号和职业快照。重连时可把已应用的最近角色 push sequence 传给 `encodeRoomReconnectReq(lastCharacterPushSequence)`；服务端补偿回放的 push 会带 `snapshotCompensation: true`。

### 基础用法

```bash
# 正常流程测试
node tools/mock-client/src/index.js --scenario happy \
  --http-base-url http://127.0.0.1:3000 \
  --login-name test001 --password Passw0rd! \
  --character-id chr_0000000000001

# 查询角色列表，输出机器可读 JSON
node tools/mock-client/src/index.js --scenario character-list \
  --http-base-url http://127.0.0.1:3000 \
  --login-name test001 --password Passw0rd! \
  --json-output

# 创建角色
node tools/mock-client/src/index.js --scenario character-create \
  --http-base-url http://127.0.0.1:3000 \
  --login-name test001 --password Passw0rd! \
  --character-name Echo \
  --character-appearance-json '{"body":"default","palette":"blue"}'

# 选择角色并展示 ticket payload 摘要
node tools/mock-client/src/index.js --scenario character-select \
  --http-base-url http://127.0.0.1:3000 \
  --login-name test001 --password Passw0rd! \
  --character-id chr_0000000000001 \
  --json-output

# 查询角色资料
node tools/mock-client/src/index.js --scenario character-profile \
  --http-base-url http://127.0.0.1:3000 \
  --login-name test001 --password Passw0rd! \
  --character-id chr_0000000000001 \
  --json-output

# 软删除和恢复角色
node tools/mock-client/src/index.js --scenario character-delete \
  --http-base-url http://127.0.0.1:3000 \
  --login-name test001 --password Passw0rd! \
  --character-id chr_0000000000001 \
  --json-output

node tools/mock-client/src/index.js --scenario character-restore \
  --http-base-url http://127.0.0.1:3000 \
  --login-name test001 --password Passw0rd! \
  --character-id chr_0000000000001 \
  --json-output

# 一条命令登录、创建角色、选择角色、连接 game-proxy 完成 AuthReq
node tools/mock-client/src/index.js --scenario character-login-auth \
  --http-base-url http://127.0.0.1:3000 \
  --host 127.0.0.1 --port 14000 \
  --login-name test001 --password Passw0rd! \
  --auto-create-character --character-name Echo

# 选择已有角色后进入房间基础流程
node tools/mock-client/src/index.js --scenario character-room-join \
  --http-base-url http://127.0.0.1:3000 \
  --host 127.0.0.1 --port 14000 \
  --login-name test001 --password Passw0rd! \
  --character-id chr_0000000000001 \
  --room-id room-character-debug

`character-room-join` 会等待 `ROOM_JOIN_RES`，中间如果先收到 `RoomFrameRatePush` 或 `RoomStatePush` 会继续读取。真实客户端也应按 `messageType + seq` 匹配请求响应，不能假设请求后的下一包一定是对应响应包。

# 查询并调试修改角色四属性
node tools/mock-client/src/index.js --scenario character-elements-debug \
  --http-base-url http://127.0.0.1:3000 \
  --host 127.0.0.1 --port 14000 \
  --login-name test001 --password Passw0rd! \
  --character-id chr_0000000000001 \
  --element-affinity-earth-delta -100 \
  --element-affinity-fire-delta 100 \
  --element-mastery-fire-delta 10 \
  --element-debug-token "$MYSERVER_CHARACTER_ELEMENT_DEBUG_TOKEN" \
  --json-output

`character-elements-debug` 依次发送 `GetCharacterElementsReq(1413)`、`DebugApplyCharacterElementChangeReq(1415)` 和 `GetCharacterElementsReq(1413)`，输出 `before`、`change`、`after`。debug 变更需要玩家 ticket 加 `MYSERVER_CHARACTER_ELEMENT_DEBUG_TOKEN` / `--element-debug-token`，仅用于非生产测试或 GM 调试；真实客户端应把四属性查询结果或后续变化推送作为异步状态更新处理。

# 查询、授予并装备角色称号
node tools/mock-client/src/index.js --scenario character-titles-debug \
  --http-base-url http://127.0.0.1:3000 \
  --host 127.0.0.1 --port 14000 \
  --login-name test001 --password Passw0rd! \
  --character-id chr_0000000000001 \
  --title-id 9001 \
  --title-debug-token "$MYSERVER_CHARACTER_TITLE_DEBUG_TOKEN" \
  --json-output

# 设置职业阶位并触发称号解锁检查
node tools/mock-client/src/index.js --scenario character-disciplines-debug \
  --http-base-url http://127.0.0.1:3000 \
  --host 127.0.0.1 --port 14000 \
  --login-name test001 --password Passw0rd! \
  --character-id chr_0000000000001 \
  --discipline-id forging \
  --discipline-tier novice \
  --discipline-points 1 \
  --title-debug-token "$MYSERVER_CHARACTER_TITLE_DEBUG_TOKEN" \
  --json-output

# 正式学习职业 / 流派
node tools/mock-client/src/index.js --scenario character-discipline-learn \
  --http-base-url http://127.0.0.1:3000 \
  --host 127.0.0.1 --port 14000 \
  --login-name test001 --password Passw0rd! \
  --character-id chr_0000000000001 \
  --discipline-id forging \
  --json-output

# 激活、切换、推进职业 / 流派
node tools/mock-client/src/index.js --scenario character-discipline-activate \
  --http-base-url http://127.0.0.1:3000 \
  --host 127.0.0.1 --port 14000 \
  --login-name test001 --password Passw0rd! \
  --character-id chr_0000000000001 \
  --discipline-id forging \
  --json-output

node tools/mock-client/src/index.js --scenario character-discipline-switch \
  --http-base-url http://127.0.0.1:3000 \
  --host 127.0.0.1 --port 14000 \
  --login-name test001 --password Passw0rd! \
  --character-id chr_0000000000001 \
  --discipline-id fire_art \
  --json-output

node tools/mock-client/src/index.js --scenario character-discipline-points \
  --http-base-url http://127.0.0.1:3000 \
  --host 127.0.0.1 --port 14000 \
  --login-name test001 --password Passw0rd! \
  --character-id chr_0000000000001 \
  --discipline-id forging \
  --discipline-points 120 \
  --json-output

# 正式触发任务 / 成就 / 活动 / 排行 / 世界事件进度奖励
node tools/mock-client/src/index.js --scenario character-progress-apply \
  --http-base-url http://127.0.0.1:3000 \
  --host 127.0.0.1 --port 14000 \
  --login-name test001 --password Passw0rd! \
  --character-id chr_0000000000001 \
  --progress-id achievement_first_forge \
  --json-output

# 角色体系聚合验收：生命周期 + profile + 职业 + 任务称号 + push + 可选后台只读
node tools/mock-client/src/index.js --scenario character-role-system-check \
  --http-base-url http://127.0.0.1:3000 \
  --admin-base-url http://127.0.0.1:3001 \
  --host 127.0.0.1 --port 14000 \
  --login-name test001 --password Passw0rd! \
  --character-name RoleStage11 \
  --discipline-id forging \
  --progress-id achievement_first_forge \
  --admin-token "$MYSERVER_ADMIN_TOKEN" \
  --json-output

# 后台只读校验；只访问 admin-api GET profile/titles，不内置管理员 token
node tools/mock-client/src/index.js --scenario admin-character-readonly-check \
  --admin-base-url http://127.0.0.1:3001 \
  --admin-token "$MYSERVER_ADMIN_TOKEN" \
  --character-id chr_0000000000001 \
  --admin-log-limit 20 \
  --json-output

`character-discipline-learn` 只发送 `LearnCharacterDisciplineReq(1425)` 的 `discipline_id`，角色身份来自 ticket 绑定的当前连接，不传 `character_id` 或 debug token。激活、停用、切换和 points 推进分别使用正式玩家协议，不需要 debug token，响应包含 `activeSkillPool` 和本次 `unlockedTitles` 摘要。`character-progress-apply` 只发送 `ApplyCharacterProgressReq(1433)` 的 `progress_id`，由服务端按 CSV 解析任务、成就、活动、排行榜或世界事件来源并返回 `ApplyCharacterProgressRes(1434)`，响应包含 `applied`、`sourceType/sourceId` 和奖励摘要。`character-titles-debug` 和 `character-disciplines-debug` 输出包含 `before`、`action`、`after`、`unlockedTitles`、`equippedTitle`、`discipline` 和 `request`，便于测试脚本断言。debug 入口需要玩家 ticket 加 `MYSERVER_CHARACTER_TITLE_DEBUG_TOKEN` / `--title-debug-token`，并且生产配置拒绝启用。手动验收依赖和步骤见 `docs/游戏服与接入层/角色与成长/角色体系与四属性设计.md`；启动 PostgreSQL、Redis、Core NATS、auth-http、game-proxy、game-server 或执行真实联调命令前，必须先由用户确认。

`character-role-system-check` 默认创建临时角色并执行软删除 / 恢复，用于覆盖生命周期和 profile 闭环；如果传入 `--character-id`，场景只使用指定角色并跳过破坏性生命周期步骤。该场景会正式学习并激活 `--discipline-id`，随后触发 `--progress-id`，按 `messageType + seq` 匹配响应，并监听 `CharacterDisciplineChangePush` 和进度奖励产生的角色状态 push。提供 `--admin-token` 时，它会额外调用 `admin-character-readonly-check` 的只读后台接口；token 仅来自参数或 `MYSERVER_ADMIN_TOKEN` / `ADMIN_API_TOKEN` 环境变量，不写死在工具中。

# 登录奖励领取与同 request id 重放
node tools/mock-client/src/index.js --scenario activity \
  --http-base-url http://127.0.0.1:3000 \
  --host 127.0.0.1 --port 14000 \
  --login-name <LOGIN_NAME> --password <PASSWORD> \
  --character-id <CHARACTER_ID> \
  --activity-id <LOGIN_REWARD_ACTIVITY_ID> --activity-version <VERSION> \
  --activity-stage-id <SERVER_STAGE_ID> --activity-action claim \
  --activity-request-id <NEW_CLAIM_REQUEST_ID>

# 免费/道具券抽奖与同 request id 重放；免费次数和券消耗均由服务端判定
node tools/mock-client/src/index.js --scenario activity \
  --http-base-url http://127.0.0.1:3000 \
  --host 127.0.0.1 --port 14000 \
  --login-name <LOGIN_NAME> --password <PASSWORD> \
  --character-id <CHARACTER_ID> \
  --activity-id <LOTTERY_ACTIVITY_ID> --activity-version <VERSION> \
  --activity-action draw --activity-request-id <NEW_DRAW_REQUEST_ID>

# 仅检查构包，不访问 auth-http 或 game-proxy
node tools/mock-client/src/index.js --scenario activity --activity-dry-run \
  --activity-id <ACTIVITY_ID> --activity-version <VERSION> \
  --activity-stage-id <SERVER_STAGE_ID> --activity-action claim \
  --activity-request-id <REQUEST_ID>

# 房间测试
node tools/mock-client/src/index.js --scenario two-client-room \
  --http-base-url http://127.0.0.1:3000 --room-id test-room

# movement_demo 位移联调
node tools/mock-client/src/index.js --scenario movement-demo \
  --http-base-url http://127.0.0.1:3000 \
  --login-name test001 --password Passw0rd! \
  --room-id room-movement-demo --policy-id movement_demo

# 客户端预测漂移 -> 权威强校正
node tools/mock-client/src/index.js --scenario movement-authoritative-correction \
  --http-base-url http://127.0.0.1:3000 \
  --room-id room-movement-correction --policy-id movement_demo

# movement_demo 断线重连恢复
node tools/mock-client/src/index.js --scenario movement-reconnect-recovery \
  --http-base-url http://127.0.0.1:3000 \
  --room-id room-movement-reconnect --policy-id movement_demo

# 全员掉线后 TTL 内双重连
node tools/mock-client/src/index.js --scenario reconnect-all-disconnected \
  --http-base-url http://127.0.0.1:3000 \
  --host 127.0.0.1 \
  --room-id room-reconnect-all

# combat_demo 双客户端联调
node tools/mock-client/src/index.js --scenario combat-dual-client \
  --http-base-url http://127.0.0.1:3000 \
  --room-id room-combat-demo --policy-id combat_demo \
  --combat-skill-id 2

# MyBevy arena.robot_sync / robot_sync_room 双客户端联调
node tools/mock-client/src/index.js --scenario robot-sync-room \
  --http-base-url http://127.0.0.1:3000 \
  --host 127.0.0.1 --port 14000 \
  --room-id robot-sync-room --policy-id robot_sync_room

# 聊天 TCP 测试（9001 是本地内部联调地址示例）
node tools/mock-client/src/index.js --scenario chat-private \
  --http-base-url http://127.0.0.1:3000 \
  --chat-port 9001 --target-id <plr_...> --content "Hello!"

# 聊天本地 WebSocket 测试（chat-server 显式开启 CHAT_WS_ENABLED 后才可用）
node tools/mock-client/src/index.js --scenario chat-private \
  --http-base-url http://127.0.0.1:3000 \
  --chat-transport ws --chat-ws-url ws://127.0.0.1:9011/ \
  --target-id <plr_...> --content "Hello over WS!"

# 聊天正式 WSS 测试（auth 的 services.chat 下发 wss://chat.bevy.zergzerg.cn/）
node tools/mock-client/src/index.js --scenario chat-private \
  --http-base-url https://api.bevy.zergzerg.cn \
  --chat-transport wss --target-id <plr_...> --content "Hello over WSS!"

# 系统发信（9003 仅为本地内部联调地址，不能使用 api.bevy.zergzerg.cn）
node tools/mock-client/src/index.js --scenario mail-send \
  --http-base-url http://127.0.0.1:3000 \
  --mail-base-url http://127.0.0.1:9003 \
  --service-token <MAIL_SERVICE_TOKEN> \
  --login-name test001 --password Passw0rd! \
  --mail-title "系统奖励" --mail-content "请查收附件"

# 正式玩家读取（services.mail 自动提供 https://api.bevy.zergzerg.cn）
node tools/mock-client/src/index.js --scenario mail-list \
  --http-base-url https://api.bevy.zergzerg.cn \
  --login-name test001 --password Passw0rd! --mail-status unread --limit 10

# 邮件通知联调（9003/9001 是本地内部联调地址，系统发信需要独立 service token）
node tools/mock-client/src/index.js --scenario mail-send-and-notify \
  --http-base-url http://127.0.0.1:3000 \
  --mail-base-url http://127.0.0.1:9003 \
  --service-token <MAIL_SERVICE_TOKEN> \
  --host 127.0.0.1 --chat-port 9001 \
  --login-name test001 --password Passw0rd! \
  --mail-title "通知测试" --mail-content "测试聊天服邮件通知"

# 公告列表（9004 是本地内部联调地址示例）
node tools/mock-client/src/index.js --scenario announce-list \
  --announce-base-url http://127.0.0.1:9004

# 创建公告（9004 是本地内部联调地址示例）
node tools/mock-client/src/index.js --scenario announce-create \
  --announce-base-url http://127.0.0.1:9004 \
  --announce-admin-token dev-only-change-this-announce-admin-token \
  --announce-title "系统公告" \
  --announce-content "今晚 20:00 维护" \
  --announce-type popup \
  --announce-priority 20 \
  --announce-duration-seconds 3600

# 改密后旧 ticket 失效验证
node tools/mock-client/src/index.js --scenario password-ticket-revoke \
  --http-base-url http://127.0.0.1:3000 \
  --login-name test001 --password OldPass123! \
  --new-password NewPass456!
```

### 命令行参数

| 参数 | 说明 | 默认值 |
|------|------|--------|
| `--scenario` | 测试场景名称 | `happy` |
| `--http-base-url` | 认证服务地址 | `http://127.0.0.1:3000` |
| `--admin-base-url` | 管理后台 API 地址，用于 `admin-character-readonly-check` 或聚合场景可选后台校验 | `http://127.0.0.1:3001` |
| `--admin-token` | admin-api Bearer token，默认读取 `MYSERVER_ADMIN_TOKEN` 或 `ADMIN_API_TOKEN`，不内置真实 token | 空 |
| `--admin-log-limit` | admin-api 角色 profile / titles 只读查询日志条数 | `20` |
| `--announce-base-url` | 公告服务地址，内部联调场景必须显式传入 | 空 |
| `--mail-base-url` | 本地 `9003` 内部联调地址；玩家场景优先使用 `services.mail`，系统发信必须显式传入 | 空 |
| `--host` | 玩家入口地址 | `127.0.0.1` |
| `--game-host` | 游戏 TCP 服务器地址；未传时使用 `--host` | 空 |
| `--port` | 玩家 TCP 入口端口，默认走 `game-proxy` TCP fallback | `14000` |
| `--chat-host` | 聊天 TCP 服务器地址；未传时使用 `--host` | 空 |
| `--chat-port` | 聊天服务器端口，内部联调场景必须显式传入 | `0` |
| `--chat-transport` | 聊天传输：`tcp`、`ws` 或 `wss`；默认保持 TCP | `tcp` |
| `--chat-ws-url` | WebSocket 根 URL；`ws`/`wss` 测试显式覆盖登录响应 `services.chat` | 空 |
| `--room-id` | 房间ID | `room-default` |
| `--login-name` | 登录用户名 | - |
| `--password` | 登录密码 | - |
| `--new-password` | `password-ticket-revoke` 改密后的新密码 | - |
| `--no-restore-password` | `password-ticket-revoke` 结束后不恢复原密码 | 默认恢复 |
| `--character-id` | 指定已有角色，签发 character-bound game ticket | 空 |
| `--character-name` | 创建角色名，或按名称选择已有角色；角色名允许重复 | 空 |
| `--character-appearance-json` | 创建角色外观 JSON 对象；PowerShell 建议用单引号包裹 | `{}` |
| `--auto-create-character` | 登录后直接创建新角色并选择 | `false` |
| `--create-character-if-missing` | 无角色或指定角色名不存在时创建角色 | `false` |
| `--character-name-prefix` | 自动生成角色名的前缀 | `MockRole` |
| `--element-affinity-earth-delta` | `character-elements-debug` 的地亲和 delta | `-100` |
| `--element-affinity-fire-delta` | `character-elements-debug` 的火亲和 delta | `100` |
| `--element-affinity-water-delta` | `character-elements-debug` 的水亲和 delta | `0` |
| `--element-affinity-wind-delta` | `character-elements-debug` 的风亲和 delta | `0` |
| `--element-mastery-earth-delta` | `character-elements-debug` 的地掌握 delta | `0` |
| `--element-mastery-fire-delta` | `character-elements-debug` 的火掌握 delta | `10` |
| `--element-mastery-water-delta` | `character-elements-debug` 的水掌握 delta | `0` |
| `--element-mastery-wind-delta` | `character-elements-debug` 的风掌握 delta | `0` |
| `--element-change-reason` | `character-elements-debug` 写入日志的原因 | `mock-client character element debug` |
| `--element-debug-token` | 四属性 debug 变更 token，默认读取 `MYSERVER_CHARACTER_ELEMENT_DEBUG_TOKEN` | 空 |
| `--title-id` | `character-titles-debug` 授予/装备的称号 ID | `9001` |
| `--discipline-id` | 职业学习、激活、停用、切换、points 推进或 `character-disciplines-debug` 设置的职业 ID | `forging` |
| `--discipline-tier` | `character-disciplines-debug` 设置的职业阶位 | `novice` |
| `--discipline-points` | `character-disciplines-debug` 设置的职业点数，或 `character-discipline-points` 的 points 增量 | `1` |
| `--progress-id` | `character-progress-apply` 触发的正式进度配置 ID | `achievement_first_forge` |
| `--role-system-skip-debug` | `character-role-system-check` 跳过 debug 授称号 / 装备称号步骤 | `false` |
| `--role-system-skip-delete-restore` | `character-role-system-check` 创建临时角色时跳过软删除 / 恢复生命周期步骤 | `false` |
| `--title-change-reason` | 称号/职业 debug 写入日志的原因 | `mock-client character title debug` |
| `--title-debug-token` | 称号/职业 debug token，默认读取 `MYSERVER_CHARACTER_TITLE_DEBUG_TOKEN` | 空 |
| `--json-output` | 输出机器可读 JSON，便于测试脚本断言 | `false` |
| `--login-name-a` | 客户端A登录用户名 | - |
| `--password-a` | 客户端A登录密码 | - |
| `--login-name-b` | 客户端B登录用户名 | - |
| `--password-b` | 客户端B登录密码 | - |
| `--ticket` | 直接指定 ticket | - |
| `--no-service-discovery` | 禁用 auth-http 登录响应中的 `services` 自动覆盖测试目标地址 | 默认启用 |
| `--timeout-ms` | 超时毫秒 | `5000` |
| `--policy-id` | 入房时指定房间策略 | 空 |
| `--move-frames` | movement-demo 发包帧列表，逗号分隔 | `1,2,3,4,5` |
| `--combat-skill-id` | `combat-dual-client` 使用的技能 ID，默认 `2`(fireball) | `2` |
| `--content` | 聊天消息内容 | `Hello from mock-client!` |
| `--mail-id` | 邮件 ID（mail-get / mail-read / mail-claim），格式为 `mail_<base32>` | 空 |
| `--mail-player-id` | 已废弃；玩家身份只能来自 `X-Game-Ticket`，传入会被拒绝 | 空 |
| `--mail-to-player-id` | 邮件接收方玩家 ID（仅内部 `mail-send`） | 空 |
| `--mail-status` | 邮件状态筛选，如 `unread` / `read` | 空 |
| `--mail-offset` | 邮件列表偏移量 | `0` |
| `--mail-title` | 邮件标题 | `Mock mail from mock-client` |
| `--mail-content` | 邮件正文 | `Hello from mock-client mail!` |
| `--mail-type` | 邮件类型 | `system` |
| `--sender-type` | 发件人类型 | `system` |
| `--sender-id` | 发件人 ID | `system` |
| `--sender-name` | 发件人展示名 | `系统` |
| `--created-by-type` | 实际触发者类型 | `script` |
| `--created-by-id` | 实际触发者 ID | `mock-client` |
| `--created-by-name` | 实际触发者展示名 | `mock-client` |
| `--attachments-json` | 邮件附件 JSON；PowerShell 建议用单引号包裹 | 空 |
| `--mail-watch-seconds` | `mail-send-and-notify` 等待通知秒数 | `15` |
| `--announce-id` | 公告 ID（`announce-get` / `announce-update` / `announce-delete`），格式为 `ann_<base32>` | 空 |
| `--announce-locale` | 公告语言，如 `default` / `zh-CN` | 空 |
| `--announce-priority` | 公告最小优先级筛选，或创建/更新时的优先级 | 空 |
| `--announce-type` | 公告类型，如 `banner` / `popup` | 空 |
| `--announce-target-group` | 公告目标组，如 `all` / `beta` | 空 |
| `--announce-offset` | 公告列表偏移量 | `0` |
| `--announce-title` | 公告标题 | 空 |
| `--announce-content` | 公告正文 | 空 |
| `--announce-start-time` | 公告开始时间；支持 ISO 字符串或 Unix 时间戳 | 空 |
| `--announce-end-time` | 公告结束时间；支持 ISO 字符串或 Unix 时间戳 | 空 |
| `--announce-duration-seconds` | 创建/更新时间窗口持续秒数；与 `--announce-end-time` 二选一 | 空 |
| `--announce-active-only` | 公告列表是否仅返回激活中的公告；传 `false` 可关闭 | `true` |
| `--announce-admin-token` | 公告写接口 token；默认读取 `ANNOUNCE_ADMIN_TOKEN` | 空 |
| `--item-uid` | 物品UID (背包测试) | - |
| `--equip-slot` | 装备槽位: Weapon/Armor/Helmet/Pants/Shoes/Accessory | - |
| `--use-item-uid` | 使用物品UID | - |
| `--discard-uid` | 丢弃物品UID | - |
| `--discard-count` | 丢弃物品数量 | - |
| `--warehouse-action` | 仓库操作: deposit/withdraw | `deposit` |
| `--deposit-uid` | 存入仓库物品UID | - |
| `--deposit-count` | 存入仓库物品数量 | - |
| `--target-id` | 私聊目标玩家 ID，格式为 `plr_<base32>` | - |
| `--group-id` | 群组 ID，格式为 `grp_<base32>` | - |
| `--group-name` | 群组名称 | - |

### 邮件测试示例

```bash
# 内部系统发信：必须显式提供内部地址和 service token，不能走 Caddy 玩家 HTTPS 地址
node tools/mock-client/src/index.js --scenario mail-send \
  --http-base-url http://127.0.0.1:3000 \
  --mail-base-url http://127.0.0.1:9003 \
  --service-token <MAIL_SERVICE_TOKEN> \
  --login-name test001 --password Passw0rd! \
  --mail-title "欢迎礼包" --mail-content "请查收测试奖励"

# 发带附件邮件
node tools/mock-client/src/index.js --scenario mail-send \
  --http-base-url http://127.0.0.1:3000 \
  --mail-base-url http://127.0.0.1:9003 \
  --service-token <MAIL_SERVICE_TOKEN> \
  --login-name test001 --password Passw0rd! \
  --attachments-json '[{"type":"item","id":1001,"count":1}]'

# 正式玩家读取：从 auth 响应 services.mail 取得 Caddy HTTPS 地址，自动附加当前 game ticket
node tools/mock-client/src/index.js --scenario mail-list \
  --http-base-url https://api.bevy.zergzerg.cn \
  --login-name test001 --password Passw0rd! \
  --mail-status unread --limit 10

# 本地玩家邮件调试：仅 descriptor 缺失或加 --no-service-discovery 时显式使用 9003；仍使用 game ticket
node tools/mock-client/src/index.js --scenario mail-read \
  --http-base-url http://127.0.0.1:3000 \
  --no-service-discovery \
  --mail-base-url http://127.0.0.1:9003 \
  --login-name test001 --password Passw0rd! \
  --mail-id <mail_...>
```

### 公告测试示例

```bash
# 查看当前生效的公告
node tools/mock-client/src/index.js --scenario announce-list \
  --announce-base-url http://127.0.0.1:9004 \
  --announce-active-only true

# 按语言和目标组筛选
node tools/mock-client/src/index.js --scenario announce-list \
  --announce-base-url http://127.0.0.1:9004 \
  --announce-locale zh-CN --announce-target-group all

# 创建一条 1 小时有效的公告
node tools/mock-client/src/index.js --scenario announce-create \
  --announce-base-url http://127.0.0.1:9004 \
  --announce-admin-token dev-only-change-this-announce-admin-token \
  --announce-title "系统公告" \
  --announce-content "今晚 20:00 维护" \
  --announce-type popup \
  --announce-priority 20 \
  --announce-duration-seconds 3600

# 查询单条公告
node tools/mock-client/src/index.js --scenario announce-get \
  --announce-base-url http://127.0.0.1:9004 \
  --announce-id <ann_...>

# 更新公告标题或时间窗口
node tools/mock-client/src/index.js --scenario announce-update \
  --announce-base-url http://127.0.0.1:9004 \
  --announce-admin-token dev-only-change-this-announce-admin-token \
  --announce-id <ann_...> \
  --announce-title "维护时间调整" \
  --announce-end-time 2026-04-17T20:00:00+08:00

# 删除公告
node tools/mock-client/src/index.js --scenario announce-delete \
  --announce-base-url http://127.0.0.1:9004 \
  --announce-admin-token dev-only-change-this-announce-admin-token \
  --announce-id <ann_...>
```

### 双客户端测试

使用 guestId 自动创建两个匿名客户端：

```bash
node tools/mock-client/src/index.js --scenario two-client-room \
  --http-base-url http://127.0.0.1:3000 --room-id test-room
```

Robot Sync 双客户端场景：

```bash
node tools/mock-client/src/index.js --scenario robot-sync-room \
  --http-base-url http://127.0.0.1:3000 \
  --host 127.0.0.1 --port 14000 \
  --room-id robot-sync-room --policy-id robot_sync_room
```

该场景会：

- 登录两个客户端并加入同一 room。
- 显式使用 `policy_id = "robot_sync_room"`，不依赖未知 policy 回退。
- ready 后由房主 start room。
- 发送合法 `PlayerInputReq(action="robot_move")`，payload 字段为 `version`、`seq`、`botTick`、`dirX`、`dirY`、`speed`。
- 等待两端都收到包含两个玩家 `robot_move` 的 `FrameBundlePush`。
- 验证非法 action、非法 JSON、方向越界、速度越界分别返回明确 `PlayerInputRes.errorCode`。
- 如果收到 `MovementSnapshotPush` 会直接失败，因为 `robot_sync_room` 第一版不广播机器人坐标。

本地完整栈建议先用仓库根目录 `scripts/dev-stack.ps1` 启动，默认会包含 `match-service`。如需单独调试 game-server，可使用 `-WithoutMatch`。默认 `--port 14000` 是 `game-proxy` TCP fallback 的常见默认值；如果本机 `apps/game-proxy/.env` 覆盖为 `17002`，按实际端口传参。

### 通过 Proxy 测试

```bash
# 通过 TCP fallback 连接 proxy
node tools/mock-client/src/index.js --scenario get-room-data \
  --http-base-url http://127.0.0.1:3000 \
  --host 127.0.0.1 --port 14000 \
  --login-name test001 --password Passw0rd!
```

### Rollout 演练入口

`tools/rollout/rollout-transfer-cli.js` 负责单个 room 的控制面迁移顺序。`tools/mock-client/src/rollout-transfer-cli.js` 仍保留为兼容入口。完整 old/new/proxy 第一阶段演练应优先使用仓库根目录脚本：

```powershell
powershell -ExecutionPolicy Bypass -File scripts/ops/rollout-three-process-drill.ps1 `
  -RolloutEpoch rollout-20260612-a `
  -RoomId room-empty-001
```

该脚本默认 dry-run，不启动服务、不调用写接口；它会优先通过 registry discovery 解析 auth-http、game-proxy admin 和 game-server admin endpoint。确认 old/new game-server、game-proxy 和 auth-http 已运行并注册后，才传 `-ExecuteSteps`。详细流程见 `docs/后台与运维/三进程灰度演练手册.md`。

故障演练入口使用 `tools/rollout/rollout-fault-drill-cli.js`。`tools/mock-client/src/rollout-fault-drill-cli.js` 仍保留为兼容入口。默认同样是 dry-run，只输出 JSON 计划，不访问服务：

```bash
node tools/rollout/rollout-fault-drill-cli.js
```

当前覆盖 `import-failure`、`route-upsert-failure`、`redirect-no-reconnect` 三类脚本级演练。可用 `--simulate` 运行纯内存 mock 验证，确认预期故障停在 `new_import` / `proxy_route_upsert` / `redirect_no_reconnect`，且不会继续 confirm/upsert/retire 或执行 reconnect：

```bash
node tools/rollout/rollout-fault-drill-cli.js --simulate
```

只有显式 `--execute` 才调用已运行服务的控制面接口；默认目标是 registry 中的 `game-server.admin` / `game-proxy.admin`，固定 `127.0.0.1:7500/7501/7101` 只适合带 `--local-debug-targets` 的本地 manual drill。测试/线上应先通过 registry discovery 解析 endpoint，或传 instance id 让 CLI 解析。该入口不启动服务、不请求停服，不代表真实 old/new/proxy 三进程故障联调或 mybevy 适配已经完成。详细流程见 `docs/后台与运维/灰度故障演练手册.md`。

## 扩展开发

### 添加新消息类型

1. 在 `constants.js` 添加 `MESSAGE_TYPE` 枚举
2. 在 `messages.js` 添加编码函数：

```javascript
export function encodeMyMessageReq(field1, field2) {
  return Buffer.concat([
    encodeStringField(1, field1),
    encodeInt32Field(2, field2)
  ]);
}
```

3. 在 `decodeByMessageType()` 添加解码逻辑：

```javascript
case MESSAGE_TYPE.MY_MESSAGE_RES:
  return {
    ok: readBool(fields, 1),
    data: readString(fields, 2)
  };
```

### 添加新测试场景

1. 在 `constants.js` 的 `SCENARIO` 添加枚举
2. 在 `scenarios/` 目录创建场景文件或添加到现有文件
3. 在 `scenarios/index.js` 导出
4. 在 `src/index.js` 的 switch 语句中添加处理逻辑

## 依赖

- Node.js 18+ (ES Module 支持)
- TCP 网络连接
- HTTP 认证服务 (`auth-http`)
- HTTP 邮件服务 (`mail-service`, 邮件场景需要)
- HTTP 公告服务 (`announce-service`, 公告场景需要)
- 游戏服务器 (game-server)
- 聊天服务器 (`chat-server`, 聊天与邮件通知场景需要)
