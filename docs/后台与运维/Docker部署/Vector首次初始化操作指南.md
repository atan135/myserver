# Vector 首次初始化操作指南

## 1. 适用范围

本文记录 MyServer 宿主机首次部署 Vector `0.47.0` 采集器的实操步骤，适用于：

- 已运行 MyServer release `v0.1.0-52ee2c839c6f` 或更早版本，从未启用 Vector preflight 的服务器（本次在 22 次部署历史中首次启用 Vector 阶段 2 的服务器即属此类）
- 全新的 MyServer 宿主机首次初始化
- Vector 二进制已损坏或 systemd unit 不可用，需要完整重装的服务器

本文不记录主机名、SSH 私钥、镜像仓库凭据、生产 secret 或具体 IP/端口。这类信息只能留在受控密码管理系统或服务器本地 `secrets/` 目录。

## 2. 前置条件

- Vector `0.47.0` 二进制 tarball（`vector-0.47.0-x86_64-unknown-linux-gnu.tar.gz`），从 <https://github.com/vectordotdev/vector/releases/download/v0.47.0/> 下载，本机已有副本优先
- 服务器能通过 `ssh gameops@<host>` 访问，`gameops` 拥有 `sudo NOPASSWD: ALL`
- 服务器上 `/data/myserver/release/<release-id>/` 已有 release bundle
- 服务器已通过 `verify-vector.sh --offline` 离线校验 bundle 内 `vector/` 目录

## 3. 首次初始化步骤

### 3.1 拷贝 Vector 二进制到服务器

Vector 二进制不在 release bundle 内（bundle 只携带 yaml / service / 脚本），必须从外部源拷贝。本机 Windows 工作区下载的 tarball 通过 WSL `scp` 传给服务器：

```bash
# 在 WSL 中执行（参数中的路径以 local_help.txt 为准）
scp -i ~/.ssh/myserver_gameops_rsa -o IdentitiesOnly=yes -P <ssh-port> \
  /mnt/c/Users/<user>/Downloads/vector-0.47.0-x86_64-unknown-linux-gnu.tar.gz \
  gameops@<host>:/tmp/
```

tarball 解包后 binary 在 `bin/vector` 子目录，不是顶层：

```bash
ssh ... "
  sudo install -m 0755 /tmp/vector-extracted/vector-x86_64-unknown-linux-gnu/bin/vector /usr/bin/vector
  /usr/bin/vector --version   # 必须返回 vector 0.47.0 (x86_64-unknown-linux-gnu ...)
"
```

### 3.2 创建 vector user / group（如已存在则跳过）

`vector` 是 system user / group，由 `install-vector.sh` 自动创建；但首次初始化时如果之前装过 Vector 留下 group 但没 user，会触发 `useradd` 失败（见 §4.1）。先用 `id vector` / `getent group vector` 检查：

```bash
ssh ... "
  getent group vector
  id vector
  # 若 group 不存在：sudo groupadd --system vector
  # 若 user 不存在且 group 已存在：sudo useradd --system --no-create-home --shell /usr/sbin/nologin -g vector vector
  # 若都不存在：交给 §3.3 install-vector.sh 自动创建
"
```

### 3.3 运行 install-vector.sh

```bash
ssh ... "
  sudo /data/myserver/release/<release-id>/scripts/install-vector.sh \
    --source /data/myserver/release/<release-id>/vector
"
```

`install-vector.sh` 在 Vector 已校验的版本与 bundle 版本一致时，会：

- 创建 `/etc/vector` (mode 0755)、`/var/lib/vector/{buffer,checkpoints,queue}` (mode 0750)、`/data/myserver/log` (mode 0750)，owner 全部 `vector:vector`
- 创建 `vector` group / user（如缺失）
- 复制 `vector.yaml` 到 `/etc/vector/vector.yaml`、`vector.service` 到 `/etc/systemd/system/vector.service`
- 复制 `rotate-vector-files.sh` / `prune-vector-files.mjs` / `vector-alerts.sh` / `vector-recovery-check.sh` 到 `/usr/local/sbin/myserver-*`
- `systemctl daemon-reload`
- 可选 `--enable`：`systemctl enable vector.service`

如果 §3.2 已经手动创建 user/group，`install-vector.sh` 会跳过 `groupadd` / `useradd`，继续后续步骤。

### 3.4 启动 Vector systemd service

```bash
ssh ... "
  sudo systemctl daemon-reload
  sudo systemctl enable --now vector.service
  sleep 3
  sudo systemctl is-active vector.service    # 必须返回 active
  sudo /usr/bin/vector --version
"
```

### 3.5 验证 vector validate 通过

Vector `0.47.0` 的 `validate` 子命令只接受 positional 配置路径，不再支持 `--config` yaml flag；加 `--no-environment` 跳过 healthcheck：

```bash
ssh ... "
  sudo /usr/bin/vector validate /etc/vector/vector.yaml --no-environment
  # 必须输出：Validated
"
```

