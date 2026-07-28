import { readFile, writeFile, mkdir } from "node:fs/promises";
import path from "node:path";

const options = new Map();
for (let index = 2; index < process.argv.length; index += 2) {
  const key = process.argv[index];
  const value = process.argv[index + 1];
  if (!key?.startsWith("--") || value === undefined) {
    throw new Error(`invalid argument near ${key ?? "end of command"}`);
  }
  options.set(key.slice(2), value);
}

const required = ["output", "release-id", "revision", "created-at", "source", "platform", "records", "dirty"];
for (const key of required) {
  if (!options.has(key)) throw new Error(`missing --${key}`);
}

const rows = (await readFile(options.get("records"), "utf8"))
  .split("\n")
  .filter(Boolean)
  .map((line) => {
    const [service, repository, digest] = line.split("\t");
    if (!service || !repository || !/^sha256:[0-9a-f]{64}$/.test(digest ?? "")) {
      throw new Error(`invalid image record: ${line}`);
    }
    return [service, repository, digest];
  });

if (rows.length === 0) throw new Error("no image records were supplied");

const releaseId = options.get("release-id");
const images = Object.fromEntries(
  rows.map(([service, repository, digest]) => [service, {
    repository,
    tag: releaseId,
    digest,
    reference: `${repository}@${digest}`
  }])
);

const output = path.resolve(options.get("output"));
await mkdir(path.dirname(output), { recursive: true });
await writeFile(output, `${JSON.stringify({
  schemaVersion: 1,
  releaseId,
  revision: options.get("revision"),
  createdAt: options.get("created-at"),
  source: options.get("source"),
  platform: options.get("platform"),
  dirtyWorktree: options.get("dirty") === "true",
  images
}, null, 2)}\n`, "utf8");
