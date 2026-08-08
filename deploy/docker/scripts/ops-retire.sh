#!/usr/bin/env bash
set -euo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/ops-common.sh"

usage() {
  cat >&2 <<'EOF'
Usage: ops-retire.sh game-server --instance-id <id> --revision <full-sha> \
  [--project <compose-project>] --confirm <id>@<full-sha>@<project> [--timeout <seconds>]
       ops-retire.sh game-server --instance-id <id> --revision <full-sha> \
  [--project <compose-project>] --confirm <id>@<full-sha>@<project> --recover

Coordinates Docker desired state while an independently approved admin-api
shutdown exits the old game-server. This command accepts no admin credential,
approval nonce or control-plane request identifier.
EOF
  exit 2
}

[[ $# -ge 1 && "$1" == game-server ]] || usage
service="$1"
shift
instance_id=''
revision=''
confirmation=''
project=myserver
timeout=360
recover=false
while [[ $# -gt 0 ]]; do
  case "$1" in
    --instance-id) instance_id="${2:-}"; shift 2 ;;
    --revision) revision="${2:-}"; shift 2 ;;
    --confirm) confirmation="${2:-}"; shift 2 ;;
    --project) project="${2:-}"; shift 2 ;;
    --timeout) timeout="${2:-}"; shift 2 ;;
    --recover) recover=true; shift ;;
    *) usage ;;
  esac
done

[[ "$instance_id" =~ ^[A-Za-z0-9][A-Za-z0-9_.:@-]{0,127}$ ]] || die 'invalid game-server instance id'
[[ "$revision" =~ ^[0-9a-f]{40}$ ]] || die 'revision must be a full lowercase Git SHA'
[[ "$project" =~ ^[a-z0-9][a-z0-9_.-]{0,127}$ ]] || die 'Compose project must use lowercase letters, digits, dot, underscore or hyphen'
[[ "$timeout" =~ ^[1-9][0-9]*$ && "$timeout" -le 900 ]] || die 'timeout must be an integer from 1 to 900 seconds'
require_confirmation "$instance_id@$revision@$project" "$confirmation"

for command in docker awk chmod flock install mktemp mv readlink rm sleep stat sync; do
  command -v "$command" >/dev/null || die "required command is unavailable: $command"
done

acquire_mutating_lock
assert_no_pending_ops_install

retire_compose() {
  local release_dir
  release_dir="$(current_release_dir)"
  docker compose --project-name "$project" --project-directory "$release_dir" \
    --env-file "$release_dir/compose.production.env" -f "$release_dir/compose.production.yml" "$@"
}

inspect_identity() {
  local container_id="$1"
  local compose_service compose_project actual_instance actual_revision
  compose_service="$(docker inspect --format '{{index .Config.Labels "com.docker.compose.service"}}' "$container_id")"
  compose_project="$(docker inspect --format '{{index .Config.Labels "com.docker.compose.project"}}' "$container_id")"
  actual_instance="$(container_env_value "$container_id" SERVICE_INSTANCE_ID)"
  actual_revision="$(docker inspect --format '{{index .Config.Labels "org.opencontainers.image.revision"}}' "$container_id")"
  [[ "$compose_service" == "$service" ]] || die 'target container Compose service does not match game-server'
  [[ "$compose_project" == "$project" ]] || die 'target container Compose project does not match confirmation'
  [[ "$actual_instance" == "$instance_id" ]] || die 'target container instance identity does not match confirmation'
  [[ "$actual_revision" == "$revision" ]] || die 'target container image revision does not match confirmation'
}

restore_retire() {
  local expected_id="$1" original_policy="$2" expected_current state
  expected_current="$(retire_compose ps -q --all "$service" || true)"
  if [[ "$expected_current" != "$expected_id" ]] || ! docker inspect "$expected_id" >/dev/null 2>&1; then
    printf 'retire_recovery_failed reason=target_identity_changed\n' >&2
    return 1
  fi
  inspect_identity "$expected_id" || return 1
  docker update --restart="$original_policy" "$expected_id" >/dev/null
  state="$(container_state "$expected_id")"
  if [[ "$state" == exited || "$state" == dead ]]; then
    docker start "$expected_id" >/dev/null
  fi
  clear_retire_journal
  printf 'retire_recovered service=%s instance=%s revision=%s\n' \
    "$service" "$instance_id" "${revision:0:12}"
}

