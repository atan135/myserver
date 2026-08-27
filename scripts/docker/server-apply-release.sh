#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: /data/myserver/apply-release.sh --release-id <id> --rollback-db-compatible [--actor <identity>]

Applies an uploaded, checksummed release bundle. This is an update workflow fo
an initialized production database; it never creates or replaces secret files.
EOF
}

release_id=""
rollback_attempt=false
rollback_db_compatible=false
readiness_source_release_id=""
actor="${USER:-operator}@$(hostname -s)"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --release-id) release_id="${2:?--release-id requires a value}"; shift 2 ;;
    --actor) actor="${2:?--actor requires a value}"; shift 2 ;;
    --rollback-db-compatible) rollback_db_compatible=true; shift ;;
    --rollback-attempt) rollback_attempt=true; shift ;;
    --readiness-source-release) readiness_source_release_id="${2:?--readiness-source-release requires a value}"; shift 2 ;;
    --help|-h) usage; exit 0 ;;
    *) echo "Unknown option: $1" >&2; usage >&2; exit 64 ;;
  esac
done

[[ -n "$release_id" ]] || { usage >&2; exit 64; }
[[ "$rollback_db_compatible" == true ]] || {
  echo "--rollback-db-compatible is required: confirm the previous application release can run against forward-applied database migrations." >&2
  exit 64
}
if [[ -n "$readiness_source_release_id" && "$rollback_attempt" != true ]]; then
  echo "--readiness-source-release is restricted to an internal rollback attempt." >&2
  exit 64
fi
if [[ "$rollback_attempt" == true && -z "$readiness_source_release_id" ]]; then
  echo "--rollback-attempt requires --readiness-source-release." >&2
  exit 64
fi
case "$release_id" in *[!A-Za-z0-9._-]*|'') echo "Invalid release ID: $release_id" >&2; exit 64 ;; esac
case "$readiness_source_release_id" in *[!A-Za-z0-9._-]*) echo "Invalid readiness source release ID: $readiness_source_release_id" >&2; exit 64 ;; esac
[[ -z "$readiness_source_release_id" || "$readiness_source_release_id" != "$release_id" ]] || {
  echo "Readiness source release must differ from the rollback target." >&2
  exit 64
}

release_root=/data/myserver/release
release_dir="$release_root/$release_id"
ops_state_root="${MYSERVER_OPS_STATE_ROOT:-/data/myserver/run}"
ops_lock_file="$ops_state_root/operations.lock"
ops_retire_journal="$ops_state_root/pending-game-server-retire"
ops_install_journal="$ops_state_root/pending-ops-install"
ops_lock_fd=9
for command in docker sha256sum awk readlink date flock install; do
  command -v "$command" >/dev/null || { echo "Required command is unavailable: $command" >&2; exit 69; }
done

install -d -m 0700 "$ops_state_root"
expected_lock_path="$(readlink -f "$ops_lock_file")"
if [[ "${MYSERVER_OPS_LOCK_FD:-}" == "$ops_lock_fd" ]]; then
  inherited_fd_path="$(readlink -f "/proc/$$/fd/$ops_lock_fd" 2>/dev/null || true)"
  [[ -n "$inherited_fd_path" && "$inherited_fd_path" == "$expected_lock_path" ]] || {
    echo "Inherited operations lock descriptor is invalid." >&2
    exit 65
  }
  flock -n "$ops_lock_fd" || { echo "Inherited operations lock is not held." >&2; exit 75; }
else
  exec 9>"$ops_lock_file"
  flock -n "$ops_lock_fd" || { echo "Another mutating operation is already running." >&2; exit 75; }
  export MYSERVER_OPS_LOCK_FD="$ops_lock_fd"
fi
[[ ! -e "$ops_install_journal" ]] || {
  echo "Pending ops install recovery is required before release apply." >&2
  exit 75
}
[[ ! -e "$ops_retire_journal" ]] || {
  echo "Pending game-server retire recovery is required before release apply." >&2
  exit 75
}

verify_release_bundle() {
  local candidate_dir="$1"
  local expected_release_id="$2"
  local actual_release_id
  [[ -d "$candidate_dir" && -f "$candidate_dir/RELEASE" && -f "$candidate_dir/SHA256SUMS" ]] || {
    echo "Release bundle is incomplete: $expected_release_id" >&2
    return 65
  }
  actual_release_id="$(awk -F= '$1 == "release_id" { print $2 }' "$candidate_dir/RELEASE")"
  [[ "$actual_release_id" == "$expected_release_id" ]] || {
    echo "Release manifest identity mismatch: $expected_release_id" >&2
    return 65
  }
  (cd "$candidate_dir" && sha256sum --check --status SHA256SUMS) || {
    echo "Release bundle checksum failed: $expected_release_id" >&2
    return 65
  }
}

verify_release_bundle "$release_dir" "$release_id"
cd "$release_dir"
compose=(docker compose --env-file compose.production.env -f compose.production.yml)
application_services=(
  game-server match-service chat-server mail-service announce-service
  metrics-collector game-proxy auth-http admin-api caddy
)
readiness_source_dir="$release_dir"
if [[ -n "$readiness_source_release_id" ]]; then
  readiness_source_dir="$release_root/$readiness_source_release_id"
  verify_release_bundle "$readiness_source_dir" "$readiness_source_release_id"
