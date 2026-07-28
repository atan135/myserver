# 服务器 Docker 初始化与更新

## 1. 定位和适用范围

本文定义 MyServer 在单台 Linux `x86_64`、`4C8G`、SSD 云服务器上的 Docker 初始化、首次发布、镜像更新、下线和回滚操作。命令以 Ubuntu 22.04/24.04 为例；其他 Linux 发行版应采用等价的包管理、服务管理和防火墙操作，不得直接复制执行。

该方案是单机可控部署，不提供 PostgreSQL、Redis、NATS、宿主机或可用区故障的高可用能力。服务发现、room route 和 local socket 均只在该主机内闭环。多机部署前必须完成 internal TCP 传输演进。

当前仓库尚未提供本文引用的 Docker Compose、迁移容器、镜像构建脚本或 release bundle。首次执行前必须先按[本地 Docker 镜像打包与发布](./本地Docker镜像打包与发布.md)实现并审阅这些资产。

已完成的 Ubuntu 宿主机 Docker 基线和可复用命令见[服务器初始化实操](./服务器初始化实操.md)。该文记录当前初始化结果，不代表业务服务已具备上线条件。

## 2. 服务器基线

### 2.1 必要条件

- 4 vCPU、8 GB RAM、至少 80 GB SSD；数据库、Redis AOF、镜像缓存、日志和备份不与系统根分区争抢不可控空间。
- Linux 内核支持 cgroup v2，系统时间同步正常，DNS 可以解析私有镜像仓库与业务域名。
- Docker Engine、Buildx 和 Docker Compose v2 已安装并由 systemd 托管。生产服务器不安装 Node、Rust、SQLx 或项目构建依赖。
- SSH 仅允许密钥认证并限制管理来源；Docker socket 不对非受信用户暴露。加入 `docker` 组等价于较高宿主机权限，应只授予经审计的运维账号。
- 生产使用独立的镜像仓库只读拉取凭据；该凭据不允许推送、删除镜像或读取无关仓库。

Docker 安装应遵循 Docker 官方的 Ubuntu 安装说明。安装完成后至少确认：

```bash
sudo systemctl enable --now docker
docker version
docker compose version
docker info
```

不要用脚本覆盖已有 `/etc/docker/daemon.json`。若需要配置 Docker 日志轮转，应在保留已有合法 JSON 配置的前提下合并以下字段，并重启 Docker 后确认业务容器不受影响：

```json
{
  "log-driver": "local",
  "log-opts": {
    "max-size": "20m",
    "max-file": "3"
  }
}
```

可设置 `vm.overcommit_memory=1` 以满足 Redis 建议。小型 swap 只能作为防止宿主机立刻失联的最后缓冲，不能用来掩盖内存预算错误；一旦出现持续 swap in/out，应降载或调整配额而非继续运行。

### 2.2 网络边界

防火墙和云安全组默认拒绝入站，仅按下表开放：

| 端口/协议 | 来源 | 用途 |
|---|---|---|
| `22/TCP` | 固定运维来源 | SSH 管理 |
| `80/TCP` | 公网，可选 | HTTPS 跳转或证书校验 |
| `443/TCP` | 公网 | `auth-http` 玩家 HTTPS 入口；运营后台须进一步鉴权/网络隔离 |
| `4000/UDP` | 公网 | `game-proxy` KCP 玩家入口 |
| TCP fallback 端口 | 公网，仅客户端需要时 | `game-proxy` 兼容调试/客户端入口 |

`3000`、`3001`、`7000`、`7500`、`7101`、`9001`、`9002`、`9003`、`9004`、`4222`、`8222`、`5432`、`6379` 均不得对公网开放。它们保留在 Docker internal network；临时排障必须经 SSH 隧道并在结束后撤销。

### 2.3 宿主机目录和权限

建议使用独立数据根目录：

```text
/srv/myserver/
  release/        # 已审阅的 compose、images.lock.json、非私密配置
  secrets/        # 仅服务器本地，0700 目录、0600 文件
  data/
    postgres/
    redis/
    nats/
  backups/
  logs/
  sockets/        # 临时 local socket，不备份、不跨主机复制
```

PostgreSQL、Redis 和 NATS 使用明确的 bind mount 或命名 volume，不能使用匿名 volume。若选择 bind mount，先用已锁定镜像确认容器运行 UID/GID，再仅对对应目录授予最小权限；不要猜测 UID 后执行递归 `chown`。数据目录不放入 Git、不随 release 清理，也不作为镜像构建上下文。

