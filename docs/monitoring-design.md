# 服务健康检查与监控告警设计方案

## 1. 方案概述

### 1.1 目标

在 admin-web 新增监控界面，实时展示各服务的运行状态、QPS、延迟等核心指标，支持时间窗口切换和数据可视化。

### 1.2 数据流

```
各服务内部计算 metrics → 每 5 秒批量写 Redis → admin-api 读 Redis → admin-web 展示
```

### 1.3 界面设计

- **总览页**：7 个服务卡片，显示存活状态、QPS、延迟，异常标红
- **详情页**：实时折线图，支持 1min / 5min / 15min / 1h 窗口切换，采样间隔 5 秒

---

## 2. Redis 数据结构

### 2.1 Metrics 数据

```
Key:    metrics:{service_name}:{timestamp_5s_bucket}
Value:  JSON
TTL:    604800 秒（7 天）
```

timestamp_5s_bucket = floor(unix_timestamp / 5) * 5

### 2.2 服务心跳

```
Key:    heartbeat:{service_name}
Value:  unix_timestamp（上次心跳时间）
TTL:    30 秒（30 秒内无心跳视为离线）
```

### 2.3 Metrics Value 格式

| 服务 | 字段 |
|------|------|
| auth-http | `{"qps": N, "latency_ms": N, "online_sessions": N}` |
| game-server | `{"qps": N, "latency_ms": N, "online_players": N, "room_count": N}` |
| game-proxy | `{"qps": N, "latency_ms": N, "connections": N}` |
| chat-server | `{"qps": N, "latency_ms": N, "online_players": N}` |
| match-service | `{"qps": N, "latency_ms": N, "pool_size": N}` |
| mail-service | `{"qps": N, "latency_ms": N}` |
| admin-api | `{"qps": N, "latency_ms": N}` |

### 2.4 服务注册信息（复用已有 service-registry）

```
Key:    services:{service_name}
Value:  JSON { host, port, version }
TTL:    60 秒（需要续期）
```

---

## 3. 各服务 Metrics 模块

### 3.1 实现要求

- **延迟计算**：服务端内部计算，不依赖外部探测
- **批量写入**：每 5 秒积累一次 metrics，批量写入 Redis
- **原子操作**：使用 Redis pipeline 批量写入

### 3.2 延迟指标说明

| 服务 | 延迟定义 |
|------|---------|
| auth-http | HTTP 请求处理耗时（ms） |
| game-server | TCP 消息处理耗时 + 心跳响应耗时 |
| chat-server | TCP 消息处理耗时 |
| match-service | gRPC 调用处理耗时 |
| mail-service | HTTP 请求处理耗时 |
| admin-api | HTTP 请求处理耗时 |
| game-proxy | KCP 包转发耗时 |

### 3.3 Node.js 服务（auth-http / mail-service / admin-api）

使用 `setInterval` 每 5 秒执行一次上报：

```typescript
// 伪代码
let metricsBuffer = [];

app.use((req, res, next) => {
  const start = Date.now();
  res.on('finish', () => {
    metricsBuffer.push({
      qps: 1,
      latency_ms: Date.now() - start,
      timestamp: floor(Date.now() / 5000) * 5000
    });
  });
  next();
});

setInterval(async () => {
  if (metricsBuffer.length === 0) return;
  const aggregated = aggregateMetrics(metricsBuffer); // 聚合
  metricsBuffer = [];
  await redis.pipeline()
    .hset(`metrics:${SERVICE_NAME}:${currentBucket}`, aggregated)
    .expire(`metrics:${SERVICE_NAME}:${currentBucket}`, 300)
    .set(`heartbeat:${SERVICE_NAME}`, Date.now(), 'EX', 30)
    .exec();
}, 5000);
```

### 3.4 Rust 服务（game-server / chat-server / match-service / game-proxy）

使用 `tokio::time::interval` 每 5 秒执行一次上报：