fi
[[ -r "$readiness_source_dir/scripts/readiness-convergence.sh" && \
   -r "$readiness_source_dir/scripts/release-readiness-probe.mjs" && \
   -r "$readiness_source_dir/compose.production.yml" && \
   -r "$readiness_source_dir/compose.production.env" ]] || {
  echo "Readiness source release is incomplete: ${readiness_source_release_id:-$release_id}" >&2
  exit 65
}
vector_preflight="$release_dir/scripts/vector-preflight.sh"
[[ -x "$vector_preflight" ]] || {
  echo "Release is missing Vector preflight: $vector_preflight" >&2
  exit 65
}
export RELEASE_READINESS_PROBE_FILE="$readiness_source_dir/scripts/release-readiness-probe.mjs"
source "$readiness_source_dir/scripts/readiness-convergence.sh"
readiness_compose=(docker compose --env-file "$readiness_source_dir/compose.production.env" \
  -f "$readiness_source_dir/compose.production.yml")
release_compose_command() {
  "${readiness_compose[@]}" "$@"
}
"${compose[@]}" config --quiet

# Vector must already be healthy before any business container is observed or
# replaced. Missing containers are allowed here because this is also the first
# release on a host; a strict driver/output check runs after application start.
"$vector_preflight" --release-dir "$release_dir" --allow-missing

previous_release_dir="$(readlink -f "$release_root/current" 2>/dev/null || true)"
previous_release_id=""
if [[ -n "$previous_release_dir" && "$previous_release_dir" != "$release_dir" ]]; then
  previous_release_id="${previous_release_dir##*/}"
fi

rollback_previous_release() {
  if [[ "$rollback_attempt" == true || "$rollback_db_compatible" != true || -z "$previous_release_id" ]]; then
    printf 'rollback_state=unavailable previous_release=%s\n' "${previous_release_id:-none}" >&2
    return 1
  fi
  printf 'rollback_state=starting previous_release=%s failed_release=%s\n' \
    "$previous_release_id" "$release_id" >&2
  if /data/myserver/apply-release.sh --release-id "$previous_release_id" \
      --actor "$actor" --rollback-db-compatible --rollback-attempt \
      --readiness-source-release "$release_id"; then
    printf 'rollback_state=converged previous_release=%s\n' "$previous_release_id" >&2
    return 0
  fi
  printf 'rollback_state=failed previous_release=%s\n' "$previous_release_id" >&2
  return 1
}

