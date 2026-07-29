#!/usr/bin/env bash
set -euo pipefail

production=false
if [ "${1:-}" = "--production" ]; then
  production=true
  shift
fi
lock_file="${1:-deploy/docker/images.lock.json}"

if [ ! -f "$lock_file" ]; then
  echo "Release lock not found: $lock_file" >&2
  exit 66
fi

node --input-type=module - "$lock_file" "$production" <<'NODE'
import { readFile } from "node:fs/promises";

const lock = JSON.parse(await readFile(process.argv[2], "utf8"));
const production = process.argv[3] === "true";
if (![1, 2].includes(lock.schemaVersion) || !lock.releaseId || !lock.revision || lock.dirtyWorktree) {
  throw new Error("release lock is incomplete or was created from a dirty worktree");
}

for (const [service, image] of Object.entries(lock.images ?? {})) {
  if (!image?.repository || !/^sha256:[0-9a-f]{64}$/.test(image.digest ?? "") || image.reference !== `${image.repository}@${image.digest}`) {
    throw new Error(`invalid locked image for ${service}`);
  }
}

if (lock.schemaVersion === 2) {
  for (const name of ["postgres", "redis", "nats"]) {
    const image = lock.infrastructure?.[name];
    if (!image?.tag || !image?.repository || !/^sha256:[0-9a-f]{64}$/.test(image.digest ?? "") || image.reference !== `${image.repository}@${image.digest}`) {
      throw new Error(`invalid locked infrastructure image for ${name}`);
    }
  }
}

if (production && (lock.schemaVersion !== 2 || lock.releaseId.includes("-docker-test-"))) {
  throw new Error("a production release requires a schemaVersion 2 non-test lock");
}

console.log(`release lock verified: ${lock.releaseId} (${Object.keys(lock.images).length} application images)`);
NODE
