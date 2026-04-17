# 聊天系统 + 邮件系统 统一架构设计

## 1. 核心定位

聊天系统和邮件系统有高度相似的架构模式：

| 特性 | 聊天 | 邮件 |
|------|------|------|
| 发送者 → 接收者 | ✅ | ✅ |
| 离线存储 | ✅ (离线消息) | ✅ (未读邮件) |
| 持久化存储 | ✅ | ✅ |
| 群组模式 | ✅ (群聊) | ❌ |
| 实时性 | ✅ | ❌ |
| 频率限制 | ✅ | ❌ |

**结论**：两者可以共用一套消息存储层，但在业务逻辑上保持独立。

当前仓库实现状态说明：

- `chat-server` 已作为独立 Rust TCP 服务落地
- `mail-service` 已作为独立 Node.js HTTP 服务落地，并已实现附件领取
- `announce-service` 已作为独立 Node.js HTTP 服务落地，支持公告 CRUD、有效公告查询、Redis 注册与 metrics 上报
- “聊天与邮件共用同一套存储层”目前仍未实现
- 离线聊天当前更接近“历史可查询”，未实现“登录后自动补发离线消息”的完整闭环

## 2. 消息系统架构

```
┌─────────────────────────────────────────────────────────┐
│                      客户端                              │
└─────────────────────────────────────────────────────────┘
                          │
                          │ KCP / HTTP
                          ▼
┌─────────────────────────────────────────────────────────┐
│                     game-proxy                           │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐    │
│  │  聊天接入   │  │  邮件接入   │  │  公告接入   │    │
│  │  (KCP)     │  │  (HTTP)    │  │  (HTTP)    │    │
│  └─────────────┘  └─────────────┘  └─────────────┘    │
└─────────────────────────────────────────────────────────┘
                          │
          ┌───────────────┼───────────────┐
          │               │               │
          ▼               ▼               ▼
┌─────────────────┐ ┌─────────────┐ ┌─────────────┐
│   chat-service  │ │  mail-service│ │announce-svc │
│  (在 game-server │ │  (独立服务)  │ │ (独立服务)  │
│   或独立)       │ │             │ │             │
└─────────────────┘ └─────────────┘ └─────────────┘
          │               │               │
          └───────────────┼───────────────┘
                          ▼
              ┌─────────────────────┐
              │    message-store    │
              │  (共用存储层)        │
              │  - chat_messages    │
              │  - mail_messages    │
              │  - announcements    │
              └─────────────────────┘
```

## 3. 聊天系统设计

### 3.1 支持范围

- **单聊**：用户A ↔ 用户B
- **群聊**：用户群组内共享消息
- **聊天与房间解耦**：聊天不依赖游戏房间

### 3.2 群组管理

群组是独立的，需要：

```rust
pub struct ChatGroup {
    pub group_id: String,
    pub name: String,
    pub owner_id: String,
    pub members: Vec<String>,       // 成员列表
    pub created_at: i64,
}
```

群聊操作：
- `CREATE_GROUP` - 创建群组
- `JOIN_GROUP` - 加入群组
- `LEAVE_GROUP` - 离开群组
- `DISMISS_GROUP` - 解散群组（仅群主）

### 3.3 聊天协议

```proto
// 单聊
message ChatPrivateReq {
  string target_id = 1;    // 接收者用户ID
  string content = 2;
}

message ChatPrivateRes {
  bool ok = 1;
  string error_code = 2;
  string msg_id = 3;
}

// 群聊
message ChatGroupReq {
  string group_id = 1;     // 群组ID
  string content = 2;
}

message ChatGroupRes {
  bool ok = 1;
  string error_code = 2;
  string msg_id = 3;
}

// 消息推送
message ChatPush {
  string msg_id = 1;
  string chat_type = 2;    // private / group
  string sender_id = 3;
  string sender_name = 4;
  string content = 5;
  int64 timestamp = 6;
  string target_id = 7;     // 单聊时为接收者ID
  string group_id = 8;      // 群聊时为群组ID
}

// 群组操作
message GroupCreateReq { string name = 1; }
message GroupJoinReq { string group_id = 1; }
message GroupLeaveReq { string group_id = 1; }
```

### 3.4 消息存储

