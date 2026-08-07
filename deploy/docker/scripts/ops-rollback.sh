#!/usr/bin/env bash
set -euo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/ops-common.sh"

usage() {
  printf 'Usage: %s --release-id <previous-id> --confirm <previous-id> --db-compatible [--actor <identity>]\n' "${0##*/}" >&2
  exit 2
}

release_id=''
confirmation=''
actor="gameops@$(hostname -s)"
db_compatible=false
while [[ $# -gt 0 ]]; do
  case "$1" in
    --release-id) release_id="${2:-}"; shift 2 ;;
    --confirm) confirmation="${2:-}"; shift 2 ;;
    --db-compatible) db_compatible=true; shift ;;
    --actor) actor="${2:-}"; shift 2 ;;
    *) usage ;;
  esac
done
[[ -n "$release_id" && -n "$confirmation" && "$db_compatible" == true ]] || usage
release_dir_for "$release_id" >/dev/null
require_confirmation "$release_id" "$confirmation"
[[ "$(current_release_dir)" != "$(release_dir_for "$release_id")" ]] || die 'target release is already current'
[[ -x /data/myserver/apply-release.sh ]] || die '/data/myserver/apply-release.sh is unavailable'

printf 'rollback_release=%s\n' "$release_id"
printf 'database compatibility was explicitly confirmed; this does not reverse database migrations.\n' >&2
exec /data/myserver/apply-release.sh --release-id "$release_id" --actor "$actor"
