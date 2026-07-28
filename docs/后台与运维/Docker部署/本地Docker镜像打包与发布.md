# 本地 Docker 镜像打包与发布

## 1. 定位

本文定义 MyServer 单机 Docker 发布的本地构建、镜像签名/推送和交付物规范。目标环境是 Linux `x86_64` 单机 `4C8G`，所有业务服务和 PostgreSQL、Redis、Core NATS 都运行在 Docker 中。

本文只定义后续 Docker 化必须满足的契约。当前仓库尚未提供 `Dockerfile`、`docker compose` 清单、镜像构建脚本或镜像仓库配置，因此本文所列 `deploy/docker/` 文件和 `scripts/docker/` 命令在实现前不可直接执行。

相关约束：

- [生产拓扑与 Room 迁移设计](../生产拓扑与Room迁移设计.md)
- [服务注册中心设计](../../周边服务/服务注册中心设计.md)
- [数据库部署准入说明](../../数据库/数据库部署准入说明.md)

## 2. 发布原则

- 镜像只能在开发机或 CI 构建；生产服务器只拉取已发布、可追溯的镜像，不能在服务器执行 `docker build`、`npm install`、`cargo build` 或 `git pull`。
- 每次发布使用不可变版本标签和镜像 digest。禁止以 `latest`、分支名或可覆盖标签作为线上实际版本。
- 每个镜像必须同时记录 Git commit、构建时间、目标平台和依赖锁文件摘要；这些信息只作为可观测 metadata，不得包含密码、token、连接串或 ticket。
- Docker build context 必须是仓库根目录，以便 Node workspace、共享 proto 和 Rust workspace/共享 crate 能被正确纳入构建。不得为图省事复制未经审查的本机 `node_modules`、`target`、`.env`、日志或数据库文件到镜像。
- 生产配置、私密值、PostgreSQL/Redis 数据、日志、CSV 热更新数据和 local socket 都不写入镜像层。配置与数据在运行期以只读挂载、secret 或受控数据目录提供。

## 3. 目标交付物

Docker 化实现完成后，仓库应提供以下受版本控制但不含私密值的发布资产：

```text
deploy/docker/
  Dockerfile.rust
  Dockerfile.node
  Dockerfile.admin-web
  compose.production.yml
  compose.production.env.example
  config/
    postgres.conf
    redis.conf
    nats.conf
  nginx/
    admin-web.conf
  images.lock.json
scripts/docker/
  build-and-push.ps1
  verify-release.ps1
```

`images.lock.json` 是发布交付物的一部分。它至少包含 release ID、Git commit、每个服务的 repository、tag、digest、构建时间和目标平台。服务器部署与更新都以该文件中的 digest 为准。

生产镜像清单如下：

| 类别 | 镜像/服务 |
|---|---|
| Rust | `game-proxy`、`game-server`、`chat-server`、`match-service` |
| Node.js | `auth-http`、`admin-api`、`announce-service`、`mail-service`、`metrics-collector` |
| Web | `admin-web` 静态站点镜像 |
| 基础设施 | 已锁定 digest 的 PostgreSQL、Redis、NATS 和反向代理镜像 |

`admin-web` 只能构建为静态文件并由反向代理镜像提供；线上不运行 Vite 开发服务器。

## 4. 本地构建前置条件

构建机需要 Docker Desktop 或 Linux Docker Engine，且启用 Buildx。构建 Linux 服务器镜像时，目标平台固定为 `linux/amd64`。构建开始前必须满足：

1. 工作树中的本次发布改动已审阅，构建输入与将要记录的 Git commit 一致。
2. 根 `package-lock.json`、各 Rust `Cargo.lock` 和共享 proto 生成结果没有未确认变更。
3. 已执行与改动范围相符的静态检查与单元测试。发布前至少应运行 `npm run check:proto` 和数据库静态准入 `npm run db:deploy -- validate --environment ci`；Rust/Node 定向检查按实际改动服务补充。
4. 本机已通过受控方式登录私有镜像仓库。登录凭据只保留在 Docker credential store 或 CI secret，不写入项目 `.env`、发布清单或控制台日志。

真实 Redis、PostgreSQL、NATS 或多服务联调不属于镜像构建的默认步骤。需要执行时，应先明确依赖、端口、测试数据和清理范围，再按项目协作约定取得确认。

## 5. 镜像构建规范

### 5.1 多阶段和非 root 运行

- Rust 和 Node 镜像均采用多阶段构建；最终镜像仅保留运行时二进制/产物、生产依赖和必要的许可证信息。
- Node 构建阶段使用锁文件的确定性安装，运行阶段设置 `NODE_ENV=production`，不得把开发依赖带入最终镜像。
- Rust 构建阶段固定 Rust toolchain 和 Linux target；最终阶段使用最小运行时镜像，并验证 TLS/CA、时区数据和动态链接库满足实际依赖。
- 业务容器以非 root 用户运行。仅 PostgreSQL、Redis 初始化数据目录等官方镜像要求的受控初始化例外需单独记录。
- 容器进程接收 `SIGTERM` 后必须走已有的正常关闭逻辑；镜像的 `ENTRYPOINT` 不得吞掉或改写该信号。

### 5.2 标签和构建参数

release ID 使用 `v<语义版本>-<短GitSHA>`，例如 `v0.4.0-a1b2c3d`。每个业务镜像都使用同一 release ID，并额外写入 OCI labels：

