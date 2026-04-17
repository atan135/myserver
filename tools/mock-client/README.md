# Mock Client 技术文档

## 概述

Mock Client 是一个用于测试 MyServer 游戏后端框架的测试工具。它模拟游戏客户端，通过 TCP 协议与 `game-server`、`chat-server` 通信，并通过 HTTP 调用 `auth-http` 与 `mail-service`，验证服务器功能的正确性。

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
│   ├── auth.js          # 认证相关函数
│   └── scenarios/       # 场景测试模块
│       ├── index.js     # 场景统一导出
│       ├── room.js      # 房间相关场景
│       ├── chat.js      # 聊天相关场景
│       ├── mail.js      # 邮件相关场景 (HTTP + 通知联调)
│       ├── game.js      # 游戏相关场景
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
- **解码器**: `decodeByMessageType()` - 根据消息类型自动解码响应

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
- `resolveAccountCredentials()` - 解析账号密码
- `formatLoginSummary()` - 格式化登录信息

### scenarios/mail.js
邮件辅助场景：
- 通过 HTTP 调用 `mail-service` 的邮件 CRUD / claim 接口
- 支持带附件的系统邮件发送
- 支持联调 `chat-server` 的 `MAIL_NOTIFY_PUSH`

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
| `create-matched-room-and-join` | 创建匹配房间并让所有玩家加入，验证完整回调 |

### 游戏场景 (game.js)
| 场景 | 说明 |
|------|------|
| `gameplay-roundtrip` | 完整游戏流程：入房→准备→开始→输入→结束 |
| `combat-dual-client` | 双客户端 `combat_demo` 联调：A 施法，B 掉血并验证快照 |
| `movement-demo` | movement_demo 单客户端位移联调 |

### 聊天场景 (chat.js, interactive.js)
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
| `mail-send` | 发送邮件到指定玩家或当前登录玩家 |
| `mail-list` | 获取指定玩家的邮件列表 |
| `mail-get` | 获取邮件详情 |
| `mail-read` | 标记邮件已读 |
| `mail-claim` | 领取邮件附件（重复领取会返回幂等结果） |
| `mail-send-and-notify` | 发邮件并等待聊天服 `MAIL_NOTIFY_PUSH` |

### 移动同步场景 (movement.js, movement-interactive.js)
| 场景 | 说明 |
|------|------|
| `movement-demo` | movement_demo 单客户端位移联调 |
| `movement-sync-validation` | 移动同步验证：MoveDir/MoveStop/FaceTo |
| `movement-dual-client-sync` | 双客户端移动同步验证 |
| `movement-snapshot-throttle` | 快照节流验证（每3帧） |
| `movement-face-to` | FaceTo 转向与 last input wins |
| `movement-interactive` | 交互式双客户端移动同步（键盘控制） |

### 背包系统场景 (inventory.js)
| 场景 | 说明 |
|------|------|
| `inventory-equip` | 装备穿戴到指定槽位 |
| `inventory-use` | 使用背包中的消耗品 |
| `inventory-discard` | 丢弃背包中的物品 |
| `inventory-warehouse` | 仓库存取操作 |
| `inventory-add` | 添加物品到背包（测试用） |
| `inventory-get` | 获取当前背包和仓库状态 |
| `inventory-full` | 完整背包流程测试 |

## 使用方法

### 基础用法

```bash
# 正常流程测试
node tools/mock-client/src/index.js --scenario happy \
  --http-base-url http://127.0.0.1:3000 \
  --login-name test001 --password Passw0rd!

# 房间测试
node tools/mock-client/src/index.js --scenario two-client-room \
  --http-base-url http://127.0.0.1:3000 --room-id test-room

# movement_demo 位移联调
node tools/mock-client/src/index.js --scenario movement-demo \
  --http-base-url http://127.0.0.1:3000 \
  --login-name test001 --password Passw0rd! \
  --room-id room-movement-demo --policy-id movement_demo

# 全员掉线后 TTL 内双重连
node tools/mock-client/src/index.js --scenario reconnect-all-disconnected \
  --http-base-url http://127.0.0.1:3000 \
  --host 127.0.0.1 --port 7000 \
  --room-id room-reconnect-all

# combat_demo 双客户端联调
node tools/mock-client/src/index.js --scenario combat-dual-client \
  --http-base-url http://127.0.0.1:3000 \
  --room-id room-combat-demo --policy-id combat_demo \
  --combat-skill-id 2

# 聊天测试
node tools/mock-client/src/index.js --scenario chat-private \
  --http-base-url http://127.0.0.1:3000 \
  --chat-port 9001 --target-id <目标ID> --content "Hello!"

# 邮件测试
node tools/mock-client/src/index.js --scenario mail-send \
  --http-base-url http://127.0.0.1:3000 \
  --mail-base-url http://127.0.0.1:9003 \
  --login-name test001 --password Passw0rd! \
  --mail-title "系统奖励" --mail-content "请查收附件"

# 邮件通知联调
node tools/mock-client/src/index.js --scenario mail-send-and-notify \
  --http-base-url http://127.0.0.1:3000 \
  --mail-base-url http://127.0.0.1:9003 \
  --host 127.0.0.1 --chat-port 9001 \
  --login-name test001 --password Passw0rd! \
  --mail-title "通知测试" --mail-content "测试聊天服邮件通知"
```

