#!/usr/bin/env bash
set -euo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/ops-common.sh"

usage() {
  printf 'Usage: %s --release-id <id> --confirm <id> --rollback-db-compatible [--actor <identity>]\n' "${0##*/}" >&2
  exit 2
}

release_id=''
confirmation=''
actor="gameops@$(hostname -s)"
rollback_db_compatible=false
while [[ $# -gt 0 ]]; do
  case "$1" in
    --release-id) release_id="${2:-}"; shift 2 ;;
    --confirm) confirmation="${2:-}"; shift 2 ;;
    --rollback-db-compatible) rollback_db_compatible=true; shift ;;
    --actor) actor="${2:-}"; shift 2 ;;
    *) usage ;;
  esac
done
[[ -n "$release_id" && -n "$confirmation" && "$rollback_db_compatible" == true ]] || usage
release_dir_for "$release_id" >/dev/null
require_confirmation "$release_id" "$confirmation"
acquire_mutating_lock
assert_no_pending_ops_install
assert_no_pending_retire
[[ -x /data/myserver/apply-release.sh ]] || die '/data/myserver/apply-release.sh is unavailable'

exec /data/myserver/apply-release.sh --release-id "$release_id" --actor "$actor" \
  --rollback-db-compatible
