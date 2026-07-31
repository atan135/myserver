import { readFile, writeFile } from "node:fs/promises";
import path from "node:path";

const options = new Map();
for (let index = 2; index < process.argv.length; index += 2) {
  const key = process.argv[index];
  const value = process.argv[index + 1];
  if (!key?.startsWith("--") || value === undefined) throw new Error(`invalid argument near ${key ?? "end of command"}`);
  options.set(key.slice(2), value);
}

for (const key of ["lock", "template", "output", "release-root", "caddy-auth-host", "caddy-admin-host", "caddy-chat-host", "caddy-email", "game-proxy-advertised-host"]) {
  if (!options.has(key)) throw new Error(`missing --${key}`);
}

function envValue(name) {
  const value = options.get(name).trim();
  if (!value || /[\r\n=]/.test(value)) throw new Error(`--${name} must be a non-empty single-line environment value without '='`);
  return value;
}

const lock = JSON.parse(await readFile(options.get("lock"), "utf8"));
if (lock.schemaVersion !== 2 || lock.dirtyWorktree || !lock.releaseId || !lock.images || !lock.infrastructure) {
  throw new Error("release lock must be a clean schemaVersion 2 lock");
}
if (lock.releaseId.includes("-docker-test-")) throw new Error("docker test releases cannot produce a production environment file");

const imageKey = (service) => `IMAGE_${service.replaceAll("-", "_").toUpperCase()}`;
const serviceNames = [
  "game-server",
  "game-proxy",
  "chat-server",
  "match-service",
  "auth-http",
  "admin-api",
  "announce-service",
  "mail-service",
  "metrics-collector",
  "caddy",
  "migration-runner"
];
const values = new Map([
  ["RELEASE_ID", lock.releaseId],
  ["POSTGRES_IMAGE", lock.infrastructure.postgres?.reference],
  ["REDIS_IMAGE", lock.infrastructure.redis?.reference],
  ["NATS_IMAGE", lock.infrastructure.nats?.reference],
  ["GAME_CSV_DIR", `${envValue("release-root")}/apps/game-server/csv`],
  ["GAME_SCENE_DIR", `${envValue("release-root")}/apps/game-server/scene`],
  ["CADDY_AUTH_HOST", envValue("caddy-auth-host")],
  ["CADDY_ADMIN_HOST", envValue("caddy-admin-host")],
  ["CADDY_CHAT_HOST", envValue("caddy-chat-host")],
  ["CADDY_EMAIL", envValue("caddy-email")],
  ["GAME_PROXY_ADVERTISED_HOST", envValue("game-proxy-advertised-host")]
]);

for (const service of serviceNames) {
  const image = lock.images[service];
  if (!image?.reference) throw new Error(`release lock is missing application image ${service}`);
  values.set(imageKey(service), image.reference);
}
for (const name of ["postgres", "redis", "nats"]) {
  if (!lock.infrastructure[name]?.reference) throw new Error(`release lock is missing infrastructure image ${name}`);
}

const template = await readFile(options.get("template"), "utf8");
const rendered = template.split(/\r?\n/).map((line) => {
  const equals = line.indexOf("=");
  if (equals <= 0) return line;
  const key = line.slice(0, equals);
  return values.has(key) ? `${key}=${values.get(key)}` : line;
}).join("\n");

const output = path.resolve(options.get("output"));
await writeFile(output, `${rendered.trimEnd()}\n`, { encoding: "utf8", mode: 0o640 });
