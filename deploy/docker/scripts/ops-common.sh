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

wait_for_target_container() {
  local service="$1"
  local timeout_seconds="$2"
  local deadline container_id state health
  deadline=$((SECONDS + timeout_seconds))

  while (( SECONDS < deadline )); do
    container_id="$(compose ps -q "$service")"
    if [[ -n "$container_id" ]]; then
      state="$(container_state "$container_id")"
      health="$(container_health "$container_id")"
      if [[ "$state" == running && ( "$health" == healthy || "$health" == none ) ]]; then
        return 0
      fi
      case "$state" in
        dead|exited|removing)
          printf 'target_container_failure service=%s state=%s health=%s\n' \
            "$service" "$state" "$health" >&2
          return 1
          ;;
      esac
    else
      state=missing
      health=unknown
    fi
    sleep 2
  done

  printf 'target_container_timeout service=%s state=%s health=%s timeout_seconds=%s\n' \
    "$service" "${state:-unknown}" "${health:-unknown}" "$timeout_seconds" >&2
  return 1
}

release_compose_command() {
  compose "$@"
}

load_release_readiness() {
  local release_dir helper probe
  release_dir="$(current_release_dir)"
  helper="$release_dir/scripts/readiness-convergence.sh"
  probe="$release_dir/scripts/release-readiness-probe.mjs"
  [[ -r "$helper" ]] || die "current release readiness helper is unavailable: $helper"
  [[ -r "$probe" ]] || die "current release readiness probe is unavailable: $probe"
  source "$helper"
}

require_confirmation() {
  local actual="$1"
  local supplied="$2"
  [[ "$actual" == "$supplied" ]] || die "confirmation must exactly match: $actual"
}
