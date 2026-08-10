# 服务器 Docker 初始化与更新

## 1. 定位和适用范围

本文定义 MyServer 在单台 Linux `x86_64`、`4C8G`、SSD 云服务器上的 Docker 初始化、首次发布、镜像更新、下线和回滚操作。命令以 Ubuntu 22.04/24.04 为例；其他 Linux 发行版应采用等价的包管理、服务管理和防火墙操作，不得直接复制执行。

该方案是单机可控部署，不提供 PostgreSQL、Redis、NATS、宿主机或可用区故障的高可用能力。服务发现、room route 和 local socket 均只在该主机内闭环。多机部署前必须完成 internal TCP 传输演进。

仓库已提供生产 Compose、独立 migration runner、镜像构建脚本和 release bundle 生成脚本。服务器只接收经过本地或 CI 发布流程验证的 release bundle 与已推送镜像，不执行 `git clone`、`git pull`、`docker build`、`npm install`、`cargo install` 或 SQLx 安装。实际正式发布以[正式 Release 上线说明](./正式Release上线说明.md)为准；本文保留宿主机基线、更新和回滚边界。

已完成的 Ubuntu 宿主机 Docker 基线和可复用命令见[服务器初始化实操](./服务器初始化实操.md)。该文记录当前初始化结果，不代表业务服务已具备上线条件。

## 2. 服务器基线

### 2.1 必要条件

- 4 vCPU、8 GB RAM、至少 50 GB SSD；数据库、Redis AOF、镜像缓存和日志不与系统根分区争抢不可控空间，恢复备份使用独立磁盘、对象存储或异机存储。
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
| `443/TCP` | 公网 | `auth-http` 玩家 HTTPS 与聊天 WSS 入口；运营后台须进一步鉴权/网络隔离 |
| `4000/UDP` | 公网 | `game-proxy` KCP 玩家入口 |

除只允许固定运维来源的 `22/TCP` 外，公网业务入站只允许 `80/TCP`、`443/TCP` 和 `4000/UDP`。`3000`、`3001`、`7000`、`7500`、`7101`、`9001`、`9011`、`9002`、`9003`、`9004`、所有 TCP fallback、`4222`、`8222`、`5432`、`6379` 均不得对公网开放。它们保留在 Docker internal network；临时排障必须经 SSH 隧道并在结束后撤销。

### 2.3 宿主机目录和权限

建议使用独立数据根目录：

```text
/data/myserver/
  docker/         # Docker daemon data-root，由 daemon 管理
  containerd/     # 独立 containerd 的 root，由 containerd 管理
  release/        # 已审阅的 release bundle，按 release ID 分目录
  secrets/        # 仅服务器本地，0700 目录、0600 文件
  backups/
  logs/
  sockets/        # 临时 local socket，不备份、不跨主机复制
```

当前生产 Compose 使用命名 volume，实际数据位于 Docker `data-root` 下；Docker 使用独立 `containerd.service` 时，镜像 content store 与 overlayfs 快照还会位于 containerd `root`。两者都必须位于 `/data/myserver/`，不能只配置 Docker `data-root` 后默认让 `/var/lib/containerd` 占用系统根分区。不得使用匿名 volume，也不得手工修改 `/data/myserver/docker` 或 `/data/myserver/containerd` 中的受管内容。Caddy 的状态数据保存自动 HTTPS 证书和 ACME 账户，必须跨容器重建保留。若后续改为 bind mount，先用已锁定镜像确认容器运行 UID/GID，再仅对对应目录授予最小权限；不要猜测 UID 后执行递归 `chown`。数据目录不放入 Git、不随 release 清理，也不作为镜像构建上下文。

`secrets/` 保存 production env 文件、Compose secret 文件、registry 拉取凭据引用和 TLS 私钥。它们不能同步回开发机、上传到镜像仓库、写入 `images.lock.json` 或通过 `docker inspect` 的环境变量明文暴露。优先使用 Compose secrets 或外部 secret manager；若暂时使用 env file，文件权限必须为 `0600`，且仅由受控运维账号读取。

若受控运维账号为 `gameops`，`release/` 应使用 `root:gameops`、模式 `2770`，使其可创建按 release ID 分隔的目录并继承组；`secrets/` 应使用 `gameops:gameops`、模式 `0700`。不要以 `root` 生成只有 `gameops` 需要读取的 env 文件。`gameops` 使用 Docker 前需要重新登录以获得受审计的 `docker` 组成员资格。

