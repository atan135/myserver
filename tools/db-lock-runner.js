import { spawnSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import pg from "pg";

import { redact } from "./db.js";

const { Client } = pg;
const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");

function result(kind, details = {}) {
  return { kind, ...details };
}

function validInput(argv, environment) {
  const [sqlx, command, action, ...rest] = argv;
  if (!sqlx || command !== "migrate" || action !== "run" || !rest.includes("--source")) {
    return false;
  }
  if (!environment.DATABASE_URL || !/^\d+$/.test(environment.MYSERVER_DB_LOCK_RUNNER_LOCK_ID || "")) {
    return false;
  }
  return true;
}

export async function main(argv = process.argv.slice(2), environment = process.env) {
  if (!validInput(argv, environment)) return result("config");
  const [sqlx, ...args] = argv;
  const lockId = environment.MYSERVER_DB_LOCK_RUNNER_LOCK_ID;
  let client;
  let acquired = false;
  try {
    client = new Client({ connectionString: environment.DATABASE_URL });
    await client.connect();
    const lock = await client.query("SELECT pg_try_advisory_lock($1) AS acquired", [lockId]);
    acquired = lock.rows[0]?.acquired === true;
    if (!acquired) return result("lock-unavailable");
    const child = spawnSync(sqlx, args, {
      cwd: projectRoot,
      env: environment,
      encoding: "utf8"
    });
    if (child.error) return result("runner-failure");
    return result("sqlx", {
      status: child.status ?? 1,
      output: redact(`${child.stdout || ""}${child.stderr || ""}`).trim()
    });
  } catch {
    return result("connection-failure");
  } finally {
    if (client) {
      if (acquired) {
        try { await client.query("SELECT pg_advisory_unlock($1)", [lockId]); } catch { /* closing the session is the final release guard */ }
      }
      try { await client.end(); } catch { /* connection cleanup cannot change the migration result */ }
    }
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().then((report) => {
    process.stdout.write(`${JSON.stringify(report)}\n`);
  }).catch(() => {
    process.stdout.write(`${JSON.stringify(result("runner-failure"))}\n`);
  });
}
