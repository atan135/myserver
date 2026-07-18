import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import pg from "pg";

const { Client } = pg;

const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const databaseConfigPath = join(projectRoot, "db", "config", "databases.json");
const sqlxConfigPath = join(projectRoot, "db", "config", "sqlx-cli.json");
const baselineAllowlistPath = join(projectRoot, "db", "schema", "baseline-allowlist.json");
const catalogSnapshotPath = join(projectRoot, "db", "schema", "catalog-snapshot.sql");

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
  if (!command || !["status", "up", "validate", "baseline"].includes(command)) {
    throw new Error("usage: db <status|up|validate|baseline> --database <auth|game|chat|announce|mail|all> [--actor <identity>]");
  }
  if (!options.database) throw new Error("--database is required");
  if (Object.keys(options).some((key) => !["database", "actor", "expected-fingerprint"].includes(key))) {
    throw new Error("only --database, --actor and --expected-fingerprint are supported");
  }
  if (command === "baseline") {
    if (options.database === "all") throw new Error("baseline requires one database, not all");
    if (!options.actor) throw new Error("--actor is required for baseline audit events");
    if (!/^[a-f0-9]{64}$/i.test(options["expected-fingerprint"] || "")) throw new Error("--expected-fingerprint must be a SHA-256 hex digest");
  } else if (options["expected-fingerprint"]) {
    throw new Error("--expected-fingerprint is only supported by baseline");
  }
  return { command, database: options.database, actor: options.actor, expectedFingerprint: options["expected-fingerprint"] };
}

export function baselinePolicy(databaseKey, expectedFingerprint, allowlist = loadJson(baselineAllowlistPath)) {
  if (typeof expectedFingerprint !== "string") {
    return { allowed: false, reason: "expected baseline fingerprint is required" };
  }
  const fingerprints = allowlist.databases?.[databaseKey]?.fingerprints || [];
  const match = fingerprints.find((entry) => entry.sha256 === expectedFingerprint.toLowerCase());
  if (!match) {
    return {
      allowed: false,
      reason: "fingerprint is not a reviewed baseline variant; refusing to write SQLx migration history"
    };
  }
  return {
    allowed: true,
    entry: match
  };
}

export function canonicalizeCatalog(rows) {
  if (!Array.isArray(rows) || rows.some((row) => !row || typeof row.object_kind !== "string" || typeof row.object_name !== "string" || typeof row.definition !== "string")) {
    throw new Error("catalog snapshot must contain object_kind, object_name and definition strings");
  }
  const normalized = rows.map(({ object_kind, object_name, definition }) => ({ object_kind, object_name, definition }))
    .sort((left, right) => left.object_kind.localeCompare(right.object_kind) || left.object_name.localeCompare(right.object_name) || left.definition.localeCompare(right.definition));
  const canonical = JSON.stringify(normalized);
  return { rows: normalized, canonical, sha256: createHash("sha256").update(canonical, "utf8").digest("hex") };
}

export function catalogQuery() {
  const query = readFileSync(catalogSnapshotPath, "utf8").trim().replace(/;$/, "");
  return `SELECT row_to_json(c)::text FROM (${query}) AS c ORDER BY object_kind, object_name, definition;`;
}

function crc32IsoHdlc(value) {
  let crc = 0xffffffff;
  for (const byte of Buffer.from(value, "utf8")) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit += 1) crc = (crc >>> 1) ^ (crc & 1 ? 0xedb88320 : 0);
  }
  return (crc ^ 0xffffffff) >>> 0;
}

export function sqlxPostgresLockId(databaseName) {
  // SQLx 0.8.6 sqlx-postgres/src/migrate.rs: generate_lock_id().
  return (0x3d32ad9en * BigInt(crc32IsoHdlc(databaseName))).toString();
}

export function sqlxMigrationMetadata(directory) {
  return validateMigrationFiles(directory).map((filename) => {
    const [version, rawDescription] = filename.replace(/\.sql$/, "").split(/_(.*)/s);
    const sql = readFileSync(join(directory, filename));
    return {
      version: BigInt(version).toString(),
      description: rawDescription.replaceAll("_", " "),
      checksum: createHash("sha384").update(sql).digest("hex")
    };
  });
}

function baselineAuditSummary(database, migrations) {
  return `database=${database.key};versions=${migrations.map(({ version }) => version).join(",")}`;
}

