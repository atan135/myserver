# WSL 本地构建与 BuildKit 配置

## 1. 定位

本文说明 MyServer 本机 WSL2 开发环境在执行 `scripts/docker/build-and-push.sh` 之前必须完成的 Docker daemon、BuildKit 与 buildx 配置，以解决受限网络下 BuildKit 系统级镜像拉取失败的问题。

适用范围：

- 仅 WSL2 Ubuntu 发行版内的 dockerd；路径以 `local_help.txt` 中 `MYSERVER_WSL_PROJECT_ROOT` 为准
- 仅开发机本地构建；不涉及服务器侧发布与部署
- 与 `scripts/docker/build-and-push.sh` 配套使用

不适用范围：

- 服务器 Docker 初始化与更新：见[服务器 Docker 初始化与更新](./服务器Docker初始化与更新.md)
- 生产镜像清单与发布边界：见[本地 Docker 镜像打包与发布](./本地Docker镜像打包与发布.md)
- 跨服务网络与日志采集：见[服务端日志采集与留存设计](../../安全与监控/服务端日志采集与留存设计.md)

## 2. 问题背景

`scripts/docker/build-and-push.sh --push` 会触发 BuildKit 拉取内部镜像 `docker.io/docker/buildkit-syft-scanner:stable-1`（用于 `--sbom=true` 产出 SBOM）。在该路径下：

- `daemon.json` 的 `registry-mirrors` 不会被 BuildKit 应用到 BuildKit 自身的「系统级」拉取，仅对 `docker pull` 命令生效
- BuildKit 直连 `registry-1.docker.io` 时，在受限网络下命中部分 IP（如 `93.179.102.140:443`）会拿到 `connection refused`
- WSL shell 内 `curl` 通过 Clash HTTP 代理能到达 Docker Hub，但 Docker Hub 当前对匿名 pull 普遍要求 token，被 `UNAUTHORIZED` 挡回

仅靠 `docker pull` 走 mirror 不能解决 BuildKit 的内部拉取；必须为 BuildKit 单独配置 `buildkitd.toml`，并通过 docker-container buildx builder 的 `--buildkitd-config` 让 buildkitd 进程真正加载。

## 3. 自动配置

`scripts/docker/setup-wsl-buildx.sh` 是幂等的一键配置入口，作用于 WSL 内的 `/etc/docker/daemon.json` 与 `/etc/buildkit/buildkitd.toml`，重建 `mybuilder` buildx builder 并把它设为默认。

```bash
cd /home/dev/src/MyServer
./scripts/docker/setup-wsl-buildx.sh
```

可选参数：

| 参数 | 默认 | 用途 |
|------|------|------|
| `--mirror <host>` | `docker.m.daocloud.io` | BuildKit registry 镜像主机；本地 WSL 默认走国内 mirror，其他内部 mirror 在迁移时覆盖 |
| `--builder <name>` | `mybuilder` | 要重建的 docker-container buildx builder 名；与 `scripts/docker/build-and-push.sh` 实际使用的 builder 对齐 |
| `--check-only` | - | 仅打印当前生效状态并退出，不修改任何文件 |

脚本依次完成：

1. 写 `/etc/buildkit/buildkitd.toml`，强制 `[registry."docker.io"].mirrors = ["<mirror>"]`
2. 把 `builder.registry-config` 合并进 `/etc/docker/daemon.json`，**保留**已有字段并生成 `daemon.json.setup-wsl-buildx.bak.<UTC 时间戳>` 备份
3. 重启 dockerd：`systemctl restart docker` 优先；裸 dockerd 走 `pkill + setsid /usr/bin/dockerd -H fd:// --containerd=/run/containerd/containerd.sock` 回退
4. 用 `--buildkitd-config` 重建 builder，**保留**原 `--driver-opt network=host` 和 `--buildkitd-flags --allow-insecure-entitlement=network.host`
5. `docker buildx use <builder>` 设为默认

## 4. 手动配置参考

如果不使用脚本，等价的手动步骤：

```toml
# /etc/buildkit/buildkitd.toml
[registry."docker.io"]
  mirrors = ["docker.m.daocloud.io"]
  http = false
  insecure = false
```

```bash
sudo jq '
  .builder = (.builder // {})
  | .builder["registry-config"] = "/etc/buildkit/buildkitd.toml"
' /etc/docker/daemon.json | sudo tee /etc/docker/daemon.json >/dev/null
sudo systemctl restart docker   # 或裸 dockerd 重启路径
```