## 3. 首次发布

### 3.1 准备 release bundle

服务器只接收以下内容：

- 经过校验的 `compose.production.yml`、基础设施配置和 `images.lock.json`。
- 与 release ID 对应的非敏感环境变量模板。
- 在服务器本地注入的 secret，不含在 release bundle 内。
- 变更说明、数据库迁移说明、备份证据和回滚条件。

先检查 release manifest 的 release ID、Git commit、目标平台和所有 image digest。不得用 tag 名字代替 digest 验证，也不得通过 `git clone` 获得未经发布流程验证的运行文件。

### 3.2 服务器交付与拉取验证

release bundle 由开发机或 CI 制作并传到服务器，不以仓库检出目录替代。每个 release 使用独立目录，至少包含：

~~~text
/data/myserver/release/<release-id>/
  compose.production.yml
  compose.production.env       # 已审阅的非敏感 Compose 变量
  images.lock.json
  infrastructure-images.json
  config/
  postgres-bootstrap/          # 只创建五个空库
  db/                          # migration runner 的 SQLx migration 与 drift policy
  apps/game-server/csv/        # 运行期只读 CSV 数据
  apps/game-server/scene/      # 运行期只读场景资产
  scripts/initialize-production-secrets.sh
  RELEASE
  SHA256SUMS
~~~

compose.production.env 由 bundle 生成，必须包含 RELEASE_ID、11 个 IMAGE_* digest reference、已审计的 PostgreSQL/Redis/NATS digest、secret 文件绝对路径、Caddy 域名、GAME_CSV_DIR 和 GAME_SCENE_DIR。应用镜像只能使用 images.lock.json 的 reference 字段，不能改写成 tag。CSV 和场景路径分别为当前 release 的 `/data/myserver/release/<release-id>/apps/game-server/csv` 和 `/data/myserver/release/<release-id>/apps/game-server/scene`，并以只读方式挂载到 `game-server` 的 `/app/csv`、`/app/scene`。

服务器上的 registry 登录使用只读拉取凭据。首次验证先拉取 lock 中的一个完整 digest reference，例如：

~~~bash
docker login --username='<registry-pull-user>' \
  crpi-aag02un1ijrswhes.cn-shenzhen.personal.cr.aliyuncs.com

docker pull \
  crpi-aag02un1ijrswhes.cn-shenzhen.personal.cr.aliyuncs.com/zerg-myserver/game-server@sha256:<digest-from-images-lock>
~~~

将 bundle 解压到 release 目录后，先通过 bundle 脚本创建服务器本地 secret，再进行 Compose 插值和镜像拉取验证，不启动业务容器：

~~~bash
cd /data/myserver/release/<release-id>
./scripts/initialize-production-secrets.sh \
  --release-dir "$PWD" \
  --origin-id <1-1023> \
  --admin-ip-allowlist <运营固定公网IP/32>

docker compose --env-file ./compose.production.env \
  -f ./compose.production.yml config --quiet
docker compose --env-file ./compose.production.env \
  -f ./compose.production.yml pull
~~~

config --quiet 必须先通过；缺少 secret 文件、基础设施 image digest、域名或任一 IMAGE_* 变量时应停止并修正交付物。名称中含 -docker-test- 的 release 仅用于 registry 拉取和 Compose 配置验证，不得导入生产数据、启动全量服务或开放公网流量。


目标 Compose 清单必须具备以下能力：

- 使用 `mem_limit` 等 Docker Compose v2 可执行约束；不得只依赖 Swarm 语义的 `deploy.resources`。
- 不设置 `container_name`，为服务 identity 注入稳定且唯一的 `SERVICE_INSTANCE_ID`。
- 基础设施和业务服务分 profile 或明确启动阶段；`depends_on` 只能辅助启动顺序，不能替代 registry readiness。
- 业务服务采用 `restart: unless-stopped` 或等价策略，并设置与正常关闭流程匹配的 `stop_grace_period`。
- 为 `game-proxy`、`game-server`、`match-service` 共享临时 socket volume；其余服务不能读取该 volume。
- 不将中间件和内部服务端口映射到宿主机。

### 3.3 迁移与启动顺序

严格环境固定使用：

