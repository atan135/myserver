#!/usr/bin/env bash
set -euo pipefail

# Read-only Vector health/threshold probe. It never removes or truncates logs.
log_root=/data/myserver/log
api_url=http://127.0.0.1:8686/metrics
warn_percent=20
protect_percent=10
critical_percent=5

usage() {
  cat <<'EOF'
Usage: vector-alerts.sh [--log-root <absolute-dir>] [--api-url <url>]
       [--warn-percent <n>] [--protect-percent <n>] [--critical-percent <n>]

Read-only Vector metrics and disk protection probe. It never deletes logs.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --log-root) log_root="${2:-}"; shift 2 ;;
    --api-url) api_url="${2:-}"; shift 2 ;;
    --warn-percent) warn_percent="${2:-}"; shift 2 ;;
    --protect-percent) protect_percent="${2:-}"; shift 2 ;;
    --critical-percent) critical_percent="${2:-}"; shift 2 ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; exit 64 ;;
  esac
done

[[ "$log_root" == /* && ! -L "$log_root" ]] || { echo 'log-root must be an absolute non-symlink path.' >&2; exit 64; }
for value in "$warn_percent" "$protect_percent" "$critical_percent"; do
  [[ "$value" =~ ^[0-9]+$ ]] || { echo 'thresholds must be non-negative integers.' >&2; exit 64; }
done
(( warn_percent > protect_percent && protect_percent > critical_percent )) || {
  echo 'thresholds must be ordered warn > protect > critical.' >&2; exit 64;
}

captured_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
disk_status=unknown
disk_free_percent=unknown
disk_available_kib=unknown
if [[ -d "$log_root" ]]; then
  disk_line="$(df -P "$log_root" 2>/dev/null | tail -n 1 || true)"
  used_percent="$(awk '{gsub(/%/, "", $5); print $5}' <<<"$disk_line")"
  disk_available_kib="$(awk '{print $4}' <<<"$disk_line")"
  if [[ "$used_percent" =~ ^[0-9]+$ && "$disk_available_kib" =~ ^[0-9]+$ ]]; then
    disk_free_percent=$((100 - used_percent))
    if (( disk_free_percent < critical_percent )); then
      disk_status=critical
    elif (( disk_free_percent < protect_percent )); then
      disk_status=protect
    elif (( disk_free_percent < warn_percent )); then
      disk_status=warning
    else
      disk_status=ok
    fi
  fi
else
  disk_status=missing
fi

metrics_status=unreachable
metrics_payload=''
if command -v curl >/dev/null 2>&1; then
  metrics_payload="$(curl --fail --silent --show-error --max-time 2 "$api_url" 2>/dev/null || true)"
  [[ -n "$metrics_payload" ]] && metrics_status=reachable
fi

metric_value() {
  local pattern="$1"
  awk -v pattern="$pattern" '$0 ~ pattern && $0 !~ /^#/ { value=$NF } END { if (value == "") print "unknown"; else print value }' <<<"$metrics_payload"
}

received_events="$(metric_value 'component_received_events_total')"
sent_events="$(metric_value 'component_sent_events_total')"
queue_bytes="$(metric_value 'buffer_byte_size')"
component_errors="$(metric_value 'component_errors_total')"
retries="$(metric_value 'component_retries_total|component_retry_total')"
dropped_events="$(metric_value 'component_discarded_events_total|component_dropped_events_total')"

action=none
case "$disk_status" in
  warning) action=alert_only_suspend_low_priority_archiving ;;
  protect) action=protect_queue_retain_unarchived_no_delete ;;
  critical) action=stop_retention_expand_or_transfer_retain_unarchived ;;
  missing) action=alert_missing_log_root ;;
esac

printf 'vector_alert schema=vector.alert.v1 captured_at=%s log_root=%s disk_status=%s disk_free_percent=%s disk_available_kib=%s metrics_status=%s received_events=%s sent_events=%s queue_bytes=%s component_errors=%s retries=%s dropped_events=%s action=%s\n' \
  "$captured_at" "$log_root" "$disk_status" "$disk_free_percent" "$disk_available_kib" "$metrics_status" \
  "$received_events" "$sent_events" "$queue_bytes" "$component_errors" "$retries" "$dropped_events" "$action"

case "$disk_status" in
  critical|missing) exit 2 ;;
  protect) exit 1 ;;
  *) exit 0 ;;
esac
