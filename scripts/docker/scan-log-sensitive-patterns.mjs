#!/usr/bin/env node
import { readdir, readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

// This checker deliberately scans only committed source/config files. It never
// reads env files, Docker state, runtime logs, or the Vector output directory.
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const files = [
  "deploy/docker/vector/vector.yaml",
  "deploy/docker/vector/vector.service",
  "deploy/docker/compose.production.yml",
  "deploy/docker/scripts/ops-logs.sh",
  "scripts/docker/vector-status.sh"
];
const forbidden = [
  [/JSON\.stringify\(extra\)/, "raw Node logger extra serialization"],
  [/JSON\.stringify\(meta\)/, "raw Node logger metadata serialization"],
  [/(?:tracing::)?(?:info|warn|error|debug)!\([\s\S]{0,600}(?:url|dsn|token|ticket|password|secret|payload|body|content|attachment|authorization|backtrace|stack)\s*=\s*%/, "raw credential/URL/payload/stack field in Rust log"],
  [/\/var\/lib\/docker\/containers|docker-json\.log|\/run\/secrets/, "private Docker or secret path in collector config"]
];

async function addIfFile(relative) {
  try {
    const entry = await readdir(path.dirname(path.join(root, relative)), { withFileTypes: true });
    if (entry.some((item) => item.isFile() && item.name === path.basename(relative))) files.push(relative);
  } catch {
    // A production bundle intentionally does not contain application source.
  }
}

for (const service of ["auth-http", "announce-service", "admin-api", "mail-service"]) {
  await addIfFile(`apps/${service}/src/logger.js`);
}

async function addRustSources(relativeDir) {
  const absolute = path.join(root, relativeDir);
  let entries;
  try { entries = await readdir(absolute, { withFileTypes: true }); } catch { return; }
  for (const entry of entries) {
    const child = path.join(relativeDir, entry.name);
    if (entry.isDirectory()) await addRustSources(child);
    else if (entry.isFile() && entry.name.endsWith(".rs")) files.push(child);
  }
}
await addRustSources("apps/game-server/src");
await addRustSources("apps/game-proxy/src");

let failures = 0;
for (const relative of files) {
  const content = await readFile(path.join(root, relative), "utf8");
  for (const [pattern, description] of forbidden) {
    if (pattern.test(content)) {
      console.error(`sensitive_log_scan=failed file=${relative} reason=${description}`);
      failures += 1;
    }
  }
}
if (failures > 0) process.exit(1);
console.log(`sensitive_log_scan=passed files=${files.length}`);
