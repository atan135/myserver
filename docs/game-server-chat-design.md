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

### 4.2 邮件特有字段

```sql
CREATE TABLE mail_messages (
    id BIGINT PRIMARY KEY AUTO_INCREMENT,
    mail_id VARCHAR(64) UNIQUE NOT NULL,
    sender_id VARCHAR(64) NOT NULL,         -- 发件人 (系统为 "SYSTEM")
    sender_name VARCHAR(128),               -- 发件人名称
    receiver_id VARCHAR(64) NOT NULL,       -- 收件人
    subject VARCHAR(256),                  -- 邮件主题
    content TEXT,                           -- 邮件正文
    attachments JSON,                       -- 附件 (物品/货币奖励)
    is_read TINYINT DEFAULT 0,              -- 是否已读
    created_at BIGINT NOT NULL,
    expires_at BIGINT,                      -- 过期时间 (可选)
    INDEX idx_receiver (receiver_id),
    INDEX idx_created (created_at)
);
```

### 4.3 邮件协议

```proto
message MailSendReq {
  string receiver_id = 1;
  string subject = 2;
  string content = 3;
  string attachments_json = 4;   // JSON: [{"type": "item", "id": 1001, "count": 1}]
}

message MailListReq {
  int32 page = 1;
  int32 page_size = 2;
}

message MailListRes {
  repeated MailItem mails = 1;
}

message MailItem {
  string mail_id = 1;
  string sender_name = 2;
  string subject = 3;
  string preview = 4;           -- 内容预览
  bool is_read = 5;
  int64 created_at = 6;
  bool has_attachments = 7;
}

message MailReadReq { string mail_id = 1; }
message MailDeleteReq { string mail_id = 1; }
message MailAttachmentClaimReq { string mail_id = 1; }  -- 领取附件
```

### 4.4 邮件类型

- **系统邮件**：由系统/管理员发送，如奖励发放、公告通知
- **玩家邮件**：玩家之间发送（可选功能）

## 5. 公告系统设计

### 5.1 定位

公告系统独立于 `auth-http`，因为 `auth-http` 只负责登录认证。

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
    title VARCHAR(256) NOT NULL,
    content TEXT NOT NULL,
    priority INT DEFAULT 0,
    type VARCHAR(32) DEFAULT 'banner',   -- popup / banner / scroll
    target_group VARCHAR(128),             -- all / vip / new_user 等
    start_time BIGINT NOT NULL,
    end_time BIGINT NOT NULL,
    created_at BIGINT NOT NULL,
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
3. 登录成功后，HTTP 请求 announce-service 获取公告列表
4. announce-service 返回当前有效公告
5. 客户端展示公告
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
- [ ] 确认使用独立 `announce-service` 还是放在其他服务内？

### 8.4 部署架构
- [ ] chat-service 和 mail-service 是否合并为一个服务？
- [ ] announce-service 确认独立部署还是集成到其他服务？
