#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/docker/install-vector.sh --source <bundle-vector-dir>
       [--target /etc/vector] [--state-dir /var/lib/vector]
       [--log-root /data/myserver/log] [--enable]

Installs the checked Vector configuration and systemd unit. It verifies the
pre-installed /usr/bin/vector binary against the bundle version. It never starts
Vector. The optional --enable only enables the unit for the next boot.
EOF
}

source_dir=''
target_dir=/etc/vector
state_dir=/var/lib/vector
log_root=/data/myserver/log
enable_unit=false
while [[ $# -gt 0 ]]; do
  case "$1" in
    --source) source_dir="${2:-}"; shift 2 ;;
    --target) target_dir="${2:-}"; shift 2 ;;
    --state-dir) state_dir="${2:-}"; shift 2 ;;
    --log-root) log_root="${2:-}"; shift 2 ;;
    --enable) enable_unit=true; shift ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; exit 64 ;;
  esac
done

[[ "$source_dir" == /* && -d "$source_dir" && ! -L "$source_dir" ]] || {
  echo 'Vector source must be an existing absolute non-symlink directory.' >&2; exit 65;
}
[[ "$target_dir" == /etc/vector && "$state_dir" == /var/lib/vector && "$log_root" == /data/myserver/log ]] || {
  echo 'Vector install paths must remain /etc/vector, /var/lib/vector and /data/myserver/log.' >&2; exit 65;
}
for file in vector.yaml vector.service vector-version.txt rotate-vector-files.sh prune-vector-files.mjs vector-alerts.sh vector-recovery-check.sh; do
  [[ -f "$source_dir/$file" && ! -L "$source_dir/$file" ]] || {
    echo "Missing or unsafe Vector bundle file: $file" >&2; exit 65;
  }
done

command -v install >/dev/null || { echo 'Required command is unavailable: install' >&2; exit 69; }
command -v systemctl >/dev/null || { echo 'Required command is unavailable: systemctl' >&2; exit 69; }
[[ "$(id -u)" == 0 ]] || { echo 'Vector installation must run as root.' >&2; exit 77; }
[[ -x /usr/bin/vector ]] || { echo 'Expected Vector binary is missing: /usr/bin/vector' >&2; exit 69; }
required_version="$(tr -d '[:space:]' < "$source_dir/vector-version.txt")"
[[ "$required_version" == 0.47.0 ]] || { echo 'Vector bundle version manifest is invalid.' >&2; exit 65; }
version_output="$(/usr/bin/vector --version 2>/dev/null || true)"
[[ "$version_output" == *"$required_version"* ]] || {
  echo "Vector binary version does not match bundle: required $required_version." >&2; exit 65;
}

install -d -m 0755 "$target_dir"
install -d -m 0750 "$state_dir" "$state_dir/buffer" "$state_dir/checkpoints" "$state_dir/queue" "$log_root"
if ! getent group vector >/dev/null; then groupadd --system vector; fi
if ! id vector >/dev/null 2>&1; then useradd --system --no-create-home --shell /usr/sbin/nologin vector; fi
chown -R vector:vector "$state_dir"
chown vector:vector "$log_root"
chmod 0750 "$state_dir" "$state_dir/buffer" "$state_dir/checkpoints" "$state_dir/queue" "$log_root"
install -m 0644 "$source_dir/vector.yaml" "$target_dir/vector.yaml"
install -m 0644 "$source_dir/vector-version.txt" "$target_dir/vector-version.txt"
install -m 0644 "$source_dir/vector.service" /etc/systemd/system/vector.service
install -m 0755 "$source_dir/rotate-vector-files.sh" /usr/local/sbin/myserver-rotate-vector-files
install -m 0755 "$source_dir/prune-vector-files.mjs" /usr/local/sbin/myserver-prune-vector-files.mjs
install -m 0755 "$source_dir/vector-alerts.sh" /usr/local/sbin/myserver-vector-alerts
install -m 0755 "$source_dir/vector-recovery-check.sh" /usr/local/sbin/myserver-vector-recovery-check
systemctl daemon-reload
if [[ "$enable_unit" == true ]]; then systemctl enable vector.service; fi
printf 'vector_installed=config:%s unit:/etc/systemd/system/vector.service enabled:%s\n' "$target_dir/vector.yaml" "$enable_unit"
printf 'next_steps=vector validate --config %s; systemctl start vector.service\n' "$target_dir/vector.yaml"
