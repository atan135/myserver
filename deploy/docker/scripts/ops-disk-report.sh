#!/usr/bin/env bash
set -euo pipefail

printf 'Docker disk usage:\n'
docker system df
printf '\nHost filesystems:\n'
df -h / /data 2>/dev/null || df -h /
