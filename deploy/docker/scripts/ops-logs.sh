#!/usr/bin/env bash
set -euo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/ops-common.sh"

usage() {
  printf 'Usage: %s <service> [--date <YYYY-MM-DD>] [--tail <lines>] [--follow]\n' "${0##*/}" >&2
  usage_service
  exit 2
}

[[ $# -ge 1 ]] || usage
service="$1"
shift
require_service "$service"
tail=200
follow=false
requested_date=''

while [[ $# -gt 0 ]]; do
  case "$1" in
    --date) requested_date="${2:-}"; shift 2 ;;
    --tail) tail="${2:-}"; shift 2 ;;
    --follow) follow=true; shift ;;
    *) usage ;;
  esac
done
[[ "$tail" =~ ^[1-9][0-9]*$ ]] || die '--tail must be a positive integer'
[[ -z "$requested_date" || "$requested_date" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]] || die '--date must use UTC YYYY-MM-DD'

readonly VECTOR_LOG_ROOT=/data/myserver/log
vector_fallback() {
  local reason="$1"
  printf 'vector_fallback=true service=%s reason=%s source=docker_logs\n' "$service" "$reason" >&2
  local container_id
  container_id="$(container_id_for "$service")"
  local -a args=(--timestamps --tail "$tail")
  [[ "$follow" == true ]] && args+=(--follow)
  exec docker logs "${args[@]}" "$container_id"
}

[[ -d /data && ! -L /data ]] || vector_fallback data_mount_unavailable
[[ -d "$VECTOR_LOG_ROOT" && ! -L "$VECTOR_LOG_ROOT" ]] || vector_fallback log_root_unavailable
service_dir="$VECTOR_LOG_ROOT/$service"
[[ ! -L "$service_dir" ]] || vector_fallback service_path_symlink
[[ -d "$service_dir" ]] || vector_fallback service_output_absent

if [[ -n "$requested_date" ]]; then
  day_dir="$service_dir/$requested_date"
else
  day_dir=''
  while IFS= read -r candidate; do
    [[ "$candidate" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]] || continue
    day_dir="$service_dir/$candidate"
    break
  done < <(find "$service_dir" -mindepth 1 -maxdepth 1 -type d ! -name '.*' -printf '%f\n' 2>/dev/null | sort -r)
  [[ -n "$day_dir" ]] || vector_fallback date_output_absent
fi
[[ -d "$day_dir" && ! -L "$day_dir" ]] || vector_fallback date_output_absent
resolved_day="$(readlink -f "$day_dir" 2>/dev/null || true)"
[[ "$resolved_day" == "$day_dir" && "$resolved_day" == "$service_dir/"* ]] || vector_fallback date_path_unsafe

mapfile -d '' vector_files < <(find "$day_dir" -mindepth 1 -maxdepth 1 -type f ! -name '*.jsonl.open' -name '*.jsonl' -print0 2>/dev/null | sort -z)
if [[ "$follow" == true ]]; then
  mapfile -d '' open_files < <(find "$day_dir" -mindepth 1 -maxdepth 1 -type f -name '*.jsonl.open' -print0 2>/dev/null | sort -z)
  vector_files+=("${open_files[@]}")
fi
if (( ${#vector_files[@]} == 0 )); then
  vector_fallback "$(if [[ "$follow" == true ]]; then echo active_output_absent; else echo closed_output_absent; fi)"
fi
for file in "${vector_files[@]}"; do
  [[ -f "$file" && ! -L "$file" ]] || vector_fallback output_file_unsafe
done

printf 'vector_source=true service=%s date=%s files=%s\n' "$service" "${day_dir##*/}" "${#vector_files[@]}" >&2
if [[ "$follow" == true ]]; then
  exec tail --follow=name --retry --sleep-interval=2 -n "$tail" "${vector_files[@]}"
fi
cat -- "${vector_files[@]}" | tail -n "$tail"
