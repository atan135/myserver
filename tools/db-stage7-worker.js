import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { EXIT, executeDatabase, redact } from "./db.js";

const temporaryDatabasePattern = /^myserver_stage7_[a-z0-9_]+$/;
const loopbackHosts = new Set(["localhost", "127.0.0.1", "::1"]);

function workerDatabase(environment) {
  const database = environment.MYSERVER_STAGE7_WORKER_DATABASE;
  const url = environment.MYSERVER_STAGE7_WORKER_URL;
  const migrationDirectory = environment.MYSERVER_STAGE7_WORKER_MIGRATION_DIRECTORY;
  const logicalOwner = environment.MYSERVER_STAGE7_WORKER_LOGICAL_OWNER;
  const key = environment.MYSERVER_STAGE7_WORKER_KEY;
  if (!temporaryDatabasePattern.test(database || "")) {
    throw new Error("stage 7 worker only accepts a guarded temporary database");
  }
  if (!/^[a-z][a-z0-9-]{2,63}$/.test(key || "")) {
    throw new Error("stage 7 worker key is invalid");
  }
  if (!/^[a-z][a-z0-9-]{2,63}$/.test(logicalOwner || "")) {
    throw new Error("stage 7 worker logical owner is invalid");
  }
  if (!/^tests\/fixtures\/db\/(?:stage7|stage4-rollout)\/[a-z0-9_\-/]+$/.test(migrationDirectory || "")) {
    throw new Error("stage 7 worker migration directory is outside approved fixtures");
  }
  let parsed;
  try {
    parsed = new URL(url);
  } catch {
    throw new Error("stage 7 worker database URL is invalid");
  }
  const host = parsed.hostname.replace(/^\[(.*)\]$/, "$1").toLowerCase();
  if (!['postgres:', 'postgresql:'].includes(parsed.protocol) || !loopbackHosts.has(host) || Number(parsed.port || "5432") !== 5432 || decodeURIComponent(parsed.pathname.replace(/^\//, "")) !== database) {
    throw new Error("stage 7 worker database URL must target localhost:5432 and its guarded temporary database");
  }
  return {
    key,
    defaultDatabase: database,
    logicalOwner,
    migrationDirectory,
    urlEnvironment: "MYSERVER_STAGE7_WORKER_URL",
    userEnvironment: "MYSERVER_STAGE7_WORKER_USER",
    passwordEnvironment: "MYSERVER_STAGE7_WORKER_PASSWORD"
  };
}

export async function main(environment = process.env) {
  try {
    const database = workerDatabase(environment);
    return executeDatabase("up", database, "stage7-drill", { environment });
  } catch (error) {
    return {
      database: "stage7-worker",
      ok: false,
      code: EXIT.CONFIG,
      error: redact(error?.message || String(error))
    };
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().then((report) => {
    process.stdout.write(`${JSON.stringify(report)}\n`);
    process.exitCode = report.code;
  }).catch((error) => {
    process.stdout.write(`${JSON.stringify({ database: "stage7-worker", ok: false, code: EXIT.EXECUTION, error: redact(error?.message || String(error)) })}\n`);
    process.exitCode = EXIT.EXECUTION;
  });
}