if [[ "$recover" == true ]]; then
  [[ -f "$OPS_RETIRE_JOURNAL" && ! -L "$OPS_RETIRE_JOURNAL" ]] || die 'no safe pending game-server retire journal is available'
  [[ "$(stat -c '%a' "$OPS_RETIRE_JOURNAL")" == 600 ]] || die 'pending retire journal permissions are invalid'
  [[ "$(retire_journal_value schema)" == 1 ]] || die 'pending retire journal schema is invalid'
  [[ "$(retire_journal_value service)" == "$service" ]] || die 'pending retire service does not match'
  [[ "$(retire_journal_value project)" == "$project" ]] || die 'pending retire Compose project does not match'
  [[ "$(retire_journal_value instance_id)" == "$instance_id" ]] || die 'pending retire instance does not match'
  [[ "$(retire_journal_value revision)" == "$revision" ]] || die 'pending retire revision does not match'
  journal_container_id="$(retire_journal_value container_id)"
  journal_policy="$(retire_journal_value original_policy)"
  journal_phase="$(retire_journal_value phase)"
  [[ "$journal_container_id" =~ ^[0-9a-f]{64}$ ]] || die 'pending retire container id is invalid'
  [[ "$journal_policy" == unless-stopped ]] || die 'pending retire restart policy is invalid'
  [[ "$(retire_journal_value original_running)" == true ]] || die 'pending retire running state is invalid'
  [[ "$journal_phase" == prepared || "$journal_phase" == waiting ]] || die 'pending retire phase is invalid'
  restore_retire "$journal_container_id" "$journal_policy" || die 'pending retire could not be recovered safely'
  exit 0
fi

assert_no_pending_retire
mapfile -t container_ids < <(retire_compose ps -q --all "$service")
(( ${#container_ids[@]} == 1 )) || die "expected exactly one container for service $service, found ${#container_ids[@]}"
container_id="${container_ids[0]}"
inspect_identity "$container_id"
[[ "$(container_state "$container_id")" == running ]] || die 'target game-server container is not running'
original_policy="$(docker inspect --format '{{.HostConfig.RestartPolicy.Name}}' "$container_id")"
[[ "$original_policy" == unless-stopped ]] || die 'target game-server restart policy must be unless-stopped'

restore_required=true
retire_succeeded=false
on_exit() {
  local status=$?
  trap - EXIT INT TERM
  if [[ "$restore_required" == true && "$retire_succeeded" != true ]]; then
    restore_retire "$container_id" "$original_policy" || true
  fi
  exit "$status"
}
trap on_exit EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

write_retire_journal "$container_id" "$instance_id" "$revision" "$original_policy" prepared "$project"
docker update --restart=no "$container_id" >/dev/null
write_retire_journal "$container_id" "$instance_id" "$revision" "$original_policy" waiting "$project"
printf 'retire_state=awaiting_control_plane_shutdown service=%s instance=%s revision=%s container=%s timeout_seconds=%s\n' \
  "$service" "$instance_id" "${revision:0:12}" "${container_id:0:12}" "$timeout"

deadline=$((SECONDS + timeout))
while (( SECONDS < deadline )); do
  current_id="$(retire_compose ps -q --all "$service" || true)"
  [[ "$current_id" == "$container_id" ]] || die 'target game-server container changed during retire'
  state="$(container_state "$container_id")"
  if [[ "$state" == exited ]]; then
    exit_code="$(docker inspect --format '{{.State.ExitCode}}' "$container_id")"
    oom_killed="$(docker inspect --format '{{.State.OOMKilled}}' "$container_id")"
    if [[ "$exit_code" == 0 && "$oom_killed" == false ]]; then
      retire_succeeded=true
      restore_required=false
      clear_retire_journal
      printf 'retire_state=completed service=%s instance=%s revision=%s container=%s exit_code=0 oom_killed=false\n' \
        "$service" "$instance_id" "${revision:0:12}" "${container_id:0:12}"
      exit 0
    fi
    die "target game-server exited unsafely: exit_code=$exit_code oom_killed=$oom_killed"
  fi
  [[ "$state" == running ]] || die "target game-server entered unexpected state: $state"
  sleep 1
done
die "target game-server did not exit within ${timeout}s"
