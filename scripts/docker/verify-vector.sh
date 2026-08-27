#!/usr/bin/env bash
set -euo pipefail

usage() {
  printf 'Usage: %s --source <vector-dir> [--vector-bin <path>] [--offline]\n' "${0##*/}" >&2
  exit 64
}

source_dir=''
vector_bin=''
offline=false
required_version='0.47.0'
while [[ $# -gt 0 ]]; do
  case "$1" in
    --source) source_dir="${2:-}"; shift 2 ;;
    --vector-bin) vector_bin="${2:-}"; shift 2 ;;
    --offline) offline=true; shift ;;
    --help|-h) usage; exit 0 ;;
    *) usage ;;
  esac
done
[[ "$source_dir" == /* && -d "$source_dir" && ! -L "$source_dir" ]] || usage
for file in vector.yaml vector.service; do
  [[ -f "$source_dir/$file" && ! -L "$source_dir/$file" ]] || { echo "Missing Vector file: $file" >&2; exit 65; }
done

config="$source_dir/vector.yaml"
for token in \
  'type: docker_logs' 'type: file' 'codec: json' 'method: newline_delimited' \
  'data_dir: /var/lib/vector' '/data/myserver/log/{{ service }}/{{ captured_at | format_timestamp' \
  'max_size: 1073741824' 'when_full: drop_newest' 'auto_partial_merge: true' \
  'com.docker.compose.service' 'com.myserver.service-instance-id' 'com.myserver.release-id' \
  'metrics-collector' 'parse_status' 'unknown'; do
  grep -F -- "$token" "$config" >/dev/null || { echo "Vector config contract missing: $token" >&2; exit 65; }
done
if grep -E '/var/lib/docker/containers|docker-json\.log|/run/secrets|PASSWORD|TOKEN|DSN|DATABASE_URL' "$config" >/dev/null; then
  echo 'Vector config contains a private Docker path or secret-like environment reference.' >&2
  exit 65
fi

if [[ -z "$vector_bin" ]]; then vector_bin="$(command -v vector 2>/dev/null || true)"; fi
if [[ "$offline" != true && -n "$vector_bin" ]]; then
  version_output="$("$vector_bin" --version 2>/dev/null || true)"
  [[ "$version_output" == *"$required_version"* ]] || {
    echo "Vector binary version must contain $required_version." >&2
    exit 65
  }
  "$vector_bin" validate --config "$config"
  printf 'vector_config_validated=cli version=%s path=%s\n' "$required_version" "$vector_bin"
else
  printf 'vector_config_validated=offline required_version=%s path=%s\n' "$required_version" "$config"
fi