```text
org.opencontainers.image.revision=<full-git-sha>
org.opencontainers.image.created=<UTC-RFC3339>
org.opencontainers.image.version=<release-id>
org.opencontainers.image.source=<repository-url>
```

构建时不允许通过 `--build-arg` 传递数据库密码、JWT/ticket secret、admin token、NATS 凭据或任何生产域名。构建参数只能表达服务名、release ID、commit 和非敏感构建开关。

### 5.3 推荐构建入口

后续实现统一提供 PowerShell 包装命令，避免手工为十个业务镜像复制 `docker buildx` 参数：

```powershell
$releaseTag = "v0.4.0-a1b2c3d"
powershell -ExecutionPolicy Bypass -File scripts/docker/build-and-push.ps1 `
  -Registry registry.example.com `
  -ReleaseTag $releaseTag `
  -Platform linux/amd64
```

该脚本的职责是构建、生成 SBOM/provenance、推送、获取各镜像 digest、生成 `images.lock.json`，最后运行 `verify-release.ps1`。脚本失败时不得生成可被视为完整发布的 lock 文件。

脚本未实现前，可使用 `docker buildx build --platform linux/amd64 --push` 验证单个 Dockerfile 设计，但不得以手工标签集合进入生产。

## 6. 单机 4C8G 资源契约

以下数值是首版 `mem_limit` 上限，不是容量承诺。镜像和 Compose 落地后必须通过 RSS、OOM、GC、帧耗时、连接数和 PostgreSQL/Redis 指标校准。

| 服务 | 初始内存上限 |
|---|---:|
| PostgreSQL | 1536 MB |
| Redis | 640 MB，`maxmemory` 为 512 MB |
| NATS Core | 128 MB |
| game-server | 768 MB |
| game-proxy / match-service / chat-server | 各 256 MB |
| auth-http / mail-service | 各 256 MB |
| admin-api | 384 MB |
| announce-service / metrics-collector | 各 128 MB |
| admin-web 反向代理 | 64 MB |

容器上限合计约 5.1 GB，至少为宿主机、Docker、内核和突发保留 1.5 GB。CPU 限制需要以压测为准，首版不做 CPU pinning；`game-server`、PostgreSQL 和 Redis 的 CPU 不应被低优先级后台服务长期抢占。

Node 服务的 `NODE_OPTIONS=--max-old-space-size=<MB>` 必须低于对应容器上限，为 native module、运行时、日志缓冲和网络连接留出空间。`game-server` 首次触及上限时，优先根据房间数与内存曲线调至 1 GB，再重新核对整机预算，不能无差别提高所有服务限制。

Redis 负责 session、ticket、service registry、route store 和 metrics 等共享状态，生产不得使用会随机淘汰关键键的 `volatile-lru` 或 `allkeys-*` 策略。首版采用 `noeviction`，内存接近阈值时告警并让写入显式失败；容量扩展或数据拆分由后续方案处理。

PostgreSQL 初始建议为 `shared_buffers=512MB`、`effective_cache_size=1536MB`、`work_mem=4MB`、`max_connections=40~60`。`effective_cache_size` 是优化器估算值而非内存预留；上线前必须核查全部应用连接池之和，必要时引入 PgBouncer。

## 7. 网络、发现和 local socket

- 生产 Compose 分为 internal network 与入口网络。Redis、NATS、PostgreSQL、game-server、chat-server、match-service、mail-service、announce-service 和两个 admin 端口不发布宿主机端口。
- 玩家公网只经 HTTPS 反向代理访问 `auth-http`，并通过宿主机或四层入口访问 `game-proxy` 的 KCP/UDP 端口。TCP fallback 仅在正式客户端确实需要时开放。
- `admin-api` 和 `admin-web` 属于运营控制面，应走 VPN、零信任入口或 IP allowlist；不与玩家入口共用无保护公网路由。
- 严格环境必须设置 `REGISTRY_ENABLED=true`、`DISCOVERY_REQUIRED=true`、`DISALLOW_LEGACY_DIRECT_CONFIG=true`。容器 bind host 与 registry advertised host 分开配置，禁止把 `127.0.0.1`、`0.0.0.0` 或本地 fallback 写入生产发现数据。
- 当前 `game-proxy -> game-server` 使用 `game-server.proxy-local`，`match-service -> game-server` 使用 `game-server.internal`，两者都是 local socket。单机 Compose 必须为相关容器挂载同一个临时 socket volume，并使用唯一 socket 名。该 volume 不保存业务数据，也不进入备份。
- local socket 方案不能跨主机扩容。未来多机或高可用部署前，必须先将这两条链路演进为 service registry 可发现的 internal TCP endpoint。

## 8. 本地发布验收和交接

构建完成后，发布者至少交付：

1. release ID、完整 Git commit 和 `images.lock.json`。
2. 所有镜像的 repository、tag、digest、目标平台和 SBOM/provenance 位置。
3. 本次数据库 migration 是否存在、是否不可逆、备份证据要求和兼容窗口。
4. 本次配置新增项、secret 新增项、默认值变化和受影响公网端口。
5. 已执行检查及结果，以及未执行真实联调的明确说明。

生产服务器只能接收上述 release bundle、已审阅的非敏感 Compose/config 文件和独立注入的 secret。服务器初始化、首次发布、拉取更新、drain 与回滚见[服务器 Docker 初始化与更新](./服务器Docker初始化与更新.md)。