```text
REGISTRY_ENABLED=true
DISCOVERY_REQUIRED=true
DISALLOW_LEGACY_DIRECT_CONFIG=true
```

首次发布和常规应用更新共享 migration/readiness 状态机，但基础设施所有权边界不同：只有首次初始化或独立基础设施变更流程可以创建、启动或重建 PostgreSQL、Redis、NATS；应用 release 和应用 rollback 只能验证并使用既有基础设施。

1. 拉取并以 digest 校验应用镜像。首次初始化先启动 PostgreSQL、Redis、NATS 并确认其数据目录和网络边界；常规更新/rollback 则 fail-closed 验证目标 Compose project 中每项基础设施恰好一个既有容器、running、healthy，且运行 image reference 与目标 schema v2 lock 和 Compose digest 精确一致，不执行任何基础设施 `up`/create/recreate。
2. 使用独立的 migration runner 执行受支持的五库入口，而不是在服务器直接安装 Node 或 SQLx：`docker compose --profile ops ... run --rm --no-deps migration-runner preflight --environment production`，审批完成后使用同一容器执行 `apply`。完整命令见[正式 Release 上线说明](./正式Release上线说明.md)。
3. 使用一次 `docker compose up -d` 批量启动 `game-server`、`match-service`、`chat-server`、`mail-service`、`announce-service`、`metrics-collector`、`game-proxy`、`auth-http`、`admin-api`。应用服务之间不通过 `depends_on` 表达固定启动顺序，只保留 PostgreSQL、Redis、NATS 和 socket 目录初始化等基础门禁。
4. 统一等待 required readiness 连续覆盖 registry heartbeat TTL `30s` 与 Ready 稳定窗口 `10s`。只有全部服务收敛且数据库 postflight 通过后才启动 Caddy；超时按结构化安全字段诊断。常规更新只有在发布前显式确认上一应用 release 兼容本次前向数据库迁移时才调用上一 release 的同一流程做单次回滚；首次发布没有回滚目标，必须保持入口关闭。不得删除未知 registry key 或 socket 继续发布。
5. 运行数据库 postflight，并在已配置的 staged readiness endpoint 上显式启用 `--check-readiness --require-readiness`。随后才将 Caddy 和游戏入口接入外部流量。

数据库命令的真实输入、备份证据、退出码和失败恢复以[数据库部署准入说明](../../数据库/数据库部署准入说明.md)为准。不可逆 migration 必须先准备每个受影响数据库的备份 artifact ID 和 checksum；失败后不得手工修改 `_sqlx_migrations`、跳过失败库或自动回滚 schema。

`game-server`、`game-proxy` 和 `chat-server` 已提供仅 Docker internal network 可访问的 `GET /readyz` endpoint，分别绑定 `7600`、`7601`、`7602`；它们与 Node 服务的 `/healthz` 一同由 migration runner 的 `--require-readiness` 检查。容器存活不能替代这些检查。

### 3.4 接流量前检查

以下条件全部满足后，才允许开放玩家和运营流量：

- 所有容器均使用 `images.lock.json` 中的 digest，且运行镜像与 release ID 一致。
- PostgreSQL、Redis、NATS 可用，且磁盘、内存、日志目录和数据目录均有剩余空间。
- registry 自身可访问；每个应注册服务都有正确的 service name、instance ID、endpoint、visibility 和 heartbeat。
- 所有关键依赖可通过 registry 发现，未使用 `GAME_PROXY_HOST`、`GAME_SERVER_ADMIN_HOST`、`MATCH_SERVICE_ADDR` 等 local fallback。
- `game-proxy` 的 Redis route store 使用生产要求的 backend；玩家入口的 advertised host/port 为客户端可达地址，而内部 endpoint 不泄露到登录响应。
- `chat-server` 必须保持单副本：production Compose 的 `deploy.replicas: 1` 和 `server-apply-release.sh` 的前后副本检查均通过；不得用 `docker compose --scale chat-server` 覆盖，因为当前 release 环境会让所有副本继承 `SERVICE_INSTANCE_ID=chat-server-1`。
- 聊天实例容量门槛已加载：`CHAT_MAX_CONNECTIONS=500`、`CHAT_WS_HANDSHAKE_RATE_MAX=120` / 秒、`CHAT_WS_MAX_PENDING_HANDSHAKES=64`、每连接 `20` 条消息/秒、每连接 `32` 条出站队列和 `256MiB` 内存上限。接流量前确认 `connection_capacity_current`、握手拒绝、出站队列失败和 `chat_push_publish_queue_depth` 没有告警；具体阈值与处置见[聊天与邮件系统设计](../../周边服务/聊天与邮件系统设计.md)。
- Node HTTP `/healthz`、后续 Rust readiness endpoint 和数据库 postflight 均通过；管理口仅能从受控网络访问。
- 内存限制、Redis `noeviction`、PostgreSQL 参数、日志轮转和备份任务已实际加载，而非只存在于配置文件。
- metrics-collector 的 `METRICS_LEGACY_WRITE_ENABLED=false` 已实际加载，启动日志显示 `metrics_legacy_write_enabled=0`；切读完成后不得因遗漏配置继续生成 7 天 TTL 的 legacy bucket。

