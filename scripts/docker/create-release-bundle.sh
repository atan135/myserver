#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  scripts/docker/create-release-bundle.sh --output <directory> \
    --caddy-auth-host <domain> --caddy-admin-host <domain> --caddy-chat-host <domain> --caddy-email <email> \
    --game-proxy-advertised-host <host> [--release-root <server-release-directory>]

Creates a server-ready, non-secret release bundle from the current schemaVersion 2
images.lock.json. The output directory must not already contain files. When
provided, --release-root must be the full deployed directory including the
release ID; its default is /data/myserver/release/<release-id>.
EOF
}

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
output=""
release_root=""
caddy_auth_host=""
caddy_admin_host=""
caddy_chat_host=""
caddy_email=""
game_proxy_advertised_host=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --output)
      output="${2:?--output requires a value}"
      shift 2
      ;;
    --release-root)
      release_root="${2:?--release-root requires a value}"
      shift 2
      ;;
    --caddy-auth-host)
      caddy_auth_host="${2:?--caddy-auth-host requires a value}"
      shift 2
      ;;
    --caddy-admin-host)
      caddy_admin_host="${2:?--caddy-admin-host requires a value}"
      shift 2
      ;;
    --caddy-chat-host)
      caddy_chat_host="${2:?--caddy-chat-host requires a value}"
      shift 2
      ;;
    --caddy-email)
      caddy_email="${2:?--caddy-email requires a value}"
      shift 2
      ;;
    --game-proxy-advertised-host)
      game_proxy_advertised_host="${2:?--game-proxy-advertised-host requires a value}"
      shift 2
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

for value in "$output" "$caddy_auth_host" "$caddy_admin_host" "$caddy_chat_host" "$caddy_email" "$game_proxy_advertised_host"; do
  if [ -z "$value" ]; then
    echo "--output, all Caddy domains, Caddy email and game-proxy advertised host are required." >&2
    exit 64
  fi
done

for command in node sha256sum; do
  command -v "$command" >/dev/null || {
    echo "Required command is unavailable: $command" >&2
    exit 69
  }
done

cd "$root"
scripts/docker/verify-release.sh --production deploy/docker/images.lock.json
release_id="$(node -e "const lock = require('./deploy/docker/images.lock.json'); process.stdout.write(lock.releaseId)")"
case "$release_id" in
  *-docker-test-*)
    echo "Refusing to create a production bundle from Docker test release $release_id." >&2
    exit 65
    ;;
esac

if [ -z "$release_root" ]; then
  release_root="/data/myserver/release/$release_id"
fi
if [[ "$release_root" != /* ]]; then
  echo "--release-root must be an absolute Linux server path." >&2
  exit 64
fi

if [ -e "$output" ] && [ -n "$(find "$output" -mindepth 1 -maxdepth 1 -print -quit)" ]; then
  echo "Refusing to overwrite a non-empty release bundle directory: $output" >&2
  exit 65
fi

install -d -m 0755 "$output"
install -d -m 0755 "$output/config" "$output/postgres-bootstrap" "$output/db" "$output/apps/game-server" "$output/scripts" "$output/scripts/ops"
install -m 0644 deploy/docker/compose.production.yml "$output/compose.production.yml"
install -m 0644 deploy/docker/compose.production.env.example "$output/compose.production.env.example"
install -m 0644 deploy/docker/images.lock.json "$output/images.lock.json"
install -m 0644 deploy/docker/infrastructure-images.json "$output/infrastructure-images.json"
cp -a deploy/docker/config/. "$output/config/"
cp -a deploy/docker/postgres-bootstrap/. "$output/postgres-bootstrap/"
cp -a db/. "$output/db/"
cp -a apps/game-server/csv "$output/apps/game-server/csv"
cp -a apps/game-server/scene "$output/apps/game-server/scene"
install -m 0755 scripts/docker/initialize-production-secrets.sh "$output/scripts/initialize-production-secrets.sh"
install -m 0755 scripts/docker/readiness-convergence.sh "$output/scripts/readiness-convergence.sh"
install -m 0644 scripts/docker/release-readiness-probe.mjs "$output/scripts/release-readiness-probe.mjs"
install -m 0755 scripts/docker/install-ops-scripts.sh "$output/scripts/install-ops-scripts.sh"
install -m 0755 scripts/docker/server-apply-release.sh "$output/scripts/server-apply-release.sh"
for script in \
  ops-common.sh ops-deploy.sh ops-disk-report.sh ops-health.sh ops-logs.sh \
  ops-replace.sh ops-restart.sh ops-retire.sh ops-rollback.sh ops-status.sh; do
  install -m 0755 "deploy/docker/scripts/$script" "$output/scripts/ops/$script"
done

node scripts/docker/render-release-env.mjs \
  --lock deploy/docker/images.lock.json \
  --template deploy/docker/compose.production.env.example \
  --output "$output/compose.production.env" \
  --release-root "$release_root" \
  --caddy-auth-host "$caddy_auth_host" \
  --caddy-admin-host "$caddy_admin_host" \
  --caddy-chat-host "$caddy_chat_host" \
  --caddy-email "$caddy_email" \
  --game-proxy-advertised-host "$game_proxy_advertised_host"

printf 'release_id=%s\nrevision=%s\nplatform=linux/amd64\n' \
  "$release_id" \
  "$(node -e "const lock = require('./deploy/docker/images.lock.json'); process.stdout.write(lock.revision)")" \
  > "$output/RELEASE"
(
  cd "$output"
  find . -type f ! -path './SHA256SUMS' -print0 | sort -z | xargs -0 sha256sum > SHA256SUMS
)
printf 'Created release bundle: %s\n' "$output"