```bash
docker buildx rm mybuilder
docker buildx create \
  --name mybuilder \
  --driver docker-container \
  --driver-opt 'network=host' \
  --buildkitd-flags '--allow-insecure-entitlement=network.host' \
  --buildkitd-config /etc/buildkit/buildkitd.toml
docker buildx use mybuilder
```

`daemon.json` 合并示例（保留仓库基线 `dns` / `registry-mirrors` / `proxies`）：

```json
{
  "dns": ["223.5.5.5", "119.29.29.29"],
  "registry-mirrors": ["https://docker.m.daocloud.io"],
  "proxies": {
    "http-proxy": "http://172.17.240.1:7897",
    "https-proxy": "http://172.17.240.1:7897",
    "no-proxy": "localhost,127.0.0.1,172.17.0.0/16"
  },
  "builder": {
    "registry-config": "/etc/buildkit/buildkitd.toml"
  }
}
```

## 5. 验证

```bash
# 1. daemon.json 已包含 builder.registry-config
jq '.builder["registry-config"]' /etc/docker/daemon.json
# 预期："/etc/buildkit/buildkitd.toml"

# 2. buildkitd 进程确实读了 buildkitd.toml
pgrep -af buildkitd | grep -o '/etc/buildkit/buildkitd.toml'
# 预期：能看到该参数（--config /etc/buildkit/buildkitd.toml）

# 3. mybuilder 是默认 builder 且 buildkit 容器已挂入 toml
docker buildx ls
docker exec buildx_buildkit_mybuilder0 cat /etc/buildkit/buildkitd.toml
# 预期：mybuilder* 带星号；容器内能读到 [registry.'docker.io'] 段

# 4. mirror 实际可达
docker pull docker.m.daocloud.io/docker/buildkit-syft-scanner:stable-1
# 预期：Status: Downloaded / up to date
```

通过验证后，`scripts/docker/build-and-push.sh --load` 应一次跑通 11 个镜像；`--push` 路径下的 `--sbom=true` 也会自动改路，不会再卡在 `registry-1.docker.io` 直连黑洞。

## 6. 回滚

```bash
# 1. 删除 buildkitd.toml
sudo rm /etc/buildkit/buildkitd.toml

# 2. 还原 daemon.json（用最新的 .setup-wsl-buildx.bak.<时间戳> 备份）
sudo cp /etc/docker/daemon.json.setup-wsl-buildx.bak.<时间戳> /etc/docker/daemon.json
sudo systemctl restart docker

# 3. 不带 --buildkitd-config 重建 mybuilder
docker buildx rm mybuilder
docker buildx create \
  --name mybuilder \
  --driver docker-container \
  --driver-opt 'network=host' \
  --buildkitd-flags '--allow-insecure-entitlement=network.host'
docker buildx use mybuilder
```

回滚后 BuildKit 会重新直连 `registry-1.docker.io`，SBOM scanner 等系统级拉取需要依赖网络可达。本配置回滚不影响仓库代码、`scripts/docker/build-and-push.sh`、镜像清单与服务器发布流程。

## 7. 影响范围

| 项 | 影响 |
|---|---|
| 仓库代码 | 不修改 |
| WSL 系统配置 | `/etc/docker/daemon.json`（新增 `builder` 块）；新增 `/etc/buildkit/buildkitd.toml` |
| BuildKit 容器 | `buildx_buildkit_mybuilder0` 内挂载 `/etc/buildkit/buildkitd.toml`；由 buildx 重建触发一次容器替换 |
| 端口 / 协议 / 业务脚本 | 不动 |
| 服务器发布 | 不影响；服务器只拉已发布镜像，不构建 |
| Docker daemon `proxies` 字段 | 不修改；与 Clash Verge HTTP 代理共存 |

## 8. 引用

- `scripts/docker/setup-wsl-buildx.sh` — 自动配置脚本
- `scripts/docker/build-and-push.sh` — 触发 SBOM scanner 拉取的本地构建入口
- `scripts/docker/publish-release.sh` — WSL 原生 worktree 下的正式发布入口
- [本地 Docker 镜像打包与发布](./本地Docker镜像打包与发布.md) — 仓库侧发布脚本与镜像清单
- [服务器 Docker 初始化与更新](./服务器Docker初始化与更新.md) — 服务器侧发布与更新
- `docs/总览/整体架构.md` — MyServer 整体拓扑（与本机开发边界）