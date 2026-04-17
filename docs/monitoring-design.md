# 服务健康检查与监控实现说明

## 1. 概述

当前仓库中的监控系统已经完成基础落地，目标是为管理后台提供：

- 服务在线 / 离线状态
- 最近 5 秒桶聚合的 QPS 与平均延迟
- 服务专属在线类指标
- 历史窗口图表查询
- 超期 metrics 数据归档

这套能力由以下模块共同组成：

- 各服务内部 metrics 模块，周期性写入 Redis
- `admin-api` 监控接口，读取 Redis，并提供手动归档入口
- `admin-web` 监控页面，展示总览卡片与详情图表

当前实现已经落地，不再是纯设计稿。

## 2. 当前实现架构

### 2.1 数据流

```text
服务运行时采集指标
  -> 每 5 秒写入 Redis metrics / metrics heartbeat
  -> admin-api 读取 Redis 生成监控接口响应
  -> admin-web 通过监控页面轮询展示
```

### 2.2 代码落点

服务端 metrics 模块：

- `apps/auth-http/src/metrics.js`
- `apps/announce-service/src/metrics.js`
- `apps/mail-service/src/metrics.js`
- `apps/admin-api/src/metrics.js`
- `apps/game-server/src/metrics.rs`
- `apps/chat-server/src/metrics.rs`
- `apps/match-service/src/metrics.rs`
- `apps/game-proxy/src/metrics.rs`

管理接口：

- `apps/admin-api/src/routes/monitoring.js`
- `apps/admin-api/src/services/archive.js`

前端页面与接线：

- `apps/admin-web/src/views/admin/Monitoring.vue`
- `apps/admin-web/src/views/admin/MonitoringDetail.vue`
- `apps/admin-web/src/router/index.js`
- `apps/admin-web/src/api/index.js`

### 2.3 固定服务列表

当前监控接口并不是动态扫描注册中心，而是使用固定服务列表：

- `auth-http`
- `game-server`
- `game-proxy`
- `chat-server`
- `match-service`
- `announce-service`
- `mail-service`
- `admin-api`

## 3. Redis 数据结构

### 3.1 Metrics 数据

```text
Key:    metrics:{service_name}:{timestamp_5s_bucket}
Value:  Redis Hash
TTL:    604800 秒（7 天）
```

5 秒桶时间戳为秒级 Unix 时间戳，按 5 秒对齐。

### 3.2 心跳数据

```text
Key:    metrics:heartbeat:{service_name}
Value:  最近一次上报时间戳字符串
TTL:    30 秒
```

`admin-api` 通过该 key 判断在线状态。

说明：

- 当前监控接口依赖的是 `metrics:heartbeat:{service_name}`
- 不依赖 `service-registry` 的 `services:{service_name}` 注册信息
- `service-registry` 仍是另一套服务发现机制，不是当前监控页面的数据源

### 3.3 Metrics Hash 字段

| 服务 | 当前写入字段 |
|------|--------------|
| `auth-http` | `qps` `latency_ms` `online_sessions` `unique_players` `active_sessions_5m` `active_window_seconds` |
| `game-server` | `qps` `latency_ms` `online_players` `room_count` |
| `game-proxy` | `qps` `latency_ms` `connections` |
| `chat-server` | `qps` `latency_ms` `online_players` |
| `match-service` | `qps` `latency_ms` `pool_size` |
| `announce-service` | `qps` `latency_ms` |
| `mail-service` | `qps` `latency_ms` |
| `admin-api` | `qps` `latency_ms` |

`admin-api` 会把各服务的在线类字段统一映射成 `online_value` 返回给前端：

- `auth-http` -> `unique_players`
- `game-server` -> `online_players`
- `game-proxy` -> `connections`
- `chat-server` -> `online_players`
- `match-service` -> `pool_size`
- `announce-service` / `mail-service` / `admin-api` -> `0`

## 4. 指标来源

### 4.1 Node.js 服务

`auth-http`、`announce-service`、`mail-service`、`admin-api` 当前通过中间件统计：

