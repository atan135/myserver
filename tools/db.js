import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const databaseConfigPath = join(projectRoot, "db", "config", "databases.json");
const sqlxConfigPath = join(projectRoot, "db", "config", "sqlx-cli.json");

export const EXIT = Object.freeze({
  OK: 0,
  CONFIG: 2,
  CONNECTION: 3,
  VALIDATION: 4,
  LOCK: 5,
  EXECUTION: 6,
  BASELINE_OR_DRIFT: 7,
  SQLX: 8
});

export function redact(value) {
  if (value === undefined || value === null) return value;
  return String(value)
    .replace(/(postgres(?:ql)?:\/\/)([^\s/@:]+)(?::[^\s/@]*)?@/gi, "$1***:***@")
    .replace(/(password|passwd|pwd|token|secret)\s*[=:]\s*[^\s,;]+/gi, "$1=***");
}

export function parseArguments(argv) {
  const [command, ...rest] = argv;
  const options = {};
  for (let index = 0; index < rest.length; index += 1) {
    const token = rest[index];
    if (!token.startsWith("--")) throw new Error(`unexpected argument: ${token}`);
    const key = token.slice(2);
    const value = rest[index + 1];
    if (!value || value.startsWith("--")) throw new Error(`missing value for --${key}`);
    if (Object.hasOwn(options, key)) throw new Error(`duplicate option --${key}`);
    options[key] = value;
    index += 1;
  }
  if (!command || !["status", "up", "validate"].includes(command)) {
    throw new Error("usage: db <status|up|validate> --database <auth|game|chat|announce|mail|all> [--actor <identity>]");
  }
  if (!options.database) throw new Error("--database is required");
  if (Object.keys(options).some((key) => !["database", "actor"].includes(key))) {
    throw new Error("only --database and --actor are supported");
  }
  return { command, database: options.database, actor: options.actor };
}

function loadJson(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}

export function resolveDatabases(databaseKey, config = loadJson(databaseConfigPath)) {
  const keys = databaseKey === "all" ? Object.keys(config.databases) : [databaseKey];
  if (keys.length === 0 || keys.some((key) => !Object.hasOwn(config.databases, key))) {
    throw new Error(`unknown database: ${databaseKey}`);
  }
  return keys.map((key) => ({ key, ...config.databases[key] }));
}

