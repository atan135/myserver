#!/usr/bin/env bash
set -euo pipefail

lock_file="${1:-deploy/docker/images.lock.json}"

if [ ! -f "$lock_file" ]; then
  echo "Release lock not found: $lock_file" >&2
  exit 66
fi

node --input-type=module - "$lock_file" <<'NODE'
import { readFile } from "node:fs/promises";

const lock = JSON.parse(await readFile(process.argv[2], "utf8"));
if (lock.schemaVersion !== 1 || !lock.releaseId || !lock.revision || lock.dirtyWorktree) {
  throw new Error("release lock is incomplete or was created from a dirty worktree");
}

for (const [service, image] of Object.entries(lock.images ?? {})) {
  if (!image?.repository || !/^sha256:[0-9a-f]{64}$/.test(image.digest ?? "") || image.reference !== `${image.repository}@${image.digest}`) {
    throw new Error(`invalid locked image for ${service}`);
  }
}

console.log(`release lock verified: ${lock.releaseId} (${Object.keys(lock.images).length} images)`);
NODE
