#!/usr/bin/env bash
set -euo pipefail

log_root=/data/myserver/log
state_dir=/var/lib/vector
config=/etc/vector/vector.yaml
if [[ "${1:-}" == '--help' || "${1:-}" == '-h' ]]; then
  printf 'Usage: %s\n' "${0##*/}"
  exit 0
fi
[[ $# -eq 0 ]] || { echo 'No positional arguments are accepted.' >&2; exit 64; }

printf 'vector_config=%s exists=%s\n' "$config" "$([[ -f "$config" ]] && echo true || echo false)"
printf 'vector_state=%s exists=%s\n' "$state_dir" "$([[ -d "$state_dir" ]] && echo true || echo false)"
printf 'vector_log_root=%s exists=%s\n' "$log_root" "$([[ -d "$log_root" ]] && echo true || echo false)"
if command -v systemctl >/dev/null 2>&1; then
  printf 'vector_systemd_active=%s\n' "$(systemctl is-active vector.service 2>/dev/null || true)"
  printf 'vector_systemd_enabled=%s\n' "$(systemctl is-enabled vector.service 2>/dev/null || true)"
fi
if [[ -d "$state_dir" ]]; then
  printf 'vector_checkpoint_files=%s\n' "$(find "$state_dir" -maxdepth 2 -type f \( -name '*.json' -o -name '*.checkpoint' \) 2>/dev/null | wc -l | tr -d ' ')"
  checkpoint_first="$(find "$state_dir" -maxdepth 2 -type f \( -name '*.json' -o -name '*.checkpoint' \) -printf '%T@ %p\n' 2>/dev/null | sort -n | head -n 1 || true)"
  checkpoint_latest="$(find "$state_dir" -maxdepth 2 -type f \( -name '*.json' -o -name '*.checkpoint' \) -printf '%T@ %p\n' 2>/dev/null | sort -nr | head -n 1 || true)"
  printf 'vector_checkpoint_first_mtime=%s\n' "${checkpoint_first%% *}"
  printf 'vector_checkpoint_first=%s\n' "${checkpoint_first#* }"
  printf 'vector_checkpoint_latest=%s\n' "${checkpoint_latest#* }"
  printf 'vector_checkpoint_latest_mtime=%s\n' "${checkpoint_latest%% *}"
fi
printf 'vector_gap_estimate=unknown reason=requires Docker retention and source/sink metric comparison\n'
if [[ -d "$log_root" ]]; then
  latest_closed="$(find "$log_root" -type f -name '*.jsonl' ! -name '*.jsonl.open' -printf '%T@ %p\n' 2>/dev/null | sort -nr | head -n 1 || true)"
  latest_open="$(find "$log_root" -type f -name '*.jsonl.open' -printf '%T@ %p\n' 2>/dev/null | sort -nr | head -n 1 || true)"
  printf 'vector_latest_closed=%s\n' "${latest_closed#* }"
  printf 'vector_latest_open=%s\n' "${latest_open#* }"
  df -P "$log_root" | tail -n 1 | awk '{printf "vector_log_disk_used_percent=%s available_kib=%s\n", $5, $4}'
fi
if command -v curl >/dev/null 2>&1; then
  metrics_payload="$(curl --fail --silent --show-error --max-time 2 http://127.0.0.1:8686/metrics 2>/dev/null || true)"
  if [[ -n "$metrics_payload" ]]; then
    printf 'vector_metrics_endpoint=reachable\n'
    printf '%s\n' "$metrics_payload" | awk '/component_received_events_total|component_sent_events_total|buffer_byte_size|component_errors_total/ {print "vector_metric=" $0}' | head -n 32
  else
    printf 'vector_metrics_endpoint=unreachable\n'
  fi
  if curl --fail --silent --show-error --max-time 2 http://127.0.0.1:8686/health >/dev/null 2>&1; then
    printf 'vector_api_reachable=true\n'
  else
    printf 'vector_api_reachable=false\n'
  fi
fi
