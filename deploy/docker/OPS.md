# Docker 运维脚本

脚本源码位于 `deploy/docker/scripts/`，部署到目标机 `/home/gameops/script` 后运行。它们仅操作当前正式 release：`/data/myserver/release/current`。

```bash
chmod 0755 /home/gameops/script/ops-*.sh
/home/gameops/script/ops-status.sh
/home/gameops/script/ops-logs.sh auth-http --tail 200
/home/gameops/script/ops-health.sh
/home/gameops/script/ops-restart.sh auth-http --confirm auth-http
/home/gameops/script/ops-disk-report.sh
```

`ops-restart.sh` 只重启现有容器，不重新读取 Compose 定义、环境文件或镜像。其余参数由脚本白名单校验，避免将服务名传递为任意 Docker 参数。

正式发布必须先通过现有 bundle 流程上传 release，再执行：

```bash
/home/gameops/script/ops-deploy.sh --release-id v0.1.0-<commit> --confirm v0.1.0-<commit>
```

该脚本调用 `/data/myserver/apply-release.sh`，保留 digest 拉取、迁移和 readiness 校验。不要直接对单个服务执行 `docker compose pull` 或 `up` 作为正式发布。

回滚同样调用 release runner，数据库迁移不会自动回退，因此必须先人工确认目标 release 与当前数据库兼容：

```bash
/home/gameops/script/ops-rollback.sh --release-id v0.1.0-<previous> --confirm v0.1.0-<previous> --db-compatible
```

脚本不包含 `docker system prune`、删除 volume 或数据库修复命令。
