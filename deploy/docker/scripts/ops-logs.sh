#!/usr/bin/env bash
set -euo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/ops-common.sh"

usage() {
  printf 'Usage: %s <service> [--tail <lines>] [--follow]\n' "${0##*/}" >&2
  usage_service
  exit 2
}

[[ $# -ge 1 ]] || usage
service="$1"
shift
require_service "$service"
tail=200
follow=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    --tail) tail="${2:-}"; shift 2 ;;
    --follow) follow=true; shift ;;
    *) usage ;;
  esac
done
[[ "$tail" =~ ^[1-9][0-9]*$ ]] || die '--tail must be a positive integer'

container_id="$(container_id_for "$service")"
args=(--timestamps --tail "$tail")
[[ "$follow" == true ]] && args+=(--follow)
exec docker logs "${args[@]}" "$container_id"
