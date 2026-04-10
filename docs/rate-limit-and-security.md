# 限流与风控设计

## 概述

各服务分层防护：
- `game-proxy` → 入口层（IP/连接）
- `auth-http` → 认证层（账号/登录）
- `game-server` → 游戏层（逻辑/操作）

---

## auth-http（登录服）

### IP 限速

- 策略：滑动窗口，1分钟内最多 60 次请求
- 触发：返回 429 Too Many Requests
- 存储：Redis，key = `ratelimit:ip:{ip}`

### 账号锁定

- 策略：同一账号密码连续错误 5 次，锁定 15 分钟
- 存储：Redis，key = `account:lock:{login_name}`
- 解锁：自动过期或 GM 手动

### ticket 校验

- 一次性 ticket，使用后立即删除
- 有效期：默认 5 分钟
- 不可篡改：HMAC 签名验证

### 验证增强（可选）

- 图片验证码 / 短信 / 行为验证
- 风险评分：异地登录、设备指纹

---

## game-proxy（接入代理）

### IP 限速（KCP 层）

- 策略：令牌桶，100 packets/s per IP
- 触发：丢弃超出数据包
- 存储：内存或 Redis

### 连接数限制

- 单 IP 最大连接数：5
- 单账号最大连接数：1（防止多开）
- 超限：拒绝新建连接

### 黑名单

- 临时封禁：违反规则后自动封禁
- 永久封禁：GM 手动添加
- 存储：Redis，key = `blocklist:ip:{ip}`

---

## game-server（游戏服）

### 消息频率限制

- 策略：滑动窗口，1秒内最多 30 条消息
- 触发：警告后断开连接
- 存储：内存（单进程）或 Redis（分布式）

### 操作冷却

| 操作 | 冷却时间 |
|------|----------|
| 发送消息 | 0.5s |
| 发起交易 | 5s |
| 加入房间 | 3s |
| 离开房间 | 3s |

### 异常检测

- 消息大小超过 1KB：异常
- 协议解析失败率 > 10%：警告
- 定时心跳超时：断开连接

---

## 实现优先级

```
1. auth-http   → IP限速 + 账号锁定（高优）
2. game-proxy  → IP限速 + 连接数限制（中优）
3. game-server → 消息频率限制（高优）
4. 全部服务    → 操作冷却 + 异常检测（中优）
```

---

## 配置项

### auth-http

```env
RATELIMIT_IP_WINDOW=60
RATELIMIT_IP_MAX=60
ACCOUNT_LOCK_MAX=5
ACCOUNT_LOCK_TTL=900
```

### game-proxy

```env
RATELIMIT_IP_RATE=100
RATELIMIT_IP_BURST=20
MAX_CONNECTIONS_PER_IP=5
MAX_CONNECTIONS_PER_ACCOUNT=1
```

### game-server

```env
MSG_RATE_WINDOW=1
MSG_RATE_MAX=30
HEARTBEAT_TIMEOUT=30
```