```rust
// 伪代码
use tokio::time::{interval, Duration};
use std::sync::atomic::{AtomicU64, Ordering};

let qps_counter = AtomicU64::new(0);
let latency_sum = AtomicU64::new(0);
let latency_count = AtomicU64::new(0);

tokio::spawn(async move {
  let mut ticker = interval(Duration::from_secs(5));
  loop {
    ticker.tick().await;
    let qps = qps_counter.swap(0, Ordering::Relaxed);
    let latency = if latency_count.load(Ordering::Relaxed) > 0 {
      latency_sum.swap(0, Ordering::Relaxed) / latency_count.swap(0, Ordering::Relaxed)
    } else { 0 };

    let bucket = floor_now() / 5000 * 5000;
    let key = format!("metrics:{}:{}", SERVICE_NAME, bucket);

    redis::pipe()
      .hset(&key, "qps", qps)
      .hset(&key, "latency_ms", latency)
      .expire(&key, 300)
      .set::<_>(format!("heartbeat:{}", SERVICE_NAME), now(), "EX", 30)
      .query_async(&mut redis_conn)
      .await;
  }
});
```

---

## 4. admin-api 接口

### 4.1 GET /api/admin/monitoring/services

获取所有服务状态（总览页用）

```json
// Response
{
  "services": [
    {
      "name": "auth-http",
      "status": "online",   // "online" | "offline"
      "qps": 120,
      "latency_ms": 5,
      "online_sessions": 350,
      "last_heartbeat": 1713000000000
    },
    {
      "name": "game-server",
      "status": "online",
      "qps": 450,
      "latency_ms": 2,
      "online_players": 128,
      "room_count": 24,
      "last_heartbeat": 1713000000000
    }
    // ...
  ]
}
```

status 判断逻辑：
- 读取 `heartbeat:{service_name}`，30 秒内无更新视为 offline

### 4.2 GET /api/admin/monitoring/services/:name/metrics

获取指定服务历史 metrics（图表用）

```
Query: window=1m|5m|15m|1h
```

```json
// Response
{
  "service": "auth-http",
  "window": "5m",
  "points": [
    { "timestamp": 1713000000, "qps": 118, "latency_ms": 5, "online_sessions": 348 },
    { "timestamp": 1713000005, "qps": 122, "latency_ms": 4, "online_sessions": 350 },
    // ...
  ]
}
```

window 对应采样点数：
- 1m: 12 个点
- 5m: 60 个点
- 15m: 180 个点
- 1h: 720 个点

---

## 5. admin-web 监控页面

### 5.1 路由

```
/admin/monitoring  (总览页)
/admin/monitoring/:service  (详情页)
```

### 5.2 总览页布局

```
+------------------------------------------------------+
|  服务监控                                    [1m][5m][15m][1h]  |
+------------------------------------------------------+
|  +--------+  +--------+  +--------+  +--------+      |
|  |auth-   |  |game-   |  |game-   |  |chat-   |      |
|  |http    |  |server  |  |proxy   |  |server  |      |
|  |        |  |        |  |        |  |        |      |
|  |QPS:120 |  |QPS:450 |  |QPS:890 |  |QPS:230 |      |
|  |延迟:5ms|  |延迟:2ms|  |延迟:1ms|  |延迟:3ms|      |
|  |在线:350|  |在线:128|  |连接:256|  |在线:89 |      |
|  +--------+  +--------+  +--------+  +--------+      |
|                                                       |
|  +--------+  +--------+  +--------+                   |
|  |match-  |  |mail-   |  |admin-  |                   |
|  |service |  |service |  |api     |                   |
|  |        |  |        |  |        |                   |
|  |QPS:45  |  |QPS:30  |  |QPS:15  |                   |
|  |延迟:8ms|  |延迟:12ms|  |延迟:3ms|                   |
|  |匹配池: |  |        |  |        |                   |
|  +--------+  +--------+  +--------+                   |
+------------------------------------------------------+
```

服务卡片：
- 顶部：服务名称
- 中部：QPS + 延迟（延迟数字标红阈值可配置）
- 底部：服务专属指标（在线人数/连接数/房间数等）
- 离线状态：整个卡片边框变红，QPS/延迟 显示 "--"

### 5.3 详情页布局