### 3.5 Redis 持久化与全局 ID lease

本单机部署中，Redis 是 session、ticket、route、registry 和 worker lease 的运行时存储，PostgreSQL 才是业务数据的权威持久化来源。`deploy/docker/config/redis.conf` 必须保留 `appendonly yes` 和 `appendfsync everysec`，并显式设置 `save ""` 禁用默认 RDB snapshot 规则。这样避免高频 RDB fork/save 与 AOF fsync 争抢磁盘 I/O，导致 worker lease 在 TTL 内无法续租。

全局 ID worker lease 使用 90 秒 TTL、每 30 秒续租。启动阶段暂时无法取得 lease 时，`game-server` 和 `match-service` 保持 live/not-ready 并在有界退避下继续恢复，不依赖 Compose restart loop 等待旧 TTL；收敛窗口到期只产生结构化告警。取得 ownership 后，任一续租 Redis 错误或 ownership 丢失仍会令对应服务以非零状态退出，避免继续发号。出现运行期 lease loss 时，先检查 Redis AOF 延迟、磁盘空间和 I/O，再恢复或扩大流量；不要在运行中的旧进程里手工恢复发号。

### 3.6 Redis `maxmemory` 与 legacy metrics 恢复

Redis 保持 `maxmemory-policy noeviction`，因此达到 `maxmemory` 后会拒绝 registry heartbeat、全局 ID worker lease、session/ticket 和 route 等关键写入。若日志出现 `OOM command not allowed when used memory > 'maxmemory'`，先停止发布和接新流量，保存 `INFO memory`、`INFO keyspace`、`INFO stats`、`INFO commandstats`、容器状态、重启计数和相关服务日志；不得用批量重启掩盖故障。

若 legacy metrics 是主要占用，恢复顺序固定为：