async function runBaselineTransaction(url, database, actor, expectedFingerprint, runtime) {
  const directory = join(projectRoot, database.migrationDirectory);
  let migrations;
  try {
    migrations = sqlxMigrationMetadata(directory);
  } catch (error) {
    return { ok: false, code: EXIT.VALIDATION, error: error.message };
  }
  const client = runtime.connectBaseline ? await runtime.connectBaseline(url) : new Client({ connectionString: url });
  let inTransaction = false;
  try {
    if (!runtime.connectBaseline) await client.connect();
    await client.query("BEGIN");
    inTransaction = true;
    await client.query("SET LOCAL search_path TO public, pg_catalog");
    // Keep the catalog read and history write in the SQLx-compatible lock session.
    await client.query("SELECT pg_advisory_lock($1)", [sqlxPostgresLockId(database.defaultDatabase)]);
    const snapshotResult = await client.query(catalogQuery());
    const snapshot = canonicalizeCatalog(snapshotResult.rows.map((row) => typeof row.row_to_json === "string" ? JSON.parse(row.row_to_json) : row.row_to_json));
    if (snapshot.sha256 !== expectedFingerprint.toLowerCase()) {
      await client.query("ROLLBACK");
      inTransaction = false;
      return { ok: false, code: EXIT.BASELINE_OR_DRIFT, error: "live catalog fingerprint does not match the expected reviewed baseline" };
    }
    const history = await client.query("SELECT to_regclass('public._sqlx_migrations') AS history");
    if (history.rows[0]?.history) {
      await client.query("ROLLBACK");
      inTransaction = false;
      return { ok: false, code: EXIT.BASELINE_OR_DRIFT, error: "_sqlx_migrations already exists; refusing repeated baseline" };
    }
    await client.query("CREATE TABLE IF NOT EXISTS _sqlx_migrations (version BIGINT PRIMARY KEY, description TEXT NOT NULL, installed_on TIMESTAMPTZ NOT NULL DEFAULT now(), success BOOLEAN NOT NULL, checksum BYTEA NOT NULL, execution_time BIGINT NOT NULL)");
    await client.query("CREATE TABLE IF NOT EXISTS public._myserver_migration_audit (id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, operation text NOT NULL, actor text NOT NULL, started_at timestamptz NOT NULL, completed_at timestamptz NOT NULL, outcome text NOT NULL, history_summary text NOT NULL)");
    for (const migration of migrations) {
      await client.query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) VALUES ($1, $2, TRUE, decode($3, 'hex'), -1)",
        [migration.version, migration.description, migration.checksum]
      );
    }
    await client.query(
      "INSERT INTO public._myserver_migration_audit (operation, actor, started_at, completed_at, outcome, history_summary) VALUES ('baseline', $1, clock_timestamp(), clock_timestamp(), 'success', $2)",
      [actor, baselineAuditSummary(database, migrations)]
    );
    await client.query("COMMIT");
    inTransaction = false;
    return { ok: true, migrations, fingerprint: snapshot.sha256 };
  } catch (error) {
    if (inTransaction) {
      try { await client.query("ROLLBACK"); } catch { /* preserve the original failure category */ }
    }
    const code = classifyFailure(error?.message);
    return { ok: false, code, error: diagnostic(code, "baseline transaction") };
  } finally {
    try { await client.end(); } catch { /* session close releases the advisory lock */ }
  }
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

function decodeUrlComponent(value) {
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}

