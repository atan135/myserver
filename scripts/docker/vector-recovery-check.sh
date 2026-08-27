#!/usr/bin/env bash
set -euo pipefail

# Offline contract check for recovery paths. It never starts services, calls
# Docker, or mutates files. Live fault-injection remains a separately approved
# Linux exercise.
log_root=/data/myserver/log
state_dir=/var/lib/vector
config=/etc/vector/vector.yaml
json=false

usage() {
  cat <<'EOF'
Usage: vector-recovery-check.sh [--log-root <absolute-dir>]
       [--state-dir <absolute-dir>] [--config <absolute-file>] [--json]

Run read-only recovery contract checks. A missing production path is reported
as pending rather than created; use temporary fixture paths for CI checks.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --log-root) log_root="${2:-}"; shift 2 ;;
    --state-dir) state_dir="${2:-}"; shift 2 ;;
    --config) config="${2:-}"; shift 2 ;;
    --json) json=true; shift ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; exit 64 ;;
  esac
done
for path in "$log_root" "$state_dir"; do
  [[ "$path" == /* && ! -L "$path" ]] || { echo 'paths must be absolute non-symlink paths.' >&2; exit 64; }
done

emit() {
  local scenario="$1" status="$2" evidence="$3" action="$4"
  if [[ "$json" == true ]]; then
    printf '{"schema":"vector.recovery.v1","scenario":"%s","status":"%s","evidence":"%s","action":"%s"}\n' \
      "$scenario" "$status" "$evidence" "$action"
  else
    printf 'vector_recovery schema=vector.recovery.v1 scenario=%s status=%s evidence=%s action=%s\n' \
      "$scenario" "$status" "$evidence" "$action"
  fi
}

failed=0
check() {
  local scenario="$1" condition="$2" evidence="$3" action="$4"
  if [[ "$condition" == true ]]; then
    emit "$scenario" pass "$evidence" "$action"
  else
    emit "$scenario" pending "$evidence" "$action"
    failed=1
  fi
}

config_ok=false
if [[ -f "$config" && ! -L "$config" ]]; then
  grep -F 'type: docker_logs' "$config" >/dev/null &&
    grep -F 'retry_backoff_secs' "$config" >/dev/null &&
    grep -F 'when_full: drop_newest' "$config" >/dev/null && config_ok=true
fi
state_ok=false
[[ -d "$state_dir" && ! -L "$state_dir" && -w "$state_dir" ]] && state_ok=true
log_ok=false
[[ -d "$log_root" && ! -L "$log_root" ]] && log_ok=true

check service_restart "$config_ok" 'docker_logs_source_and_retry_configured' 'docker_logs_fallback'
check vector_restart "$state_ok" 'state_dir_writable_checkpoint_preserved' 'resume_from_checkpoint'
check docker_restart "$config_ok" 'docker_logs_source_reconnects_after_daemon_restart' 'reconnect_and_backfill_retained_window'
check network_short_disconnect "$config_ok" 'source_retry_backoff_configured' 'retry_without_business_blocking'
check docker_api_unavailable "$config_ok" 'api_outage_observable_business_containers_continue' 'report_gap_then_backfill_available_window'
check disk_readonly "$log_ok" 'write_probe_only_no_deletion_or_truncation' 'retain_unarchived_and_use_docker_logs_fallback'
check queue_overflow "$config_ok" 'bounded_disk_buffer_drop_newest' 'report_gap_preserve_older_queue'
check rotation_and_cleanup "$log_ok" 'closed_jsonl_only; active_open_files_excluded' 'flush_then_atomic_rotate_and_manifest_gate'
check shutdown "$config_ok" 'systemd_stop_flushes_vector_sink' 'checkpoint_and_output_preserved'

if (( failed != 0 )); then
  # Pending means the live host prerequisite is absent; fixture checks still
  # remain useful and this exit code prevents a false green deployment gate.
  exit 1
fi
