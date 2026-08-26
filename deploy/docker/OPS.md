# Docker 运维脚本

脚本源码位于 `deploy/docker/scripts/`，由校验过完整 `SHA256SUMS` 的 release bundle 携带，并通过 `scripts/install-ops-scripts.sh` 按固定十文件白名单事务化安装到 `/home/gameops/script`。安装器全程持有 `/data/myserver/run/operations.lock`，同时切换 ops generation 与 `/data/myserver/apply-release.sh`。目录 rename 之间存在极短的 target 缺失窗口；durable `pending-ops-install` journal 记录新旧路径和内容哈希，普通失败立即回滚，SIGKILL 后下一次安装会先做路径和身份围栏恢复。journal 未恢复时其他写运维脚本关闭失败。安装过程拒绝缺失、额外或符号链接条目，不使用 `ops-*` glob 做增量覆盖。

```bash
/home/gameops/script/ops-status.sh
/home/gameops/script/ops-logs.sh auth-http --tail 200
/home/gameops/script/ops-health.sh
/home/gameops/script/ops-restart.sh auth-http --confirm auth-http
/home/gameops/script/ops-replace.sh auth-http --confirm auth-http
/home/gameops/script/ops-retire.sh game-server \
  --instance-id game-server-old --revision <full-git-sha> \
  --confirm game-server-old@<full-git-sha>@myserver
/home/gameops/script/ops-disk-report.sh
```

当前 `ops-logs.sh` 直接读取目标容器的 `docker logs`，对应 Docker `local` driver 的有限保留窗口。Vector 完成 v2 Checklist 后，日常排障入口应优先切换为 Vector 在 `/data/myserver/log` 的按日输出；`docker logs` 仍保留为 Vector 延迟、重启或落盘故障时的短期兜底。Vector 尚未落地前，不要假定该目录已有完整日志。

`ops-restart.sh` 只重启现有容器，不重新读取 Compose 定义、环境文件或镜像。`ops-replace.sh` 使用当前 release 的 Compose 定义执行单服务 `up -d --no-deps`，不会连带重建依赖。两者先确认目标容器已运行且其可用 healthcheck 已通过，再复用当前 release 的统一 readiness 收敛函数；不能以容器 `running` 或单次 health 成功作为整个操作的完成条件。其余参数由脚本白名单校验，避免将服务名传递为任意 Docker 参数。

`ops-retire.sh` 是 game-server 滚动替换的部署平台 stop hook。候选实例 Ready 后先启动该脚本；它精确核验 Compose service/project、旧实例 ID 和完整镜像 Git revision，并将 project 纳入确认串，再将旧容器 restart policy 临时改为 `no`，然后只等待正式 admin-api 两人审批和 break-glass 链触发的 graceful shutdown。drain 后旧实例发布 unhealthy，不再进入玩家路由或普通 healthy discovery；shutdown 路径只通过隔离的 live admin discovery 访问 heartbeat 仍活跃、service/instance identity 精确匹配且声明 `admin`/`admin`/`tcp` 传输元数据的同一实例，再以实际连接、签名断言认证和停机响应确认控制口可用，不会把旧实例重新加入业务流量。endpoint 的 `healthy` 是整体 readiness 的投影，drain 后允许为 false；连接、认证或响应失败仍会 fail-closed。脚本不接受或读取管理员 token、nonce，也不调用 admin-api。只有同一容器以 exit code 0 且非 OOM 退出时才保留 stopped；失败会恢复原 restart policy 和运行期望。异常中断遗留的 pending journal 会阻止 deploy/restart/replace/rollback，必须使用相同 instance/revision/project/confirm 加 `--recover` 精确恢复。即使旧容器已经干净退出，`--recover` 也会恢复 `unless-stopped` 并重新启动旧实例，清除 journal 后本次 retire 即告结束；确认候选状态后必须发起新的 retire 周期，不能把 recover 当成继续等待。该 stop hook 不负责自动创建候选实例，当前 release runner 也不因此自动具备 old/new 灰度编排。

新版 restart/replace 脚本依赖当前 release bundle 的 `scripts/readiness-convergence.sh`。`upload-release-bundle.sh` 在服务器校验 bundle 后调用同一事务安装器更新完整脚本集合与 apply runner；脚本会在变更容器前检查 helper，旧 bundle 不满足条件时会关闭失败，不会先重启或替换服务。

正式发布必须先通过现有 bundle 流程上传 release，再执行：

```bash
/home/gameops/script/ops-deploy.sh --release-id v0.1.0-<commit> --confirm v0.1.0-<commit> --rollback-db-compatible
```

该脚本调用 `/data/myserver/apply-release.sh`，保留 digest 拉取、迁移、应用服务单次批量启动以及 registry TTL + Ready 稳定窗口校验。`--rollback-db-compatible` 表示操作者已确认：本次前向迁移执行后，上一应用 release 仍能使用当前数据库 schema；未确认时 runner 拒绝发布。readiness 超时时 runner 输出结构化安全诊断，并在存在上一 release 时调用其同一流程做单次版本回滚；原始发布仍明确失败。不要直接对单个服务执行 `docker compose pull` 或 `up` 作为正式发布，也不要删除未知 registry key 或 socket 绕过收敛失败。

回滚同样调用 release runner，数据库迁移不会自动回退，因此必须先人工确认目标 release 与当前数据库兼容：

```bash
/home/gameops/script/ops-rollback.sh --release-id v0.1.0-<previous> --confirm v0.1.0-<previous> --db-compatible
```

回滚模式只替换应用版本，不用旧 release 的 migration catalog 重跑 preflight/apply。目标 release 与发起回滚的当前/失败 source release 都会重新核对 manifest identity 和完整 `SHA256SUMS`；通过后才复用 source release migration-runner 校验现有数据库 history、drift、required readiness 和稳定窗口，避免把已应用的新 migration 误判为旧版本 catalog 的未知记录。

脚本不包含 `docker system prune`、删除 volume 或数据库修复命令。