assert_chat_server_replica_count() {
  local expected="$1"
  local -a containers=()
  mapfile -t containers < <("${compose[@]}" ps -q chat-server)
  if (( ${#containers[@]} != expected )); then
    echo "Production chat-server replica gate requires $expected instance(s), found ${#containers[@]}." >&2
    echo "Do not use docker compose --scale chat-server with this release topology: every replica would inherit SERVICE_INSTANCE_ID=chat-server-1." >&2
    return 1
  fi
}

# Reject a previously overridden --scale before an update can replace or route
# traffic through duplicate chat-server instance IDs.
declare -a existing_chat_servers=()
mapfile -t existing_chat_servers < <("${compose[@]}" ps -q chat-server)
if (( ${#existing_chat_servers[@]} > 1 )); then
  echo "Production chat-server replica gate requires at most one existing instance, found ${#existing_chat_servers[@]}." >&2
  echo "Resolve the duplicate deployment before applying a release; do not scale this topology in place." >&2
  exit 65
fi

"${compose[@]}" pull "${application_services[@]}"
"${readiness_compose[@]}" --profile ops pull migration-runner
target_game_server_instance_id="$(
  "${compose[@]}" config --format json | \
    "${readiness_compose[@]}" --profile ops run --rm --no-deps -T --entrypoint node \
      --volume "$RELEASE_READINESS_PROBE_FILE:/app/tools/release-readiness-probe.mjs:ro" \
      migration-runner /app/tools/release-readiness-probe.mjs --extract-game-server-instance-id
)"
[[ "$target_game_server_instance_id" =~ ^[A-Za-z0-9_.:-]{1,128}$ ]] || {
  echo "Invalid resolved game-server SERVICE_INSTANCE_ID." >&2
  exit 65
}
export MYSERVER_RELEASE_GAME_SERVER_INSTANCE_ID="$target_game_server_instance_id"

resolved_infrastructure_images="$(
  "${compose[@]}" config --format json | \
    "${readiness_compose[@]}" --profile ops run --rm --no-deps -T --entrypoint node \
      --volume "$RELEASE_READINESS_PROBE_FILE:/app/tools/release-readiness-probe.mjs:ro" \
      --volume "$release_dir/images.lock.json:/app/release/images.lock.json:ro" \
      migration-runner /app/tools/release-readiness-probe.mjs \
        --extract-infrastructure-images /app/release/images.lock.json
)"
declare -A expected_infrastructure_images=()
while IFS=$'\t' read -r service image_reference; do
  [[ -n "$service" && -n "$image_reference" ]] || continue
  case "$service" in
    postgres|redis|nats) expected_infrastructure_images["$service"]="$image_reference" ;;
    *)
      printf 'infrastructure_gate_failure service=%s reason=unexpected_lock_service\n' "$service" >&2
      exit 65
      ;;
  esac
done <<< "$resolved_infrastructure_images"
if (( ${#expected_infrastructure_images[@]} != 3 )); then
  printf 'infrastructure_gate_failure service=all reason=incomplete_schema_v2_lock expected=3 actual=%s\n' \
    "${#expected_infrastructure_images[@]}" >&2
  exit 65
fi

assert_existing_infrastructure_healthy() {
  local service="$1"
  local expected_image="$2"
  local container expected_image_id running_image_id running_image_reference running status attempt
  local -a containers=()
  mapfile -t containers < <("${compose[@]}" ps --all --quiet "$service")
  if (( ${#containers[@]} != 1 )); then
    printf 'infrastructure_gate_failure service=%s reason=container_count expected=1 actual=%s\n' \
      "$service" "${#containers[@]}" >&2
    return 1
  fi
  container="${containers[0]}"
  if ! running="$(docker inspect --format '{{.State.Running}}' "$container")"; then
    printf 'infrastructure_gate_failure service=%s reason=container_inspect_failed\n' \
      "$service" >&2
    return 1
  fi
  if [[ "$running" != true ]]; then
    printf 'infrastructure_gate_failure service=%s reason=not_running\n' "$service" >&2
    return 1
  fi
  if ! running_image_reference="$(docker inspect --format '{{.Config.Image}}' "$container")"; then
    printf 'infrastructure_gate_failure service=%s reason=container_image_inspect_failed\n' \
      "$service" >&2
    return 1
  fi
  if [[ "$running_image_reference" != "$expected_image" ]]; then
    printf 'infrastructure_gate_failure service=%s reason=image_mismatch\n' "$service" >&2
    return 1
  fi
  if ! expected_image_id="$(docker image inspect --format '{{.Id}}' "$expected_image")"; then
    printf 'infrastructure_gate_failure service=%s reason=expected_image_unavailable\n' \
      "$service" >&2
    return 1
  fi
  if ! running_image_id="$(docker inspect --format '{{.Image}}' "$container")"; then
    printf 'infrastructure_gate_failure service=%s reason=runtime_image_inspect_failed\n' \
      "$service" >&2
    return 1
  fi
  if [[ "$running_image_id" != "$expected_image_id" ]]; then
    printf 'infrastructure_gate_failure service=%s reason=runtime_image_id_mismatch\n' \
      "$service" >&2
    return 1
  fi
  for attempt in $(seq 1 45); do
    status="$(docker inspect --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' "$container")"
    if [[ "$status" == healthy ]]; then
      mapfile -t containers < <("${compose[@]}" ps --all --quiet "$service")
      if (( ${#containers[@]} == 1 )) && [[ "${containers[0]}" == "$container" ]]; then
        printf 'infrastructure_gate_pass service=%s state=running health=healthy image_match=true\n' \
          "$service"
        return 0
      fi
      printf 'infrastructure_gate_failure service=%s reason=container_changed_during_gate\n' \
        "$service" >&2
      return 1
    fi
    sleep 2
  done
  printf 'infrastructure_gate_failure service=%s reason=health_timeout health=%s\n' \
    "$service" "$status" >&2
  return 1
}
for infrastructure_service in postgres redis nats; do
  assert_existing_infrastructure_healthy \
    "$infrastructure_service" \
    "${expected_infrastructure_images[$infrastructure_service]}"
done
if [[ "$rollback_attempt" == true ]]; then
  printf 'database_migration_state=preserved readiness_source_release=%s\n' \
    "$readiness_source_release_id"
fi

if [[ "$rollback_attempt" == false ]]; then
  # The runner refuses invalid history, pending unapproved migrations and missing backup evidence.
  "${compose[@]}" --profile ops run --rm --no-deps migration-runner preflight --environment production
  "${compose[@]}" --profile ops run --rm --no-deps migration-runner apply --environment production --actor "$actor"
fi

"${compose[@]}" up -d --no-deps game-server match-service chat-server mail-service announce-service \
  metrics-collector game-proxy auth-http admin-api
assert_chat_server_replica_count 1
"$vector_preflight" --release-dir "$release_dir"

if wait_for_release_readiness; then
  :
else
  readiness_status=$?
  printf 'release_failure release=%s stage=readiness error_code=READINESS_CONVERGENCE_TIMEOUT\n' \
    "$release_id" >&2
  rollback_previous_release || true
  exit "$readiness_status"
fi

"${readiness_compose[@]}" --profile ops run --rm --no-deps migration-runner \
  postflight --environment production --check-readiness --require-readiness
"${compose[@]}" up -d --no-deps caddy

ln -sfn "$release_dir" "$release_root/current"
printf 'current_release=%s\n' "$(readlink -f "$release_root/current")"
"${compose[@]}" ps