`secrets/` 保存 production env 文件、Compose secret 文件、registry 拉取凭据引用和 TLS 私钥。它们不能同步回开发机、上传到镜像仓库、写入 `images.lock.json` 或通过 `docker inspect` 的环境变量明文暴露。优先使用 Compose secrets 或外部 secret manager；若暂时使用 env file，文件权限必须为 `0600`，且仅由受控运维账号读取。

## 3. 首次发布

### 3.1 准备 release bundle

服务器只接收以下内容：

- 经过校验的 `compose.production.yml`、基础设施配置、反向代理配置和 `images.lock.json`。
- 与 release ID 对应的非敏感环境变量模板。
- 在服务器本地注入的 secret，不含在 release bundle 内。
- 变更说明、数据库迁移说明、备份证据和回滚条件。

先检查 release manifest 的 release ID、Git commit、目标平台和所有 image digest。不得用 tag 名字代替 digest 验证，也不得通过 `git clone` 获得未经发布流程验证的运行文件。

目标 Compose 清单必须具备以下能力：

- 使用 `mem_limit` 和 `cpus` 等 Docker Compose v2 可执行约束；不得只依赖 Swarm 语义的 `deploy.resources`。
- 不设置 `container_name`，为服务 identity 注入稳定且唯一的 `SERVICE_INSTANCE_ID`。
- 基础设施和业务服务分 profile 或明确启动阶段；`depends_on` 只能辅助启动顺序，不能替代 registry readiness。
- 业务服务采用 `restart: unless-stopped` 或等价策略，并设置与正常关闭流程匹配的 `stop_grace_period`。
- 为 `game-proxy`、`game-server`、`match-service` 共享临时 socket volume；其余服务不能读取该 volume。
- 不将中间件和内部服务端口映射到宿主机。

### 3.2 迁移与启动顺序

严格环境固定使用：

```text
REGISTRY_ENABLED=true
DISCOVERY_REQUIRED=true
DISALLOW_LEGACY_DIRECT_CONFIG=true
```

首次发布和每次更新均按以下状态机执行：

1. 拉取并以 digest 校验全部镜像；先启动 PostgreSQL、Redis、NATS，确认其容器健康、数据目录可写且不暴露公网端口。
2. 使用独立的 migration runner 执行受支持的五库入口，而不是直接运行 SQLx：`npm run db:deploy -- preflight --environment production`，审批完成后执行 `npm run db:deploy -- apply --environment production --actor <release-operator>`。
3. 先启动内部服务：`game-server`、`match-service`、`chat-server`、`mail-service`、`announce-service`、`metrics-collector`。每个注册实例都必须在 Redis registry 中具备正确 endpoint 和未过期 heartbeat。
4. 启动入口和控制面：`game-proxy`、`auth-http`、`admin-api`、`admin-web`。`game-proxy` 必须发现 `game-server.proxy-local`；`auth-http` 必须发现 `game-proxy.client`；`admin-api` 必须发现两个 admin endpoint。
5. 运行数据库 postflight，并在已配置的 staged readiness endpoint 上显式启用 `--check-readiness --require-readiness`。随后才将反向代理和游戏入口接入外部流量。

数据库命令的真实输入、备份证据、退出码和失败恢复以[数据库部署准入说明](../../数据库/数据库部署准入说明.md)为准。不可逆 migration 必须先准备每个受影响数据库的备份 artifact ID 和 checksum；失败后不得手工修改 `_sqlx_migrations`、跳过失败库或自动回滚 schema。

当前 `deploy-gate.json` 已为 `game-server`、`game-proxy` 和 `chat-server` 声明 HTTP readiness URL，但这三个服务尚未形成统一的专用 HTTP readiness endpoint。Docker 实现必须先补充受网络隔离保护的 readiness endpoint 或 adapter，才能在生产诚实地使用 `--require-readiness`。在此之前，不得将“容器存活”伪装成数据库 postflight 的服务健康。

### 3.3 接流量前检查

以下条件全部满足后，才允许开放玩家和运营流量：

- 所有容器均使用 `images.lock.json` 中的 digest，且运行镜像与 release ID 一致。
- PostgreSQL、Redis、NATS 可用，且磁盘、内存、日志目录和数据目录均有剩余空间。
- registry 自身可访问；每个应注册服务都有正确的 service name、instance ID、endpoint、visibility 和 heartbeat。
- 所有关键依赖可通过 registry 发现，未使用 `GAME_PROXY_HOST`、`GAME_SERVER_ADMIN_HOST`、`MATCH_SERVICE_ADDR` 等 local fallback。
- `game-proxy` 的 Redis route store 使用生产要求的 backend；玩家入口的 advertised host/port 为客户端可达地址，而内部 endpoint 不泄露到登录响应。
- Node HTTP `/healthz`、后续 Rust readiness endpoint 和数据库 postflight 均通过；管理口仅能从受控网络访问。
- 内存限制、Redis `noeviction`、PostgreSQL 参数、日志轮转和备份任务已实际加载，而非只存在于配置文件。