### 命令行参数

| 参数 | 说明 | 默认值 |
|------|------|--------|
| `--scenario` | 测试场景名称 | `happy` |
| `--http-base-url` | 认证服务地址 | `http://127.0.0.1:3000` |
| `--mail-base-url` | 邮件服务地址 | `http://127.0.0.1:9003` |
| `--host` | 游戏服务器地址 | `127.0.0.1` |
| `--port` | 游戏服务器端口 | `7000` |
| `--chat-port` | 聊天服务器端口 | `9001` |
| `--room-id` | 房间ID | `room-default` |
| `--login-name` | 登录用户名 | - |
| `--password` | 登录密码 | - |
| `--login-name-a` | 客户端A登录用户名 | - |
| `--password-a` | 客户端A登录密码 | - |
| `--login-name-b` | 客户端B登录用户名 | - |
| `--password-b` | 客户端B登录密码 | - |
| `--ticket` | 直接指定 ticket | - |
| `--timeout-ms` | 超时毫秒 | `5000` |
| `--policy-id` | 入房时指定房间策略 | 空 |
| `--move-frames` | movement-demo 发包帧列表，逗号分隔 | `1,2,3,4,5` |
| `--combat-skill-id` | `combat-dual-client` 使用的技能 ID，默认 `2`(fireball) | `2` |
| `--content` | 聊天消息内容 | `Hello from mock-client!` |
| `--mail-id` | 邮件 ID（mail-get / mail-read / mail-claim） | 空 |
| `--mail-player-id` | 邮件所属玩家 ID（mail-list / mail-read / mail-claim） | 空 |
| `--mail-to-player-id` | 邮件接收方玩家 ID（mail-send） | 空 |
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
| `--item-uid` | 物品UID (背包测试) | - |
| `--equip-slot` | 装备槽位: Weapon/Armor/Helmet/Pants/Shoes/Accessory | - |
| `--use-item-uid` | 使用物品UID | - |
| `--discard-uid` | 丢弃物品UID | - |
| `--discard-count` | 丢弃物品数量 | - |
| `--warehouse-action` | 仓库操作: deposit/withdraw | `deposit` |
| `--deposit-uid` | 存入仓库物品UID | - |
| `--deposit-count` | 存入仓库物品数量 | - |
| `--target-id` | 私聊目标玩家ID | - |
| `--group-id` | 群组ID | - |
| `--group-name` | 群组名称 | - |

### 邮件测试示例

```bash
# 发给当前登录玩家
node tools/mock-client/src/index.js --scenario mail-send \
  --http-base-url http://127.0.0.1:3000 \
  --mail-base-url http://127.0.0.1:9003 \
  --login-name test001 --password Passw0rd! \
  --mail-title "欢迎礼包" --mail-content "请查收测试奖励"

# 发带附件邮件
node tools/mock-client/src/index.js --scenario mail-send \
  --http-base-url http://127.0.0.1:3000 \
  --mail-base-url http://127.0.0.1:9003 \
  --login-name test001 --password Passw0rd! \
  --attachments-json '[{"type":"item","id":1001,"count":1}]'

# 查看未读邮件
node tools/mock-client/src/index.js --scenario mail-list \
  --http-base-url http://127.0.0.1:3000 \
  --mail-base-url http://127.0.0.1:9003 \
  --login-name test001 --password Passw0rd! \
  --mail-status unread --limit 10

# 标记已读
node tools/mock-client/src/index.js --scenario mail-read \
  --http-base-url http://127.0.0.1:3000 \
  --mail-base-url http://127.0.0.1:9003 \
  --login-name test001 --password Passw0rd! \
  --mail-id <mail_id>
```

### 双客户端测试

使用 guestId 自动创建两个匿名客户端：

```bash
node tools/mock-client/src/index.js --scenario two-client-room \
  --http-base-url http://127.0.0.1:3000 --room-id test-room
```

### 通过 Proxy 测试

```bash
# 通过 TCP fallback 连接 proxy
node tools/mock-client/src/index.js --scenario get-room-data \
  --http-base-url http://127.0.0.1:3000 \
  --host 127.0.0.1 --port 14000 \
  --login-name test001 --password Passw0rd!
```

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
- 游戏服务器 (game-server)
- 聊天服务器 (`chat-server`, 聊天与邮件通知场景需要)
