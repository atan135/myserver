# 本地 sqlx-cli 安装说明

本文档说明 Windows 开发环境下如何安装 / 刷新 bin/sqlx.exe 以及它为什么必须使用 scripts/install-sqlx-cli.ps1，而不是直接 cargo install sqlx-cli。

## 1. 背景

tools/db.js、tools/db-deploy.js、scripts/db.ps1 在调用 sqlx migrate run 之前会通过 tools/db.js#resolveSqlxBinary 读取 db/config/sqlx-cli.json 中登记的 win32-x64 制品信息，**逐字节校验 bin/sqlx.exe 的 SHA-256**。校验失败会直接 fail-closed，不会下载、不会安装、也不会回退到 PATH 上的同名二进制。

- 这意味着：开发机每次重装或刷新 sqlx-cli 时，**产物的 SHA-256 必须与 json 登记值完全一致**。
- sqlx-cli 的 0.8.6 版本是 2024 年发布时的产物，**rustc 1.94.1 在 Windows MSVC target 上默认编译出来的 PE 文件不写 deterministic build-id**——同一份 Cargo.lock、同一份源码、同一台机器、连续两次 cargo install --force sqlx-cli，产物的 SHA-256 也会不同。
- 因此，直接 cargo install sqlx-cli@0.8.6 --locked --no-default-features --features postgres,rustls 在 dev-stack / 部署验收中会得到一个**一次性 hash**，与 json 登记值比对失败，db up 走不到 SQLx CLI。

## 2. 安装 / 刷新步骤

在仓库根（Windows PowerShell）执行：

`powershell
powershell -ExecutionPolicy Bypass -File scripts/install-sqlx-cli.ps1
`

脚本会：

1. 读 db/config/sqlx-cli.json，取出 version 和 win32-x64.buildRustflags。
2. 设置 RUSTFLAGS=-C link-arg=/Brepro，然后调用 cargo install --force --version <version> --locked --no-default-features --features postgres,rustls sqlx-cli。/Brepro 是 MSVC link.exe 的开关（Visual Studio 2019 16.10+），告诉链接器在产物里只写入 source-content-derived 的 metadata，写出可重现的 PE 镜像。
3. 把 ~/.cargo/bin/sqlx.exe 复制到 bin/sqlx.exe。
4. 计算新产物的 SHA-256 并与 json 登记的 sha256 比对。

成功时输出：

`	ext
sqlx-cli installed and verified.
  D:\project\myserver\bin\sqlx.exe
`

失败时**拒绝留场**并提示排查方向——本地 Rust/MSVC 工具链与登记 hash 的工具链不一致。

## 3. 什么时候需要更新 json 的 sha256

仅在以下情况：

- Rust 工具链或 MSVC 工具链**整体升级**（例如 rustc 1.94.1 -> 1.95.0，或 Visual Studio Build Tools 升大版本）。
- json 登记的 version 字段变更（升级到 sqlx-cli 0.8.7 / 0.9.x 等）。

更新步骤：

1. powershell -ExecutionPolicy Bypass -File scripts/install-sqlx-cli.ps1 -SkipHashCheck 安装并记录新 hash。
2. 再跑一次（同样命令、同样工具链），确认两次产物的 SHA-256 完全一致——证明工具链组合确实 deterministic。
3. 把新 SHA-256 写回 db/config/sqlx-cli.json 的 win32-x64.sha256。
4. 在 commit message 中说明 rustc / MSVC 工具链变更原因。

**不要**只在 cargo install 后看到 hash 不一致就盲目更新 json——两次连续安装必须先证实 stable，再写回。

## 4. 已知约束

- bin/sqlx.exe 不在 git 中（/bin/ 在 .gitignore），开发机本地需自行通过 install-sqlx-cli.ps1 准备。
- CI 的 .github/workflows/database-migration.yml 只跑 npm run db:ci 静态 gate，不下载或编译 sqlx-cli，因此 CI 不需要 bin/sqlx.exe。
- Linux 部署仍走 container image（MYSERVER_SQLX_CLI_PATH / MYSERVER_SQLX_CLI_SOURCE=container-image），不依赖本仓库 bin/sqlx.exe。
- win32-x64.buildRustflags 字段必须保留；install-sqlx-cli.ps1 读到缺失会拒绝安装，避免不小心退回到非 deterministic 编译。