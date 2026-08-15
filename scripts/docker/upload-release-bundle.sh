#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/docker/upload-release-bundle.sh [options]

Creates a complete, checksummed release bundle from the Git commit that owns
the selected images.lock.json, then uploads and verifies it on the server.

Required options (or matching environment variables):
  --release-id <id>                 MYSERVER_RELEASE_ID
  --host <host>                     MYSERVER_SSH_HOST
  --identity <native-linux-key>     MYSERVER_SSH_IDENTITY
  --caddy-landing-host <domain>     MYSERVER_CADDY_LANDING_HOST
  --caddy-auth-host <domain>        MYSERVER_CADDY_AUTH_HOST
  --caddy-admin-host <domain>       MYSERVER_CADDY_ADMIN_HOST
  --caddy-chat-host <domain>        MYSERVER_CADDY_CHAT_HOST
  --caddy-email <email>             MYSERVER_CADDY_EMAIL
  --game-proxy-host <host>          MYSERVER_GAME_PROXY_HOST

Optional:
  --user <name>                     Default: MYSERVER_SSH_USER or gameops
  --port <number>                   Default: MYSERVER_SSH_PORT or 22
  --runtime-env <production|test>   Default: MYSERVER_RUNTIME_ENV or production
EOF
}

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
release_id="${MYSERVER_RELEASE_ID:-}"
ssh_host="${MYSERVER_SSH_HOST:-}"
ssh_user="${MYSERVER_SSH_USER:-gameops}"
ssh_port="${MYSERVER_SSH_PORT:-22}"
ssh_identity="${MYSERVER_SSH_IDENTITY:-}"
runtime_env="${MYSERVER_RUNTIME_ENV:-production}"
caddy_landing_host="${MYSERVER_CADDY_LANDING_HOST:-}"
caddy_auth_host="${MYSERVER_CADDY_AUTH_HOST:-}"
caddy_admin_host="${MYSERVER_CADDY_ADMIN_HOST:-}"
caddy_chat_host="${MYSERVER_CADDY_CHAT_HOST:-}"
caddy_email="${MYSERVER_CADDY_EMAIL:-}"
game_proxy_host="${MYSERVER_GAME_PROXY_HOST:-}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --release-id) release_id="${2:?--release-id requires a value}"; shift 2 ;;
    --host) ssh_host="${2:?--host requires a value}"; shift 2 ;;
    --user) ssh_user="${2:?--user requires a value}"; shift 2 ;;
    --port) ssh_port="${2:?--port requires a value}"; shift 2 ;;
    --identity) ssh_identity="${2:?--identity requires a value}"; shift 2 ;;
    --runtime-env) runtime_env="${2:?--runtime-env requires a value}"; shift 2 ;;
    --caddy-landing-host) caddy_landing_host="${2:?--caddy-landing-host requires a value}"; shift 2 ;;
    --caddy-auth-host) caddy_auth_host="${2:?--caddy-auth-host requires a value}"; shift 2 ;;
    --caddy-admin-host) caddy_admin_host="${2:?--caddy-admin-host requires a value}"; shift 2 ;;
    --caddy-chat-host) caddy_chat_host="${2:?--caddy-chat-host requires a value}"; shift 2 ;;
    --caddy-email) caddy_email="${2:?--caddy-email requires a value}"; shift 2 ;;
    --game-proxy-host) game_proxy_host="${2:?--game-proxy-host requires a value}"; shift 2 ;;
    --help|-h) usage; exit 0 ;;
    *) echo "Unknown option: $1" >&2; usage >&2; exit 64 ;;
  esac
done

for value in "$release_id" "$ssh_host" "$ssh_identity" "$caddy_landing_host" "$caddy_auth_host" "$caddy_admin_host" "$caddy_chat_host" "$caddy_email" "$game_proxy_host"; do
  [[ -n "$value" ]] || { usage >&2; exit 64; }
