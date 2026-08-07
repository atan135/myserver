#!/usr/bin/env bash
set -euo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/ops-common.sh"

release_dir="$(current_release_dir)"
printf 'current_release=%s\n\n' "$release_dir"
compose ps
printf '\nDocker disk usage:\n'
docker system df