- QPS：HTTP 请求完成数
- 延迟：请求开始到响应结束的耗时

其中：

- `auth-http` 会额外扫描 session 与 session-activity key，统计 `online_sessions`、`unique_players`、`active_sessions_5m`
- `announce-service`、`mail-service` 与 `admin-api` 当前只上报基础 HTTP 指标

### 4.2 Rust 服务

`game-server`、`chat-server`、`match-service`、`game-proxy` 当前已经接入真实运行时路径：

| 服务 | QPS / 延迟来源 | 在线类指标来源 |
|------|----------------|----------------|
| `game-server` | TCP 消息主分发路径 | 在线玩家数、房间数 |
| `chat-server` | 聊天消息主循环 | 在线聊天会话数 |
| `game-proxy` | 代理会话处理路径 | 连接数 |
| `match-service` | gRPC handler | 匹配池大小 |

## 5. admin-api 监控接口

### 5.1 挂载位置

监控路由定义在：

- `apps/admin-api/src/routes/monitoring.js`

并由 `apps/admin-api/src/routes.js` 通过下面的前缀挂载：

```text
/api/admin/monitoring
```

### 5.2 `GET /api/admin/monitoring/services`

用于监控总览页。

返回示例：

```json
{
  "services": [
    {
      "name": "auth-http",
      "status": "online",
      "qps": 120,
      "latency_ms": 5,
      "online_value": 320,
      "online_sessions": 350,
      "unique_players": 320,
      "active_sessions_5m": 128,
      "active_window_seconds": 300,
      "last_heartbeat": 1713000000000
    }
  ]
}
```

当前逻辑：

- 从固定服务列表逐个读取 `metrics:heartbeat:{service_name}`
- 30 秒内有心跳则视为 `online`
- 在线状态下再扫描 `metrics:{service_name}:*` 找到最新桶
- 返回 `qps`、`latency_ms`、`online_value` 以及原始指标字段

### 5.3 `GET /api/admin/monitoring/services/:name/metrics`

用于监控详情页图表。

请求参数：

```text
window=1m|5m|15m|1h
```

返回示例：

```json
{
  "service": "auth-http",
  "window": "5m",
  "points": [
    {
      "timestamp": 1713000000,
      "qps": 118,
      "latency_ms": 5,
      "online_value": 320,
      "online_sessions": 348,
      "unique_players": 320,
      "active_sessions_5m": 126
    }
  ]
}
```

当前逻辑：

- 只接受固定窗口 `1m` / `5m` / `15m` / `1h`
- 只从 Redis 扫描对应时间范围内的 key
- 结果按时间戳升序返回

注意：

- 当前详情接口没有从 MySQL `metrics_archive` 回查历史数据
- 因此归档后的老数据目前不会通过该接口重新读出来

### 5.4 `POST /api/admin/monitoring/archive`

手动触发归档任务。

返回示例：

```json
{
  "ok": true,
  "archived": 120960,
  "duration_ms": 3500
}
```

当前归档逻辑：

- 扫描 8 天前到 7 天前之间的 Redis metrics 数据
- 写入 MySQL `metrics_archive`
- 写入后删除原 Redis key

实现位置：

- `apps/admin-api/src/services/archive.js`

## 6. admin-web 监控页面

### 6.1 实际前端路由

当前前端监控页面路由定义在 `apps/admin-web/src/router/index.js`，真实路径是：

```text
/monitoring
/monitoring/:service
```

不是 `/admin/monitoring`。

说明：

- `/api/admin/monitoring/*` 是后端接口前缀
- `/monitoring*` 才是管理前端页面路由

### 6.2 API 接线

`apps/admin-web/src/api/index.js` 中监控请求使用单独的 axios 实例：

```text
baseURL = /api/admin/monitoring
```

当前监控请求包括：

- `getServices()`
- `getServiceMetrics(name, window)`
- `triggerArchive()`

与普通 `/api/v1/*` 接口不同，这个实例当前没有注入 JWT 头。

### 6.3 总览页

组件位置：

