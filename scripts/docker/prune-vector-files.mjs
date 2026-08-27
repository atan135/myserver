#!/usr/bin/env node
import { appendFile, lstat, readdir, readFile, stat, unlink } from "node:fs/promises";
import { createHash } from "node:crypto";
import path from "node:path";

const args = process.argv.slice(2);
const get = (name, fallback) => {
  const value = args.find((arg) => arg.startsWith(`${name}=`));
  return value ? value.slice(name.length + 1) : fallback;
};
const logRoot = get("--log-root", "/data/myserver/log");
const stateDir = get("--state-dir", "/var/lib/vector");
const retentionDays = Number(get("--retention-days", "14"));
const apply = args.includes("--apply");
const confirm = get("--confirm", "");
const allowlist = new Set([
  "game-server", "game-proxy", "auth-http", "admin-api", "chat-server",
  "match-service", "mail-service", "announce-service", "metrics-collector"
]);
if (logRoot !== "/data/myserver/log" || stateDir !== "/var/lib/vector" || !Number.isInteger(retentionDays) || retentionDays < 1 || retentionDays > 3650) {
  throw new Error("retention paths or days violate the Vector contract");
}
if (apply && confirm !== "vector-retention-v2") throw new Error("--apply requires --confirm vector-retention-v2");

const isSafeRoot = async (target) => {
  const info = await lstat(target);
  return info.isDirectory() && !info.isSymbolicLink();
};
if (!(await isSafeRoot(logRoot))) throw new Error(`unsafe log root: ${logRoot}`);
if (!(await isSafeRoot(stateDir))) throw new Error(`unsafe vector state directory: ${stateDir}`);
const manifestPath = path.join(stateDir, "archive-manifest.jsonl");
let archived = new Map();
try {
  const lines = (await readFile(manifestPath, "utf8")).split(/\r?\n/).filter(Boolean);
  archived = new Map(lines.map((line) => JSON.parse(line)).filter((entry) => entry?.path && entry?.sha256 && Number.isInteger(entry.size))
    .map((entry) => [entry.path, entry]));
} catch (error) {
  if (error.code !== "ENOENT") throw error;
}

const cutoff = Date.now() - retentionDays * 86400000;
const actions = [];
const services = await readdir(logRoot, { withFileTypes: true });
for (const serviceEntry of services) {
  if (!serviceEntry.isDirectory() || serviceEntry.isSymbolicLink() || !allowlist.has(serviceEntry.name)) continue;
  const serviceDir = path.join(logRoot, serviceEntry.name);
  for (const dateEntry of await readdir(serviceDir, { withFileTypes: true })) {
    if (!dateEntry.isDirectory() || dateEntry.isSymbolicLink() || !/^\d{4}-\d{2}-\d{2}$/.test(dateEntry.name)) continue;
    const dateDir = path.join(serviceDir, dateEntry.name);
    if (Date.parse(`${dateEntry.name}T00:00:00Z`) >= cutoff) continue;
    for (const fileEntry of await readdir(dateDir, { withFileTypes: true })) {
      if (!fileEntry.isFile() || fileEntry.isSymbolicLink() || !/^[A-Za-z0-9._-]+\.[0-9a-f]{12}\.\d{4}\.jsonl$/.test(fileEntry.name)) continue;
      const absolute = path.join(dateDir, fileEntry.name);
      const relative = path.posix.relative(logRoot.replaceAll("\\", "/"), absolute.replaceAll("\\", "/"));
      const record = archived.get(relative);
      if (!record) { actions.push({ action: "skip", reason: "not_archived", path: relative }); continue; }
      const fileStat = await stat(absolute);
      const hash = createHash("sha256");
      hash.update(await readFile(absolute));
      if (fileStat.size !== record.size || hash.digest("hex") !== record.sha256) {
        actions.push({ action: "skip", reason: "manifest_mismatch", path: relative });
        continue;
      }
      const item = { action: apply ? "delete" : "candidate", path: relative, size: fileStat.size, sha256: record.sha256 };
      actions.push(item);
      if (apply) {
        await appendFile(path.join(stateDir, "retention-actions.jsonl"), `${JSON.stringify({ ...item, at: new Date().toISOString() })}\n`, { mode: 0o600 });
        await unlink(absolute);
      }
    }
  }
}
for (const action of actions) process.stdout.write(`${JSON.stringify(action)}\n`);
