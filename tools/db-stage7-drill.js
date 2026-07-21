import { spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import pg from "pg";

import {
  EXIT,
  executeDatabase,
  executeDrift,
  redact,
  resolveDatabases,
  sqlxMigrationMetadata,
  sqlxPostgresLockId
} from "./db.js";

const { Client } = pg;
const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const workerPath = join(projectRoot, "tools", "db-stage7-worker.js");
const temporaryDatabasePrefix = "myserver_stage7_";
const temporaryDatabasePattern = /^myserver_stage7_[a-z0-9_]+$/;
const loopbackHosts = new Set(["localhost", "127.0.0.1", "::1"]);

function drillError(message, code = EXIT.EXECUTION) {
  const error = new Error(message);
  error.code = code;
  return error;
}

function delay(milliseconds) {
  return new Promise((resolveDelay) => setTimeout(resolveDelay, milliseconds));
}

export function temporaryDatabaseName(token, scope) {
  if (!/^[a-z0-9]{8,24}$/.test(token || "") || !/^[a-z][a-z0-9_]{1,30}$/.test(scope || "")) {
    throw new Error("stage 7 temporary database token or scope is invalid");
  }
  const name = `${temporaryDatabasePrefix}${token}_${scope}`;
  if (name.length > 63 || !temporaryDatabasePattern.test(name)) {
    throw new Error("stage 7 temporary database name is invalid");
  }
  return name;
}

export function temporaryBootstrapUrl(environment = process.env) {
  if (environment.MYSERVER_STAGE7_RUN_POSTGRES !== "1") {
    throw new Error("MYSERVER_STAGE7_RUN_POSTGRES=1 is required for the stage 7 PostgreSQL drill");
  }
  const raw = environment.MYSERVER_STAGE7_POSTGRES_URL;
  if (!raw) throw new Error("MYSERVER_STAGE7_POSTGRES_URL is required for the stage 7 PostgreSQL drill");
  let url;
  try {
    url = new URL(raw);
  } catch {
    throw new Error("MYSERVER_STAGE7_POSTGRES_URL must be a PostgreSQL URL");
  }
  const host = url.hostname.replace(/^\[(.*)\]$/, "$1").toLowerCase();
  if (!['postgres:', 'postgresql:'].includes(url.protocol) || !loopbackHosts.has(host) || Number(url.port || "5432") !== 5432 || decodeURIComponent(url.pathname.replace(/^\//, "")) !== "postgres") {
    throw new Error("stage 7 bootstrap URL must target localhost:5432/postgres");
  }
  if ([...url.searchParams.keys()].some((key) => key === "options" || /^options\[.*\]$/.test(key))) {
    throw new Error("stage 7 bootstrap URL must not set PostgreSQL options");
  }
  if (environment.MYSERVER_STAGE7_POSTGRES_USER) url.username = environment.MYSERVER_STAGE7_POSTGRES_USER;
  if (environment.MYSERVER_STAGE7_POSTGRES_PASSWORD) url.password = environment.MYSERVER_STAGE7_POSTGRES_PASSWORD;
  return url.toString();
}

function quoteIdentifier(identifier) {
  if (!temporaryDatabasePattern.test(identifier)) throw new Error("refusing to operate on a non-stage7 temporary database");
  return `"${identifier}"`;
}

function temporaryDatabaseUrl(bootstrapUrl, name) {
  if (!temporaryDatabasePattern.test(name)) throw new Error("refusing to build a URL for a non-stage7 temporary database");
  const url = new URL(bootstrapUrl);
  url.pathname = `/${name}`;
  return url.toString();
}

function temporaryDatabaseConfig(base, name, scope) {
  const environmentKey = `MYSERVER_STAGE7_${scope.toUpperCase()}_URL`;
  return {
    ...base,
    defaultDatabase: name,
    urlEnvironment: environmentKey,
    userEnvironment: `${environmentKey}_USER`,
    passwordEnvironment: `${environmentKey}_PASSWORD`
  };
}

function temporaryDatabaseEnvironment(environment, url, config) {
  return {
    ...environment,
    [config.urlEnvironment]: url,
    [config.userEnvironment]: undefined,
    [config.passwordEnvironment]: undefined
  };
}

async function withClient(url, callback) {
  const client = new Client({ connectionString: url });
  await client.connect();
  try {
    return await callback(client);
  } finally {
    await client.end();
  }
}

async function createTemporaryDatabase(bootstrapUrl, name) {
  await withClient(bootstrapUrl, async (client) => {
    await client.query(`CREATE DATABASE ${quoteIdentifier(name)}`);
  });
}

async function dropTemporaryDatabase(bootstrapUrl, name) {
  if (!temporaryDatabasePattern.test(name)) throw new Error("refusing to clean a non-stage7 temporary database");
  await withClient(bootstrapUrl, async (client) => {
    await client.query("SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = $1 AND pid <> pg_backend_pid()", [name]);
    await client.query(`DROP DATABASE IF EXISTS ${quoteIdentifier(name)}`);
  });
}

async function createFixtureContext(bootstrapUrl, created, environment, token, scope, base) {
  const name = temporaryDatabaseName(token, scope);
  await createTemporaryDatabase(bootstrapUrl, name);
  created.push(name);
  const url = temporaryDatabaseUrl(bootstrapUrl, name);
  const config = temporaryDatabaseConfig(base, name, scope);
  return {
    name,
    url,
    config,
    environment: temporaryDatabaseEnvironment(environment, url, config)
  };
}

function requireSuccess(report, label) {
  if (!report?.ok || report.code !== EXIT.OK) {
    throw drillError(`${label} returned exit code ${report?.code ?? "unknown"}`, report?.code || EXIT.EXECUTION);
  }
}

function requireCode(report, expectedCode, label) {
  if (report?.code !== expectedCode || report.ok !== false) {
    throw drillError(`${label} returned exit code ${report?.code ?? "unknown"}, expected ${expectedCode}`, report?.code || EXIT.EXECUTION);
  }
}

async function auditEvidence(url, migrations) {
  const expectedVersions = migrations.map(({ version }) => String(version));
  return withClient(url, async (client) => {
    const history = await client.query("SELECT version::text AS version, success FROM public._sqlx_migrations ORDER BY version");
    const historyVersions = history.rows.map(({ version }) => String(version));
    if (historyVersions.join(",") !== expectedVersions.join(",") || history.rows.some(({ success }) => success !== true)) {
      throw drillError("stage 7 current-state history is incomplete or unsuccessful", EXIT.VALIDATION);
    }
    const audit = await client.query("SELECT history_summary FROM public._myserver_migration_audit WHERE operation = 'up' ORDER BY id");
    const versionReference = `versions=${expectedVersions.join(",")}`;
    if (audit.rows.length < 2 || audit.rows.some(({ history_summary }) => !String(history_summary).includes(versionReference))) {
      throw drillError("stage 7 migration audit does not preserve exact applied versions", EXIT.EXECUTION);
    }
    return {
      historyVersions,
      auditRecords: audit.rows.length,
      auditExactVersionReference: true
    };
  });
}

async function tableExists(url, relation) {
  return withClient(url, async (client) => {
    const result = await client.query("SELECT to_regclass($1) IS NOT NULL AS exists", [relation]);
    return result.rows[0]?.exists === true;
  });
}

async function columnExists(url, table, column) {
  return withClient(url, async (client) => {
    const result = await client.query(
      "SELECT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_schema = 'public' AND table_name = $1 AND column_name = $2) AS exists",
      [table, column]
    );
    return result.rows[0]?.exists === true;
  });
}

async function runFiveDatabaseLifecycle(bootstrapUrl, created, environment, token) {
  const entries = [];
  for (const base of resolveDatabases("all")) {
    const scope = `current_${base.key}`;
    const context = await createFixtureContext(bootstrapUrl, created, environment, token, scope, base);
    const migrations = sqlxMigrationMetadata(join(projectRoot, base.migrationDirectory));
    const firstUp = executeDatabase("up", context.config, "stage7-drill", { environment: context.environment });
    requireSuccess(firstUp, `${base.key} empty database migration`);
    const currentValidation = executeDatabase("validate", context.config, undefined, { environment: context.environment });
    requireSuccess(currentValidation, `${base.key} current database validation`);
    const repeatedUp = executeDatabase("up", context.config, "stage7-drill", { environment: context.environment });
    requireSuccess(repeatedUp, `${base.key} repeated migration`);
    const drift = await executeDrift(context.config, "stage7-drill", { environment: context.environment });
    requireSuccess(drift, `${base.key} current schema drift check`);
    const audit = await auditEvidence(context.url, migrations);
    entries.push({
      database: base.key,
      initialMigrationVersions: firstUp.audit?.migrationVersions || [],
      targetMigrationVersion: firstUp.audit?.targetMigrationVersion,
      driftTargetVersion: drift.drift?.target?.migration?.version,
      initialMetrics: firstUp.metrics,
      repeatedMetrics: repeatedUp.metrics,
      audit
    });
  }
  return {
    name: "empty-current-repeat",
    databases: entries
  };
}

function fixtureBase(key, migrationDirectory, logicalOwner) {
  return {
    key,
    defaultDatabase: "unused",
    migrationDirectory,
    logicalOwner,
    urlEnvironment: "unused",
    userEnvironment: "unused",
    passwordEnvironment: "unused"
  };
}

async function runChecksumDrill(bootstrapUrl, created, environment, token) {
  const context = await createFixtureContext(
    bootstrapUrl,
    created,
    environment,
    token,
    "checksum",
    fixtureBase("stage7-checksum", "tests/fixtures/db/stage7/checksum/applied", "stage7-checksum-test")
  );
  const applied = executeDatabase("up", context.config, "stage7-drill", { environment: context.environment });
  requireSuccess(applied, "stage 7 checksum fixture migration");
  const tampered = executeDatabase("up", {
    ...context.config,
    migrationDirectory: "tests/fixtures/db/stage7/checksum/tampered"
  }, "stage7-drill", { environment: context.environment });
  requireCode(tampered, EXIT.VALIDATION, "stage 7 checksum tamper migration");
  if (!await tableExists(context.url, "public.stage7_checksum_fixture")) {
    throw drillError("stage 7 checksum failure changed the previously applied schema");
  }
  return {
    name: "checksum-tamper",
    appliedVersion: applied.audit?.targetMigrationVersion,
    failureCode: tampered.code,
    priorSchemaPreserved: true
  };
}

async function runSqlFailureDrill(bootstrapUrl, created, environment, token) {
  const context = await createFixtureContext(
    bootstrapUrl,
    created,
    environment,
    token,
    "sql_failure",
    fixtureBase("stage7-sql-failure", "tests/fixtures/db/stage7/sql-failure", "stage7-sql-failure-test")
  );
  const failed = executeDatabase("up", context.config, "stage7-drill", { environment: context.environment });
  requireCode(failed, EXIT.EXECUTION, "stage 7 invalid SQL migration");
  if (await tableExists(context.url, "public.stage7_sql_failure_sentinel")) {
    throw drillError("stage 7 SQL failure did not stop later migrations");
  }
  return {
    name: "sql-failure-stop",
    failureCode: failed.code,
    laterMigrationStopped: true
  };
}

function runWorker(context, key) {
  const environment = {
    ...context.environment,
    MYSERVER_STAGE7_WORKER_DATABASE: context.name,
    MYSERVER_STAGE7_WORKER_URL: context.url,
    MYSERVER_STAGE7_WORKER_MIGRATION_DIRECTORY: context.config.migrationDirectory,
    MYSERVER_STAGE7_WORKER_LOGICAL_OWNER: context.config.logicalOwner,
    MYSERVER_STAGE7_WORKER_KEY: key,
    MYSERVER_STAGE7_WORKER_USER: undefined,
    MYSERVER_STAGE7_WORKER_PASSWORD: undefined
  };
  return new Promise((resolveWorker, rejectWorker) => {
    const child = spawn(process.execPath, [workerPath], {
      cwd: projectRoot,
      env: environment,
      stdio: ["ignore", "pipe", "ignore"]
    });
    let output = "";
    child.stdout.setEncoding("utf8");
    child.stdout.on("data", (chunk) => {
      if (output.length <= 65536) output += chunk;
    });
    child.once("error", () => rejectWorker(drillError("stage 7 controlled worker could not start")));
    child.once("close", (status) => {
      try {
        const line = output.trim().split(/\r?\n/).at(-1);
        const report = JSON.parse(line || "");
        if (!report || typeof report !== "object" || typeof report.code !== "number") {
          throw new Error("invalid report");
        }
        resolveWorker({ status, report });
      } catch {
        rejectWorker(drillError(`stage 7 controlled worker returned no valid report (exit ${status ?? "unknown"})`));
      }
    });
  });
}

async function waitForMigrationMarker(url, applicationName, marker) {
  const deadline = Date.now() + 10_000;
  return withClient(url, async (client) => {
    while (Date.now() < deadline) {
      const result = await client.query(
        "SELECT pid FROM pg_stat_activity WHERE datname = current_database() AND application_name = $1 AND state = 'active' AND query LIKE $2 AND pid <> pg_backend_pid() ORDER BY pid LIMIT 1",
        [applicationName, `%${marker}%`]
      );
      if (result.rows[0]?.pid) return Number(result.rows[0].pid);
      await delay(50);
    }
    throw drillError("stage 7 timed out waiting for its controlled migration worker");
  });
}

async function terminateMigrationWorker(url, pid) {
  await withClient(url, async (client) => {
    const result = await client.query("SELECT pg_terminate_backend($1) AS terminated", [pid]);
    if (result.rows[0]?.terminated !== true) throw drillError("stage 7 could not terminate its controlled migration connection");
  });
}

async function runConnectionInterruptionDrill(bootstrapUrl, created, environment, token) {
  const context = await createFixtureContext(
    bootstrapUrl,
    created,
    environment,
    token,
    "connection_interrupt",
    fixtureBase("stage7-interruption", "tests/fixtures/db/stage7/connection-interruption", "stage7-interruption-test")
  );
  const worker = runWorker(context, "stage7-interruption");
  try {
    const pid = await waitForMigrationMarker(context.url, "myserver-db-migrate-stage7-interruption", "stage7-interruption-hold");
    await terminateMigrationWorker(context.url, pid);
    const completed = await worker;
    requireCode(completed.report, EXIT.CONNECTION, "stage 7 controlled connection interruption");
  } catch (error) {
    await Promise.allSettled([worker]);
    throw error;
  }
  if (await tableExists(context.url, "public.stage7_connection_interruption_sentinel")) {
    throw drillError("stage 7 connection interruption did not stop its migration");
  }
  return {
    name: "connection-interruption",
    failureCode: EXIT.CONNECTION,
    laterMigrationStopped: true
  };
}

async function runConcurrentDrill(bootstrapUrl, created, environment, token) {
  const context = await createFixtureContext(
    bootstrapUrl,
    created,
    environment,
    token,
    "concurrent",
    fixtureBase("stage7-concurrent", "tests/fixtures/db/stage7/concurrent", "stage7-concurrent-test")
  );
  const lockId = sqlxPostgresLockId(context.name);
  let lockClient;
  let blocked;
  try {
    lockClient = new Client({ connectionString: context.url });
    await lockClient.connect();
    await lockClient.query("SELECT pg_advisory_lock($1)", [lockId]);
    blocked = await runWorker(context, "stage7-concurrent");
    requireCode(blocked.report, EXIT.LOCK, "stage 7 migration blocked by its SQLx advisory lock");
  } finally {
    if (lockClient) {
      try { await lockClient.query("SELECT pg_advisory_unlock($1)", [lockId]); } finally { await lockClient.end(); }
    }
  }
  const retried = await runWorker(context, "stage7-concurrent");
  requireSuccess(retried.report, "stage 7 migration after advisory lock release");
  if (!await tableExists(context.url, "public.stage7_concurrent_fixture")) {
    throw drillError("stage 7 migration after advisory lock release did not create its expected object");
  }
  return {
    name: "concurrent-advisory-lock",
    lockHolder: "sqlx-postgres-advisory",
    blockedRunnerCode: blocked.report.code,
    retryRunnerCode: retried.report.code,
    serialized: true
  };
}

async function runLockTimeoutDrill(bootstrapUrl, created, environment, token) {
  const context = await createFixtureContext(
    bootstrapUrl,
    created,
    environment,
    token,
    "lock_timeout",
    fixtureBase("stage7-lock-timeout", "tests/fixtures/db/stage7/lock-timeout/base", "stage7-lock-timeout-test")
  );
  const base = executeDatabase("up", context.config, "stage7-drill", { environment: context.environment });
  requireSuccess(base, "stage 7 DDL lock timeout base migration");

  context.config.migrationDirectory = "tests/fixtures/db/stage7/lock-timeout/blocked";
  let lockClient;
  let blocked;
  try {
    lockClient = new Client({ connectionString: context.url });
    await lockClient.connect();
    await lockClient.query("BEGIN");
    await lockClient.query("LOCK TABLE stage7_lock_timeout_fixture IN ACCESS EXCLUSIVE MODE");
    blocked = executeDatabase("up", context.config, "stage7-drill", { environment: context.environment });
    requireCode(blocked, EXIT.LOCK, "stage 7 DDL lock timeout migration");
  } finally {
    if (lockClient) {
      try { await lockClient.query("ROLLBACK"); } catch { /* connection close releases the controlled table lock */ }
      await lockClient.end();
    }
  }

  if (await columnExists(context.url, "stage7_lock_timeout_fixture", "blocked_value")) {
    throw drillError("stage 7 DDL lock timeout unexpectedly changed the blocked table");
  }
  if (await tableExists(context.url, "public.stage7_lock_timeout_sentinel")) {
    throw drillError("stage 7 DDL lock timeout did not stop the later migration");
  }
  const retried = executeDatabase("up", context.config, "stage7-drill", { environment: context.environment });
  requireSuccess(retried, "stage 7 DDL lock timeout migration after release");
  if (!await columnExists(context.url, "stage7_lock_timeout_fixture", "blocked_value") || !await tableExists(context.url, "public.stage7_lock_timeout_sentinel")) {
    throw drillError("stage 7 DDL lock timeout retry did not apply its migrations");
  }
  return {
    name: "ddl-lock-timeout-retry",
    lockTimeoutMs: 500,
    failureCode: blocked.code,
    blockedDdlNotApplied: true,
    laterMigrationStopped: true,
    retryRunnerCode: retried.code
  };
}

async function runCompatibilityRecoveryDrill(bootstrapUrl, created, environment, token) {
  const context = await createFixtureContext(
    bootstrapUrl,
    created,
    environment,
    token,
    "compatibility",
    fixtureBase("stage7-compatibility", "tests/fixtures/db/stage4-rollout/legacy", "stage4-rollout-test")
  );
  let report = executeDatabase("up", context.config, "stage7-drill", { environment: context.environment });
  requireSuccess(report, "stage 7 legacy compatibility fixture");
  await withClient(context.url, async (client) => {
    await client.query("INSERT INTO stage4_rollout_accounts (legacy_name) VALUES ($1)", ["old-before-expand"]);
  });

  context.config.migrationDirectory = "tests/fixtures/db/stage4-rollout/expand";
  const firstExpand = executeDatabase("up", context.config, "stage7-drill", { environment: context.environment });
  requireSuccess(firstExpand, "stage 7 first expand compatibility fixture");
  const repeatedExpand = executeDatabase("up", context.config, "stage7-drill", { environment: context.environment });
  requireSuccess(repeatedExpand, "stage 7 repeated expand compatibility fixture");
  await withClient(context.url, async (client) => {
    const oldRead = await client.query("SELECT legacy_name FROM stage4_rollout_accounts WHERE legacy_name = $1", ["old-before-expand"]);
    if (oldRead.rowCount !== 1) throw drillError("stage 7 old caller could not read through expand");
    await client.query("INSERT INTO stage4_rollout_accounts (legacy_name) VALUES ($1)", ["old-after-expand"]);
    await client.query("INSERT INTO stage4_rollout_accounts (legacy_name, display_name) VALUES ($1, $2)", ["dual-write", "dual-write"]);
    await client.query("CREATE TABLE stage4_rollout_accounts_backup AS TABLE stage4_rollout_accounts");
  });

  context.config.migrationDirectory = "tests/fixtures/db/stage4-rollout/contract";
  report = executeDatabase("up", context.config, "stage7-drill", { environment: context.environment });
  requireSuccess(report, "stage 7 contract compatibility fixture");
  await withClient(context.url, async (client) => {
    let oldColumnRejected = false;
    try {
      await client.query("SELECT legacy_name FROM stage4_rollout_accounts");
    } catch (error) {
      oldColumnRejected = error?.code === "42703";
    }
    if (!oldColumnRejected) throw drillError("stage 7 contract did not remove the old compatibility column");
    await client.query("ALTER TABLE stage4_rollout_accounts ADD COLUMN legacy_name text");
    await client.query("UPDATE stage4_rollout_accounts AS target SET legacy_name = backup.legacy_name FROM stage4_rollout_accounts_backup AS backup WHERE backup.id = target.id");
    await client.query("ALTER TABLE stage4_rollout_accounts ALTER COLUMN legacy_name SET NOT NULL");
    await client.query("INSERT INTO stage4_rollout_accounts (legacy_name, display_name) VALUES ($1, $2)", ["old-after-recovery", "recovered"]);
    const recovered = await client.query("SELECT legacy_name FROM stage4_rollout_accounts WHERE legacy_name = $1", ["old-after-recovery"]);
    if (recovered.rowCount !== 1) throw drillError("stage 7 contract recovery did not restore old caller writes");
  });
  return {
    name: "expand-contract-recovery",
    expandOldCallerCompatible: true,
    expandFirstRunCode: firstExpand.code,
    expandRepeatedRunCode: repeatedExpand.code,
    contractRecoveryRestoredOldCaller: true,
    contractVersion: report.audit?.targetMigrationVersion
  };
}

export async function runStage7Drill(overrides = {}) {
  const environment = overrides.environment || process.env;
  const report = {
    command: "stage7-verification-drill",
    ok: false,
    code: EXIT.EXECUTION,
    services: "not-started-by-stage7-drill",
    readiness: "not-probed",
    temporaryDatabases: { created: [], cleanup: [] },
    scenarios: [],
    observability: {
      report: "single-line-json-with-migration-versions",
      audit: "history_summary includes exact applied versions",
      runtimeMetrics: {
        state: "implemented",
        transport: "core-nats",
        subject: "myserver.metrics.db-migration.<base64url-instance>",
        producer: "tools/db-migration-metrics.js",
        delivery: environment.MYSERVER_DB_MIGRATION_METRICS_ENABLED === "1" ? "enabled" : "disabled",
        failurePolicy: "best-effort delivery records unavailable without changing the migration result or audit"
      }
    }
  };
  let bootstrapUrl;
  try {
    bootstrapUrl = temporaryBootstrapUrl(environment);
    const token = overrides.randomToken ? overrides.randomToken() : randomBytes(8).toString("hex");
    if (!/^[a-z0-9]{8,24}$/.test(token)) throw drillError("stage 7 random temporary database token is invalid", EXIT.CONFIG);
    report.scenarios.push(await runFiveDatabaseLifecycle(bootstrapUrl, report.temporaryDatabases.created, environment, token));
    report.scenarios.push(await runChecksumDrill(bootstrapUrl, report.temporaryDatabases.created, environment, token));
    report.scenarios.push(await runSqlFailureDrill(bootstrapUrl, report.temporaryDatabases.created, environment, token));
    report.scenarios.push(await runConnectionInterruptionDrill(bootstrapUrl, report.temporaryDatabases.created, environment, token));
    report.scenarios.push(await runConcurrentDrill(bootstrapUrl, report.temporaryDatabases.created, environment, token));
    report.scenarios.push(await runLockTimeoutDrill(bootstrapUrl, report.temporaryDatabases.created, environment, token));
    report.scenarios.push(await runCompatibilityRecoveryDrill(bootstrapUrl, report.temporaryDatabases.created, environment, token));
    report.ok = true;
    report.code = EXIT.OK;
  } catch (error) {
    report.code = Number.isInteger(error?.code) ? error.code : EXIT.EXECUTION;
    report.error = redact(error?.message || String(error));
  } finally {
    if (bootstrapUrl) {
      for (const name of [...report.temporaryDatabases.created].reverse()) {
        try {
          await dropTemporaryDatabase(bootstrapUrl, name);
          report.temporaryDatabases.cleanup.push({ database: name, dropped: true });
        } catch {
          report.temporaryDatabases.cleanup.push({ database: name, dropped: false });
        }
      }
    }
    if (report.temporaryDatabases.cleanup.some(({ dropped }) => !dropped)) {
      report.ok = false;
      report.code = EXIT.EXECUTION;
      report.error = "stage 7 temporary database cleanup failed";
    }
  }
  return report;
}

export async function main() {
  return runStage7Drill();
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().then((report) => {
    process.stdout.write(`${JSON.stringify(report)}\n`);
    process.exitCode = report.code;
  }).catch((error) => {
    process.stdout.write(`${JSON.stringify({ command: "stage7-verification-drill", ok: false, code: EXIT.EXECUTION, error: redact(error?.message || String(error)) })}\n`);
    process.exitCode = EXIT.EXECUTION;
  });
}
