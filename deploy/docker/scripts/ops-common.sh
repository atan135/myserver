#!/usr/bin/env bash
# Shared helpers for production Docker operations. This file is sourced by ops-*.sh.
set -euo pipefail

readonly OPS_RELEASE_ROOT="${MYSERVER_RELEASE_ROOT:-/data/myserver/release}"
readonly OPS_SERVICES=(
  postgres redis nats game-server match-service chat-server mail-service
  announce-service metrics-collector game-proxy auth-http admin-api caddy
)

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

usage_service() {
  printf 'allowed services: %s\n' "${OPS_SERVICES[*]}" >&2
}

require_service() {
  local service="$1"
  local candidate
  for candidate in "${OPS_SERVICES[@]}"; do
    [[ "$candidate" == "$service" ]] && return 0
  done
  usage_service
  die "unsupported service: $service"
}

current_release_dir() {
  local release_dir
  release_dir="$(readlink -f "$OPS_RELEASE_ROOT/current")" || die "current release link is unavailable"
  [[ -f "$release_dir/compose.production.yml" ]] || die "current release has no compose.production.yml: $release_dir"
  [[ -f "$release_dir/compose.production.env" ]] || die "current release has no compose.production.env: $release_dir"
  printf '%s\n' "$release_dir"
}

release_dir_for() {
  local release_id="$1"
  [[ "$release_id" =~ ^v[0-9A-Za-z._-]+$ ]] || die "invalid release id: $release_id"
  local release_dir="$OPS_RELEASE_ROOT/$release_id"
  [[ -d "$release_dir" ]] || die "release does not exist: $release_dir"
  [[ -f "$release_dir/compose.production.yml" ]] || die "release is incomplete: $release_dir"
  printf '%s\n' "$release_dir"
}

compose() {
  local release_dir
  release_dir="$(current_release_dir)"
  docker compose --project-directory "$release_dir" --env-file "$release_dir/compose.production.env" -f "$release_dir/compose.production.yml" "$@"
}

container_id_for() {
  local service="$1"
  local container_id
  container_id="$(compose ps -q "$service")"
  [[ -n "$container_id" ]] || die "no container found for service: $service"
  printf '%s\n' "$container_id"
}

container_state() {
  docker inspect --format '{{.State.Status}}' "$1"
}

container_health() {
  docker inspect --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' "$1"
}

wait_for_healthy() {
  local service="$1"
  local timeout_seconds="${2:-120}"
  local started_at="$SECONDS"
  local container_id state health

  while (( SECONDS - started_at < timeout_seconds )); do
    container_id="$(container_id_for "$service")"
    state="$(container_state "$container_id")"
    health="$(container_health "$container_id")"
    printf '%s state=%s health=%s\n' "$service" "$state" "$health"

    [[ "$state" == 'running' && ( "$health" == 'healthy' || "$health" == 'none' ) ]] && return 0
    [[ "$state" == 'exited' || "$state" == 'dead' ]] && die "$service entered terminal state: $state"
    sleep 2
  done

  die "timed out waiting for $service to become healthy"
}

require_confirmation() {
  local actual="$1"
  local supplied="$2"
  [[ "$actual" == "$supplied" ]] || die "confirmation must exactly match: $actual"
}
