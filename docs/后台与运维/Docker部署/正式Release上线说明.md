# 正式 Release 上线说明

日志边界：当前发布流程以 Docker `local` driver 提供的有限日志窗口为准，排障入口是 `docker logs`。普通运行日志采集器属于 v2 目标态，完成后应按[服务端日志采集与留存设计](../../安全与监控/服务端日志采集与留存设计.md)作为独立运维组件纳入发布准入和回滚保留范围；在采集器落地前，不把 `/data/myserver/log` 当作已可用的发布产物或日志事实源。

## 1. 适用范围

本文是 MyServer 单机 Docker 正式上线的执行手册，适用于 schema v2 `images.lock.json` 的 release。它覆盖 `Rust`、`Node`、PostgreSQL、Redis、NATS、Caddy 和独立的 `migration-runner`，不适用于名称含 `-docker-test-` 的验证镜像。

生产服务器只拉取镜像和 bundle，不执行 `git clone`、`git pull`、`docker build`、`npm install`、`cargo install` 或 SQLx 安装。它只需要 Docker、Docker Compose、OpenSSL 和发布 bundle 中的脚本。

上线前必须满足：

- `/data` 为至少 `50 GB` 的 SSD 专用数据盘；Docker data-root、独立 containerd root、数据库、镜像缓存和日志不得落入系统根分区，备份应使用异机或对象存储并持续监控剩余空间。
- ACR 只读拉取登录可用；Docker daemon 数据根为 `/data/myserver/docker`，独立 `containerd.service` 的 root 为 `/data/myserver/containerd`。
- DNS 已将 `bevy.zergzerg.cn`、API、后台和聊天公网地址指向该服务器。Caddy 所需 `80/TCP`、`443/TCP` 已在云安全组和主机防火墙受控放行。游戏入口 `4000/UDP` 的放行只在 postflight 通过后执行。
- 已审阅本次 migration；不可逆 migration 已具备数据库文档要求的备份 artifact ID 与 checksum。

## 2. 正式 release 的组成

`images.lock.json` 必须使用 schema version `2`，并同时锁定以下内容：

- 11 个应用镜像：10 个业务/Caddy 镜像和 `migration-runner`。
- 3 个基础设施镜像，均为 `linux/amd64` digest：PostgreSQL 16.6、Redis 7.4.2、NATS 2.10.21。

基础设施镜像使用已实测可访问的 Daocloud Registry 源，但运行时使用完整的 `repository@sha256:digest`，不会依赖可变 tag。

`migration-runner` 是正式 release 的一部分。它以非 root 用户运行，内含 Node、`psql` 和受控 SQLx CLI 0.8.6；服务器只通过它调用 `tools/db-deploy.js`，不会下载依赖或编译工具。

## 3. 构建和交付

在 WSL 原生工作区 `~/src/MyServer` 中，先提交本次发布代码并 push。构建正式镜像的 tag 必须对应构建时的 Git commit，例如：

```bash
cd ~/src/MyServer
release_tag="v0.1.0-<full-or-short-commit>"
./scripts/docker/build-and-push.sh \
  --release-tag "$release_tag" \
  --push
```

推送成功后，脚本会生成 schema v2 `deploy/docker/images.lock.json`。提交该 lock 文件并 push 后，使用实际域名生成服务器 bundle：

```bash
./scripts/docker/create-release-bundle.sh \
  --output "/tmp/myserver-release/$release_tag" \
  --caddy-auth-host api.example.com \
  --caddy-admin-host admin.example.com \
  --caddy-email ops@example.com \
  --game-proxy-advertised-host game.example.com
```

脚本会拒绝 test tag、脏 lock 和缺失的镜像。生成物包含：

默认 bundle 使用 `MYSERVER_RUNTIME_ENV=production`。隔离的测试或预发布服务器可在创建 bundle 时显式传入 `--runtime-env test`，或设置同名环境变量；该参数只允许 `production` 和 `test`，并会传递给全部应用及 `migration-runner` 的 `NODE_ENV`/`APP_ENV`。它不改变 TLS、严格安全、Redis 服务发现或 legacy direct config 禁令。