## 4. 日常镜像更新

### 4.1 更新前

1. 阅读 release change log，确认应用版本、数据库 migration、配置变更、secret 轮换、端口变化和回滚边界。
2. 核对 `images.lock.json` 的 digest、Git commit、平台和镜像签名/SBOM 证据。
3. 对有 migration 的发布先完成备份验证；不可逆 migration 必须满足数据库文档的 backup evidence 和审批要求。
4. 检查宿主机内存、磁盘、PostgreSQL/Redis 健康、registry heartbeat、在线连接和 room drain 状态。存在未处理 OOM、磁盘告警、heartbeat 丢失或迁移中 room 时停止发布。
5. 通过管理控制面开启维护或将入口从接新流量目标移除；不要只依赖 `docker compose stop` 阻止新登录。

### 4.2 更新步骤

```text
验证 release bundle/digest
  -> docker compose pull
  -> 受控 migration preflight/apply
  -> 分批更新内部服务并验证 registry/readiness
  -> 更新 game-proxy、auth-http、admin-api/admin-web
  -> database postflight + 端到端入口检查
  -> 恢复接新流量并持续观察
```

`docker compose pull` 之后仍必须验证实际运行 digest；`docker compose up -d` 不是发布完成信号。更新 `game-server` 时，先经控制面进入 drain，确认连接数、owned room、迁移中 room 等安全闸条件归零，再请求受控 graceful shutdown。不得直接使用 `docker compose down`、`docker kill` 或重启 Docker daemon 终止正在承载玩家的 game-server。

更新 `mail-service`、`chat-server` 和 `game-server` 时，还必须遵守[邮件可靠链路灰度与回滚手册](../邮件可靠链路灰度与回滚手册.md)的消费者先行、兼容开关、outbox/领取工作流观察和回滚条件。

发布后至少观察一个业务高峰窗口或约定观察期，重点检查容器 RSS/OOM、PostgreSQL 慢查询和连接数、Redis 内存与 rejected writes、NATS 断连、registry heartbeat、proxy 路由错误、游戏帧耗时及登录失败率。

### 4.3 回滚

- 仅应用镜像异常且数据库 migration 保持向后兼容时，可按 release manifest 回滚到上一个已验证 digest，并保持当前数据库 schema。
- 已应用不可逆 migration、contract migration、数据回填或新状态机记录时，禁止自动回滚数据库。按 migration `Recovery command`、备份与经审批的恢复演练执行。
- 回滚前保留至少一个能处理现有 mail workflow、game 幂等记录和数据库 schema 的兼容版本；不能因为回滚而删除 outbox、lease、route 或 registry 数据。
- 若 game-server 未排空，先保持维护和入口隔离，按 drain/迁移流程处理；不能以回滚镜像为理由强杀进程。

## 5. 备份、恢复和巡检

- PostgreSQL：至少每日逻辑备份或物理备份，保留异机副本；定期在隔离环境执行恢复演练。备份成功不等于可恢复，必须记录恢复时长和校验结果。
- Redis：根据 AOF/RDB 策略备份持久文件；其恢复不能替代 PostgreSQL 恢复。恢复后须核对 registry、session/ticket 失效策略和 route store 一致性。
- NATS：当前使用 Core NATS，不应把它当作持久业务队列或备份来源。
- 配置与 release bundle：备份受版本控制的非敏感配置和 `images.lock.json`；secret 按 secret manager 的独立恢复流程处理。
- 每日巡检：磁盘、inode、Docker 日志、容器重启次数、OOM、CPU steal、内存/Swap、PostgreSQL 连接和慢查询、Redis 内存、NATS 连接、registry heartbeat、备份最近成功时间。

## 6. 单机限制

- 单机故障会同时影响全部服务与数据层，Docker restart 不能提供高可用。
- 共享 local socket volume 只能在同一台宿主机使用，不能通过复制 volume 实现跨主机扩容或故障转移。
- `docker compose` 的健康检查和 `depends_on` 不替代 service registry 的注册、heartbeat 与依赖发现校验。
- 宿主机内存不足时，优先限制或下线非核心服务、开启维护并保护 PostgreSQL/Redis；不要通过扩大 swap、取消所有内存限制或批量重启容器处理。
