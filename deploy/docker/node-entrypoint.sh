#!/bin/sh
set -eu

if [ "$#" -gt 0 ]; then
  exec "$@"
fi

if [ -z "${SERVICE:-}" ]; then
  echo "SERVICE is required" >&2
  exit 64
fi

exec node "/app/apps/${SERVICE}/src/server.js"