### 3.6 跑 vector-preflight.sh（用 sudo 调用）

`vector-preflight.sh` 检查 `/var/lib/vector` (mode 0750 owner vector) 等系统目录，gameops 无法 stat 该目录，必须以 root 调用。本仓库 `server-apply-release.sh` 自 commit `e30226a` 起已自动用 `sudo` 调用 preflight；手工验证也必须用 sudo：

```bash
ssh ... "
  sudo /data/myserver/release/<release-id>/scripts/vector-preflight.sh \
    --release-dir /data/myserver/release/<release-id> --allow-missing
  # 必须输出最后一行：vector_preflight=passed version=0.47.0 ...
"
```

## 4. 常见坑

### 4.1 install-vector.sh 失败：`useradd: group vector exists - if you want to add this user to that group, use -g`

原因：之前已经创建过 `vector` group（来自上一次手动初始化），但 `vector` user 不存在。`install-vector.sh` 调 `useradd` 不带 `-g` 参数，useradd 拒绝在已存在同名 group 上创建 user。

修复（先手动建 user，再跑 install-vector.sh）：

```bash
ssh ... "
  sudo useradd --system --no-create-home --shell /usr/sbin/nologin -g vector vector
  sudo /data/myserver/release/<release-id>/scripts/install-vector.sh \
    --source /data/myserver/release/<release-id>/vector
"
```

### 4.2 /var/lib/vector/{buffer,checkpoints,queue} 子目录缺失导致 vector-preflight 失败

如果 §4.1 错误让 `install-vector.sh` 在 `set -e` 下中途退出，子目录创建步骤可能未完成，vector preflight 会报：

```
vector_preflight=failed reason=unsafe_or_missing_directory_/var/lib/vector/buffer
```

修复：

```bash
ssh ... "
  sudo install -d -m 0750 -o vector -g vector \
    /var/lib/vector/buffer /var/lib/vector/checkpoints /var/lib/vector/queue
  sudo chown -R vector:vector /var/lib/vector /data/myserver/log
  sudo /data/myserver/release/<release-id>/scripts/vector-preflight.sh \
    --release-dir /data/myserver/release/<release-id> --allow-missing
"
```

### 4.3 apply-release.sh 用 gameops 跑会被 vector preflight 挡住

原因：`vector:vector` 0750 目录对 gameops 不可见，`vector-preflight.sh` 的 `assert_dir` 和子目录检查都会失败。

修复（任选其一）：

- 用 `sudo /data/myserver/apply-release.sh ...` 跑整个脚本（root 权限通过 preflight，但需 §4.4 解决 root 无凭据问题）
- 升级 release bundle 到 commit `e30226a` 之后，让 `server-apply-release.sh` 内部 `sudo "$vector_preflight"`（对 gameops 调用者透明）

### 4.4 sudo apply-release.sh 时 root 没有 registry 凭据

`gameops` 用户通过 `~/.docker/config.json` 持有 ACR 凭据；root 默认 `/root/.docker/config.json` 没有该凭据，`docker pull` 在 sudo 跑 apply-release.sh 时会失败：

```
Error pull access denied for crpi-aag02un1ijrswhes.cn-shenzhen.personal.cr.aliyuncs.com/...
repository does not exist or may require 'docker login'
```

临时修复（注意卫生：长期保留 root 持有 gameops 凭据不干净）：

```bash
ssh ... "
  sudo cp /home/gameops/.docker/config.json /root/.docker/config.json
  sudo chmod 600 /root/.docker/config.json
  sudo chown root:root /root/.docker/config.json
"
```

更稳妥做法：升级 release bundle 到 commit `e30226a` 之后，用 gameops 跑 apply-release.sh（不再需要 root docker 凭据）。

清理临时文件：

```bash
ssh ... "sudo rm /root/.docker/config.json"
```

## 5. 验证清单

首次 Vector 初始化完成后，必须确认：

- [ ] `/usr/bin/vector --version` 输出 `vector 0.47.0 (x86_64-unknown-linux-gnu ...)`
- [ ] `systemctl is-active vector.service` 返回 `active`
- [ ] `/usr/bin/vector validate /etc/vector/vector.yaml --no-environment` 输出 `Validated`
- [ ] `vector-preflight.sh --release-dir /data/myserver/release/<release-id> --allow-missing` 最后一行 `vector_preflight=passed version=0.47.0 ...`
- [ ] `curl -sf http://127.0.0.1:8686/health` 返回 200
- [ ] `/data/myserver/log/<service>/<UTC 日期>/` 出现 `.jsonl.open` 文件（业务容器日志正在被采集）
- [ ] `docker logs <container>` 在 Vector 不可用时仍能作为短期兜底

阶段 3 的归档清理（`myserver-rotate-vector-files`、`myserver-prune-vector-files`）按[正式 Release 上线说明](./正式Release上线说明.md) 阶段 4 准入定义启用，本指南不涉及。