#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: /data/myserver/apply-release.sh --release-id <id> [--actor <identity>]

Applies an uploaded, checksummed release bundle. This is an update workflow fo
an initialized production database; it never creates or replaces secret files.
EOF
}

release_id=""
actor="${USER:-operator}@$(hostname -s)"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --release-id) release_id="${2:?--release-id requires a value}"; shift 2 ;;
    --actor) actor="${2:?--actor requires a value}"; shift 2 ;;
    --help|-h) usage; exit 0 ;;
    *) echo "Unknown option: $1" >&2; usage >&2; exit 64 ;;
  esac
done

[[ -n "$release_id" ]] || { usage >&2; exit 64; }
case "$release_id" in *[!A-Za-z0-9._-]*|'') echo "Invalid release ID: $release_id" >&2; exit 64 ;; esac

release_root=/data/myserver/release
release_dir="$release_root/$release_id"
[[ -d "$release_dir" ]] || { echo "Release directory does not exist: $release_dir" >&2; exit 66; }
[[ -f "$release_dir/RELEASE" && -f "$release_dir/SHA256SUMS" ]] || {
  echo "Release bundle is incomplete: $release_dir" >&2
  exit 65
}
actual_release_id="$(awk -F= '$1 == "release_id" { print $2 }' "$release_dir/RELEASE")"
[[ "$actual_release_id" == "$release_id" ]] || {
  echo "Release manifest mismatch: expected $release_id, got $actual_release_id" >&2
  exit 65
}

for command in docker sha256sum awk readlink; do
  command -v "$command" >/dev/null || { echo "Required command is unavailable: $command" >&2; exit 69; }
done

cd "$release_dir"
sha256sum --check --status SHA256SUMS
compose=(docker compose --env-file compose.production.env -f compose.production.yml)
"${compose[@]}" config --quiet
"${compose[@]}" pull
"${compose[@]}" --profile ops pull migration-runner

"${compose[@]}" up -d postgres redis nats
wait_healthy() {
  local service="$1"
  local container status attempt
  container="$("${compose[@]}" ps -q "$service")"
  [[ -n "$container" ]] || { echo "No container for service: $service" >&2; return 1; }
  for attempt in $(seq 1 45); do
    status="$(docker inspect --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' "$container")"
    if [[ "$status" == healthy ]]; then
      return 0
    fi
    sleep 2
  done
  echo "Service did not become healthy: $service ($status)" >&2
  return 1
}
wait_healthy postgres
wait_healthy redis
wait_healthy nats

# The runner refuses invalid history, pending unapproved migrations and missing backup evidence.
"${compose[@]}" --profile ops run --rm migration-runner preflight --environment production
"${compose[@]}" --profile ops run --rm migration-runner apply --environment production --actor "$actor"

"${compose[@]}" up -d game-server match-service chat-server mail-service announce-service metrics-collector
"${compose[@]}" up -d game-proxy auth-http admin-api
"${compose[@]}" --profile ops run --rm migration-runner \
  postflight --environment production --check-readiness --require-readiness
"${compose[@]}" up -d caddy

ln -sfn "$release_dir" "$release_root/current"
printf 'current_release=%s\n' "$(readlink -f "$release_root/current")"
"${compose[@]}" ps
