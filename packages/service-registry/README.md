# 服务注册中心配置说明

## game-server 配置

启用服务注册后，game-server 启动时会自动注册到 Redis。

```bash
# 启用服务注册
REGISTRY_ENABLED=true

# Redis 连接地址
REGISTRY_URL=redis://127.0.0.1:6379

# 心跳间隔（秒）
REGISTRY_HEARTBEAT_INTERVAL=10

# 服务标识
SERVICE_NAME=game-server
SERVICE_INSTANCE_ID=game-server-001
```

**多实例配置示例：**

```bash
# 实例 1
SERVICE_INSTANCE_ID=game-server-001
GAME_PORT=7000
GAME_LOCAL_SOCKET_NAME=myserver-game-server-001.sock

# 实例 2
SERVICE_INSTANCE_ID=game-server-002
GAME_PORT=7010
GAME_LOCAL_SOCKET_NAME=myserver-game-server-002.sock
```

## game-proxy 配置

启用服务发现后，game-proxy 会从 Redis 动态发现 game-server。

```bash
# 启用服务注册
REGISTRY_ENABLED=true

# Redis 连接地址
REGISTRY_URL=redis://127.0.0.1:6379

# 发现间隔（秒）
REGISTRY_DISCOVER_INTERVAL_SECS=5

# 上游服务名称
UPSTREAM_SERVICE_NAME=game-server
```

## 数据结构

Redis 中的数据结构：

```
# 服务实例注册
service:game-server:instances:game-server-001 = {
    "id": "game-server-001",
    "name": "game-server",
    "host": "127.0.0.1",
    "port": 7000,
    "admin_port": 7001,
    "local_socket": "myserver-game-server-001.sock",
    "tags": ["game", "tcp"],
    "weight": 100,
    "registered_at": 1712736000000,
    "healthy": true
}

# 心跳 Key（TTL 30秒）
heartbeat:game-server:game-server-001 = 1
```

## 验证

查看 Redis 中的注册信息：

```bash
redis-cli
> KEYS service:*
> HGETALL service:game-server:instances:game-server-001
```
