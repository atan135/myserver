#!/usr/bin/env bash
set -euo pipefail

usage() {
  printf 'Usage: %s --source <verified-ops-dir> --runner-source <verified-runner> --target /home/gameops/script [--test-root <absolute-dir> [--test-crash-after <ops-switched|runner-switched>] [--test-fail-after ops-switched]]\n' "${0##*/}" >&2
  exit 64
}

source_dir=''
runner_source=''
target_dir=''
test_root=''
test_crash_after=''
test_fail_after=''
while [[ $# -gt 0 ]]; do
  case "$1" in
    --source) source_dir="${2:-}"; shift 2 ;;
    --runner-source) runner_source="${2:-}"; shift 2 ;;
    --target) target_dir="${2:-}"; shift 2 ;;
    --test-root) test_root="${2:-}"; shift 2 ;;
    --test-crash-after) test_crash_after="${2:-}"; shift 2 ;;
    --test-fail-after) test_fail_after="${2:-}"; shift 2 ;;
    *) usage ;;
  esac
done

for command in awk chmod cmp dirname find flock install kill mktemp mv readlink rm sha256sum sort stat sync xargs; do
  command -v "$command" >/dev/null || { echo "Required command is unavailable: $command" >&2; exit 69; }
done
[[ "$source_dir" == /* && "$runner_source" == /* && "$target_dir" == /* ]] || usage
[[ -d "$source_dir" && ! -L "$source_dir" ]] || { echo 'Verified ops source directory is invalid.' >&2; exit 65; }
[[ -f "$runner_source" && ! -L "$runner_source" ]] || { echo 'Verified runner source is invalid.' >&2; exit 65; }

source_dir="$(readlink -f "$source_dir")"
runner_source="$(readlink -f "$runner_source")"
target_dir="$(readlink -m "$target_dir")"
if [[ -n "$test_root" ]]; then
  [[ "$test_root" == /* && -d "$test_root" && ! -L "$test_root" ]] || { echo 'Test root is invalid.' >&2; exit 65; }
  test_root="$(readlink -f "$test_root")"
  [[ "$test_root" != / ]] || { echo 'Test root must not be filesystem root.' >&2; exit 65; }
  allowed_target="$test_root/home/gameops/script"
  runner_target="$test_root/data/myserver/apply-release.sh"
  export MYSERVER_OPS_STATE_ROOT="$test_root/data/myserver/run"
  privileged=()
else
  [[ -z "$test_crash_after$test_fail_after" ]] || { echo 'Failure injection requires --test-root.' >&2; exit 65; }
  allowed_target=/home/gameops/script
  runner_target=/data/myserver/apply-release.sh
  export MYSERVER_OPS_STATE_ROOT=/data/myserver/run
  privileged=(sudo -n)
  command -v sudo >/dev/null || { echo 'Required command is unavailable: sudo' >&2; exit 69; }
fi
[[ "$target_dir" == "$allowed_target" ]] || { echo "Ops target must exactly match: $allowed_target" >&2; exit 65; }
case "$source_dir/" in "$target_dir/"*) echo 'Ops source must not be inside the target directory.' >&2; exit 65 ;; esac
case "$target_dir/" in "$source_dir/"*) echo 'Ops target must not be inside the source directory.' >&2; exit 65 ;; esac
[[ "$test_crash_after" == '' || "$test_crash_after" == ops-switched || "$test_crash_after" == runner-switched ]] || usage
[[ "$test_fail_after" == '' || "$test_fail_after" == ops-switched ]] || usage

readonly scripts=(
  ops-common.sh ops-deploy.sh ops-disk-report.sh ops-health.sh ops-logs.sh
  ops-replace.sh ops-restart.sh ops-retire.sh ops-rollback.sh ops-status.sh
)
expected="$(printf '%s\n' "${scripts[@]}" | sort)"
actual="$(find "$source_dir" -mindepth 1 -maxdepth 1 -printf '%f\n' | sort)"
[[ "$actual" == "$expected" ]] || { echo 'Verified ops source does not match the script whitelist.' >&2; exit 65; }
for script in "${scripts[@]}"; do
  [[ -f "$source_dir/$script" && ! -L "$source_dir/$script" ]] || { echo "Verified ops source entry is invalid: $script" >&2; exit 65; }
done

# The verified bundle helper is the lock contract source; the installed helper may be replaced.
source "$source_dir/ops-common.sh"
acquire_mutating_lock

target_parent="$(readlink -m "$(dirname "$target_dir")")"
runner_parent="$(readlink -m "$(dirname "$runner_target")")"
[[ "$target_parent" == "$(dirname "$allowed_target")" ]] || { echo 'Resolved ops target parent is invalid.' >&2; exit 65; }
[[ "$runner_parent" == "$(dirname "$runner_target")" ]] || { echo 'Resolved runner target parent is invalid.' >&2; exit 65; }
if [[ -n "$test_root" ]]; then
  install -d -m 0755 "$target_parent" "$runner_parent"
else
  [[ -d "$target_parent" && ! -L "$target_parent" && -w "$target_parent" ]] || { echo 'Ops target parent is not writable.' >&2; exit 77; }
  "${privileged[@]}" true
  "${privileged[@]}" test -d "$runner_parent"
fi

readonly install_journal="$OPS_STATE_ROOT/pending-ops-install"
directory_hash() {
  local directory="$1"
  [[ -d "$directory" && ! -L "$directory" ]] || return 1
  [[ -z "$(find "$directory" -mindepth 1 -maxdepth 1 ! -type f -print -quit)" ]] || return 1
  (cd "$directory" && find . -mindepth 1 -maxdepth 1 -type f -print0 | sort -z | xargs -0 sha256sum) | sha256sum | awk '{print $1}'
}
file_hash() { sha256sum "$1" | awk '{print $1}'; }
journal_value() { awk -F= -v key="$1" '$1 == key { print substr($0, length(key) + 2) }' "$install_journal"; }
safe_ephemeral() {
  local value="$1" parent="$2" prefix="$3"
  [[ "$value" == "$parent/$prefix"* && "$(dirname "$value")" == "$parent" && "$value" != "$parent/$prefix" ]]
}
remove_safe_dir() {
  local value="$1" parent="$2" prefix="$3"
  safe_ephemeral "$value" "$parent" "$prefix" || { echo 'Refusing unsafe ops transaction directory cleanup.' >&2; return 1; }
  [[ ! -e "$value" ]] || rm -rf -- "$value"
}
remove_safe_runner() {
  local value="$1" parent="$2" prefix="$3"
  safe_ephemeral "$value" "$parent" "$prefix" || { echo 'Refusing unsafe runner transaction cleanup.' >&2; return 1; }
  [[ ! -e "$value" ]] || "${privileged[@]}" rm -f -- "$value"
}
remove_exact_target() {
  [[ "$target_dir" == "$allowed_target" && "$(readlink -m "$(dirname "$target_dir")")" == "$target_parent" ]] || {
    echo 'Refusing unsafe ops target cleanup.' >&2; return 1;
  }
  [[ ! -e "$target_dir" ]] || rm -rf -- "$target_dir"
}

rollback_pending() {
  [[ -f "$install_journal" && ! -L "$install_journal" && "$(stat -c '%a' "$install_journal")" == 600 ]] || {
    echo 'Pending ops install journal is unsafe.' >&2; return 1;
  }
  local j_target j_runner j_stage j_backup j_runner_stage j_runner_backup
  local new_ops old_ops new_runner old_runner old_ops_present old_runner_present
  local phase
  [[ "$(journal_value schema)" == 1 ]] || return 1
  j_target="$(journal_value target)"; j_runner="$(journal_value runner_target)"
  j_stage="$(journal_value stage)"; j_backup="$(journal_value backup)"
  j_runner_stage="$(journal_value runner_stage)"; j_runner_backup="$(journal_value runner_backup)"
  new_ops="$(journal_value new_ops_hash)"; old_ops="$(journal_value old_ops_hash)"
  new_runner="$(journal_value new_runner_hash)"; old_runner="$(journal_value old_runner_hash)"
  old_ops_present="$(journal_value old_ops_present)"; old_runner_present="$(journal_value old_runner_present)"
  phase="$(journal_value phase)"
  [[ "$j_target" == "$target_dir" && "$j_runner" == "$runner_target" ]] || return 1
  safe_ephemeral "$j_stage" "$target_parent" .myserver-ops-stage. || return 1
  safe_ephemeral "$j_backup" "$target_parent" .myserver-ops-backup. || return 1
  safe_ephemeral "$j_runner_stage" "$runner_parent" .apply-release-stage. || return 1
  safe_ephemeral "$j_runner_backup" "$runner_parent" .apply-release-backup. || return 1
  [[ "$new_ops" =~ ^[0-9a-f]{64}$ && "$new_runner" =~ ^[0-9a-f]{64}$ ]] || return 1

  if [[ "$phase" == committed ]]; then
    [[ "$(directory_hash "$target_dir")" == "$new_ops" ]] || return 1
    [[ -f "$runner_target" && "$(file_hash "$runner_target")" == "$new_runner" ]] || return 1
    if [[ -e "$j_backup" ]]; then [[ "$old_ops_present" == true && "$(directory_hash "$j_backup")" == "$old_ops" ]] || return 1; remove_safe_dir "$j_backup" "$target_parent" .myserver-ops-backup.; fi
    if [[ -e "$j_runner_backup" ]]; then [[ "$old_runner_present" == true && "$(file_hash "$j_runner_backup")" == "$old_runner" ]] || return 1; remove_safe_runner "$j_runner_backup" "$runner_parent" .apply-release-backup.; fi
    remove_safe_dir "$j_stage" "$target_parent" .myserver-ops-stage.
    remove_safe_runner "$j_runner_stage" "$runner_parent" .apply-release-stage.
    rm -f -- "$install_journal"; sync -f "$OPS_STATE_ROOT"
    printf 'recovered_pending_ops_install=committed\n'
    return 0
  fi
  [[ "$phase" == installing ]] || return 1

  if [[ -e "$j_runner_backup" ]]; then
    [[ "$old_runner_present" == true && "$(file_hash "$j_runner_backup")" == "$old_runner" ]] || return 1
    if [[ -e "$runner_target" ]]; then [[ -f "$runner_target" && "$(file_hash "$runner_target")" == "$new_runner" ]] || return 1; "${privileged[@]}" rm -f -- "$runner_target"; fi
    "${privileged[@]}" mv -- "$j_runner_backup" "$runner_target"
  elif [[ "$old_runner_present" == true ]]; then
    [[ -f "$runner_target" && "$(file_hash "$runner_target")" == "$old_runner" ]] || return 1
  elif [[ -e "$runner_target" ]]; then
    [[ -f "$runner_target" && "$(file_hash "$runner_target")" == "$new_runner" ]] || return 1
    "${privileged[@]}" rm -f -- "$runner_target"
  fi

  if [[ -e "$j_backup" ]]; then
    [[ "$old_ops_present" == true && "$(directory_hash "$j_backup")" == "$old_ops" ]] || return 1
    if [[ -e "$target_dir" ]]; then [[ "$(directory_hash "$target_dir")" == "$new_ops" ]] || return 1; remove_exact_target; fi
    mv -- "$j_backup" "$target_dir"
  elif [[ "$old_ops_present" == true ]]; then
    [[ "$(directory_hash "$target_dir")" == "$old_ops" ]] || return 1
  elif [[ -e "$target_dir" ]]; then
    [[ "$(directory_hash "$target_dir")" == "$new_ops" ]] || return 1
    remove_exact_target
  fi
  if [[ -e "$j_stage" ]]; then [[ "$(directory_hash "$j_stage")" == "$new_ops" ]] || return 1; remove_safe_dir "$j_stage" "$target_parent" .myserver-ops-stage.; fi
  if [[ -e "$j_runner_stage" ]]; then [[ "$(file_hash "$j_runner_stage")" == "$new_runner" ]] || return 1; remove_safe_runner "$j_runner_stage" "$runner_parent" .apply-release-stage.; fi
  rm -f -- "$install_journal"
  sync -f "$OPS_STATE_ROOT"
  printf 'recovered_pending_ops_install=true\n'
}

if [[ -e "$install_journal" ]]; then
  rollback_pending || { echo 'Pending ops install cannot be recovered safely.' >&2; exit 65; }
fi
assert_no_pending_retire

stage_dir="$(mktemp -d "$target_parent/.myserver-ops-stage.XXXXXX")"
token="${stage_dir##*.myserver-ops-stage.}"
backup_dir="$target_parent/.myserver-ops-backup.$token"
runner_stage="$runner_parent/.apply-release-stage.$token"
runner_backup="$runner_parent/.apply-release-backup.$token"
for path in "$backup_dir" "$runner_stage" "$runner_backup"; do [[ ! -e "$path" ]] || { echo 'Transaction path already exists.' >&2; exit 65; }; done
for script in "${scripts[@]}"; do install -m 0755 "$source_dir/$script" "$stage_dir/$script"; done
new_ops_hash="$(directory_hash "$stage_dir")"
new_runner_hash="$(file_hash "$runner_source")"
old_ops_present=false; old_ops_hash='none'
if [[ -e "$target_dir" ]]; then [[ -d "$target_dir" && ! -L "$target_dir" ]] || { echo 'Existing ops target is unsafe.' >&2; exit 65; }; old_ops_present=true; old_ops_hash="$(directory_hash "$target_dir")"; fi
old_runner_present=false; old_runner_hash='none'
if [[ -e "$runner_target" ]]; then [[ -f "$runner_target" && ! -L "$runner_target" ]] || { echo 'Existing runner target is unsafe.' >&2; exit 65; }; old_runner_present=true; old_runner_hash="$(file_hash "$runner_target")"; fi
"${privileged[@]}" install -m 0755 "$runner_source" "$runner_stage"
[[ "$(file_hash "$runner_stage")" == "$new_runner_hash" ]] || { echo 'Staged runner checksum mismatch.' >&2; exit 65; }

install -d -m 0700 "$OPS_STATE_ROOT"
umask 077
journal_tmp="$(mktemp "$OPS_STATE_ROOT/.pending-ops-install.XXXXXX")"
printf '%s\n' schema=1 phase=installing "target=$target_dir" "runner_target=$runner_target" "stage=$stage_dir" "backup=$backup_dir" \
  "runner_stage=$runner_stage" "runner_backup=$runner_backup" "new_ops_hash=$new_ops_hash" "old_ops_hash=$old_ops_hash" \
  "new_runner_hash=$new_runner_hash" "old_runner_hash=$old_runner_hash" "old_ops_present=$old_ops_present" \
  "old_runner_present=$old_runner_present" > "$journal_tmp"
chmod 0600 "$journal_tmp"; sync "$journal_tmp"; mv -f "$journal_tmp" "$install_journal"; sync -f "$OPS_STATE_ROOT"

transaction_complete=false
on_exit() {
  local status=$?
  trap - EXIT INT TERM
  if [[ "$transaction_complete" != true && -e "$install_journal" ]]; then rollback_pending || true; fi
  exit "$status"
}
trap on_exit EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

if [[ "$old_ops_present" == true ]]; then mv -- "$target_dir" "$backup_dir"; fi
mv -- "$stage_dir" "$target_dir"; chmod 0755 "$target_dir"
[[ "$(directory_hash "$target_dir")" == "$new_ops_hash" ]] || { echo 'Installed ops generation checksum mismatch.' >&2; exit 65; }
[[ "$test_crash_after" != ops-switched ]] || kill -KILL $$
[[ "$test_fail_after" != ops-switched ]] || { echo 'Injected runner switch failure.' >&2; exit 74; }
if [[ "$old_runner_present" == true ]]; then "${privileged[@]}" mv -- "$runner_target" "$runner_backup"; fi
"${privileged[@]}" mv -- "$runner_stage" "$runner_target"
[[ "$(file_hash "$runner_target")" == "$new_runner_hash" ]] || { echo 'Installed runner checksum mismatch.' >&2; exit 65; }
[[ "$test_crash_after" != runner-switched ]] || kill -KILL $$

commit_tmp="$(mktemp "$OPS_STATE_ROOT/.pending-ops-install.XXXXXX")"
awk '$0 == "phase=installing" { print "phase=committed"; next } { print }' "$install_journal" > "$commit_tmp"
chmod 0600 "$commit_tmp"; sync "$commit_tmp"; mv -f "$commit_tmp" "$install_journal"; sync -f "$OPS_STATE_ROOT"
remove_safe_dir "$backup_dir" "$target_parent" .myserver-ops-backup.
remove_safe_runner "$runner_backup" "$runner_parent" .apply-release-backup.
rm -f -- "$install_journal"; sync -f "$OPS_STATE_ROOT"
transaction_complete=true
trap - EXIT INT TERM
printf 'installed_ops_scripts=%s target=%s runner=%s\n' "${#scripts[@]}" "$target_dir" "$runner_target"
