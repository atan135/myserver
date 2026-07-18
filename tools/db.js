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
const migrationSafetyConfigPath = join(projectRoot, "db", "config", "migration-safety.json");
const baselineAllowlistPath = join(projectRoot, "db", "schema", "baseline-allowlist.json");
const catalogSnapshotPath = join(projectRoot, "db", "schema", "catalog-snapshot.sql");

const SAFETY_HEADER_KEYS = Object.freeze([
  "Logical owner",
  "Compatibility phase",
  "Irreversible risk",
  "Transaction",
  "Non-transaction reason",
  "Lock timeout",
  "Statement timeout",
  "Backup point",
  "Recovery command",
  "Risk summary"
]);
const COMPATIBILITY_PHASES = new Set(["expand", "migrate", "contract"]);
const IRREVERSIBLE_RISKS = new Set(["none", "data-loss", "data-rewrite", "external-state"]);
const LEGACY_INITIAL_SCHEMA_FILENAME = "20260718161350_initial_schema.sql";
const LEGACY_INITIAL_SCHEMA_CHECKSUMS = new Map([
  ["announce-service", "5a93d41a465799d901e715551aa38040e30c9c2954876fc77b833d1e54fd307c5c3b38d369e3a631d4a011b48ba65096"],
  ["auth-http", "880b0807f925b0dcefe3610d323e26dad3ce2043f680338aeb3e7eaa3699afd519f3352217aaf86563cb73d8991e8e08"],
  ["chat-server", "1b17a65df95a929bbd79c69eafe1e4f9a42797f7c8bc44da3338d221a91aeda06fe93d063134900c4b37117a488dd946"],
  ["game-server", "5c9f8dd9d795d39b22217e2cf7196d4a6bebb732c0408201135fb116d8774c95e64bc344ab01f6b791d874e755989565"],
  ["mail-service", "a0601e1052c897ddbb878ef83e2198457845815f6d2c1dc71b5da4c3ae879621e063cfd9d6501488a24298e5d1ad6ffc"]
]);

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

