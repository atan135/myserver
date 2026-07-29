# 正式 Release 上线说明

## 1. 适用范围

本文是 MyServer 单机 Docker 正式上线的执行手册，适用于 schema v2 `images.lock.json` 的 release。它覆盖 `Rust`、`Node`、PostgreSQL、Redis、NATS、Caddy 和独立的 `migration-runner`，不适用于名称含 `-docker-test-` 的验证镜像。

生产服务器只拉取镜像和 bundle，不执行 `git clone`、`git pull`、`docker build`、`npm install`、`cargo install` 或 SQLx 安装。它只需要 Docker、Docker Compose、OpenSSL 和发布 bundle 中的脚本。

上线前必须满足：

- `/data` 已扩容至至少 `80 GB` SSD；当前 `60 GB` 数据盘不足以同时容纳 Docker data-root、数据库、镜像缓存、日志和可恢复备份。
- ACR 只读拉取登录可用，Docker daemon 数据根为 `/data/myserver/docker`。
- DNS 已将 API、后台和游戏公网地址指向该服务器；Caddy 所需 `80/TCP`、`443/TCP` 已在云安全组和主机防火墙受控放行。游戏入口 `4000/UDP` 的放行只在 postflight 通过后执行。
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

```text
compose.production.yml
compose.production.env
images.lock.json
infrastructure-images.json
config/
postgres-bootstrap/
db/
apps/game-server/csv/
scripts/initialize-production-secrets.sh
RELEASE
SHA256SUMS
```

将目录传输到服务器 `/data/myserver/release/<release-id>/`。服务器解压后先校验：

```bash
cd /data/myserver/release/<release-id>
sha256sum --check SHA256SUMS
docker compose --env-file compose.production.env -f compose.production.yml config --quiet
docker compose --env-file compose.production.env -f compose.production.yml pull
docker compose --profile ops --env-file compose.production.env -f compose.production.yml \
  pull migration-runner
```

## 4. 服务器本地密钥

在 `/data/myserver/secrets/` 创建目录权限为 `0700`、每个 env 文件为 `0600`。这些文件不进入 Git、release bundle、镜像或命令行历史。首次发布时，使用 bundle 内的脚本一次性生成受控 secret 文件；它拒绝覆盖已有文件且不会在终端回显 secret：

```bash
./scripts/initialize-production-secrets.sh \
  --release-dir "$PWD" \
  --origin-id 1
```

`origin-id` 是该正式初始服不可复用的编号，范围为 `1..1023`；为同一服的所有服务共用。脚本会生成 `admin-api.env` 中的初始管理员密码，必须立即纳入获批的 secret manager，不要通过聊天、工单或 shell history 传播。

除已有服务 env 文件外，首次正式 release 至少需要：

- `infrastructure.env`：`POSTGRES_USER`、`POSTGRES_PASSWORD`、`POSTGRES_DB=myserver_control`、`REDIS_PASSWORD`、`NATS_TOKEN`。
- `migration.env`：五个 `MYSERVER_DB_MIGRATION_<DATABASE>_URL`。URL 指向 Compose 内部 `postgres:5432` 的 `myserver_auth`、`myserver_game`、`myserver_chat`、`myserver_announce`、`myserver_mail`，并使用受控数据库凭据；不可逆 migration 额外提供每库的 backup ID 与 checksum。
- 各业务服务的运行密钥：数据库 URL、Redis/NATS 凭据、ticket/JWT/admin token、签名私钥或公钥、允许的管理来源等。

不要以 `db/init.sql` 初始化生产数据库。Compose 首次创建 PostgreSQL volume 时只执行 `postgres-bootstrap/01-create-databases.sql` 建立五个空库；schema 与 `_sqlx_migrations` 只能由 migration runner 在同一流程写入。

## 5. 首次启动顺序

以下命令都在 bundle 根目录执行：

```bash
cd /data/myserver/release/<release-id>

docker compose --env-file compose.production.env -f compose.production.yml \
  up -d postgres redis nats
docker compose --env-file compose.production.env -f compose.production.yml ps

docker compose --profile ops --env-file compose.production.env -f compose.production.yml \
  run --rm migration-runner preflight --environment production
docker compose --profile ops --env-file compose.production.env -f compose.production.yml \
  run --rm migration-runner apply --environment production --actor <release-operator>

docker compose --env-file compose.production.env -f compose.production.yml \
  up -d game-server match-service chat-server mail-service announce-service metrics-collector
docker compose --env-file compose.production.env -f compose.production.yml \
  up -d game-proxy auth-http admin-api

docker compose --profile ops --env-file compose.production.env -f compose.production.yml \
  run --rm migration-runner postflight --environment production \
  --check-readiness --require-readiness

docker compose --env-file compose.production.env -f compose.production.yml \
  up -d caddy
```

`game-server`、`game-proxy` 和 `chat-server` 分别在 Docker internal network 的 `7600`、`7601`、`7602` 提供 `GET /readyz`。它们没有宿主机端口映射，只用于 postflight；Node HTTP 服务使用自身 `/healthz`。postflight 失败时，不要启动 Caddy 或开放 `4000/UDP`、`80/TCP`、`443/TCP`，也不要手工修改 `_sqlx_migrations`。

## 6. 接流量与记录

仅在以下条件都满足后，才开放公网入口：

- `docker compose ps` 中基础设施为 healthy，业务服务无反复重启。
- postflight 的五库 history、drift、关键表和所有 required readiness 均成功。
- Redis registry 中的实例 endpoint 与 heartbeat 正确，且没有 legacy direct fallback。
- 实际运行镜像 digest 与 `images.lock.json` 完全一致。
- 域名证书已由 Caddy 成功获取，且 API、后台和游戏入口的端到端检查通过。

发布记录至少保存 release ID、Git revision、`images.lock.json`、bundle `SHA256SUMS`、迁移 JSON 输出、备份证据标识、实际开放端口和发布操作者。数据库 migration 的回滚边界以[数据库部署准入说明](../../数据库/数据库部署准入说明.md)为准；不可逆或 contract migration 不允许自动回滚 schema。
