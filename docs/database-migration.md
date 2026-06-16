# 数据库迁移现状

## 文档定位

本文记录当前仓库数据库初始化与 migration runner 的事实状态。数据库方案后续计划从 MySQL/MariaDB 调整到 PostgreSQL，因此当前不继续推进 MySQL 体系下的完整 schema 拆分、baseline、回滚或修复脚本开发。

## 当前能力

- `db/init.sql` 仍是本地开发和空库 bootstrap 的主要初始化脚本。
- `db/migrations/0001_create_schema_migrations.sql` 已创建 `schema_migrations` 元表。
- `tools/db-migrate.js` 已支持 migration 文件名、顺序、重复版本和 checksum 校验。
- `tools/db-migrate.js --dry-run` / `--list` 可在不连接数据库的情况下打印执行顺序和 checksum。
- 真实执行会读取 `MYSQL_URL` 或 `DATABASE_URL`，应用未执行 migration，并检查已应用 migration 的名称和 checksum 是否漂移。
- `scripts/db-migrate.ps1` 提供 PowerShell 封装。
- 根脚本已提供：
  - `npm run check:migrations`
  - `npm run db:migrate:dry-run`
  - `npm run db:migrate`

## 暂停范围

以下工作暂不在 MySQL/MariaDB 下继续推进，等待 PostgreSQL 方案确定后重新设计：

- 将 `db/init.sql` 完整拆分为版本化初始 schema migration。
- 为已有开发库或测试库设计 baseline 标记策略。
- 编写 MySQL 增量升级、回滚和数据修复脚本。
- 将 Node 服务启动期的零散 schema 初始化全部改造成 migration 驱动。

## 当前使用建议

本地空库仍可使用：

```powershell
mysql -uroot -p < db/init.sql
```

检查 migration 文件元数据：

```powershell
npm run check:migrations
npm run db:migrate:dry-run
```

在未明确数据库切换方案前，不要把新的业务表开发绑定到 MySQL 专用 migration 体系。后续 PostgreSQL 方案确定后，应重新定义 schema 目录、baseline 规则、升级顺序、回滚/修复策略和 CI 检查命令。
