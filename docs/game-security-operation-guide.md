# 游戏服务安全分层与敏感操作处理指南

本文用于指导后续开发时如何处理玩家身份、真实金钱、游戏内资产、实时战斗和审计回放等安全问题。

本文不是“当前已经全部实现”的说明，而是开发决策参考。当前实现状态仍以代码、[安全设计](./security-design.md) 和 [限流与安全现状](./rate-limit-and-security.md) 为准。

---

## 1. 核心原则

### 1.1 客户端只提交意图

客户端请求只能表达“我想做什么”，不能表达“结果是什么”。

正确例子：

```text
ItemUseReq(item_uid)
ShopBuyReq(goods_id, count)
MoveInputReq(input, frame_id)
```

错误例子：

```text
AddItemReq(item_id, count)
SetHpReq(hp)
SetPositionReq(x, y)
PurchaseSuccessReq(order_id, paid=true)
```

除调试、GM 或内部服务链路外，普通客户端不应拥有直接改变资产或最终状态的接口。

### 1.2 外部使用凭证，内部使用 playerId

公网入口不能直接信任客户端传来的 `playerId`。

推荐身份链路：

```text
auth-http 校验账号 / session / access token
auth-http 签发 ticket
game-proxy / game-server / chat-server 校验 ticket
服务端从 ticket 或 session 中得到 playerId
后续业务使用连接上下文中的 playerId
```

也就是：

```text
外部身份凭证：access token / ticket / session
内部业务身份：playerId
```

跨服务内部调用可以传 `playerId`，但调用链本身必须可信，例如内网隔离、service token、mTLS 或内部签名。

### 1.3 按风险分层，不同操作不同安全强度

| 操作类型 | 推荐链路 | 安全要求 |
|----------|----------|----------|
| 登录、改密码 | HTTPS `auth-http` | 高，必须防暴力破解和审计 |
| 真实充值、现金购买 | HTTPS `order-service` / `payment-service` | 最高，订单、验签、幂等、流水 |
| 支付回调 | HTTPS + 支付平台验签 | 最高，只信服务端验签后的回调 |
| 发货、资产变更 | `game-server` 或内部发货服务 | 高，必须服务端权威和可审计 |
| 游戏内购买 | 游戏连接到 `game-server` 或专门 `shop-service` | 中高，服务端查配置、扣款、发货 |
| 道具使用、装备、仓库 | 游戏连接到 `game-server` | 中高，服务端校验归属和状态 |
| 移动、战斗输入 | KCP 到 `game-server` | 低延迟，实时基础校验 + 回放审计 |
| 聊天 | `chat-server` | 鉴权、频率限制、内容审计 |
| GM、后台 | HTTPS `admin-api` + 内部控制面 | 高，角色权限、审计、内网边界 |

---

## 2. 当前服务职责建议

### 2.1 `auth-http`

负责：

- 账号登录、游客登录
- access token / session
- game ticket 签发和撤销
- 登录限流、账号锁定、安全审计
- 改密码等账号安全操作

不应负责：

- 游戏内购买结算
- 道具使用
- 背包增删
- 战斗结算
- 支付回调后的直接发货

### 2.2 `game-proxy`

负责：

- KCP / TCP fallback 接入
- `AuthReq(ticket)` 本地校验
- 解析并记录 `playerId`
- 路由到后端 `game-server`
- 基础接入层防刷、连接控制、维护模式

不应负责：

- 玩法结算
- 扣钱、发货、背包修改
- 战斗胜负判断

`game-proxy` 可以识别 `playerId`，但仍应把 `AuthReq` replay 给 `game-server`，由 `game-server` 做最终业务鉴权和状态处理。

### 2.3 `game-server`

负责：

- 玩家连接上下文
- 房间、帧推进、移动、战斗
- 道具使用、装备、仓库、背包
- 游戏内资产变更
- GM 发货、邮件附件最终入账
- 可回放输入和状态审计

原则：

- 所有会改变游戏状态的客户端请求，都应先从连接上下文拿 `playerId`。
- 不能信任请求体中的 `player_id` 作为操作者身份。
- 资产变更必须由服务端配置和服务端状态推导结果。

### 2.4 `mail-service`

当前负责邮件 CRUD 和附件领取入口。

推荐边界：

- `mail-service` 可以做邮件展示、读取状态、领取入口校验。
- 附件真正发放应调用 `game-server admin` 或内部发货服务。
- 领取接口必须补齐玩家鉴权，不能只信请求体里的 `player_id`。
- 领取必须幂等，重复请求不能重复发货。

### 2.5 未来 `order-service` / `payment-service`

真实金钱相关能力建议独立成服务，不放入 `auth-http`。

负责：

- 创建订单
- 查询订单
- 接收支付平台回调
- 验签
- 订单状态机
- 充值流水
- 生成发货单

