#!/bin/sh
set -eu

if [ "$#" -eq 0 ]; then
  echo "Specify a db-deploy command, for example: preflight --environment production" >&2
  exit 64
fi

case "$1" in
  validate|preflight|initialize|apply|postflight|rebuild-check)
    set -- node tools/db-deploy.js "$@"
    ;;
esac

exec "$@"
