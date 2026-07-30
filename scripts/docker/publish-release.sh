#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/docker/publish-release.sh

Updates the current clean WSL-native checkout, publishes all application images,
then commits and pushes the generated digest lock. Docker login must already be
configured for the selected registry.
EOF
}

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  usage
  exit 0
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
case "$root" in
  /mnt/*)
    echo "Run this script from a WSL-native checkout, not a /mnt/* mount: $root" >&2
    exit 65
    ;;
esac

for command in git node docker; do
  command -v "$command" >/dev/null || {
    echo "Required command is unavailable: $command" >&2
    exit 69
  }
done

cd "$root"
if [[ -n "$(git status --porcelain --untracked-files=all)" ]]; then
  echo "Refusing to publish from a dirty worktree. Commit or stash local changes first." >&2
  exit 65
fi

branch="$(git branch --show-current)"
if [[ -z "$branch" ]]; then
  echo "Publishing requires a checked-out branch, not detached HEAD." >&2
  exit 65
fi

git fetch origin "$branch"
git pull --ff-only origin "$branch"

if [[ -n "$(git status --porcelain --untracked-files=all)" ]]; then
  echo "Worktree became dirty after update; refusing to publish." >&2
  exit 65
fi

# The code commit must be on origin before an immutable image tag references it.
git push origin "$branch"

package_version="$(node --input-type=module -e "import pkg from './package.json' with { type: 'json' }; process.stdout.write(pkg.version)")"
revision="$(git rev-parse HEAD)"
short_revision="$(git rev-parse --short=12 HEAD)"
release_tag="v${package_version}-${short_revision}"

./scripts/docker/build-and-push.sh \
  --release-tag "$release_tag" \
  --push

git add -- deploy/docker/images.lock.json
git diff --cached --check
if ! git diff --cached --quiet; then
  git commit \
    -m "build(release): 锁定 ${short_revision} 镜像摘要" \
    -m "记录 ${release_tag} 的应用和基础设施镜像不可变 digest，供服务器按 release bundle 校验拉取。"
  git push origin "$branch"
fi

printf 'release_id=%s\nrevision=%s\n' "$release_tag" "$revision"