```text
compose.production.yml
compose.production.env
images.lock.json
infrastructure-images.json
config/
postgres-bootstrap/
db/
apps/game-server/csv/
apps/game-server/scene/
scripts/initialize-production-secrets.sh
scripts/install-ops-scripts.sh
scripts/server-apply-release.sh
scripts/ops/                 # 固定十文件运维脚本集合
RELEASE
SHA256SUMS
```

`apps/game-server/csv/` 和 `apps/game-server/scene/` 是同一 release 的运行期只读资产；二者必须一起交付。将目录传输到服务器 `/data/myserver/release/<release-id>/`。服务器解压后先校验 bundle 完整性：

```bash
cd /data/myserver/release/<release-id>
sha256sum --check SHA256SUMS
```

Compose 会读取 `compose.production.env` 中指定的服务器本地 secret 文件，因此必须在下一节创建 secret 后才能执行 Compose 配置校验和镜像拉取。标准 `upload-release-bundle.sh` 在服务端通过 `SHA256SUMS` 后，使用 bundle 内安装器校验固定十文件白名单，并在共享 operations lock 内事务化切换 `/home/gameops/script` 与 `/data/myserver/apply-release.sh`；不得手工用 glob 或逐文件复制。跨目录 rename 不是单条原子操作，SIGKILL 可能留下短暂 target 缺失或 backup，但 `pending-ops-install` journal 持久记录新旧内容哈希；下一次安装只在路径和 identity 精确匹配时回滚，无法匹配则保留现场并关闭失败。其他写运维操作在 journal 清除前拒绝执行。

## 4. 服务器本地密钥

在 `/data/myserver/secrets/` 创建目录权限为 `0700`、每个 env 文件为 `0600`。这些文件不进入 Git、release bundle、镜像或命令行历史。首次发布时，使用 bundle 内的脚本一次性生成受控 secret 文件；它拒绝覆盖已有文件且不会在终端回显 secret：

```bash
./scripts/initialize-production-secrets.sh \
  --release-dir "$PWD" \
  --origin-id 1 \
  --admin-ip-allowlist <运营固定公网IP/32>
```

`origin-id` 是该正式初始服不可复用的编号，范围为 `1..1023`；为同一服的所有服务共用。`--admin-ip-allowlist` 是逗号分隔的精确 IP 或 IPv4 CIDR，例如 `203.0.113.8/32,198.51.100.0/24`，它会写入 `admin-api.env` 且是后台生产启动的必填安全边界。脚本会生成初始管理员密码，必须立即纳入获批的 secret manager，不要通过聊天、工单或 shell history 传播。

secret 创建成功后，验证 Compose 完整插值并拉取所有已锁定镜像，仍不启动容器：

```bash
docker compose --env-file compose.production.env -f compose.production.yml config --quiet
docker compose --env-file compose.production.env -f compose.production.yml pull
docker compose --profile ops --env-file compose.production.env -f compose.production.yml \
  pull migration-runner
```

除已有服务 env 文件外，首次正式 release 至少需要：

- `infrastructure.env`：`POSTGRES_USER`、`POSTGRES_PASSWORD`、`POSTGRES_DB=myserver_control`、`REDIS_PASSWORD`、`NATS_TOKEN`。
- `migration.env`：五个 `MYSERVER_DB_MIGRATION_<DATABASE>_URL`。URL 指向 Compose 内部 `postgres:5432` 的 `myserver_auth`、`myserver_game`、`myserver_chat`、`myserver_announce`、`myserver_mail`，并使用受控数据库凭据；不可逆 migration 额外提供每库的 backup ID 与 checksum。
- 各业务服务的运行密钥：数据库 URL、Redis/NATS 凭据、ticket/JWT/admin token、签名私钥或公钥、允许的管理来源等。

不要以 `db/init.sql` 初始化生产数据库。Compose 首次创建 PostgreSQL volume 时只执行 `postgres-bootstrap/01-create-databases.sql` 建立五个空库；schema 与 `_sqlx_migrations` 只能由 migration runner 在同一流程写入。

## 5. 首次启动顺序