function connectionUrl(database, environment = process.env) {
  const rawUrl = environment[database.urlEnvironment];
  if (!rawUrl) throw new Error(`${database.urlEnvironment} is required`);
  let url;
  try {
    url = new URL(rawUrl);
  } catch {
    throw new Error(`${database.urlEnvironment} must be a PostgreSQL URL`);
  }
  if (!["postgres:", "postgresql:"].includes(url.protocol)) {
    throw new Error(`${database.urlEnvironment} must use postgres:// or postgresql://`);
  }
  const configuredUser = environment[database.userEnvironment];
  const configuredPassword = environment[database.passwordEnvironment];
  if (configuredUser) url.username = configuredUser;
  if (configuredPassword) url.password = configuredPassword;
  const actualDatabase = decodeURIComponent(url.pathname.replace(/^\//, ""));
  if (actualDatabase !== database.defaultDatabase) {
    throw new Error(`${database.urlEnvironment} targets ${actualDatabase || "no database"}, expected ${database.defaultDatabase}`);
  }
  return url.toString();
}

function platformKey() {
  const architecture = process.arch === "x64" ? "x64" : process.arch;
  return `${process.platform}-${architecture}`;
}

export function resolveSqlxBinary(config = loadJson(sqlxConfigPath)) {
  const platform = config.platforms[platformKey()];
  if (!platform) throw new Error(`sqlx-cli ${config.version} has no approved artifact for ${platformKey()}`);
  const binary = isAbsolute(platform.binary) ? platform.binary : join(projectRoot, platform.binary);
  if (platform.provisioned !== true || typeof platform.artifactUrl !== "string" || !/^https:\/\//.test(platform.artifactUrl) || !/^[a-f0-9]{64}$/i.test(platform.sha256 || "")) {
    throw new Error(`sqlx-cli artifact is not provisioned for ${platformKey()}`);
  }
  if (!existsSync(binary)) throw new Error(`approved sqlx-cli binary is missing: ${platform.binary}`);
  const actualHash = createHash("sha256").update(readFileSync(binary)).digest("hex");
  if (actualHash.toLowerCase() !== platform.sha256.toLowerCase()) {
    throw new Error(`sqlx-cli SHA-256 mismatch for ${platform.binary}`);
  }
  return { binary, version: config.version };
}

function validateMigrationDirectory(directory) {
  if (!existsSync(directory)) throw new Error(`migration directory is missing: ${directory}`);
}

export function validateMigrationFiles(directory) {
  validateMigrationDirectory(directory);
  const migrations = readdirSync(directory, { withFileTypes: true })
    .filter((entry) => entry.isFile() && entry.name.endsWith(".sql"))
    .map((entry) => entry.name)
    .sort();
  let previousVersion = "";
  for (const filename of migrations) {
    if (!/^[\x20-\x7e]+$/.test(filename)) throw new Error(`migration filename must be ASCII: ${filename}`);
    const match = /^(\d{14})_([a-z][a-z0-9]*(?:_[a-z0-9]+)*)\.sql$/.exec(filename);
    if (!match) throw new Error(`invalid migration filename: ${filename}`);
    if (match[1] <= previousVersion) throw new Error(`migration versions must be strictly increasing: ${filename}`);
    previousVersion = match[1];
  }
  return migrations;
}

export function classifyFailure(output) {
  const message = String(output || "").toLowerCase();
  if (/checksum|migration.*(missing|invalid)|duplicate.*migration|version.*(order|invalid)/.test(message)) return EXIT.VALIDATION;
  if (/advisory lock|lock.*timeout|could not obtain lock/.test(message)) return EXIT.LOCK;
  if (/password authentication|authentication failed|connection refused|could not connect|tls|certificate/.test(message)) return EXIT.CONNECTION;
  return EXIT.EXECUTION;
}

function diagnostic(code, operation) {
  const category = {
    [EXIT.CONNECTION]: "connection or authentication failure",
    [EXIT.VALIDATION]: "migration history or checksum validation failure",
    [EXIT.LOCK]: "migration lock unavailable",
    [EXIT.EXECUTION]: "migration execution failure",
    [EXIT.SQLX]: "approved migration tool unavailable or incompatible"
  }[code] || "database migration failure";
  return `${operation}: ${category}`;
}

function run(command, args, environment) {
  const result = spawnSync(command, args, {
    cwd: projectRoot,
    env: environment,
    encoding: "utf8"
  });
  if (result.error) throw result.error;
  return {
    status: result.status ?? 1,
    output: redact(`${result.stdout || ""}${result.stderr || ""}`).trim()
  };
}

function runSqlx(sqlx, action, database, url, runtime) {
  const directory = join(projectRoot, database.migrationDirectory);
  validateMigrationFiles(directory);
  const result = runtime.run(sqlx.binary, ["migrate", action, "--source", directory], {
    ...runtime.environment,
    DATABASE_URL: url
  });
  return result;
}

function inspectDatabase(url, runtime) {
  try {
    const result = runtime.run("psql", [
      "--no-psqlrc",
      "--tuples-only",
      "--no-align",
      "--quiet",
      "--field-separator=,",
      "--command",
      "SELECT to_regclass('public._sqlx_migrations') IS NOT NULL, EXISTS (SELECT 1 FROM information_schema.tables WHERE table_schema = 'public' AND table_type = 'BASE TABLE' AND table_name <> '_sqlx_migrations')"
    ], {
      ...runtime.environment,
      DATABASE_URL: url
    });
    if (result.status !== 0) return { ok: false, code: classifyFailure(result.output) };
    const [history, managedTables] = result.output.trim().split(",");
    return { ok: true, history: history === "t", managedTables: managedTables === "t" };
  } catch (error) {
    return { ok: false, code: EXIT.SQLX };
  }
}

function verifySqlxVersion(sqlx, runtime) {
  try {
    const result = runtime.run(sqlx.binary, ["--version"], runtime.environment);
    if (result.status !== 0 || !new RegExp(`sqlx-cli\\s+${sqlx.version.replaceAll(".", "\\.")}`).test(result.output)) {
      return { ok: false };
    }
    return { ok: true };
  } catch (error) {
    return { ok: false };
  }
}

function writeAudit(url, database, actor, startedAt, outcome, runtime) {
  try {
    const result = runtime.run("psql", [
    "--no-psqlrc",
    "--set", `actor=${actor}`,
    "--set", `database=${database.key}`,
    "--set", `started_at=${startedAt}`,
    "--command",
    "CREATE TABLE IF NOT EXISTS public._myserver_migration_audit (id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, operation text NOT NULL, actor text NOT NULL, started_at timestamptz NOT NULL, completed_at timestamptz NOT NULL, outcome text NOT NULL, history_summary text NOT NULL); INSERT INTO public._myserver_migration_audit (operation, actor, started_at, completed_at, outcome, history_summary) VALUES ('up', :'actor', :'started_at'::timestamptz, clock_timestamp(), 'success', (SELECT concat('count=', count(*), ';min=', coalesce(min(version)::text, ''), ';max=', coalesce(max(version)::text, '')) FROM public._sqlx_migrations));"
    ], {
      ...runtime.environment,
      DATABASE_URL: url
    });
    return result.status === 0;
  } catch {
    return false;
  }
}

export function executeDatabase(command, database, actor, overrides = {}) {
  const runtime = {
    environment: overrides.environment || process.env,
    run: overrides.run || run,
    resolveSqlxBinary: overrides.resolveSqlxBinary || resolveSqlxBinary,
    now: overrides.now || (() => new Date().toISOString())
  };
  const startedAt = runtime.now();
  let url;
  try {
    url = connectionUrl(database, runtime.environment);
  } catch (error) {
    return { database: database.key, ok: false, code: EXIT.CONFIG, error: redact(error.message) };
  }

  const inspection = inspectDatabase(url, runtime);
  if (!inspection.ok) return { database: database.key, ok: false, code: inspection.code, error: diagnostic(inspection.code, "database preflight") };
  if (!inspection.history && command === "status") {
    return { database: database.key, ok: true, code: EXIT.OK, output: "_sqlx_migrations is absent" };
  }
  if (!inspection.history && command === "validate") {
    return { database: database.key, ok: false, code: EXIT.VALIDATION, error: "_sqlx_migrations is absent" };
  }

  if (command === "up") {
    if (!actor) return { database: database.key, ok: false, code: EXIT.CONFIG, error: "--actor is required for up audit events" };
    if (!inspection.history && inspection.managedTables) {
      return {
        database: database.key,
        ok: false,
        code: EXIT.BASELINE_OR_DRIFT,
        error: "public user tables exist but _sqlx_migrations is absent; refuse to migrate an unbaselined database. Run the stage 3 fingerprint baseline workflow."
      };
    }
  }

  let sqlx;
  try {
    sqlx = runtime.resolveSqlxBinary();
  } catch (error) {
    return { database: database.key, ok: false, code: EXIT.SQLX, error: diagnostic(EXIT.SQLX, "sqlx artifact check") };
  }
  const version = verifySqlxVersion(sqlx, runtime);
  if (!version.ok) return { database: database.key, ok: false, code: EXIT.SQLX, error: diagnostic(EXIT.SQLX, "sqlx version check") };
  const localDirectory = join(projectRoot, database.migrationDirectory);
  try {
    validateMigrationFiles(localDirectory);
  } catch (error) {
    return { database: database.key, ok: false, code: EXIT.VALIDATION, error: error.message };
  }

  const validation = runSqlx(sqlx, "info", database, url, runtime);
  if (validation.status !== 0) {
    const code = classifyFailure(validation.output);
    return { database: database.key, ok: false, code, error: diagnostic(code, "sqlx migrate info") };
  }
  if (command === "up") {
    const migration = runSqlx(sqlx, "run", database, url, runtime);
    if (migration.status !== 0) {
      const code = classifyFailure(migration.output);
      return { database: database.key, ok: false, code, error: diagnostic(code, "sqlx migrate run") };
    }
    if (!writeAudit(url, database, actor, startedAt, "success", runtime)) {
      return { database: database.key, ok: false, code: EXIT.EXECUTION, error: "migration audit write failed" };
    }
  }
  return {
    database: database.key,
    ok: true,
    code: EXIT.OK,
    audit: command === "up" ? { actor, startedAt, completedAt: runtime.now() } : undefined,
    output: validation.output || "sqlx migration command completed"
  };
}

export function main(argv = process.argv.slice(2)) {
  let parsed;
  try {
    parsed = parseArguments(argv);
    const reports = [];
    for (const database of resolveDatabases(parsed.database)) {
      const report = executeDatabase(parsed.command, database, parsed.actor || process.env.MYSERVER_DB_MIGRATION_ACTOR);
      reports.push(report);
      if (!report.ok) break;
    }
    const failed = reports.find((report) => !report.ok);
    process.stdout.write(`${JSON.stringify({ command: parsed.command, reports })}\n`);
    return failed ? failed.code : EXIT.OK;
  } catch (error) {
    process.stderr.write(`${JSON.stringify({ error: redact(error.message) })}\n`);
    return EXIT.CONFIG;
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exitCode = main();
}