```
+------------------------------------------------------+
|  < 返回  auth-http 监控详情              [1m][5m][15m][1h]  |
+------------------------------------------------------+
|  当前 QPS: 120        当前延迟: 5ms        在线: 350   |
+------------------------------------------------------+
|                    QPS 折线图                         |
|  150 |                    ****                        |
|      |               ****        ****                 |
|  100 |          ****                              |
|      |     ****                                        |
|   50 |                                                   |
|      +----------+----------+----------+----------+     |
|            12:00    12:01    12:02    12:03              |
+------------------------------------------------------+
|                    延迟折线图                          |
|   10 |      *                                    |
|      |  *****                                         |
|    5 |                                                   |
|      +----------+----------+----------+----------+     |
|            12:00    12:01    12:02    12:03              |
+------------------------------------------------------+
```

图表要求：
- 使用 ECharts
- 折线图自动滚动最新数据
- 悬停显示具体数值tooltip
- Y 轴自适应，X 轴为时间轴

### 5.4 告警规则

- 服务离线：卡片边框标红 + 状态文字显示 "离线"
- 延迟超标：延迟数值标红（阈值可配置，默认 > 500ms）
- QPS 异常：暂不设置主动告警，仅展示

---

## 6. 实现计划

### 6.1 第一阶段：基础设施

1. 各服务新增 metrics 模块（Rust 服务 + Node.js 服务）
2. 验证 Redis 数据写入正确性
3. admin-api 新增 /api/admin/monitoring 接口

### 6.2 第二阶段：admin-web 页面

1. 总览页开发（服务卡片网格）
2. 详情页开发（实时折线图）
3. 时间窗口切换功能

### 6.3 文件变更

| 文件 | 变更 |
|------|------|
| `apps/auth-http/src/metrics.ts` | 新增 |
| `apps/mail-service/src/metrics.ts` | 新增 |
| `apps/admin-api/src/metrics.ts` | 新增 |
| `apps/game-server/src/metrics.rs` | 新增 |
| `apps/chat-server/src/metrics.rs` | 新增 |
| `apps/match-service/src/metrics.rs` | 新增 |
| `apps/game-proxy/src/metrics.rs` | 新增 |
| `apps/admin-api/src/routes/monitoring.ts` | 新增（含归档逻辑） |
| `apps/admin-api/src/services/archive.ts` | 新增 |
| `db/init.sql` | 新增 metrics_archive 表 |
| `apps/admin-web/src/views/admin/Monitoring.vue` | 新增 |
| `apps/admin-web/src/views/admin/MonitoringDetail.vue` | 新增 |
| `apps/admin-web/src/router/index.ts` | 更新路由 |

---

## 7. 数据归档

### 7.1 策略概述

Redis 中保留最近 7 天热数据，超期数据归档到 MySQL 永久保留。

```
Redis (热数据, 7天TTL)  →  定时任务迁移  →  MySQL (冷数据, 永久保留)
```

### 7.2 MySQL 归档表

```sql
CREATE TABLE metrics_archive (
  id BIGINT AUTO_INCREMENT PRIMARY KEY,
  service_name VARCHAR(64) NOT NULL,
  bucket_time INT NOT NULL,
  qps INT DEFAULT 0,
  latency_ms INT DEFAULT 0,
  online_value INT DEFAULT 0,
  extra JSON,
  created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
  INDEX idx_service_time (service_name, bucket_time)
);
```

### 7.3 归档 API

**POST /api/admin/monitoring/archive**

手动触发归档任务，将 7 天前~8 天前的 Redis 数据迁移到 MySQL。

```json
// Response
{
  "archived": 120960,
  "duration_ms": 3500
}
```

**触发时机**：部署时调用一次，后续可通过外部调度器（如 cron）定期触发。

### 7.4 归档查询逻辑

```
查询时间范围在最近 7 天内  →  读 Redis
查询时间范围超过 7 天      →  读 MySQL
```

---

## 8. 注意事项

1. **Redis 连接复用**：各服务复用已有的 Redis 连接，不新建连接池
2. **指标聚合**：5 秒内的多次请求在服务端聚合后写入，而非逐请求写 Redis
3. **优雅关闭**：服务停止时不再写入 metrics，但保留已有数据（TTL 自然过期）
4. **多实例兼容**：metrics key 包含时间戳，支持多实例部署时数据叠加或取最大
5. **时间一致性**：所有服务使用相同的 5 秒对齐时间戳桶，避免时区问题
6. **归档时机**：选择低峰期执行，避免与正常监控查询竞争 Redis 资源