以下命令都在 bundle 根目录执行。`up -d postgres redis nats` 只属于服务器首次初始化：它创建基础设施容器和 volume，不得复制到常规应用更新或 rollback 流程。初始化完成后，基础设施版本升级必须使用独立、经审批且带备份/恢复方案的运维流程。

```bash
cd /data/myserver/release/<release-id>

docker compose --env-file compose.production.env -f compose.production.yml \
  up -d postgres redis nats
docker compose --env-file compose.production.env -f compose.production.yml ps

docker compose --profile ops --env-file compose.production.env -f compose.production.yml \
  run --rm --no-deps migration-runner initialize --environment production \
  --actor <release-operator> \
  --confirm-empty-databases initialize-empty-databases

docker compose --env-file compose.production.env -f compose.production.yml \
  up -d game-server match-service chat-server mail-service announce-service \
  metrics-collector game-proxy auth-http admin-api

source ./scripts/readiness-convergence.sh
release_compose_command() {
  docker compose --profile ops --env-file compose.production.env \
    -f compose.production.yml "$@"
}
wait_for_release_readiness

docker compose --profile ops --env-file compose.production.env -f compose.production.yml \
  run --rm --no-deps migration-runner postflight --environment production \
  --check-readiness --require-readiness

docker compose --env-file compose.production.env -f compose.production.yml \
  up -d caddy
```

`game-server`、`game-proxy`、`chat-server` 和 `match-service` 分别在 Docker internal network 的 `7600`、`7601`、`7602`、`7603` 提供健康监听。正式 release runner 和上述首次启动命令都会通过 bundle 内的统一 probe 检查这四个 `/readyz`，同时检查 required Node HTTP 服务的 `/healthz`。`GET /livez` 只证明 runtime 存活，发布与接流量判断必须使用 required readiness 收敛结果。这些端口不映射到宿主机公网。任一 required readiness 失败时，不要启动 Caddy，也不要手工修改 `_sqlx_migrations`、删除未知 registry key 或删除 socket 来绕过失败。

常规应用更新和应用 rollback 不创建、启动、停止或重建 PostgreSQL、Redis、NATS。runner 在 migration 和应用更新前 fail-closed 验证目标 Compose project 对每个基础设施服务恰好存在一个容器，且容器为 running/healthy、运行时 image reference 与目标 schema v2 `images.lock.json` 及解析后的 Compose digest reference 精确一致。容器缺失、重复、停止、不健康或 digest 不匹配时发布立即停止，由独立基础设施流程处理；不得让应用 release 自动修复。

game-server 旧实例的受控退出必须由已安装的 `/home/gameops/script/ops-retire.sh` 协调 Docker desired state。production 默认 Compose project 为 `myserver`，确认串是 `<instance-id>@<full-revision>@myserver`。隔离演练必须显式传 `--project <exact-project>`，该值同时用于 Compose lookup、容器 label 围栏、journal 和确认串，不接受宽松环境变量覆盖。候选实例连续 Ready 后，以旧实例 ID、旧镜像完整 Git revision、project 和组合确认串启动 stop hook；脚本进入 `awaiting_control_plane_shutdown` 后，管理员仍通过 admin-api 完成 drain、break-glass、预检、独立审批和 shutdown 执行。drain 后旧实例按设计发布 unhealthy，普通 healthy discovery、readiness、玩家路由及其他 GM 写操作都继续拒绝它；只有 `service.shutdown` controller 显式启用隔离的 live admin discovery，在 heartbeat/TTL 仍活跃、service/instance identity 精确匹配且 endpoint 声明 `name=admin`、`visibility=admin`、`protocol=tcp` 时尝试停服。endpoint 的 `healthy` 随实例整体 readiness 投影，drain 后允许为 false；控制面仍必须实际连通并通过签名断言认证、收到停机响应，任何连接、认证或响应失败均 fail-closed。该能力不允许 fallback 或 direct endpoint，也不会把旧实例重新纳入业务流量。stop hook 不接收管理员凭据或审批 nonce，只等待同一旧容器安全退出，从而避免 `restart: unless-stopped` 将 self-graceful exit 重新拉起。超时、非零退出、OOM 或目标身份变化会失败并恢复原 policy；异常终止留下的 pending journal 会阻止其他写运维操作，必须使用相同 identity/revision/project 显式 `--recover`。recover 即使面对已经干净退出的旧容器，也会恢复 restart policy、重新启动旧实例并结束该次 retire；之后必须重新发起 retire，不能沿用旧等待周期。这一 helper 不创建 candidate，完整 old/new 创建与切流仍由部署平台灰度编排负责。