不负责：

- 直接改玩家背包
- 直接信任客户端支付成功声明

---

## 3. KCP 连接与 ticket 使用

### 3.1 ticket 不需要每包携带

KCP 是会话型传输，推荐只在鉴权阶段发送 ticket：

```text
client -> game-proxy: AuthReq(ticket)
game-proxy -> client: AuthRes(playerId)
game-proxy -> game-server: replay AuthReq(ticket)
game-server -> game-proxy: AuthRes(playerId)
后续请求使用连接上下文中的 playerId
```

不推荐每个游戏包都携带 ticket：

- 增加泄露面
- 增加带宽
- 增加验签和 Redis 查询成本
- 对高频输入包不友好

### 3.2 KCP 不等于安全通道

KCP 解决可靠传输，不提供完整安全认证。

当前 ticket 能防止：

```text
攻击者伪造 playerId 直接登录别人账号
```

但 KCP 本身不能彻底防止：

```text
攻击者伪造或注入 UDP 包
攻击者重放已知会话包
攻击者在能抓包的网络环境中构造类似包
```

生产环境如果面对公网，应逐步补齐：

1. UDP 握手 cookie，降低伪造源地址建连和反射攻击风险
2. 会话级 `session_key`
3. 业务包级 `seq` / `nonce`
4. 业务包 HMAC / AEAD tag
5. replay window
6. 单 IP / 单账号连接上限和接入层限流

### 3.3 业务包 HMAC 的使用建议

如果后续为 KCP 业务包加 HMAC，建议在应用层业务消息上做，不要对 KCP 内部分片或 ack 包逐个签名。

推荐格式：

```text
packet_header:
  msg_type
  seq
  body_len
  mac_tag

mac_tag = Truncate(HMAC-SHA256(session_key, msg_type + seq + body), 16)
```

服务端校验：

```text
1. seq 是否在可接受窗口内
2. seq 是否已经处理过
3. mac_tag 是否正确
4. msg_type 是否允许当前连接状态发送
```

性能注意：

- HMAC 会增加 CPU 成本，但中小规模通常可接受。
- 优先保护状态改变请求，例如移动输入、战斗输入、背包操作、商城购买。
- 可以对 HMAC 输出截断为 8 或 16 字节 tag，降低带宽。
- 如果同时需要加密和认证，后续可考虑 AEAD，例如 AES-GCM 或 ChaCha20-Poly1305。

---

## 4. 真实金钱购买

真实金钱链路必须以服务端和支付平台为准，不能相信客户端的“我已经支付成功”。

推荐流程：

```text
client -> order-service: CreateOrder(goods_id)
order-service:
  - 从 access token 得到 playerId
  - 查询商品配置和价格
  - 创建 order_id
  - 返回支付参数

client -> 第三方支付

第三方支付 -> payment-service: webhook(order_id, paid, signature)
payment-service:
  - 验证平台签名
  - 检查订单状态
  - 写支付流水
  - 生成发货单
  - 调用 game-server / mail-service 发货
  - 标记订单已发货
```

必须具备：

- `order_id` 全局唯一
- 支付回调验签
- 订单状态机
- 发货幂等键
- 充值和发货流水
- 后台可查询
- 异常可补单

订单状态建议：

```text
CREATED
PAYING
PAID
DELIVERING
DELIVERED
FAILED
REFUNDED
```

发货幂等键建议：

```text
deliver_key = payment:{order_id}:{player_id}
```

重复回调时应返回成功状态，但不能重复发货。

---

## 5. 游戏内购买

游戏内购买指金币、钻石、代币等游戏内资源购买道具。

推荐由 `game-server` 或专门 `shop-service` 处理，不能由客户端直接加道具。

推荐流程：

```text
client -> game-server: ShopBuyReq(goods_id, count)
game-server:
  - 从连接上下文拿 playerId
  - 查询商品配置
  - 检查商品是否上架
  - 检查等级、区服、限购、库存等条件
  - 检查货币余额
  - 原子扣除货币
  - 发放物品
  - 写资产流水
  - 返回购买结果
```

客户端请求不应包含：

```text
price
discount
final_items
paid=true
```

这些都应由服务端配置和服务端状态计算。

---

## 6. 道具使用、装备与背包

当前 `game-server` 已有背包相关协议和服务处理：

- `ItemEquipReq`
- `ItemUseReq`
- `ItemDiscardReq`
- `WarehouseAccessReq`
- `GetInventoryReq`
- `ItemAddReq`

其中 `ItemUseReq`、`ItemEquipReq` 等普通玩法请求应由 `game-server` 使用连接上下文中的 `playerId` 处理。

道具使用推荐流程：

