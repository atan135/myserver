#!/usr/bin/env bash
# Shared helpers for production Docker operations. This file is sourced by ops-*.sh.
set -euo pipefail

readonly OPS_RELEASE_ROOT="${MYSERVER_RELEASE_ROOT:-/data/myserver/release}"
readonly OPS_STATE_ROOT="${MYSERVER_OPS_STATE_ROOT:-/data/myserver/run}"
readonly OPS_LOCK_FILE="$OPS_STATE_ROOT/operations.lock"
readonly OPS_RETIRE_JOURNAL="$OPS_STATE_ROOT/pending-game-server-retire"
readonly OPS_INSTALL_JOURNAL="$OPS_STATE_ROOT/pending-ops-install"
readonly OPS_LOCK_FD=9
readonly OPS_SERVICES=(
  postgres redis nats game-server match-service chat-server mail-service
  announce-service metrics-collector game-proxy auth-http admin-api caddy
)

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

acquire_mutating_lock() {
  local inherited_fd_path expected_lock_path
  command -v flock >/dev/null || die 'required command is unavailable: flock'
  install -d -m 0700 "$OPS_STATE_ROOT"
  expected_lock_path="$(readlink -f "$OPS_LOCK_FILE")"
  if [[ "${MYSERVER_OPS_LOCK_FD:-}" == "$OPS_LOCK_FD" ]]; then
    inherited_fd_path="$(readlink -f "/proc/$$/fd/$OPS_LOCK_FD" 2>/dev/null || true)"
    [[ -n "$inherited_fd_path" && "$inherited_fd_path" == "$expected_lock_path" ]] ||
      die 'inherited operations lock descriptor is invalid'
    flock -n "$OPS_LOCK_FD" || die 'inherited operations lock is not held'
    return 0
  fi
  exec 9>"$OPS_LOCK_FILE"
  flock -n "$OPS_LOCK_FD" || die 'another mutating operation is already running'
  export MYSERVER_OPS_LOCK_FD="$OPS_LOCK_FD"
}

assert_no_pending_retire() {
  [[ ! -e "$OPS_RETIRE_JOURNAL" ]] ||
    die "pending game-server retire recovery is required: $OPS_RETIRE_JOURNAL"
}

assert_no_pending_ops_install() {
  [[ ! -e "$OPS_INSTALL_JOURNAL" ]] ||
    die "pending ops install recovery is required: $OPS_INSTALL_JOURNAL"
}

retire_journal_value() {
  local key="$1"
  [[ -f "$OPS_RETIRE_JOURNAL" ]] || die 'pending game-server retire journal is unavailable'
  awk -F= -v key="$key" '$1 == key { value = substr($0, length(key) + 2) } END { print value }' \
    "$OPS_RETIRE_JOURNAL"
}

write_retire_journal() {
  local container_id="$1" instance_id="$2" revision="$3" original_policy="$4" phase="$5" project="$6"
  local temporary
  install -d -m 0700 "$OPS_STATE_ROOT"
  umask 077
  temporary="$(mktemp "$OPS_STATE_ROOT/.pending-game-server-retire.XXXXXX")"
  printf '%s\n' \
    'schema=1' \
    'service=game-server' \
    "project=$project" \
    "container_id=$container_id" \
    "instance_id=$instance_id" \
    "revision=$revision" \
    "original_policy=$original_policy" \
    'original_running=true' \
    "phase=$phase" > "$temporary"
  chmod 0600 "$temporary"
  sync "$temporary"
  mv -f "$temporary" "$OPS_RETIRE_JOURNAL"
  sync -f "$OPS_STATE_ROOT"
}

clear_retire_journal() {
  rm -f "$OPS_RETIRE_JOURNAL"
  sync -f "$OPS_STATE_ROOT"
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

container_id_for_all_states() {
  local service="$1"
  local -a container_ids=()
  mapfile -t container_ids < <(compose ps -q --all "$service")
  (( ${#container_ids[@]} == 1 )) ||
    die "expected exactly one container for service $service, found ${#container_ids[@]}"
  printf '%s\n' "${container_ids[0]}"
}

container_env_value() {
  local container_id="$1" key="$2"
  docker inspect --format '{{range .Config.Env}}{{println .}}{{end}}' "$container_id" |
    awk -F= -v key="$key" '$1 == key { value = substr($0, length(key) + 2) } END { print value }'
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