```sql
-- 消息存储表
CREATE TABLE chat_messages (
    id BIGINT PRIMARY KEY AUTO_INCREMENT,
    msg_id VARCHAR(64) UNIQUE NOT NULL,     -- 消息唯一ID
    chat_type TINYINT NOT NULL,            -- 1=私聊, 2=群聊
    sender_id VARCHAR(64) NOT NULL,
    content TEXT NOT NULL,
    created_at BIGINT NOT NULL,

    -- 索引用于查询
    target_id VARCHAR(64),                  -- 私聊时为对方用户ID
    group_id VARCHAR(64),                   -- 群聊时为群组ID
    INDEX idx_sender (sender_id),
    INDEX idx_target (target_id),
    INDEX idx_group (group_id),
    INDEX idx_created (created_at)
);

-- 群组表
CREATE TABLE chat_groups (
    id BIGINT PRIMARY KEY AUTO_INCREMENT,
    group_id VARCHAR(64) UNIQUE NOT NULL,
    name VARCHAR(128) NOT NULL,
    owner_id VARCHAR(64) NOT NULL,
    created_at BIGINT NOT NULL,
    INDEX idx_owner (owner_id)
);

-- 群组成员表
CREATE TABLE chat_group_members (
    id BIGINT PRIMARY KEY AUTO_INCREMENT,
    group_id VARCHAR(64) NOT NULL,
    player_id VARCHAR(64) NOT NULL,
    joined_at BIGINT NOT NULL,
    UNIQUE KEY uk_group_player (group_id, player_id),
    INDEX idx_player (player_id)
);
```

### 3.5 离线消息

- 用户上线时查询 `created_at > last_login_time` 的消息
- 推送离线消息给用户

## 4. 邮件系统设计

### 4.1 与聊天系统共用部分

邮件系统可以复用聊天系统的：
- 消息存储表结构
- 用户收件箱查询逻辑
- 离线未读推送机制

当前仓库实现说明：
- `mail-service` 已独立落地为 Node.js HTTP 服务
- 新邮件通知通过 Redis Pub/Sub 下发到 `mail:notify:{player_id}`
- 附件领取由 `mail-service` 调用 `game-server admin` 完成真实发奖
- 当前未与 `chat-service` 共用同一套存储表

### 4.2 邮件特有字段

```sql
CREATE TABLE mail_messages (
    id BIGINT PRIMARY KEY AUTO_INCREMENT,
    mail_id VARCHAR(64) UNIQUE NOT NULL,
    sender_type VARCHAR(32) NOT NULL,       -- system / player
    sender_id VARCHAR(64) NOT NULL,         -- 玩家侧看到的发件人ID；系统固定为 "system"
    sender_name VARCHAR(128),               -- 玩家侧看到的发件人名称
    receiver_id VARCHAR(64) NOT NULL,       -- 收件人
    subject VARCHAR(256),                  -- 邮件主题
    content TEXT,                           -- 邮件正文
    attachments JSON,                       -- 附件 (物品/货币奖励)
    created_by_type VARCHAR(32) NOT NULL,   -- system / admin / player / service
    created_by_id VARCHAR(64),              -- 实际触发方，用于审计
    created_by_name VARCHAR(128),           -- 实际触发方展示名
    is_read TINYINT DEFAULT 0,              -- 是否已读
    created_at BIGINT NOT NULL,
    expires_at BIGINT,                      -- 过期时间 (可选)
    INDEX idx_receiver (receiver_id),
    INDEX idx_created (created_at)
);
```

建议约定：
- 面向玩家展示的发件人，固定使用 `sender_type / sender_id / sender_name`
- 面向后台审计的操作者，固定使用 `created_by_type / created_by_id / created_by_name`
- 系统奖励邮件不需要真实可登录账号，系统发件人统一保留值 `sender_id = "system"`

### 4.3 当前实现协议

当前邮件能力对外通过 HTTP 暴露，核心接口如下：

```http
POST /api/v1/mails
GET  /api/v1/mails?player_id=<player_id>&status=<status>&limit=<n>&offset=<n>
GET  /api/v1/mails/:mailId
PUT  /api/v1/mails/:mailId/read
POST /api/v1/mails/:mailId/claim
```

其中附件领取接口的当前行为为：
- 请求体需要带 `player_id`
- 只能领取属于自己的邮件
- 已过期邮件不可领取
- 当前真实发奖只支持 `attachments[].type = "item"`
- `mail-service` 会先调用 `game-server admin` 发奖，成功后才把邮件状态更新为 `claimed`
- 返回体包含 `claimed`、`already_claimed`、`status`、`read_at`、`claimed_at`
- 重复领取是幂等的，不会重复发放道具

当前附件领取链路：

```text
client
  -> POST /api/v1/mails/:mailId/claim
  -> mail-service
  -> game-server admin (GrantItemsReq / GrantItemsRes)
  -> player inventory updated
  -> mail status = claimed
```

### 4.4 邮件类型

- **系统邮件**：由系统/管理员发送，如奖励发放、公告通知
- **玩家邮件**：玩家之间发送（可选功能）

## 5. 公告系统设计

### 5.1 定位

公告系统独立于 `auth-http`，因为 `auth-http` 只负责登录认证。

当前仓库实现说明：

- 已落地独立 `announce-service`
- 当前对外使用 HTTP 接口，而不是独立 TCP / protobuf 公告协议
- 当前支持：
  - `GET /api/v1/announcements`
  - `GET /api/v1/announcements/:announceId`
  - `POST /api/v1/announcements`
  - `PUT /api/v1/announcements/:announceId`
  - `DELETE /api/v1/announcements/:announceId`
