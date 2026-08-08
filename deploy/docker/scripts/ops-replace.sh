#!/usr/bin/env bash
set -euo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/ops-common.sh"

usage() {
  printf 'Usage: %s <service> --confirm <service> [--timeout <seconds>]\n' "${0##*/}" >&2
  usage_service
  exit 2
}

[[ $# -ge 3 ]] || usage
service="$1"
shift
require_service "$service"
confirmation=''
timeout=180
while [[ $# -gt 0 ]]; do
  case "$1" in
    --confirm) confirmation="${2:-}"; shift 2 ;;
    --timeout) timeout="${2:-}"; shift 2 ;;
    *) usage ;;
  esac
done
[[ "$timeout" =~ ^[1-9][0-9]*$ ]] || die '--timeout must be a positive integer'
require_confirmation "$service" "$confirmation"
load_release_readiness
validate_release_readiness_window "$timeout"

printf 'replacing_service=%s\n' "$service"
compose up -d --no-deps "$service"
wait_for_target_container "$service" "$timeout"
wait_for_release_readiness "$timeout"