done
case "$root" in /mnt/*) echo "Run from a WSL-native checkout: $root" >&2; exit 65 ;; esac
case "$release_id" in *[!A-Za-z0-9._-]*|'') echo "Invalid release ID: $release_id" >&2; exit 64 ;; esac
[[ "$ssh_port" =~ ^[0-9]+$ ]] || { echo "Invalid SSH port: $ssh_port" >&2; exit 64; }
case "$runtime_env" in production|test) ;; *) echo "--runtime-env must be production or test." >&2; exit 64 ;; esac

for command in git node sha256sum tar ssh scp; do
  command -v "$command" >/dev/null || { echo "Required command is unavailable: $command" >&2; exit 69; }
done
[[ -f "$ssh_identity" ]] || { echo "SSH private key does not exist: $ssh_identity" >&2; exit 66; }
key_mode="$(stat -c '%a' "$ssh_identity")"
if (( (8#$key_mode & 077) != 0 )); then
  echo "SSH private key must not be readable by group or others: $ssh_identity" >&2
  exit 65
fi

cd "$root"
[[ -z "$(git status --porcelain --untracked-files=all)" ]] || {
  echo "Refusing to bundle from a dirty worktree." >&2
  exit 65
}
./scripts/docker/verify-release.sh --production deploy/docker/images.lock.json

lock_release_id="$(node -e "const lock = require('./deploy/docker/images.lock.json'); process.stdout.write(lock.releaseId)")"
lock_revision="$(node -e "const lock = require('./deploy/docker/images.lock.json'); process.stdout.write(lock.revision)")"
[[ "$lock_release_id" == "$release_id" ]] || {
  echo "Release ID does not match deploy/docker/images.lock.json: $lock_release_id" >&2
  exit 65
}

# Bundle from the committed tree that owns this exact lock, not from a later checkout.
lock_hash="$(sha256sum deploy/docker/images.lock.json | awk '{print $1}')"
lock_commit=""
while read -r candidate; do
  candidate_hash="$(git show "$candidate:deploy/docker/images.lock.json" | sha256sum | awk '{print $1}')"
  if [[ "$candidate_hash" == "$lock_hash" ]]; then
    lock_commit="$candidate"
    break
  fi
done < <(git log --format='%H' --all -- deploy/docker/images.lock.json)
[[ -n "$lock_commit" ]] || { echo "Current lock is not committed; run publish-release.sh first." >&2; exit 65; }
git merge-base --is-ancestor "$lock_revision" "$lock_commit" || {
  echo "Lock revision is not an ancestor of the lock commit." >&2
  exit 65
}

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/myserver-release.XXXXXX")"
worktree="$tmp_dir/worktree"
bundle="$tmp_dir/$release_id"
archive="$tmp_dir/$release_id.tar.gz"
trap 'git worktree remove --force "$worktree" 2>/dev/null || true; rm -rf "$tmp_dir"' EXIT

git worktree add --detach "$worktree" "$lock_commit" >/dev/null
"$worktree/scripts/docker/create-release-bundle.sh" \
  --output "$bundle" \
  --release-root "/data/myserver/release/$release_id" \
  --runtime-env "$runtime_env" \
  --caddy-landing-host "$caddy_landing_host" \
  --caddy-auth-host "$caddy_auth_host" \
  --caddy-admin-host "$caddy_admin_host" \
  --caddy-chat-host "$caddy_chat_host" \
  --caddy-email "$caddy_email" \
  --game-proxy-advertised-host "$game_proxy_host"
(
  cd "$tmp_dir"
  tar -czf "$archive" "$release_id"
)
bundle_sha256="$(sha256sum "$archive" | awk '{print $1}')"
ssh_target="${ssh_user}@${ssh_host}"
ssh_options=(
  -i "$ssh_identity"
  -p "$ssh_port"
  -o BatchMode=yes
  -o ConnectTimeout=15
  -o ServerAliveInterval=15
  -o ServerAliveCountMax=3
  -o StrictHostKeyChecking=accept-new
)
scp_options=(
  -i "$ssh_identity"
  -P "$ssh_port"
  -o BatchMode=yes
  -o ConnectTimeout=15
  -o ServerAliveInterval=15
  -o ServerAliveCountMax=3
  -o StrictHostKeyChecking=accept-new
)

scp "${scp_options[@]}" "$archive" "$ssh_target:/tmp/"

ssh "${ssh_options[@]}" "$ssh_target" \
  "RELEASE_ID='$release_id' BUNDLE_SHA256='$bundle_sha256' bash -s" <<'REMOTE'
set -euo pipefail
release_root=/data/myserver/release
archive="/tmp/${RELEASE_ID}.tar.gz"
target="$release_root/$RELEASE_ID"

[[ -d "$release_root" && -w "$release_root" ]] || {
  echo "Release root must already exist and be writable by the deployment user: $release_root" >&2
  exit 77
}
printf '%s  %s\n' "$BUNDLE_SHA256" "$archive" | sha256sum --check --status -
if [[ -e "$target" ]]; then
  [[ -d "$target" && ! -L "$target" ]] || { echo "Existing release target is unsafe: $target" >&2; exit 65; }
  [[ "$(awk -F= '$1 == "release_id" { print $2 }' "$target/RELEASE")" == "$RELEASE_ID" ]] || {
    echo "Existing release identity does not match: $target" >&2
    exit 65
  }
  printf 'resuming_verified_release=%s\n' "$target"
else
  tar -xzf "$archive" -C "$release_root"
fi
(
  cd "$target"
  sha256sum --check --status SHA256SUMS
)
"$target/scripts/install-ops-scripts.sh" \
  --source "$target/scripts/ops" \
  --runner-source "$target/scripts/server-apply-release.sh" \
  --target /home/gameops/script
rm -f "$archive"
printf 'uploaded_release=%s\nserver_command=/data/myserver/apply-release.sh --release-id %s --rollback-db-compatible\n' "$target" "$RELEASE_ID"
REMOTE