1. 确认 admin-api 和归档任务只读 metrics v2，保存当前 v2 snapshot/history 连续性证据。
2. 发布并确认 metrics-collector 启动日志为 `metrics_legacy_write_enabled=0`，观察至少两个 5 秒上报周期，确认 legacy 最新 bucket 不再前移。
3. 按[Registry 监控读模型设计](../../../安全与监控/Registry监控读模型设计.md#82-legacy-清理工具)先执行 dry-run，复核目标、排除分类和数量。
4. 获得明确线上变更授权后，用限速 `UNLINK` 工具释放 legacy Hash，持续观察 Redis CPU、延迟和 `used_memory/maxmemory`；内存降至 75% 以下后停止扩大删除速率。
5. 验证 Redis OOM 错误不再增长，再按依赖顺序恢复 `game-server`、`match-service`、`chat-server` 和 `auth-http`，确认 worker lease、registry heartbeat、readiness/health 与重启计数稳定。

只提高 Docker `mem_limit` 不会改变 Redis `maxmemory`。若确需调整容量，必须同时核对两层限制、memory-swap、4C8G 宿主机总预算和回退值；禁止 `FLUSHDB`、无前缀删除、直接删除 `metrics:v2:*` 或在 legacy producer 仍写入时循环清理。

当前生产校准值为 Redis `mem_limit=768m`、`memswap_limit=768m`，内部 `maxmemory=512mb` 与 `noeviction` 保持不变；多出的 256 MiB 只承载 allocator、AOF 和运行时开销，不能视为可写业务数据容量。`announce-service` 使用 `mem_limit=256m`、`memswap_limit=256m`。这两个服务不允许使用额外 swap；调整依据是 2026-08-03 故障恢复期间 Redis 640 MiB cgroup 上限事件和 announce-service 128 MiB cgroup 上限、约 56 MiB swap 的实测证据。回退前必须先确认 cgroup 峰值、`memory.events`、Redis `used_memory/maxmemory` 和宿主机可用内存均满足旧上限。

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
  -> 单批启动应用服务并验证 registry/readiness
  -> database postflight
  -> 启动 Caddy（含 admin-web 静态文件）
  -> database postflight + 端到端入口检查
  -> 恢复接新流量并持续观察
```

`docker compose pull` 之后仍必须验证实际运行 digest；`docker compose up -d` 不是发布完成信号。更新 `game-server` 时，先经控制面进入 drain，确认连接数、owned room、迁移中 room 等安全闸条件归零，再请求受控 graceful shutdown。不得直接使用 `docker compose down`、`docker kill` 或重启 Docker daemon 终止正在承载玩家的 game-server。

更新 `mail-service`、`chat-server` 和 `game-server` 时，还必须遵守[邮件可靠链路灰度与回滚手册](../邮件可靠链路灰度与回滚手册.md)的消费者先行、兼容开关、outbox/领取工作流观察和回滚条件。

发布后至少观察一个业务高峰窗口或约定观察期，重点检查容器 RSS/OOM、PostgreSQL 慢查询和连接数、Redis 内存与 rejected writes、NATS 断连、registry heartbeat、proxy 路由错误、游戏帧耗时及登录失败率。

### 4.3 回滚

- 仅应用镜像异常且数据库 migration 保持向后兼容时，可按 release manifest 回滚到上一个已验证 digest，并保持当前数据库 schema。
- 应用版本回滚跳过旧 release 的 migration preflight/apply，复用发起回滚的当前 release migration-runner 对现有数据库 history、drift 和旧应用 required readiness 执行 postflight。
- 已应用不可逆 migration、contract migration、数据回填或新状态机记录时，禁止自动回滚数据库。按 migration `Recovery command`、备份与经审批的恢复演练执行。
- 回滚前保留至少一个能处理现有 mail workflow、game 幂等记录和数据库 schema 的兼容版本；不能因为回滚而删除 outbox、lease、route 或 registry 数据。
- 若 game-server 未排空，先保持维护和入口隔离，按 drain/迁移流程处理；不能以回滚镜像为理由强杀进程。

## 5. 备份、恢复和巡检

- PostgreSQL：至少每日逻辑备份或物理备份，保留异机副本；定期在隔离环境执行恢复演练。备份成功不等于可恢复，必须记录恢复时长和校验结果。
- Redis：本部署以 AOF 为恢复来源，RDB snapshot 已禁用；备份 AOF 持久文件。其恢复不能替代 PostgreSQL 恢复。恢复后须核对 registry、session/ticket 失效策略和 route store 一致性。
- NATS：当前使用 Core NATS，不应把它当作持久业务队列或备份来源。
- 配置与 release bundle：备份受版本控制的非敏感配置和 `images.lock.json`；secret 按 secret manager 的独立恢复流程处理。
- 每日巡检：磁盘、inode、Docker 日志、容器重启次数、OOM、CPU steal、内存/Swap、PostgreSQL 连接和慢查询、Redis 内存、NATS 连接、registry heartbeat、备份最近成功时间。

## 6. 单机限制

- 单机故障会同时影响全部服务与数据层，Docker restart 不能提供高可用。
- 共享 local socket volume 只能在同一台宿主机使用，不能通过复制 volume 实现跨主机扩容或故障转移。
- `docker compose` 的健康检查和 `depends_on` 不替代 service registry 的注册、heartbeat 与依赖发现校验。
- 宿主机内存不足时，优先限制或下线非核心服务、开启维护并保护 PostgreSQL/Redis；不要通过扩大 swap、取消所有内存限制或批量重启容器处理。
- 当前聊天 WSS 生产拓扑只支持一副本。将来启用多实例时，必须替换这套 Compose/release apply 门禁为受控编排：为每个副本签发稳定且唯一的 `SERVICE_INSTANCE_ID`，确认 Caddy 的 `dynamic a` 能解析全部 endpoint，保留 Redis owner-fenced route 与实例级 NATS push，并完成多实例登录、断线重连、摘流、旧 route、NATS 失败和容量告警演练。不能以直接 `--scale` 或复制 `compose.production.yml` 作为扩容方式。