- 当前存储支持：
  - `MYSQL_ENABLED=true` 时使用 MySQL 持久化
  - `MYSQL_ENABLED=false` 时回退到内存存储
- 已验证 MySQL 模式下公告在服务重启后仍可查询
- 已通过 `tools/mock-client` 完成 `announce-list/get/create/update/delete` 联调验证

### 5.2 建议放置位置

**方案A**：独立 `announce-service`
- 完全独立，职责单一
- 可以被 `game-proxy` 或直接被客户端 HTTP 调用

**方案B**：放在 `game-proxy` 内
- 与 `game-proxy` 部署在一起
- 减少服务数量

**推荐方案A**，原因：
- 公告更新频率低，但逻辑可能扩展（如定时公告、区域公告）
- 独立服务更容易单独扩缩容
- 职责清晰，不与代理混淆

### 5.3 公告协议

```proto
message AnnouncementListReq {
  string locale = 1;              -- 语言
  int32 priority = 2;             -- 优先级筛选
}

message AnnouncementListRes {
  repeated Announcement announcements = 1;
}

message Announcement {
  string id = 1;
  string title = 2;
  string content = 3;
  int32 priority = 4;
  int64 start_time = 5;
  int64 end_time = 6;
  string type = 7;               -- popup / banner /滚屏
  string target_group = 8;       -- 目标玩家群体
}

message AnnouncementPushReq {     -- 管理员推送
  string title = 1;
  string content = 2;
  int32 priority = 3;
  int64 duration = 4;            -- 持续时间(秒)
  string target_group = 5;
}
```

### 5.4 公告存储

```sql
CREATE TABLE announcements (
    id BIGINT PRIMARY KEY AUTO_INCREMENT,
    announce_id VARCHAR(64) UNIQUE NOT NULL,
    locale VARCHAR(32) NOT NULL DEFAULT 'default',
    title VARCHAR(256) NOT NULL,
    content TEXT NOT NULL,
    priority INT DEFAULT 0,
    announce_type VARCHAR(32) DEFAULT 'banner', -- popup / banner / scroll
    target_group VARCHAR(128) DEFAULT 'all',    -- all / vip / new_user 等
    start_time DATETIME(3) NOT NULL,
    end_time DATETIME(3) NOT NULL,
    created_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    updated_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3) ON UPDATE CURRENT_TIMESTAMP(3),
    INDEX idx_announcements_locale (locale),
    INDEX idx_announcements_priority (priority),
    INDEX idx_time (start_time, end_time)
);
```

## 6. 服务职责划分

| 服务 | 职责 |
|------|------|
| `auth-http` | 登录、认证、token 发放（保持现状） |
| `game-proxy` | KCP 接入、流量代理 |
| `game-server` | 游戏逻辑、房间系统 |
| `chat-service` | 聊天（单聊、群聊）、离线消息 |
| `mail-service` | 邮件系统（可共用 chat-service 的存储层） |
| `announce-service` | 公告系统 |

## 7. 客户端获取公告的流程

```
1. 客户端启动
2. 请求 auth-http 登录
3. 登录成功后，可从返回的 `services.announce` 获取公告服务地址
4. HTTP 请求 announce-service 获取公告列表
5. announce-service 返回当前有效公告
6. 客户端展示公告
```

当前仓库实际情况补充：

- `auth-http` 在 `REGISTRY_ENABLED=true` 时会把 `announce-service` 写入统一的 `services` 对象
- 同时仍保留旧的 `gameProxyHost/gameProxyPort` 字段兼容现有客户端
- 当前公告获取链路是“登录后主动 HTTP 拉取”，不是 `game-proxy` 自动推送

```json
{
  "services": {
    "announce": {
      "host": "127.0.0.1",
      "port": 9004,
      "protocol": "http"
    }
  }
}
```

或者：

```
1. 客户端连接 game-proxy 时
2. game-proxy 自动推送当前有效公告
```

## 8. 待确认问题

请确认以下设计决策：

### 8.1 聊天系统
- [ ] 群聊成员上限是多少？（如 100 人/群）
- [ ] 群聊是否需要 @ 提及功能？
- [ ] 聊天记录保留多长时间？

### 8.2 邮件系统
- [ ] 邮件附件支持哪些类型？（物品、货币、兑换码？）
- [ ] 邮件是否需要支持 HTML 格式？
- [ ] 系统邮件由谁触发？（管理员后台？游戏逻辑？）
- [ ] 邮件过期时间是多久？

### 8.3 公告系统
- [ ] 公告是否需要定时发布功能？
- [ ] 公告是否需要区域/玩家群体筛选？
- [ ] 公告推送方式？（弹窗、横幅、滚动）
- [x] 当前实现已经采用独立 `announce-service`

### 8.4 部署架构
- [ ] chat-service 和 mail-service 是否合并为一个服务？
- [x] 当前实现已经独立部署 `announce-service`
