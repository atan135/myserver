#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/docker/rotate-vector-files.sh [--apply]
       [--log-root /data/myserver/log] [--max-bytes 268435456]

Rotates oversized Vector .jsonl.open files into immutable shard files. A
write is performed only with --apply; otherwise the script reports candidates.
--apply briefly stops Vector so its file sink flushes/closes before fsync and
atomic rename, then starts it again from the persistent checkpoint.
EOF
}

log_root=/data/myserver/log
max_bytes=268435456
apply=false
while [[ $# -gt 0 ]]; do
  case "$1" in
    --log-root) log_root="${2:-}"; shift 2 ;;
    --max-bytes) max_bytes="${2:-}"; shift 2 ;;
    --apply) apply=true; shift ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; exit 64 ;;
  esac
done
[[ "$log_root" == /data/myserver/log && "$max_bytes" == 268435456 ]] || {
  echo 'Rotation paths and size must remain under the Vector contract.' >&2; exit 65;
}
for command in find stat sort sync mv flock; do
  command -v "$command" >/dev/null || { echo "Required command is unavailable: $command" >&2; exit 69; }
done
[[ -d "$log_root" && ! -L "$log_root" ]] || { echo 'Vector log root is missing or unsafe.' >&2; exit 65; }

state_dir=/var/lib/vector
lock_file="$state_dir/rotation.lock"
if [[ "$apply" == true ]]; then
  command -v systemctl >/dev/null || { echo 'Required command is unavailable: systemctl' >&2; exit 69; }
  [[ "$(id -u)" == 0 ]] || { echo '--apply requires root.' >&2; exit 77; }
  install -d -m 0750 "$state_dir"
fi

exec 9>"$lock_file"
flock -n 9 || { echo 'rotation_already_running=true' >&2; exit 75; }
vector_stopped=false
cleanup() {
  local status=$?
  if [[ "$vector_stopped" == true ]]; then
    systemctl start vector.service >/dev/null 2>&1 || true
  fi
  exit "$status"
}
trap cleanup EXIT

mapfile -t candidates < <(find "$log_root" -type f -name '*.jsonl.open' -printf '%s %p\n' | sort -n)
rotated=0
for entry in "${candidates[@]}"; do
  size="${entry%% *}"
  file="${entry#* }"
  (( size >= max_bytes )) || continue
  [[ -f "$file" && ! -L "$file" ]] || { echo "unsafe_rotation_candidate=$file" >&2; continue; }
  relative="${file#"$log_root/"}"
  [[ "$relative" != "$file" && "$relative" =~ ^([A-Za-z0-9._-]+)/([0-9]{4}-[0-9]{2}-[0-9]{2})/([A-Za-z0-9._-]+)\.([0-9a-f]{12})\.jsonl\.open$ ]] || {
    echo "invalid_rotation_path=$file" >&2; continue;
  }
  dir="$(dirname "$file")"
  stem="${BASH_REMATCH[3]}"
  container_prefix="${BASH_REMATCH[4]}"
  instance_id="${stem%.$container_prefix}"
  [[ -n "$instance_id" ]] || { echo "invalid_instance_id=$file" >&2; continue; }
  shard=1
  while :; do
    target="$dir/$instance_id.$container_prefix.$(printf '%04d' "$shard").jsonl"
    [[ ! -e "$target" ]] && break
    ((shard++))
    (( shard <= 9999 )) || { echo "shard_exhausted=$file" >&2; continue 2; }
  done
  printf 'rotation_candidate size=%s open=%s target=%s\n' "$size" "$file" "$target"
  [[ "$apply" == true ]] || continue

  systemctl stop vector.service
  vector_stopped=true
  if ! sync -f "$file"; then
    echo "rotation_fsync_failed=$file" >&2
    systemctl start vector.service || true
    exit 74
  fi
  mv -- "$file" "$target"
  sync -f "$target"
  if ! systemctl start vector.service; then
    echo "vector_restart_failed_after_rotation=$target" >&2
    exit 74
  fi
  vector_stopped=false
  rotated=$((rotated + 1))
done

printf 'rotated_files=%s apply=%s\n' "$rotated" "$apply"