export function psqlConnectionEnvironment(url, environment = process.env) {
  const parsed = new URL(url);
  const psqlEnvironment = { ...environment };
  const queryMappings = {
    application_name: "PGAPPNAME",
    connect_timeout: "PGCONNECT_TIMEOUT",
    options: "PGOPTIONS",
    sslcert: "PGSSLCERT",
    sslcrl: "PGSSLCRL",
    sslkey: "PGSSLKEY",
    sslmode: "PGSSLMODE",
    sslrootcert: "PGSSLROOTCERT"
  };
  const managedKeys = ["PGHOST", "PGPORT", "PGUSER", "PGPASSWORD", "PGDATABASE", ...Object.values(queryMappings)];
  for (const key of managedKeys) delete psqlEnvironment[key];

  const host = parsed.hostname.replace(/^\[(.*)\]$/, "$1");
  const database = decodeUrlComponent(parsed.pathname.replace(/^\//, ""));
  if (host) psqlEnvironment.PGHOST = host;
  if (parsed.port) psqlEnvironment.PGPORT = parsed.port;
  if (parsed.username) psqlEnvironment.PGUSER = decodeUrlComponent(parsed.username);
  if (parsed.password) psqlEnvironment.PGPASSWORD = decodeUrlComponent(parsed.password);
  if (database) psqlEnvironment.PGDATABASE = database;
  for (const [parameter, key] of Object.entries(queryMappings)) {
    if (parsed.searchParams.has(parameter)) psqlEnvironment[key] = parsed.searchParams.get(parameter);
  }
  return psqlEnvironment;
}

function platformKey() {
  const architecture = process.arch === "x64" ? "x64" : process.arch;
  return `${process.platform}-${architecture}`;
}

export function resolveSqlxBinary(config = loadJson(sqlxConfigPath)) {
  const platform = config.platforms[platformKey()];
  if (!platform) throw new Error(`sqlx-cli ${config.version} has no approved artifact for ${platformKey()}`);
  const binary = isAbsolute(platform.binary) ? platform.binary : join(projectRoot, platform.binary);
  const approvedArtifact = typeof platform.artifactUrl === "string" && (
    /^https:\/\//.test(platform.artifactUrl) ||
    /^local:\/\/cargo-install\/sqlx-cli-\d+\.\d+\.\d+\?locked=true&features=postgres%2Crustls$/.test(platform.artifactUrl)
  );
  if (platform.provisioned !== true || !approvedArtifact || !/^[a-f0-9]{64}$/i.test(platform.sha256 || "")) {
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
  if (/refuse baseline|catalog fingerprint|baseline fingerprint/.test(message)) return EXIT.BASELINE_OR_DRIFT;
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
      ...psqlConnectionEnvironment(url, runtime.environment)
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

function sqlLiteral(value) {
  return `'${String(value).replaceAll("\0", "").replaceAll("'", "''")}'`;
}

function writeAudit(url, database, actor, startedAt, outcome, runtime) {
  try {
    const result = runtime.run("psql", [
    "--no-psqlrc",
    "--command",
    `CREATE TABLE IF NOT EXISTS public._myserver_migration_audit (id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, operation text NOT NULL, actor text NOT NULL, started_at timestamptz NOT NULL, completed_at timestamptz NOT NULL, outcome text NOT NULL, history_summary text NOT NULL); INSERT INTO public._myserver_migration_audit (operation, actor, started_at, completed_at, outcome, history_summary) VALUES ('up', ${sqlLiteral(actor)}, ${sqlLiteral(startedAt)}::timestamptz, clock_timestamp(), 'success', (SELECT concat('count=', count(*), ';min=', coalesce(min(version)::text, ''), ';max=', coalesce(max(version)::text, '')) FROM public._sqlx_migrations));`
    ], {
      ...psqlConnectionEnvironment(url, runtime.environment)
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
    connectBaseline: overrides.connectBaseline,
    now: overrides.now || (() => new Date().toISOString())
  };
  const startedAt = runtime.now();
  if (command === "baseline") {
    const policy = baselinePolicy(database.key, overrides.expectedFingerprint, overrides.allowlist);
    if (!policy.allowed) {
      return {
        database: database.key,
        ok: false,
        code: EXIT.BASELINE_OR_DRIFT,
        error: policy.reason
      };
    }
    let baselineUrl;
    try {
      baselineUrl = connectionUrl(database, runtime.environment);
    } catch (error) {
      return { database: database.key, ok: false, code: EXIT.CONFIG, error: redact(error.message) };
    }
    return runBaselineTransaction(baselineUrl, database, actor, overrides.expectedFingerprint, runtime).then((transaction) => {
      if (!transaction.ok) return { database: database.key, ok: false, code: transaction.code, error: transaction.error };
      return {
        database: database.key,
        ok: true,
        code: EXIT.OK,
        output: `baseline recorded ${transaction.migrations.length} SQLx migration(s)`,
        audit: { actor, startedAt, completedAt: runtime.now(), fingerprint: transaction.fingerprint }
      };
    });
  }
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

export async function main(argv = process.argv.slice(2)) {
  let parsed;
  try {
    parsed = parseArguments(argv);
    const reports = [];
    for (const database of resolveDatabases(parsed.database)) {
      const report = await executeDatabase(parsed.command, database, parsed.actor || process.env.MYSERVER_DB_MIGRATION_ACTOR, {
        expectedFingerprint: parsed.expectedFingerprint
      });
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
  main().then((code) => { process.exitCode = code; }).catch((error) => {
    process.stderr.write(`${JSON.stringify({ error: redact(error.message) })}\n`);
    process.exitCode = EXIT.EXECUTION;
  });
}