- `apps/admin-web/src/views/admin/Monitoring.vue`

当前行为：

- 进入页面后立即请求 `/api/admin/monitoring/services`
- 每 5 秒轮询一次
- 使用卡片网格展示所有服务
- 离线服务会加红色样式
- 延迟大于 `500ms` 时延迟数值标红
- 点击卡片跳转到 `/monitoring/:service?window=<currentWindow>`

当前页面展示字段：

- 服务名
- 在线 / 离线状态
- QPS
- 延迟
- 在线类指标
- `auth-http` 额外展示 `5 分钟活跃会话`

### 6.4 详情页

组件位置：

- `apps/admin-web/src/views/admin/MonitoringDetail.vue`

当前行为：

- 使用路由参数 `:service`
- 使用查询参数 `window`，默认 `5m`
- 每 5 秒轮询：
  - `/api/admin/monitoring/services`
  - `/api/admin/monitoring/services/:name/metrics`
- 返回按钮跳回 `/monitoring`

当前页面展示内容：

- 当前 QPS 卡片
- 当前延迟卡片
- 当前在线类指标卡片
- `auth-http` 额外展示 `5 分钟活跃会话`
- QPS 折线图
- 延迟折线图

### 6.5 当前图表实现细节

详情页当前使用 ECharts，但实现比原设计稿更简单：

- X 轴是格式化后的时间字符串分类轴，不是 time 轴
- Y 轴最小值固定为 `0`
- 线图启用了 `smooth`
- 启用了简单面积填充 `areaStyle`
- 当前没有显式配置 tooltip、legend、dataZoom、自动滚动窗口

因此如果后续要补强“更完整的监控图表交互”，需要继续迭代代码，而不是只改文档。

## 7. 归档实现现状

### 7.1 MySQL 表

归档表定义在 `db/init.sql`：

```sql
CREATE TABLE IF NOT EXISTS metrics_archive (
  id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
  service_name VARCHAR(64) NOT NULL,
  bucket_time INT UNSIGNED NOT NULL,
  qps INT UNSIGNED NOT NULL DEFAULT 0,
  latency_ms INT UNSIGNED NOT NULL DEFAULT 0,
  online_value INT UNSIGNED NOT NULL DEFAULT 0,
  extra JSON NULL,
  created_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
  INDEX idx_metrics_archive_service_time (service_name, bucket_time)
);
```

### 7.2 当前归档策略

- Redis 保留最近 7 天热数据
- 归档接口负责把更早的 1 天窗口迁移到 MySQL
- 当前仓库中没有内置定时任务自动调用归档接口
- 需要通过外部调度器或人工调用触发

### 7.3 当前查询限制

当前监控查询接口只读 Redis，不读 MySQL。

因此“归档”当前的实际效果是：

- 减少 Redis 中的历史 metrics 占用
- 为后续补 MySQL 历史查询能力预留数据

但它还不是“完整冷热分层查询”。

## 8. 与旧设计稿的主要差异

当前实现与旧版设计文档相比，已经有以下明确差异：

1. 前端路由实际是 `/monitoring` 与 `/monitoring/:service`，不是 `/admin/monitoring/*`。
2. 后端监控接口前缀是 `/api/admin/monitoring`，前后端路径不能混写。
3. 实现文件当前以 `.js` 为主，不是文档里旧写法的 `.ts`。
4. 监控接口使用固定服务列表，不依赖 `service-registry` 里的服务注册信息。
5. 历史 metrics 查询当前只读 Redis，没有实现“超过 7 天自动查 MySQL”。
6. 详情页图表实现比旧设计简化，没有完整 tooltip、自动滚动和 time 轴能力。

## 9. 注意事项

1. 监控接口当前未挂 JWT 鉴权，不应直接暴露到公网。
2. `admin-web` 的监控页面虽然受前端路由守卫保护，但后端监控接口本身仍是匿名可访问的。
3. 归档接口当前只是手动入口，没有内建调度。
4. 如果后续补上 MySQL 历史查询、服务注册联动或接口鉴权，本文需要继续同步。
