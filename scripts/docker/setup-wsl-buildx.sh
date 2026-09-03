#!/usr/bin/env bash
set -euo pipefail

# Configure WSL2 Docker + BuildKit so every docker.io image, including
# BuildKit-internal ones (e.g. docker.io/docker/buildkit-syft-scanner pulled
# for --sbom=true), is fetched through the configured mirror. Idempotent.

usage() {
  cat <<'EOF' >&2
Usage: scripts/docker/setup-wsl-buildx.sh [--mirror <host>] [--builder <name>] [--check-only]

Defaults:
  --mirror   docker.m.daocloud.io   (the China Docker Hub mirror the local WSL uses)
  --builder  mybuilder             (the docker-container buildx builder used by
                                   scripts/docker/build-and-push.sh)
  --check-only                     print effective state and exit
EOF
  exit 64
}

mirror='docker.m.daocloud.io'
builder_name='mybuilder'
check_only=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    --mirror) mirror="${2:?--mirror requires a value}"; shift 2 ;;
    --builder) builder_name="${2:?--builder requires a value}"; shift 2 ;;
    --check-only) check_only=true; shift ;;
    --help|-h) usage ;;
    *) echo "Unknown option: $1" >&2; usage ;;
  esac
done

for cmd in docker jq sudo; do
  command -v "$cmd" >/dev/null || { echo "Required command is unavailable: $cmd" >&2; exit 69; }
done

buildkitd_dir='/etc/buildkit'
buildkitd_toml="$buildkitd_dir/buildkitd.toml"
daemon_json='/etc/docker/daemon.json'

print_state() {
  echo "=== $daemon_json ==="
  if [[ -s "$daemon_json" ]]; then
    jq . "$daemon_json"
  else
    echo "(missing or empty)"
  fi
  echo ""
  echo "=== $buildkitd_toml ==="
  if [[ -f "$buildkitd_toml" ]]; then
    cat "$buildkitd_toml"
  else
    echo "(missing)"
  fi
  echo ""
  echo "=== docker daemon state ==="
  docker info 2>&1 | grep -E 'Server Version|Registry Mirrors' -A 3 || true
  echo ""
  echo "=== buildx builders ==="
  docker buildx ls 2>&1 || true
}

if [[ "$check_only" = true ]]; then
  print_state
  exit 0
fi

# 1. Write buildkitd.toml.
echo "=== writing $buildkitd_toml ==="
sudo mkdir -p "$buildkitd_dir"
sudo tee "$buildkitd_toml" >/dev/null <<EOF
# BuildKit registry mirror for MyServer local WSL2 builds.
# daemon.json's registry-mirrors is not honoured for BuildKit-internal image
# pulls (e.g. docker.io/docker/buildkit-syft-scanner pulled for --sbom=true).
# This file is the authoritative redirect for the docker-container buildx
# builder used by scripts/docker/build-and-push.sh.
[registry."docker.io"]
  mirrors = ["$mirror"]
  http = false
  insecure = false
EOF

# 2. Merge builder.registry-config into daemon.json (preserves all other fields).
echo "=== merging builder.registry-config into $daemon_json ==="
if [[ ! -s "$daemon_json" ]]; then
  sudo mkdir -p "$(dirname "$daemon_json")"
  echo '{}' | sudo tee "$daemon_json" >/dev/null
fi
ts=$(date -u +%Y%m%dT%H%M%SZ)
sudo cp "$daemon_json" "$daemon_json.setup-wsl-buildx.bak.$ts"

sudo jq --arg rc "$buildkitd_toml" '
  .builder = (.builder // {})
  | .builder["registry-config"] = $rc
' "$daemon_json" | sudo tee "$daemon_json" >/dev/null

# Validate merged JSON.
jq empty "$daemon_json" || { echo "daemon.json is invalid after merge" >&2; exit 70; }

# 3. Restart dockerd (systemctl first, bare dockerd fallback).
echo "=== restarting dockerd ==="
if command -v systemctl >/dev/null && systemctl is-active docker >/dev/null 2>&1; then
  sudo systemctl restart docker
else
  sudo pkill -TERM dockerd 2>/dev/null || true
  for _ in 1 2 3 4 5; do
    pgrep dockerd >/dev/null || break
    sleep 1
  done
  if pgrep dockerd >/dev/null; then
    sudo pkill -KILL dockerd
    sleep 1
  fi
  sudo setsid nohup /usr/bin/dockerd \
    -H fd:// --containerd=/run/containerd/containerd.sock \
    > /tmp/dockerd.log 2>&1 < /dev/null &
  for _ in $(seq 1 25); do
    docker info >/dev/null 2>&1 && break
    sleep 1
  done
fi

# 4. Recreate the docker-container buildx builder with --buildkitd-config.
echo "=== recreating buildx builder: $builder_name ==="
docker buildx rm "$builder_name" 2>/dev/null || true
docker buildx create \
  --name "$builder_name" \
  --driver docker-container \
  --driver-opt 'network=host' \
  --buildkitd-flags '--allow-insecure-entitlement=network.host' \
  --buildkitd-config "$buildkitd_toml"
docker buildx use "$builder_name"

# 5. Print final state.
echo ""
echo "=== final state ==="
print_state