```text
client -> game-server: ItemUseReq(item_uid)
game-server:
  - ensure_authenticated
  - 从连接上下文获取 playerId
  - 读取玩家背包
  - 检查 item_uid 是否属于玩家
  - 检查道具配置
  - 检查冷却、场景、等级、状态
  - 扣除道具或更新数量
  - 应用效果
  - 保存玩家数据
  - 推送背包变化和属性变化
```

注意：

- `ItemAddReq` 更像调试或 GM 能力，生产环境不应作为普通客户端开放接口。
- 客户端不能直接决定道具效果、属性变化或最终背包状态。
- 高价值资产变化应写资产流水。

---

## 7. 移动、战斗与作弊处理

移动和战斗是高频实时操作，不能使用真实金钱链路那种重型同步校验。

推荐策略：

```text
实时基础校验 + 服务端权威 + 异步审计 / 回放
```

实时校验：

- 移动速度上限
- 瞬移距离阈值
- 地图阻挡 / 可行走区域
- 技能 CD
- 技能距离
- 目标合法性
- 帧号过早 / 过晚限制
- 客户端时间戳偏移限制
- 输入频率限制

实时输入的收包阶段只应完成鉴权、房间状态、帧号窗口、基础格式和数字合法性校验，并将通过校验的输入写入待处理帧缓存。`RoomLogic::on_player_input` 只能用于统计、审计或非权威临时记录；会改变权威状态的移动、战斗和技能结算必须放在 `RoomLogic::on_tick` 中，基于服务端已经接受的帧输入集合统一执行。

处理方式：

```text
轻微异常：纠正位置或丢弃输入
连续异常：累计 cheat strike
严重异常：踢出房间或断开连接
高风险账号：进入后台审核或风控队列
```

不建议一发现异常就永久封号。应先保留证据并按阈值处理。

---

## 8. 操作回放与审计

为了支持作弊审核和线上问题定位，游戏服应保存足够的回放材料。

推荐记录输入：

```text
player_id
room_id
frame_id
msg_type
input_body_hash
client_time
server_time
position_before
position_after
validation_result
reject_reason
connection_id
client_addr
```

推荐记录快照：

```text
room_id
frame_id
snapshot_hash
snapshot_blob
created_at
```

推荐记录资产流水：

```text
flow_id
player_id
source_type
source_id
item_id
item_uid
count_delta
currency_delta
before_value
after_value
operator_type
operator_id
idempotency_key
created_at
```

典型 `source_type`：

- `shop_buy`
- `mail_claim`
- `gm_grant`
- `quest_reward`
- `drop_reward`
- `item_use`
- `item_discard`
- `payment_deliver`

---

## 9. 新增敏感操作开发检查表

新增任何会改变玩家状态的操作前，先回答这些问题：

1. 这个操作是否涉及真实金钱？
2. 这个操作是否改变资产、货币、背包、角色属性或战斗结果？
3. 客户端提交的是意图，还是结果？
4. 操作者 `playerId` 是否来自已鉴权上下文？
5. 是否会信任请求体中的 `player_id`？
6. 是否需要幂等键？
7. 是否需要资产流水？
8. 是否需要后台审计？
9. 是否需要频率限制或冷却？
10. 是否需要 replay window / seq 防重放？
11. 失败后能否安全重试？
12. 重复请求是否会重复扣款或重复发货？

最低要求：

- 真实金钱：必须 HTTPS、订单、验签、幂等、流水。
- 游戏资产：必须服务端权威、校验归属、写关键流水。
- 实时输入：必须基础校验、异常记录、可回放。
- 内部调用：必须有 service token、内网隔离或更强机制。

---

## 10. 建议落地顺序

### P0：当前最应先补

1. `mail-service` / `announce-service` 补齐玩家鉴权和后台权限边界。
2. `admin-api` 后端角色授权真正生效。
3. `/api/admin/monitoring/*` 挂鉴权或限制内网访问。
4. 普通客户端禁用或收敛 `ItemAddReq` 这类直接加物品能力。
5. 邮件附件领取增加幂等键和资产流水。

### P1：接入层和实时安全

1. `game-proxy` 增加单 IP / 单账号连接上限。
2. KCP pre-auth 增加握手 cookie。
3. 状态改变业务包增加 `seq` 和 replay window。
4. 关键游戏业务包增加 HMAC tag。
5. 移动 / 战斗异常输入写入可查询审计。

### P2：真实金钱与资产闭环

1. 新增 `order-service` / `payment-service`。
2. 建立订单状态机和支付流水。
3. 支付回调验签。
4. 发货幂等和补单工具。
5. 统一资产流水表。

---

## 11. 一句话总结

```text
真实金钱链路重安全、强审计、可幂等；
游戏实时链路轻验证、服务端权威、可回放；
资产变化统一由可信服务端逻辑落地，客户端只提交意图。
```
