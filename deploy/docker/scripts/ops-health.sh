#!/usr/bin/env bash
set -euo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/ops-common.sh"

if [[ $# -gt 1 ]]; then
  printf 'Usage: %s [service]\n' "${0##*/}" >&2
  exit 2
fi
services=("${OPS_SERVICES[@]}")
if [[ $# -eq 1 ]]; then
  require_service "$1"
  services=("$1")
fi

failed=false
for service in "${services[@]}"; do
  container_id="$(compose ps -q "$service" || true)"
  if [[ -z "$container_id" ]]; then
    printf '%-20s state=%-10s health=%s\n' "$service" 'missing' 'missing'
    failed=true
    continue
  fi
  state="$(container_state "$container_id")"
  health="$(container_health "$container_id")"
  printf '%-20s state=%-10s health=%s\n' "$service" "$state" "$health"
  [[ "$state" == 'running' && "$health" != 'unhealthy' ]] || failed=true
done
[[ "$failed" == false ]]
