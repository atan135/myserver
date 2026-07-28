#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  scripts/docker/build-and-push.sh [options]

Build all MyServer runtime images for linux/amd64. --push requires a clean Git
worktree and an existing docker login for the selected registry.

Options:
  --registry <host>       Registry host. Default: ACR Shenzhen personal endpoint.
  --namespace <name>      Registry namespace. Default: zerg-myserver.
  --release-tag <tag>     Immutable tag. Default: v<package-version>-<short-sha>.
  --push                  Push images and write a digest lock file.
  --allow-dirty           Permit a non-release test push from a dirty worktree.
  --lock-file <path>      Digest lock output. Default: deploy/docker/images.lock.json.
  --help                  Show this help.
EOF
}

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
registry="crpi-aag02un1ijrswhes.cn-shenzhen.personal.cr.aliyuncs.com"
namespace="zerg-myserver"
release_tag=""
lock_file="deploy/docker/images.lock.json"
build_progress="${BUILDKIT_PROGRESS:-plain}"
push=false
allow_dirty=false

while [ "$#" -gt 0 ]; do
  case "$1" in
    --registry)
      registry="${2:?--registry requires a value}"
      shift 2
      ;;
    --namespace)
      namespace="${2:?--namespace requires a value}"
      shift 2
      ;;
    --release-tag)
      release_tag="${2:?--release-tag requires a value}"
      shift 2
      ;;
    --lock-file)
      lock_file="${2:?--lock-file requires a value}"
      shift 2
      ;;
    --push)
      push=true
      shift
      ;;
    --allow-dirty)
      allow_dirty=true
      shift
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage >&2
      exit 64
      ;;
  esac
done

for command in docker git node; do
  command -v "$command" >/dev/null || {
    echo "Required command is unavailable: $command" >&2
    exit 69
  }
done

docker buildx version >/dev/null

cd "$root"
if git diff --quiet; then
  worktree_clean=0
else
  worktree_clean=1
fi
if git diff --cached --quiet; then
  index_clean=0
else
  index_clean=1
fi
dirty=false
if [ "$worktree_clean" -ne 0 ] || [ "$index_clean" -ne 0 ]; then
  dirty=true
fi

if [ -z "$release_tag" ]; then
  package_version="$(node --input-type=module -e "import pkg from './package.json' with { type: 'json' }; process.stdout.write(pkg.version)")"
  release_tag="v${package_version}-$(git rev-parse --short=12 HEAD)"
fi

case "$release_tag" in
  *[!A-Za-z0-9._-]*|'')
    echo "Release tag must contain only letters, digits, '.', '_' or '-': $release_tag" >&2
    exit 64
    ;;
esac

if [ "$push" = true ] && [ "$dirty" = true ] && [ "$allow_dirty" = false ]; then
  echo "Refusing a release push from a dirty worktree. Commit the Docker assets first, or use --allow-dirty only for a disposable test image." >&2
  exit 65
fi

if [ "$push" = true ]; then
  if ! docker info --format '{{json .RegistryConfig.IndexConfigs}}' >/dev/null; then
    echo "Docker daemon is unavailable." >&2
    exit 69
  fi
fi

revision="$(git rev-parse HEAD)"
created_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
source_url="$(git config --get remote.origin.url || true)"
records_file="$(mktemp)"
trap 'rm -f "$records_file"' EXIT

build_image() {
  local service="$1"
  local repository="$2"
  local dockerfile="$3"
  shift 3

  local image="${registry}/${namespace}/${repository}:${release_tag}"
  local args=(
    --file "$dockerfile"
    --platform linux/amd64
    --progress "$build_progress"
    --tag "$image"
    --label "org.opencontainers.image.revision=${revision}"
    --label "org.opencontainers.image.created=${created_at}"
    --label "org.opencontainers.image.version=${release_tag}"
    --label "org.opencontainers.image.source=${source_url}"
  )

  while [ "$#" -gt 0 ]; do
    args+=(--build-arg "$1")
    shift
  done

  if [ "$push" = true ]; then
    args+=(--provenance=mode=max --sbom=true --push)
  else
    args+=(--load)
  fi

  echo "Building ${service}: ${image}"
  docker buildx build "${args[@]}" .

  if [ "$push" = true ]; then
    local digest
    digest="$(docker buildx imagetools inspect "$image" --format '{{.Manifest.Digest}}')"
    if [[ ! "$digest" =~ ^sha256:[0-9a-f]{64}$ ]]; then
      echo "Unable to resolve a manifest digest for ${image}: ${digest}" >&2
      exit 70
    fi
    printf '%s\t%s\t%s\n' "$service" "${registry}/${namespace}/${repository}" "$digest" >> "$records_file"
  fi
}

build_image game-server game-server deploy/docker/Dockerfile.rust SERVICE=game-server
build_image game-proxy game-proxy deploy/docker/Dockerfile.rust SERVICE=game-proxy
build_image chat-server chat-server deploy/docker/Dockerfile.rust SERVICE=chat-server
build_image match-service match-service deploy/docker/Dockerfile.rust SERVICE=match-service

build_image auth-http auth-http deploy/docker/Dockerfile.node SERVICE=auth-http
build_image admin-api admin-api deploy/docker/Dockerfile.node SERVICE=admin-api
build_image announce-service announce-service deploy/docker/Dockerfile.node SERVICE=announce-service
build_image mail-service mail-service deploy/docker/Dockerfile.node SERVICE=mail-service
build_image metrics-collector metrics-collector deploy/docker/Dockerfile.node SERVICE=metrics-collector
build_image caddy caddy deploy/docker/Dockerfile.caddy

if [ "$push" = true ]; then
  node scripts/docker/write-images-lock.mjs \
    --output "$lock_file" \
    --release-id "$release_tag" \
    --revision "$revision" \
    --created-at "$created_at" \
    --source "$source_url" \
    --platform linux/amd64 \
    --records "$records_file" \
    --dirty "$dirty"
  scripts/docker/verify-release.sh "$lock_file"
  echo "Published release lock: ${lock_file}"
else
  echo "Built local images with tag ${release_tag}. Run again with --push after docker login and a clean worktree."
fi
