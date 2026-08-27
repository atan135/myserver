#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: vector-preflight.sh [--release-dir <absolute-dir>] [--allow-missing]

Checks the host Vector installation, log paths, Docker local logging options,
and the isolation boundary for the production application services. It is
read-only and never starts, stops, or recreates a service.
EOF
  exit 64
}

readonly required_vector_version=0.47.0
readonly log_root=/data/myserver/log
readonly state_root=/var/lib/vector
readonly config=/etc/vector/vector.yaml
readonly vector_unit=vector.service
readonly services=(
  game-server game-proxy auth-http admin-api chat-server match-service
  mail-service announce-service metrics-collector
)
release_dir=/data/myserver/release/current
allow_missing=false
while [[ $# -gt 0 ]]; do
  case "$1" in
    --release-dir) release_dir="${2:-}"; shift 2 ;;
    --allow-missing) allow_missing=true; shift ;;
    --help|-h) usage ;;
    *) usage ;;
  esac
done
[[ "$release_dir" == /* ]] || { echo 'release directory must be absolute.' >&2; exit 64; }

die() { printf 'vector_preflight=failed reason=%s\n' "$*" >&2; exit 1; }
warn() { printf 'vector_preflight=warning reason=%s\n' "$*" >&2; }
require_command() { command -v "$1" >/dev/null || die "missing_command_$1"; }
for command in docker systemctl stat readlink findmnt df date awk runuser; do require_command "$command"; done

assert_dir() {
  local path="$1" expected_mode="$2"
  [[ -d "$path" && ! -L "$path" ]] || die "unsafe_or_missing_directory_$path"
  [[ "$(readlink -f "$path")" == "$path" ]] || die "directory_resolves_outside_contract_$path"
  [[ "$(stat -c '%a' "$path")" == "$expected_mode" ]] || die "directory_mode_$path"
}
assert_dir /data 755
assert_dir /data/myserver 755
assert_dir "$log_root" 750
assert_dir "$state_root" 750
for directory in "$state_root/buffer" "$state_root/checkpoints" "$state_root/queue"; do
  [[ -d "$directory" && ! -L "$directory" ]] || die "unsafe_or_missing_directory_$directory"
  [[ "$(readlink -f "$directory")" == "$directory" ]] || die "directory_resolves_outside_contract_$directory"
done

mount_info="$(findmnt -T /data -no TARGET,FSTYPE,OPTIONS 2>/dev/null || true)"
[[ -n "$mount_info" ]] || die data_mount_unavailable
printf 'vector_preflight=data_mount %s\n' "$mount_info"
for path in "$log_root" "$state_root"; do
  [[ -w "$path" ]] || die "root_not_writable_$path"
done
if id vector >/dev/null 2>&1; then
  for path in "$log_root" "$state_root" "$state_root/buffer" "$state_root/checkpoints" "$state_root/queue"; do
    runuser -u vector -- test -w "$path" || die "vector_not_writable_$path"
    owner_group="$(stat -c '%U:%G' "$path")"
    [[ "$owner_group" == vector:vector ]] || die "owner_group_$path"
  done
else
  die vector_user_missing
fi

[[ -f "$config" && ! -L "$config" ]] || die vector_config_missing_or_symlink
[[ -x /usr/bin/vector ]] || die vector_binary_missing
version_output="$(/usr/bin/vector --version 2>/dev/null || true)"
[[ "$version_output" == *"$required_vector_version"* ]] || die vector_version_mismatch
/usr/bin/vector validate --config "$config" >/dev/null || die vector_config_invalid
systemctl is-active --quiet "$vector_unit" || die vector_unit_inactive
systemctl is-enabled --quiet "$vector_unit" || warn vector_unit_not_enabled

compose_file="$release_dir/compose.production.yml"
env_file="$release_dir/compose.production.env"
[[ -f "$compose_file" && ! -L "$compose_file" ]] || die compose_file_missing
[[ -f "$env_file" && ! -L "$env_file" ]] || die compose_env_missing
compose=(docker compose --env-file "$env_file" -f "$compose_file")

for service in "${services[@]}"; do
  container_id="$("${compose[@]}" ps -q --all "$service" 2>/dev/null || true)"
  if [[ -z "$container_id" ]]; then
    if [[ "$allow_missing" == true ]]; then
      warn "container_missing_$service"
      continue
    fi
    die "container_missing_$service"
  fi
  [[ "$container_id" != *$'\n'* ]] || die "multiple_containers_$service"
  driver="$(docker inspect --format '{{.HostConfig.LogConfig.Type}}' "$container_id")"
  max_size="$(docker inspect --format '{{index .HostConfig.LogConfig.Config "max-size"}}' "$container_id")"
  max_file="$(docker inspect --format '{{index .HostConfig.LogConfig.Config "max-file"}}' "$container_id")"
  [[ "$driver" == local ]] || die "logging_driver_${service}_$driver"
  [[ "$max_size" == 20m ]] || die "logging_max_size_${service}_$max_size"
  [[ "$max_file" == 3 ]] || die "logging_max_file_${service}_$max_file"
  label_service="$(docker inspect --format '{{index .Config.Labels "com.docker.compose.service"}}' "$container_id")"
  [[ "$label_service" == "$service" ]] || die "service_label_${service}"
  mounts="$(docker inspect --format '{{range .Mounts}}{{println .Source "\t" .Destination}}{{end}}' "$container_id")"
  [[ "$mounts" != *'/var/run/docker.sock'* && "$mounts" != *'/data/myserver/log'* ]] || die "unsafe_host_mount_${service}"
  printf 'vector_preflight=container service=%s id=%s driver=%s max_size=%s max_file=%s\n' \
    "$service" "${container_id:0:12}" "$driver" "$max_size" "$max_file"
done

latest_output="$(find "$log_root" -type f \( -name '*.jsonl' -o -name '*.jsonl.open' \) ! -name '.*' -printf '%T@ %p\n' 2>/dev/null | sort -nr | head -n 1 || true)"
if [[ -n "$latest_output" ]]; then
  latest_epoch="${latest_output%% *}"
  now_epoch="$(date -u +%s)"
  age_seconds=$((now_epoch - ${latest_epoch%.*}))
  (( age_seconds >= 0 )) || age_seconds=0
  printf 'vector_preflight=output latest=%s age_seconds=%s\n' "${latest_output#* }" "$age_seconds"
  (( age_seconds <= 300 )) || warn output_latency_over_300_seconds
else
  [[ "$allow_missing" == true ]] || die output_missing
  warn output_missing
fi
if command -v curl >/dev/null 2>&1; then
  curl --fail --silent --show-error --max-time 2 http://127.0.0.1:8686/metrics >/dev/null || die vector_api_unreachable
  printf 'vector_preflight=api reachable=true\n'
else
  warn curl_missing_api_not_checked
fi
df -P "$log_root" | tail -n 1 | awk '{print "vector_preflight=disk used_percent=" $5 " available_kib=" $4}'
printf 'vector_preflight=passed version=%s log_root=%s state_root=%s\n' \
  "$required_vector_version" "$log_root" "$state_root"