共享运维锁的真实并发、同进程重入及 release runner 自动回滚子进程的 FD 继承，必须在 WSL 原生 Linux checkout 中运行 `tests/deploy/ops-lock-linux-fixture.test.mjs` 留证；Windows 下该夹具明确跳过，静态断言不能替代 Linux `flock` 证据。

dependency-aware Rust 服务的 production 窗口固定为：启动收敛 `120s`、Ready 稳定 `10s`、依赖 stale `60s`。release runner 另有 `180s` 有界总等待，并要求所有 required 服务连续成功覆盖 registry heartbeat TTL `30s` 加 Ready 稳定窗口 `10s` 后才允许接流量。超时诊断只输出服务、实例 ID、dependency state 和错误码。常规更新仅在发布命令携带 `--rollback-db-compatible`、已确认上一应用 release 兼容前向迁移后的数据库时，才调用上一 release 的同一 runner 做单次版本回滚；回滚只替换应用版本，不回退 migration，也不使用旧 catalog 重跑 preflight/apply，而由发起回滚的 release migration-runner 对当前数据库和旧应用 readiness 做 postflight。最终仍保留原始发布失败状态。首次部署没有上一 release，超时后必须保持 Caddy 未启动和流量关闭，按诊断人工处置。不得通过容器 restart loop 重新碰运气。

`match-service` 获取全局 ID worker lease 时同样使用 `GLOBAL_ID_WORKER_LEASE_WAIT_TIMEOUT_SECS=120`、`GLOBAL_ID_WORKER_LEASE_RETRY_INITIAL_MS=250`、`GLOBAL_ID_WORKER_LEASE_RETRY_MAX_MS=5000`。lease 暂时被上一实例占用或 Redis 短暂不可用时，进程保持 live/not-ready 并持续退避重试；120 秒窗口到期只记录一次结构化收敛超时，不退出进程。取得 lease 后才初始化发号器并开放 gRPC listener；非法 ID/等待配置、Redis client/auth 配置错误仍立即失败，运行期 ownership 丢失仍触发 fatal shutdown。

窗口变量只在未设置时使用默认值。发布配置若包含非数字、零、溢出值、超过 `600/120/600` 秒上限，或不满足 `stability <= convergence`、`stale > stability`，服务必须启动失败；不得通过删掉错误日志或依赖默认回退继续发布。

`initialize` 只用于首次上线，且会先验证五个逻辑库均不存在 `_sqlx_migrations`、不存在业务表。它随后通过同一个受控 runner 写入 schema 与 SQLx history，并执行不要求服务 readiness 的数据库 postflight。任一库已有 history 或业务表时它都会拒绝；这类环境必须改用常规的 `preflight` 和 `apply`，不得以初始化命令绕过迁移审计。

## 6. 接流量与记录

仅在以下条件都满足后，才开放公网入口：

- `docker compose ps` 中基础设施为 healthy，业务服务无反复重启。
- postflight 的五库 history、drift、关键表均成功，统一 readiness probe 覆盖全部 required 服务并连续通过 TTL 加稳定窗口。
- Redis registry heartbeat 已跨过一个完整 TTL 观察窗口，且没有 legacy direct fallback。
- 实际运行镜像 digest 与 `images.lock.json` 完全一致。
- 新域名证书已由 Caddy 成功获取，且 Rust 静态页、API、后台、聊天 WSS 和游戏 KCP 入口的端到端检查通过。

发布记录至少保存 release ID、Git revision、`images.lock.json`、bundle `SHA256SUMS`、迁移 JSON 输出、备份证据标识、实际开放端口和发布操作者。数据库 migration 的回滚边界以[数据库部署准入说明](../../数据库/数据库部署准入说明.md)为准；不可逆或 contract migration 不允许自动回滚 schema。