export function baselinePolicy(databaseKey, expectedFingerprint, allowlist = loadJson(baselineAllowlistPath), migrations = []) {
  if (typeof expectedFingerprint !== "string") {
    return { allowed: false, reason: "expected baseline fingerprint is required" };
  }
  if (!allowlist || allowlist.schema !== 2) {
    return { allowed: false, reason: "baseline allowlist schema must be 2" };
  }
  const fingerprints = allowlist.databases?.[databaseKey]?.fingerprints;
  if (!Array.isArray(fingerprints)) {
    return {
      allowed: false,
      reason: "baseline allowlist has no reviewed fingerprint entries for this database"
    };
  }
  const matches = fingerprints.filter((entry) => entry && typeof entry.sha256 === "string" && entry.sha256.toLowerCase() === expectedFingerprint.toLowerCase());
  if (matches.length !== 1) {
    return {
      allowed: false,
      reason: "fingerprint is not a reviewed baseline variant; refusing to write SQLx migration history"
    };
  }
  const entry = matches[0];
  if (!/^[a-f0-9]{64}$/i.test(entry.sha256) || typeof entry.version !== "string" || !/^\d{14}$/.test(entry.version) || typeof entry.description !== "string" || !/^[\x20-\x7e]+$/.test(entry.description) || entry.description !== entry.description.trim()) {
    return {
      allowed: false,
      reason: "reviewed baseline fingerprint must bind a SHA-256, 14-digit target version and ASCII target description"
    };
  }
  if (!Array.isArray(migrations) || migrations.length === 0 || migrations.some((migration) => !migration || !/^\d{14}$/.test(migration.version || "") || typeof migration.description !== "string")) {
    return {
      allowed: false,
      reason: "local migration directory has no valid target range for baseline"
    };
  }
  const highestVersion = migrations.at(-1).version;
  if (BigInt(entry.version) > BigInt(highestVersion)) {
    return {
      allowed: false,
      reason: "reviewed baseline target version is beyond the local migration directory"
    };
  }
  const targetIndex = migrations.findIndex((migration) => migration.version === entry.version);
  if (targetIndex === -1 || migrations[targetIndex].description !== entry.description) {
    return {
      allowed: false,
      reason: "reviewed baseline target version and description do not match a local migration"
    };
  }
  return {
    allowed: true,
    entry,
    targetMigration: migrations[targetIndex],
    migrations: migrations.slice(0, targetIndex + 1)
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

function baselineAuditSummary(database, targetMigration, migrations) {
  return `database=${database.key};target_version=${targetMigration.version};target_description=${targetMigration.description};versions=${migrations.map(({ version }) => version).join(",")}`;
}

async function runBaselineTransaction(url, database, actor, expectedFingerprint, migrations, targetMigration, runtime) {
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
      [actor, baselineAuditSummary(database, targetMigration, migrations)]
    );
    await client.query("COMMIT");
    inTransaction = false;
    return { ok: true, migrations, targetMigration, fingerprint: snapshot.sha256 };
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
  for (const key of url.searchParams.keys()) {
    if (key === "options" || /^options\[.*\]$/.test(key)) {
      throw new Error(`${database.urlEnvironment} must not set PostgreSQL options; the migration CLI owns lock_timeout and statement_timeout`);
    }
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

function positiveInteger(value, label) {
  if (!Number.isSafeInteger(value) || value <= 0) throw new Error(`${label} must be a positive integer`);
  return value;
}

export function migrationSafetyConfig(config = loadJson(migrationSafetyConfigPath)) {
  if (!config || config.schema !== 1 || typeof config !== "object") {
    throw new Error("migration safety config schema must be 1");
  }
  const defaults = config.defaults || {};
  const maximums = config.maximums || {};
  const normalized = {
    defaultLockTimeoutMs: positiveInteger(defaults.lockTimeoutMs, "default lock timeout"),
    defaultStatementTimeoutMs: positiveInteger(defaults.statementTimeoutMs, "default statement timeout"),
    maximumLockTimeoutMs: positiveInteger(maximums.lockTimeoutMs, "maximum lock timeout"),
    maximumStatementTimeoutMs: positiveInteger(maximums.statementTimeoutMs, "maximum statement timeout"),
    nonTransactionReasons: new Set(config.nonTransactionReasons || [])
  };
  if (normalized.defaultLockTimeoutMs > normalized.maximumLockTimeoutMs) {
    throw new Error("default lock timeout exceeds the maximum lock timeout");
  }
  if (normalized.defaultStatementTimeoutMs > normalized.maximumStatementTimeoutMs) {
    throw new Error("default statement timeout exceeds the maximum statement timeout");
  }
  if (normalized.nonTransactionReasons.size === 0 || [...normalized.nonTransactionReasons].some((reason) => !/^[a-z][a-z0-9-]+$/.test(reason))) {
    throw new Error("non-transaction reasons must be a non-empty list of lower-case identifiers");
  }
  return normalized;
}

function parseTimeout(value, label) {
  const match = /^(\d+)(ms|s|m|min)$/i.exec(value || "");
  if (!match) throw new Error(`${label} must use a positive ms, s or min duration`);
  const multiplier = { ms: 1, s: 1000, m: 60_000, min: 60_000 }[match[2].toLowerCase()];
  const milliseconds = Number(match[1]) * multiplier;
  return positiveInteger(milliseconds, label);
}

function migrationHeader(source, filename) {
  const lines = source.split(/\r?\n/);
  const hasNoTransactionDirective = source.startsWith("-- no-transaction");
  if (hasNoTransactionDirective && lines[0] !== "-- no-transaction") {
    throw new Error(`${filename}: SQLx no-transaction directive must be exactly the first line`);
  }
  const metadata = new Map();
  let index = hasNoTransactionDirective ? 1 : 0;
  for (; index < lines.length; index += 1) {
    const line = lines[index];
    if (!line.startsWith("--")) break;
    const match = /^-- ([A-Za-z][A-Za-z -]*): (.+)$/.exec(line);
    if (!match) continue;
    const [, key, value] = match;
    if (!SAFETY_HEADER_KEYS.includes(key)) continue;
    if (!/^[\x20-\x7e]+$/.test(value) || value !== value.trim()) {
      throw new Error(`${filename}: ${key} metadata must be trimmed ASCII text`);
    }
    if (metadata.has(key)) throw new Error(`${filename}: duplicate ${key} metadata`);
    metadata.set(key, value);
  }
  return { metadata, hasNoTransactionDirective };
}

function migrationSqlBody(source) {
  return source
    .replace(/^\s*--.*$/gm, "")
    .replace(/\/\*[\s\S]*?\*\//g, "");
}

function containsExplicitTransactionControl(source) {
  const sql = migrationSqlBody(source);
  return /(?:^|[;\n])\s*(?:BEGIN(?:\s+(?:WORK|TRANSACTION))?|START\s+TRANSACTION|COMMIT(?:\s+(?:WORK|TRANSACTION))?|ROLLBACK(?:\s+(?:WORK|TRANSACTION))?)\s*;/im.test(sql);
}

function containsExpandIncompatibleDdl(source) {
  const sql = migrationSqlBody(source);
  if (/\bDROP\s+(?:TABLE|TYPE|SCHEMA)\b/i.test(sql)) return true;
  const alterStatements = sql.match(/\bALTER\s+TABLE\b[^;]*/gi) || [];
  return alterStatements.some((statement) => /\b(?:DROP\s+COLUMN|RENAME\s+COLUMN|ALTER\s+COLUMN\s+(?:"[^"]+"|[A-Za-z_][A-Za-z0-9_$]*)\s+TYPE)\b/i.test(statement));
}

function containsDataLossDdl(source) {
  const sql = migrationSqlBody(source);
  if (/\bDROP\s+(?:TABLE|TYPE|SCHEMA)\b/i.test(sql)) return true;
  const alterStatements = sql.match(/\bALTER\s+TABLE\b[^;]*/gi) || [];
  return alterStatements.some((statement) => /\b(?:DROP\s+COLUMN|ALTER\s+COLUMN\s+(?:"[^"]+"|[A-Za-z_][A-Za-z0-9_$]*)\s+TYPE)\b/i.test(statement));
}

function matchesNonTransactionReason(reason, source) {
  const sql = migrationSqlBody(source);
  const patterns = {
    "create-index-concurrently": /\bCREATE\s+(?:UNIQUE\s+)?INDEX\s+CONCURRENTLY\b/i,
    "drop-index-concurrently": /\bDROP\s+INDEX\s+CONCURRENTLY\b/i,
    "reindex-concurrently": /\bREINDEX\b[^;]*\bCONCURRENTLY\b/i
  };
  return patterns[reason]?.test(sql) || false;
}

function metadataValue(metadata, key, filename) {
  const value = metadata.get(key);
  if (!value) throw new Error(`${filename}: ${key} metadata is required`);
  return value;
}

function isLegacyInitialSchema(filename, source, metadata, hasNoTransactionDirective) {
  const expectedChecksum = LEGACY_INITIAL_SCHEMA_CHECKSUMS.get(metadata.get("Logical owner"));
  return filename === LEGACY_INITIAL_SCHEMA_FILENAME &&
    !hasNoTransactionDirective &&
    metadata.has("Logical owner") &&
    metadata.get("Compatibility phase") === "expand" &&
    metadata.has("Irreversible risk") &&
    !metadata.has("Transaction") &&
    expectedChecksum === createHash("sha384").update(source).digest("hex");
}

export function migrationSafetyForFile(filename, source, options = {}) {
  const safety = options.safetyConfig || migrationSafetyConfig();
  const { metadata, hasNoTransactionDirective } = migrationHeader(source, filename);
  if (containsExplicitTransactionControl(source)) {
    throw new Error(`${filename}: migration SQL must not issue BEGIN, COMMIT or ROLLBACK`);
  }
  if (isLegacyInitialSchema(filename, source, metadata, hasNoTransactionDirective)) {
    if (options.expectedOwner && metadata.get("Logical owner") !== options.expectedOwner) {
      throw new Error(`${filename}: logical owner must be ${options.expectedOwner}`);
    }
    return {
      filename,
      legacy: true,
      logicalOwner: metadata.get("Logical owner"),
      compatibilityPhase: "expand",
      transaction: "required",
      lockTimeoutMs: safety.defaultLockTimeoutMs,
      statementTimeoutMs: safety.defaultStatementTimeoutMs
    };
  }

  const logicalOwner = metadataValue(metadata, "Logical owner", filename);
  const compatibilityPhase = metadataValue(metadata, "Compatibility phase", filename);
  const irreversibleRisk = metadataValue(metadata, "Irreversible risk", filename);
  const transaction = metadataValue(metadata, "Transaction", filename);
  const lockTimeoutMs = parseTimeout(metadataValue(metadata, "Lock timeout", filename), `${filename}: lock timeout`);
  const statementTimeoutMs = parseTimeout(metadataValue(metadata, "Statement timeout", filename), `${filename}: statement timeout`);
  const backupPoint = metadataValue(metadata, "Backup point", filename);
  const recoveryCommand = metadataValue(metadata, "Recovery command", filename);

  if (options.expectedOwner && logicalOwner !== options.expectedOwner) {
    throw new Error(`${filename}: logical owner must be ${options.expectedOwner}`);
  }
  if (!COMPATIBILITY_PHASES.has(compatibilityPhase)) {
    throw new Error(`${filename}: compatibility phase must be expand, migrate or contract`);
  }
  if (!IRREVERSIBLE_RISKS.has(irreversibleRisk)) {
    throw new Error(`${filename}: irreversible risk must be none, data-loss, data-rewrite or external-state`);
  }
  if (!/^(required|no-transaction)$/.test(transaction)) {
    throw new Error(`${filename}: Transaction metadata must be required or no-transaction`);
  }
  if (transaction === "no-transaction" !== hasNoTransactionDirective) {
    throw new Error(`${filename}: Transaction metadata and the first-line SQLx no-transaction directive must agree`);
  }
  if (lockTimeoutMs > safety.maximumLockTimeoutMs || statementTimeoutMs > safety.maximumStatementTimeoutMs) {
    throw new Error(`${filename}: timeout exceeds the approved migration safety budget`);
  }
  if (recoveryCommand === "not-required") {
    throw new Error(`${filename}: Recovery command must describe the verified failure path`);
  }
  if (compatibilityPhase === "expand" && (irreversibleRisk !== "none" || containsExpandIncompatibleDdl(source))) {
    throw new Error(`${filename}: expand migrations must be additive; use migrate/contract for renames, type changes and destructive DDL`);
  }
  if (irreversibleRisk === "none" && containsDataLossDdl(source)) {
    throw new Error(`${filename}: destructive DDL requires an irreversible risk classification and backup point`);
  }

  const nonTransactionReason = metadata.get("Non-transaction reason");
  if (transaction === "no-transaction") {
    if (!nonTransactionReason || !safety.nonTransactionReasons.has(nonTransactionReason)) {
      throw new Error(`${filename}: no-transaction migrations require an approved Non-transaction reason`);
    }
    if (!matchesNonTransactionReason(nonTransactionReason, source)) {
      throw new Error(`${filename}: non-transaction SQL does not contain the declared approved operation`);
    }
  } else if (nonTransactionReason) {
    throw new Error(`${filename}: Non-transaction reason is only valid with Transaction: no-transaction`);
  }

  if (irreversibleRisk === "none") {
    if (backupPoint !== "not-required") {
      throw new Error(`${filename}: reversible migrations must use Backup point: not-required`);
    }
    if (metadata.has("Risk summary")) {
      throw new Error(`${filename}: Risk summary is only valid for an irreversible migration`);
    }
  } else {
    if (backupPoint === "not-required") {
      throw new Error(`${filename}: irreversible migrations require a named backup point`);
    }
    metadataValue(metadata, "Risk summary", filename);
  }

  return {
    filename,
    legacy: false,
    logicalOwner,
    compatibilityPhase,
    irreversibleRisk,
    transaction,
    nonTransactionReason,
    lockTimeoutMs,
    statementTimeoutMs,
    backupPoint,
    recoveryCommand,
    riskSummary: metadata.get("Risk summary")
  };
}

export function migrationSafetyForDirectory(directory, options = {}) {
  const safetyConfig = options.safetyConfig || migrationSafetyConfig();
  const migrations = validateMigrationFiles(directory, { ...options, safetyConfig });
  return migrations.map((filename) => migrationSafetyForFile(
    filename,
    readFileSync(join(directory, filename), "utf8"),
    { ...options, safetyConfig }
  ));
}

export function migrationTimeoutBudget(migrations, safetyConfig = migrationSafetyConfig()) {
  if (!Array.isArray(migrations) || migrations.length === 0) {
    return {
      lockTimeoutMs: safetyConfig.defaultLockTimeoutMs,
      statementTimeoutMs: safetyConfig.defaultStatementTimeoutMs
    };
  }
  const requested = migrations.reduce((budget, migration) => ({
    lockTimeoutMs: Math.max(budget.lockTimeoutMs, migration.lockTimeoutMs),
    statementTimeoutMs: Math.max(budget.statementTimeoutMs, migration.statementTimeoutMs)
  }), { lockTimeoutMs: 0, statementTimeoutMs: 0 });
  return {
    lockTimeoutMs: Math.min(requested.lockTimeoutMs, safetyConfig.maximumLockTimeoutMs),
    statementTimeoutMs: Math.min(requested.statementTimeoutMs, safetyConfig.maximumStatementTimeoutMs)
  };
}

function migrationChildEnvironment(environment, database, budget) {
  return {
    ...environment,
    PGAPPNAME: `myserver-db-migrate-${database.key}`,
    PGOPTIONS: `-c lock_timeout=${budget.lockTimeoutMs}ms -c statement_timeout=${budget.statementTimeoutMs}ms`
  };
}

export function validateMigrationFiles(directory, options = {}) {
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
    migrationSafetyForFile(filename, readFileSync(join(directory, filename), "utf8"), options);
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

function runSqlx(sqlx, action, database, url, runtime, migrations, safetyConfig) {
  const directory = join(projectRoot, database.migrationDirectory);
  const safety = migrations || migrationSafetyForDirectory(directory, {
    expectedOwner: database.logicalOwner,
    safetyConfig
  });
  const budget = migrationTimeoutBudget(safety, safetyConfig);
  const result = runtime.run(sqlx.binary, ["migrate", action, "--source", directory], {
    ...migrationChildEnvironment(runtime.environment, database, budget),
    DATABASE_URL: url
  });
  return result;
}

function inspectDatabase(url, runtime, database, budget) {
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
      ...migrationChildEnvironment(psqlConnectionEnvironment(url, runtime.environment), database, budget)
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

function writeAudit(url, database, actor, startedAt, outcome, runtime, budget) {
  try {
    const result = runtime.run("psql", [
    "--no-psqlrc",
    "--command",
    `CREATE TABLE IF NOT EXISTS public._myserver_migration_audit (id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, operation text NOT NULL, actor text NOT NULL, started_at timestamptz NOT NULL, completed_at timestamptz NOT NULL, outcome text NOT NULL, history_summary text NOT NULL); INSERT INTO public._myserver_migration_audit (operation, actor, started_at, completed_at, outcome, history_summary) VALUES ('up', ${sqlLiteral(actor)}, ${sqlLiteral(startedAt)}::timestamptz, clock_timestamp(), 'success', (SELECT concat('count=', count(*), ';min=', coalesce(min(version)::text, ''), ';max=', coalesce(max(version)::text, '')) FROM public._sqlx_migrations));`
    ], {
      ...migrationChildEnvironment(psqlConnectionEnvironment(url, runtime.environment), database, budget)
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
    let localMigrations;
    try {
      localMigrations = sqlxMigrationMetadata(join(projectRoot, database.migrationDirectory));
    } catch (error) {
      return { database: database.key, ok: false, code: EXIT.VALIDATION, error: redact(error.message) };
    }
    const policy = baselinePolicy(database.key, overrides.expectedFingerprint, overrides.allowlist, localMigrations);
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
    return runBaselineTransaction(baselineUrl, database, actor, overrides.expectedFingerprint, policy.migrations, policy.targetMigration, runtime).then((transaction) => {
      if (!transaction.ok) return { database: database.key, ok: false, code: transaction.code, error: transaction.error };
      return {
        database: database.key,
        ok: true,
        code: EXIT.OK,
        output: `baseline recorded ${transaction.migrations.length} SQLx migration(s) through version ${transaction.targetMigration.version}`,
        audit: { actor, startedAt, completedAt: runtime.now(), fingerprint: transaction.fingerprint, targetVersion: transaction.targetMigration.version }
      };
    });
  }
  let url;
  try {
    url = connectionUrl(database, runtime.environment);
  } catch (error) {
    return { database: database.key, ok: false, code: EXIT.CONFIG, error: redact(error.message) };
  }

  let safetyConfig;
  let migrations = [];
  try {
    safetyConfig = migrationSafetyConfig(overrides.migrationSafetyConfig);
    if (command !== "status") {
      migrations = migrationSafetyForDirectory(join(projectRoot, database.migrationDirectory), {
        expectedOwner: database.logicalOwner,
        safetyConfig
      });
    }
  } catch (error) {
    return { database: database.key, ok: false, code: EXIT.VALIDATION, error: redact(error.message) };
  }
  const timeoutBudget = migrationTimeoutBudget(migrations, safetyConfig);
  const inspection = inspectDatabase(url, runtime, database, timeoutBudget);
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
  const validation = runSqlx(sqlx, "info", database, url, runtime, migrations, safetyConfig);
  if (validation.status !== 0) {
    const code = classifyFailure(validation.output);
    return { database: database.key, ok: false, code, error: diagnostic(code, "sqlx migrate info") };
  }
  if (command === "up") {
    const migration = runSqlx(sqlx, "run", database, url, runtime, migrations, safetyConfig);
    if (migration.status !== 0) {
      const code = classifyFailure(migration.output);
      return { database: database.key, ok: false, code, error: diagnostic(code, "sqlx migrate run") };
    }
    if (!writeAudit(url, database, actor, startedAt, "success", runtime, timeoutBudget)) {
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
