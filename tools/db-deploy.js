import { randomBytes } from "node:crypto";
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import pg from "pg";

import {
  EXIT,
  classifyFailure,
  connectionUrl,
  executeDatabase,
  executeDrift,
  loadDriftTarget,
  migrationSafetyForDirectory,
  redact,
  resolveDatabases,
  sqlxMigrationMetadata,
  sqlxPostgresLockId
} from "./db.js";

const { Client } = pg;
const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const deployGatePath = join(projectRoot, "db", "config", "deploy-gate.json");
const deploymentCommands = new Set(["validate", "preflight", "apply", "postflight", "rebuild-check"]);
const readinessFlags = new Set(["check-readiness", "require-readiness"]);
const temporaryConfirmation = "stage6-temporary-rebuild";
const temporaryDatabasePrefix = "myserver_stage6_";
const temporaryDatabasePattern = /^myserver_stage6_[a-z0-9_]+$/;
const loopbackHosts = new Set(["localhost", "127.0.0.1", "::1"]);
const versionPattern = /^\d{14}$/;
const tablePattern = /^public\.[a-z_][a-z0-9_]*$/;
const servicePattern = /^[a-z][a-z0-9-]{1,63}$/;
const environmentPattern = /^[a-z][a-z0-9-]{0,63}$/;
const actorPattern = /^[A-Za-z0-9_.@-]{1,128}$/;
const backupIdPattern = /^[A-Za-z0-9][A-Za-z0-9._:-]{2,127}$/;
const backupChecksumPattern = /^(?:[a-f0-9]{64}|[a-f0-9]{128})$/i;

export function parseDeploymentArguments(argv) {
  const [command, ...rest] = argv;
  if (!deploymentCommands.has(command)) {
    throw new Error("usage: db-deploy <validate|preflight|apply|postflight|rebuild-check> --environment <name> [--actor <identity>] [--check-readiness] [--require-readiness] [--confirm-temporary-rebuild stage6-temporary-rebuild]");
  }
  const options = {};
  for (let index = 0; index < rest.length; index += 1) {
    const token = rest[index];
    if (!token.startsWith("--")) throw new Error(`unexpected argument: ${token}`);
    const key = token.slice(2);
    if (Object.hasOwn(options, key)) throw new Error(`duplicate option --${key}`);
    if (readinessFlags.has(key)) {
      options[key] = true;
      continue;
    }
    const value = rest[index + 1];
    if (!value || value.startsWith("--")) throw new Error(`missing value for --${key}`);
    options[key] = value;
    index += 1;
  }
  const supported = new Set(["environment", "actor", "confirm-temporary-rebuild", ...readinessFlags]);
  if (Object.keys(options).some((key) => !supported.has(key))) {
    throw new Error("only --environment, --actor, --check-readiness, --require-readiness and --confirm-temporary-rebuild are supported");
  }
  if (!environmentPattern.test(options.environment || "")) {
    throw new Error("--environment requires a lower-case deployment environment name");
  }
  if (options.actor !== undefined && !actorPattern.test(options.actor)) {
    throw new Error("--actor must contain only letters, digits, dot, underscore, at sign or hyphen");
  }
  if (options["require-readiness"] && !options["check-readiness"]) {
    throw new Error("--require-readiness requires --check-readiness");
  }
  if (command === "apply" && !options.actor) {
    throw new Error("apply requires --actor for migration audit events");
  }
  if (command === "rebuild-check") {
    if (options["confirm-temporary-rebuild"] !== temporaryConfirmation) {
      throw new Error(`rebuild-check requires --confirm-temporary-rebuild ${temporaryConfirmation}`);
    }
    if (options.actor || options["check-readiness"] || options["require-readiness"]) {
      throw new Error("rebuild-check only supports --environment and --confirm-temporary-rebuild");
    }
  } else if (options["confirm-temporary-rebuild"] !== undefined) {
    throw new Error("--confirm-temporary-rebuild is only supported by rebuild-check");
  }
  return {
    command,
    environment: options.environment,
    actor: options.actor,
    checkReadiness: options["check-readiness"] === true,
    requireReadiness: options["require-readiness"] === true,
    confirmTemporaryRebuild: options["confirm-temporary-rebuild"]
  };
}

function loadJson(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}

function codeForIssues(issues) {
  return issues.find(({ code }) => code !== EXIT.OK)?.code || EXIT.OK;
}

function reportError(command, environment, error, phase = "configuration") {
  return {
    command,
    environment,
    phase,
    ok: false,
    code: EXIT.CONFIG,
    stopped: true,
    error: redact(error?.message || String(error)),
    recovery: ["Correct the version-controlled deployment configuration or command input, then rerun the deployment gate."]
  };
}

function deploymentRuntime(overrides = {}) {
  return {
    environment: overrides.environment || process.env,
    databases: overrides.databases,
    loadGateConfig: overrides.loadGateConfig || (() => loadJson(deployGatePath)),
    resolveDatabases: overrides.resolveDatabases || resolveDatabases,
    sqlxMigrationMetadata: overrides.sqlxMigrationMetadata || sqlxMigrationMetadata,
    migrationSafetyForDirectory: overrides.migrationSafetyForDirectory || migrationSafetyForDirectory,
    loadDriftTarget: overrides.loadDriftTarget || loadDriftTarget,
    connectionUrl: overrides.connectionUrl || connectionUrl,
    connect: overrides.connect,
    executeDatabase: overrides.executeDatabase || executeDatabase,
    executeDrift: overrides.executeDrift || executeDrift,
    fetch: overrides.fetch || globalThis.fetch,
    randomToken: overrides.randomToken || (() => randomBytes(8).toString("hex"))
  };
}

function ensureGateEntry(database, entry, target, localMigration) {
  if (!entry || typeof entry !== "object" || Array.isArray(entry)) {
    throw new Error(`deploy gate has no configuration for database ${database.key}`);
  }
  if (!Array.isArray(entry.keyTables) || entry.keyTables.length === 0 || entry.keyTables.some((table) => !tablePattern.test(table))) {
    throw new Error(`deploy gate ${database.key} must define non-empty public keyTables`);
  }
  if (new Set(entry.keyTables).size !== entry.keyTables.length) {
    throw new Error(`deploy gate ${database.key} keyTables must be unique`);
  }
  if (!Array.isArray(entry.services) || entry.services.length === 0) {
    throw new Error(`deploy gate ${database.key} must declare static service compatibility`);
  }
  const serviceNames = new Set();
  for (const service of entry.services) {
    if (!service || !servicePattern.test(service.name || "") || !versionPattern.test(service.minimumMigrationVersion || "") || !versionPattern.test(service.maximumMigrationVersion || "") || service.minimumMigrationVersion > service.maximumMigrationVersion) {
      throw new Error(`deploy gate ${database.key} has an invalid service compatibility declaration`);
    }
    if (serviceNames.has(service.name)) throw new Error(`deploy gate ${database.key} repeats service ${service.name}`);
    serviceNames.add(service.name);
  }
  if (!Array.isArray(entry.readiness)) {
    throw new Error(`deploy gate ${database.key} readiness must be an array`);
  }
  const readinessServices = new Set();
  for (const readiness of entry.readiness) {
    if (!readiness || !serviceNames.has(readiness.service) || !/^MYSERVER_DB_DEPLOY_[A-Z0-9_]+_READINESS_URL$/.test(readiness.urlEnvironment || "")) {
      throw new Error(`deploy gate ${database.key} has an invalid readiness declaration`);
    }
    if (readinessServices.has(readiness.service)) throw new Error(`deploy gate ${database.key} repeats readiness service ${readiness.service}`);
    readinessServices.add(readiness.service);
  }
  if (!target || !target.migration || !versionPattern.test(target.migration.version || "")) {
    throw new Error(`drift target for ${database.key} must bind a migration version`);
  }
  if (!localMigration || target.migration.version !== localMigration.version || target.migration.description !== localMigration.description || target.migration.checksum !== localMigration.checksum) {
    throw new Error(`deploy gate ${database.key} requires the reviewed drift target to match the latest local migration`);
  }
  const targetTables = new Set(target.objects.filter(({ object_kind }) => object_kind === "table").map(({ object_identity }) => object_identity));
  for (const table of entry.keyTables) {
    if (!targetTables.has(table)) throw new Error(`deploy gate ${database.key} key table is absent from the reviewed drift target: ${table}`);
  }
}

export function deploymentPlans(overrides = {}) {
  const runtime = deploymentRuntime(overrides);
  const config = runtime.loadGateConfig();
  if (!config || config.schema !== 1 || typeof config !== "object" || Array.isArray(config) || !config.databases || typeof config.databases !== "object") {
    throw new Error("deploy gate config schema must be 1 with a databases object");
  }
  const databases = runtime.databases || runtime.resolveDatabases("all");
  const keys = databases.map(({ key }) => key);
  const configuredKeys = Object.keys(config.databases).sort();
  if (configuredKeys.length !== keys.length || configuredKeys.some((key) => !keys.includes(key))) {
    throw new Error("deploy gate database keys must exactly match the migration database configuration");
  }
  return databases.map((database) => {
    const directory = join(projectRoot, database.migrationDirectory);
    const migrationMetadata = runtime.sqlxMigrationMetadata(directory);
    const migrationSafety = runtime.migrationSafetyForDirectory(directory, { expectedOwner: database.logicalOwner });
    if (migrationMetadata.length === 0 || migrationMetadata.length !== migrationSafety.length) {
      throw new Error(`deployment migration metadata is incomplete for ${database.key}`);
    }
    const migrationByVersion = new Map(migrationMetadata.map((migration) => [migration.version, migration]));
    const safetyByVersion = new Map(migrationSafety.map((safety) => [safety.filename.slice(0, 14), safety]));
    if (safetyByVersion.size !== migrationByVersion.size || migrationMetadata.some(({ version }) => !safetyByVersion.has(version))) {
      throw new Error(`deployment migration safety metadata is incomplete for ${database.key}`);
    }
    const localTarget = migrationMetadata.at(-1);
    const target = runtime.loadDriftTarget(database);
    const gate = config.databases[database.key];
    ensureGateEntry(database, gate, target, localTarget);
    const serviceCompatibility = gate.services.map((service) => ({
      service: service.name,
      minimumMigrationVersion: service.minimumMigrationVersion,
      maximumMigrationVersion: service.maximumMigrationVersion,
      targetMigrationVersion: localTarget.version,
      compatible: service.minimumMigrationVersion <= localTarget.version && localTarget.version <= service.maximumMigrationVersion,
      source: "version-controlled-static-declaration",
      runtimeObserved: false
    }));
    if (serviceCompatibility.some(({ compatible }) => !compatible)) {
      throw new Error(`deploy gate ${database.key} static service compatibility does not cover migration ${localTarget.version}`);
    }
    return {
      database,
      gate,
      migrationMetadata,
      migrationByVersion,
      safetyByVersion,
      localTarget,
      target,
      serviceCompatibility
    };
  });
}

async function openClient(url, database, runtime) {
  if (runtime.connect) return runtime.connect(url, database);
  const client = new Client({ connectionString: url });
  await client.connect();
  return client;
}

function historySummary(rows, migrations) {
  const applied = rows.map((row) => ({
    version: String(row.version),
    description: String(row.description),
    checksum: String(row.checksum || "").toLowerCase(),
    success: row.success === true
  }));
  const byVersion = new Map(applied.map((entry) => [entry.version, entry]));
  const pending = migrations.filter(({ version }) => !byVersion.has(version)).map(({ version }) => version);
  const unexpected = applied.filter(({ version }) => !migrations.some((migration) => migration.version === version)).map(({ version }) => version);
  const checksumMismatches = [];
  const failedVersions = [];
  for (const migration of migrations) {
    const actual = byVersion.get(migration.version);
    if (!actual) continue;
    if (actual.description !== migration.description || actual.checksum !== migration.checksum.toLowerCase()) {
      checksumMismatches.push(migration.version);
    }
    if (!actual.success) failedVersions.push(migration.version);
  }
  return {
    appliedVersions: applied.map(({ version }) => version),
    pendingVersions: pending,
    unexpectedVersions: unexpected,
    checksumMismatches,
    failedVersions,
    valid: unexpected.length === 0 && checksumMismatches.length === 0 && failedVersions.length === 0
  };
}

export async function inspectDeploymentDatabase(plan, options = {}, overrides = {}) {
  const runtime = deploymentRuntime(overrides);
  let url;
  try {
    url = runtime.connectionUrl(plan.database, runtime.environment);
  } catch (error) {
    return { database: plan.database.key, ok: false, code: EXIT.CONFIG, error: redact(error.message) };
  }
  let client;
  let lockAcquired = false;
  try {
    client = await openClient(url, plan.database, runtime);
    const identity = await client.query(
      "SELECT current_database() AS database_name, to_regclass('public._sqlx_migrations') AS history, EXISTS (SELECT 1 FROM information_schema.tables WHERE table_schema = 'public' AND table_type = 'BASE TABLE' AND table_name NOT IN ('_sqlx_migrations', '_myserver_migration_audit', '_myserver_backfill_state', '_myserver_backfill_audit')) AS managed_tables"
    );
    const identityRow = identity.rows[0] || {};
    if (identityRow.database_name !== plan.database.defaultDatabase) {
      return { database: plan.database.key, ok: false, code: EXIT.CONFIG, error: "migration URL resolved to an unexpected database" };
    }
    const historyExists = Boolean(identityRow.history);
    let history = {
      exists: historyExists,
      managedTables: identityRow.managed_tables === true,
      appliedVersions: [],
      pendingVersions: plan.migrationMetadata.map(({ version }) => version),
      unexpectedVersions: [],
      checksumMismatches: [],
      failedVersions: [],
      valid: false
    };
    if (historyExists) {
      const result = await client.query("SELECT version::text AS version, description, encode(checksum, 'hex') AS checksum, success FROM public._sqlx_migrations ORDER BY version");
      history = { exists: true, managedTables: identityRow.managed_tables === true, ...historySummary(result.rows, plan.migrationMetadata) };
    }
    const lock = await client.query("SELECT pg_try_advisory_lock($1) AS acquired", [sqlxPostgresLockId(plan.database.defaultDatabase)]);
    lockAcquired = lock.rows[0]?.acquired === true;
    const advisoryLock = { checked: true, available: lockAcquired };
    if (lockAcquired) {
      const released = await client.query("SELECT pg_advisory_unlock($1) AS released", [sqlxPostgresLockId(plan.database.defaultDatabase)]);
      lockAcquired = false;
      advisoryLock.released = released.rows[0]?.released === true;
      if (!advisoryLock.released) advisoryLock.available = false;
    }
    const keyTables = [];
    if (options.includeKeyTables) {
      for (const table of plan.gate.keyTables) {
        const result = await client.query("SELECT to_regclass($1) IS NOT NULL AS exists", [table]);
        keyTables.push({ table, exists: result.rows[0]?.exists === true });
      }
    }
    return {
      database: plan.database.key,
      ok: true,
      code: EXIT.OK,
      state: {
        history,
        advisoryLock,
        keyTables
      }
    };
  } catch (error) {
    const code = classifyFailure(error?.message);
    return { database: plan.database.key, ok: false, code, error: redact(error?.message || "deployment database inspection failed") };
  } finally {
    if (client) {
      if (lockAcquired) {
        try { await client.query("SELECT pg_advisory_unlock($1)", [sqlxPostgresLockId(plan.database.defaultDatabase)]); } catch { /* closing the session is the final lock release guard */ }
      }
      try { await client.end(); } catch { /* inspection is read-only and must not mask its primary result */ }
    }
  }
}

function backupEvidence(plan, pendingVersions, environment) {
  const irreversible = pendingVersions.map((version) => ({ version, safety: plan.safetyByVersion.get(version) }))
    .filter(({ safety }) => safety && !safety.legacy && safety.irreversibleRisk !== "none");
  const required = irreversible.length > 0;
  const key = plan.database.key.toUpperCase();
  const identifier = environment[`MYSERVER_DB_DEPLOY_BACKUP_${key}_ID`];
  const checksum = environment[`MYSERVER_DB_DEPLOY_BACKUP_${key}_CHECKSUM`];
  const identifierValid = !identifier || backupIdPattern.test(identifier);
  const checksumValid = !checksum || backupChecksumPattern.test(checksum);
  const present = Boolean(identifier) && Boolean(checksum) && identifierValid && checksumValid;
  return {
    required,
    requiredMigrationVersions: irreversible.map(({ version }) => version),
    identifierProvided: Boolean(identifier),
    checksumProvided: Boolean(checksum),
    identifierValid,
    checksumValid,
    verified: required ? present : true
  };
}

function issue(code, kind, message) {
  return { code, kind, message };
}

function recoverySteps(reports) {
  const failed = reports.find((report) => !report.ok);
  const steps = ["Do not continue later database or service deployment steps."];
  if (!failed) return steps;
  const kinds = new Set((failed.issues || []).map(({ kind }) => kind));
  if (kinds.has("advisory-lock")) steps.push("Identify the active migration holder, wait for it to release the SQLx advisory lock, then rerun the gate.");
  if (kinds.has("history") || kinds.has("checksum")) steps.push("Do not edit _sqlx_migrations manually; restore the reviewed migration files or history from the verified recovery path before retrying.");
  if (kinds.has("backup")) steps.push("Capture and verify the required backup artifact, then provide only its identifier and checksum through the deployment environment.");
  if (kinds.has("drift")) steps.push("Review the reported schema drift against the approved target; use a new reviewed migration or approved allowance, not ad hoc DDL.");
  if (kinds.has("readiness")) steps.push("Keep the last compatible service version in place and repair the failed readiness dependency before advancing traffic.");
  for (const version of failed.migration?.pendingVersions || []) {
    const recovery = failed.recoveryCommands?.find((entry) => entry.version === version);
    if (recovery?.command) steps.push(`Migration ${version}: ${recovery.command}`);
  }
  return [...new Set(steps)];
}

function recoveryCommands(plan, pendingVersions) {
  return pendingVersions.map((version) => ({ version, command: plan.safetyByVersion.get(version)?.recoveryCommand }))
    .filter(({ command }) => typeof command === "string" && command !== "not-required");
}

async function preflightDatabase(plan, options, runtime) {
  const inspection = await inspectDeploymentDatabase(plan, {}, runtime);
  if (!inspection.ok) {
    return {
      database: plan.database.key,
      ok: false,
      code: inspection.code,
      issues: [issue(inspection.code, "connection", inspection.error)],
      migration: { localTargetVersion: plan.localTarget.version },
      serviceCompatibility: plan.serviceCompatibility,
      recoveryCommands: []
    };
  }
  const { history, advisoryLock } = inspection.state;
  const backup = backupEvidence(plan, history.pendingVersions, runtime.environment);
  const issues = [];
  if (!history.exists) {
    if (history.managedTables) {
      issues.push(issue(EXIT.BASELINE_OR_DRIFT, "history", "user tables exist but SQLx history is absent; baseline review is required"));
    } else if (!options.allowUninitialized) {
      issues.push(issue(EXIT.VALIDATION, "history", "SQLx history is absent; deployment preflight only accepts initialized databases"));
    }
  } else if (!history.valid) {
    issues.push(issue(EXIT.VALIDATION, "checksum", "SQLx history contains an unexpected version, checksum mismatch or unsuccessful migration"));
  }
  if (!advisoryLock.available) {
    issues.push(issue(EXIT.LOCK, "advisory-lock", "SQLx advisory lock is unavailable"));
  }
  if (!backup.verified) {
    issues.push(issue(EXIT.VALIDATION, "backup", "pending irreversible migration lacks verified backup identifier and checksum evidence"));
  }
  let validation = { state: "skipped-prior-gate-failure" };
  if (issues.length === 0 && history.exists) {
    const result = await runtime.executeDatabase("validate", plan.database, undefined, { environment: runtime.environment });
    validation = { ok: result.ok, code: result.code };
    if (!result.ok) issues.push(issue(result.code, "migration-validation", result.error || "SQLx migration validation failed"));
  } else if (options.allowUninitialized && !history.exists && !history.managedTables) {
    validation = { state: "skipped-empty-temporary-database" };
  }
  const code = codeForIssues(issues);
  return {
    database: plan.database.key,
    ok: issues.length === 0,
    code,
    issues,
    migration: {
      localTargetVersion: plan.localTarget.version,
      history,
      pendingVersions: history.pendingVersions
    },
    advisoryLock,
    backup,
    serviceCompatibility: plan.serviceCompatibility,
    validation,
    recoveryCommands: recoveryCommands(plan, history.pendingVersions)
  };
}

export async function runPreflight(options, overrides = {}) {
  const runtime = deploymentRuntime(overrides);
  let plans;
  try {
    plans = deploymentPlans({ ...runtime, environment: runtime.environment });
  } catch (error) {
    return reportError("preflight", options.environment, error, "preflight");
  }
  const reports = [];
  for (const plan of plans) {
    const report = await preflightDatabase(plan, options, runtime);
    reports.push(report);
    if (!report.ok) {
      return {
        command: "preflight",
        environment: options.environment,
        phase: "preflight",
        ok: false,
        code: report.code,
        stopped: true,
        reports,
        recovery: recoverySteps(reports)
      };
    }
  }
  return {
    command: "preflight",
    environment: options.environment,
    phase: "preflight",
    ok: true,
    code: EXIT.OK,
    stopped: false,
    reports
  };
}

function readinessEndpoint(value) {
  if (!value) return undefined;
  let endpoint;
  try {
    endpoint = new URL(value);
  } catch {
    throw new Error("readiness endpoint must be an HTTP or HTTPS URL");
  }
  if (!["http:", "https:"].includes(endpoint.protocol) || endpoint.username || endpoint.password || endpoint.hash || endpoint.search) {
    throw new Error("readiness endpoint must be a credential-free HTTP or HTTPS URL without query or fragment");
  }
  return endpoint.toString();
}

function readinessTimeout(environment) {
  const raw = environment.MYSERVER_DB_DEPLOY_READINESS_TIMEOUT_MS;
  if (raw === undefined || raw === "") return 5000;
  if (!/^\d{3,5}$/.test(raw)) throw new Error("MYSERVER_DB_DEPLOY_READINESS_TIMEOUT_MS must be an integer from 100 to 60000");
  const value = Number(raw);
  if (value < 100 || value > 60000) throw new Error("MYSERVER_DB_DEPLOY_READINESS_TIMEOUT_MS must be an integer from 100 to 60000");
  return value;
}

async function readinessReport(plan, options, runtime) {
  const timeout = options.checkReadiness ? readinessTimeout(runtime.environment) : undefined;
  const reports = [];
  for (const readiness of plan.gate.readiness) {
    const rawEndpoint = runtime.environment[readiness.urlEnvironment];
    let endpoint;
    try {
      endpoint = readinessEndpoint(rawEndpoint);
    } catch (error) {
      reports.push({ service: readiness.service, endpointEnvironment: readiness.urlEnvironment, state: "invalid-config", error: redact(error.message) });
      continue;
    }
    if (!endpoint) {
      reports.push({ service: readiness.service, endpointEnvironment: readiness.urlEnvironment, state: "not-configured" });
      continue;
    }
    if (!options.checkReadiness) {
      reports.push({ service: readiness.service, endpointEnvironment: readiness.urlEnvironment, state: "not-checked" });
      continue;
    }
    try {
      const response = await runtime.fetch(endpoint, {
        method: "GET",
        redirect: "error",
        signal: AbortSignal.timeout(timeout),
        headers: { accept: "application/json" }
      });
      let healthy = response.ok;
      const contentType = response.headers?.get?.("content-type") || "";
      if (healthy && /application\/json/i.test(contentType)) {
        const body = await response.text();
        if (body.length <= 8192) {
          try {
            const payload = JSON.parse(body);
            if (payload && typeof payload === "object" && payload.ok === false) healthy = false;
          } catch { /* a 2xx non-JSON-compatible health response still has a successful transport status */ }
        }
      }
      reports.push({
        service: readiness.service,
        endpointEnvironment: readiness.urlEnvironment,
        state: healthy ? "healthy" : "unhealthy",
        status: response.status
      });
    } catch {
      reports.push({ service: readiness.service, endpointEnvironment: readiness.urlEnvironment, state: "unreachable" });
    }
  }
  return reports;
}

async function postflightDatabase(plan, options, runtime) {
  const inspection = await inspectDeploymentDatabase(plan, { includeKeyTables: true }, runtime);
  if (!inspection.ok) {
    return {
      database: plan.database.key,
      ok: false,
      code: inspection.code,
      issues: [issue(inspection.code, "connection", inspection.error)],
      migration: { localTargetVersion: plan.localTarget.version },
      serviceCompatibility: plan.serviceCompatibility,
      readiness: [],
      recoveryCommands: []
    };
  }
  const { history, advisoryLock, keyTables } = inspection.state;
  const issues = [];
  if (!history.exists || !history.valid || history.pendingVersions.length > 0) {
    issues.push(issue(EXIT.VALIDATION, "history", "postflight requires complete and valid SQLx history with no pending migration"));
  }
  if (!advisoryLock.available) {
    issues.push(issue(EXIT.LOCK, "advisory-lock", "SQLx advisory lock is unavailable during postflight"));
  }
  if (keyTables.some(({ exists }) => !exists)) {
    issues.push(issue(EXIT.BASELINE_OR_DRIFT, "key-objects", "a required key table is absent after migration"));
  }
  let validation = { state: "skipped-prior-gate-failure" };
  if (issues.length === 0) {
    const result = await runtime.executeDatabase("validate", plan.database, undefined, { environment: runtime.environment });
    validation = { ok: result.ok, code: result.code };
    if (!result.ok) issues.push(issue(result.code, "migration-validation", result.error || "SQLx migration validation failed"));
  }
  let drift = { state: "skipped-prior-gate-failure" };
  if (issues.length === 0) {
    const result = await runtime.executeDrift(plan.database, options.environment, { environment: runtime.environment });
    drift = result.ok
      ? { ok: true, code: result.code, target: result.drift?.target, actual: result.drift?.actual, unapprovedCount: result.drift?.differences?.unapproved?.length || 0 }
      : { ok: false, code: result.code, unapprovedCount: result.drift?.differences?.unapproved?.length || 0 };
    if (!result.ok) issues.push(issue(result.code, "drift", result.error || "schema drift check failed"));
  }
  let readiness = [];
  if (issues.length === 0) {
    readiness = await readinessReport(plan, options, runtime);
    if (readiness.some(({ state }) => state === "invalid-config")) {
      issues.push(issue(EXIT.CONFIG, "readiness", "a configured readiness endpoint is invalid"));
    }
    if (readiness.some(({ state }) => state === "unhealthy" || state === "unreachable")) {
      issues.push(issue(EXIT.EXECUTION, "readiness", "a configured readiness endpoint did not report healthy"));
    }
    if (options.requireReadiness && readiness.some(({ state }) => state === "not-configured" || state === "not-checked")) {
      issues.push(issue(EXIT.CONFIG, "readiness", "required readiness endpoint is not configured or was not checked"));
    }
  }
  const code = codeForIssues(issues);
  return {
    database: plan.database.key,
    ok: issues.length === 0,
    code,
    issues,
    migration: {
      localTargetVersion: plan.localTarget.version,
      history
    },
    advisoryLock,
    keyTables,
    serviceCompatibility: plan.serviceCompatibility,
    readiness,
    validation,
    drift,
    recoveryCommands: recoveryCommands(plan, history.pendingVersions)
  };
}

export async function runPostflight(options, overrides = {}) {
  const runtime = deploymentRuntime(overrides);
  let plans;
  try {
    plans = deploymentPlans({ ...runtime, environment: runtime.environment });
  } catch (error) {
    return reportError("postflight", options.environment, error, "postflight");
  }
  const reports = [];
  for (const plan of plans) {
    const report = await postflightDatabase(plan, options, runtime);
    reports.push(report);
    if (!report.ok) {
      return {
        command: "postflight",
        environment: options.environment,
        phase: "postflight",
        ok: false,
        code: report.code,
        stopped: true,
        reports,
        recovery: recoverySteps(reports)
      };
    }
  }
  return {
    command: "postflight",
    environment: options.environment,
    phase: "postflight",
    ok: true,
    code: EXIT.OK,
    stopped: false,
    reports
  };
}

export function runStaticValidation(options, overrides = {}) {
  try {
    const plans = deploymentPlans(overrides);
    return {
      command: "validate",
      environment: options.environment,
      phase: "static-validation",
      ok: true,
      code: EXIT.OK,
      stopped: false,
      reports: plans.map((plan) => ({
        database: plan.database.key,
        localTarget: plan.localTarget,
        targetObjectCount: plan.target.objects.length,
        keyTables: plan.gate.keyTables,
        serviceCompatibility: plan.serviceCompatibility,
        readinessConfiguredByEnvironment: plan.gate.readiness.map(({ service, urlEnvironment }) => ({ service, urlEnvironment }))
      }))
    };
  } catch (error) {
    return reportError("validate", options.environment, error, "static-validation");
  }
}

export async function runApply(options, overrides = {}) {
  const runtime = deploymentRuntime(overrides);
  const preflight = await runPreflight(options, runtime);
  if (!preflight.ok) {
    return {
      command: "apply",
      environment: options.environment,
      phase: "preflight",
      ok: false,
      code: preflight.code,
      stopped: true,
      preflight,
      migrations: [],
      postflight: { state: "not-run" },
      recovery: preflight.recovery
    };
  }
  let plans;
  try {
    plans = deploymentPlans(runtime);
  } catch (error) {
    return reportError("apply", options.environment, error, "migration");
  }
  const migrations = [];
  for (const plan of plans) {
    const result = await runtime.executeDatabase("up", plan.database, options.actor, { environment: runtime.environment });
    const report = { database: plan.database.key, ok: result.ok, code: result.code };
    if (!result.ok) report.error = result.error || "migration execution failed";
    migrations.push(report);
    if (!report.ok) {
      const preflightReport = preflight.reports.find(({ database }) => database === plan.database.key);
      const recovery = [
        "Do not continue later database or service deployment steps.",
        "Inspect the failed migration history and execute only the reviewed recovery command for the affected migration before retrying.",
        ...(preflightReport?.recoveryCommands || []).map(({ version, command }) => `Migration ${version}: ${command}`)
      ];
      return {
        command: "apply",
        environment: options.environment,
        phase: "migration",
        ok: false,
        code: report.code,
        stopped: true,
        preflight,
        migrations,
        postflight: { state: "not-run" },
        recovery: [...new Set(recovery)]
      };
    }
  }
  const postflight = await runPostflight(options, runtime);
  if (!postflight.ok) {
    return {
      command: "apply",
      environment: options.environment,
      phase: "postflight",
      ok: false,
      code: postflight.code,
      stopped: true,
      preflight,
      migrations,
      postflight,
      recovery: postflight.recovery
    };
  }
  return {
    command: "apply",
    environment: options.environment,
    phase: "complete",
    ok: true,
    code: EXIT.OK,
    stopped: false,
    preflight,
    migrations,
    postflight,
    serviceDeployment: "not-started-by-database-tool"
  };
}

export function temporaryDatabaseName(token, databaseKey) {
  if (!/^[a-z0-9_]{4,40}$/.test(token) || !/^[a-z][a-z0-9_]{1,20}$/.test(databaseKey)) {
    throw new Error("temporary database token or database key is invalid");
  }
  const name = `${temporaryDatabasePrefix}${token}_${databaseKey}`;
  if (name.length > 63 || !temporaryDatabasePattern.test(name)) throw new Error("temporary database name is invalid");
  return name;
}

function quoteIdentifier(identifier) {
  if (!temporaryDatabasePattern.test(identifier)) throw new Error("refusing to operate on a non-stage6 temporary database");
  return `"${identifier}"`;
}

export function temporaryBootstrapUrl(environment = process.env) {
  if (environment.MYSERVER_DB_DEPLOY_TEMPORARY_REBUILD !== "1") {
    throw new Error("MYSERVER_DB_DEPLOY_TEMPORARY_REBUILD=1 is required for temporary rebuilds");
  }
  const raw = environment.MYSERVER_DB_DEPLOY_TEMPORARY_POSTGRES_URL;
  if (!raw) throw new Error("MYSERVER_DB_DEPLOY_TEMPORARY_POSTGRES_URL is required for temporary rebuilds");
  let url;
  try {
    url = new URL(raw);
  } catch {
    throw new Error("MYSERVER_DB_DEPLOY_TEMPORARY_POSTGRES_URL must be a PostgreSQL URL");
  }
  const host = url.hostname.replace(/^\[(.*)\]$/, "$1").toLowerCase();
  if (!["postgres:", "postgresql:"].includes(url.protocol) || !loopbackHosts.has(host) || Number(url.port || "5432") !== 5432 || decodeURIComponent(url.pathname.replace(/^\//, "")) !== "postgres") {
    throw new Error("temporary rebuild bootstrap URL must target localhost:5432/postgres");
  }
  if ([...url.searchParams.keys()].some((key) => key === "options" || /^options\[.*\]$/.test(key))) {
    throw new Error("temporary rebuild bootstrap URL must not set PostgreSQL options");
  }
  const configuredUser = environment.MYSERVER_DB_DEPLOY_TEMPORARY_POSTGRES_USER;
  const configuredPassword = environment.MYSERVER_DB_DEPLOY_TEMPORARY_POSTGRES_PASSWORD;
  if (configuredUser) url.username = configuredUser;
  if (configuredPassword) url.password = configuredPassword;
  return url.toString();
}

function temporaryEnvironment(baseEnvironment, bootstrapUrl, database, temporaryName) {
  const url = new URL(bootstrapUrl);
  url.pathname = `/${temporaryName}`;
  const key = database.key.toUpperCase();
  return {
    ...baseEnvironment,
    [`MYSERVER_STAGE6_${key}_URL`]: url.toString(),
    [`MYSERVER_STAGE6_${key}_USER`]: undefined,
    [`MYSERVER_STAGE6_${key}_PASSWORD`]: undefined
  };
}

function temporaryDatabaseConfig(database, temporaryName) {
  const key = database.key.toUpperCase();
  return {
    ...database,
    defaultDatabase: temporaryName,
    urlEnvironment: `MYSERVER_STAGE6_${key}_URL`,
    userEnvironment: `MYSERVER_STAGE6_${key}_USER`,
    passwordEnvironment: `MYSERVER_STAGE6_${key}_PASSWORD`
  };
}

async function createTemporaryDatabase(bootstrapUrl, name, runtime) {
  let client;
  try {
    client = await openClient(bootstrapUrl, { key: "stage6-bootstrap", defaultDatabase: "postgres" }, runtime);
    await client.query(`CREATE DATABASE ${quoteIdentifier(name)}`);
  } finally {
    if (client) await client.end();
  }
}

async function dropTemporaryDatabase(bootstrapUrl, name, runtime) {
  if (!temporaryDatabasePattern.test(name)) throw new Error("refusing to clean a non-stage6 temporary database");
  let client;
  try {
    client = await openClient(bootstrapUrl, { key: "stage6-bootstrap", defaultDatabase: "postgres" }, runtime);
    await client.query("SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = $1 AND pid <> pg_backend_pid()", [name]);
    await client.query(`DROP DATABASE IF EXISTS ${quoteIdentifier(name)}`);
  } finally {
    if (client) await client.end();
  }
}

export async function runRebuildCheck(options, overrides = {}) {
  const runtime = deploymentRuntime(overrides);
  let bootstrapUrl;
  let basePlans;
  try {
    bootstrapUrl = temporaryBootstrapUrl(runtime.environment);
    basePlans = deploymentPlans(runtime);
  } catch (error) {
    return reportError("rebuild-check", options.environment, error, "temporary-rebuild");
  }
  const token = runtime.randomToken();
  const names = [];
  const cleanup = [];
  let operation;
  try {
    for (const plan of basePlans) {
      const name = temporaryDatabaseName(token, plan.database.key);
      await createTemporaryDatabase(bootstrapUrl, name, runtime);
      names.push(name);
    }
    const environment = { ...runtime.environment };
    const databases = basePlans.map((plan) => {
      const name = temporaryDatabaseName(token, plan.database.key);
      Object.assign(environment, temporaryEnvironment(environment, bootstrapUrl, plan.database, name));
      return temporaryDatabaseConfig(plan.database, name);
    });
    const temporaryRuntime = { ...runtime, environment, databases };
    const preflight = await runPreflight({ ...options, allowUninitialized: true }, temporaryRuntime);
    if (!preflight.ok) {
      operation = {
        command: "rebuild-check",
        environment: options.environment,
        phase: "preflight",
        ok: false,
        code: preflight.code,
        stopped: true,
        preflight,
        migrations: [],
        postflight: { state: "not-run" },
        recovery: preflight.recovery
      };
      return operation;
    }
    const migrations = [];
    for (const database of databases) {
      const result = await temporaryRuntime.executeDatabase("up", database, "stage6-temporary-rebuild", { environment });
      const report = { database: database.key, ok: result.ok, code: result.code };
      if (!result.ok) report.error = result.error || "temporary migration execution failed";
      migrations.push(report);
      if (!report.ok) {
        operation = {
          command: "rebuild-check",
          environment: options.environment,
          phase: "migration",
          ok: false,
          code: report.code,
          stopped: true,
          preflight,
          migrations,
          postflight: { state: "not-run" },
          recovery: ["Temporary rebuild stopped before later databases.", "Inspect the temporary database failure, then rerun a new protected temporary rebuild."]
        };
        return operation;
      }
    }
    const postflight = await runPostflight({ ...options, checkReadiness: false, requireReadiness: false }, temporaryRuntime);
    operation = postflight.ok
      ? {
        command: "rebuild-check",
        environment: options.environment,
        phase: "complete",
        ok: true,
        code: EXIT.OK,
        stopped: false,
        preflight,
        migrations,
        postflight,
        serviceDeployment: "not-started-by-temporary-rebuild"
      }
      : {
        command: "rebuild-check",
        environment: options.environment,
        phase: "postflight",
        ok: false,
        code: postflight.code,
        stopped: true,
        preflight,
        migrations,
        postflight,
        recovery: postflight.recovery
      };
    return operation;
  } catch (error) {
    operation = reportError("rebuild-check", options.environment, error, "temporary-rebuild");
    return operation;
  } finally {
    for (const name of [...names].reverse()) {
      try {
        await dropTemporaryDatabase(bootstrapUrl, name, runtime);
        cleanup.push({ database: name, dropped: true });
      } catch {
        cleanup.push({ database: name, dropped: false });
      }
    }
    if (operation) {
      operation.temporaryDatabases = { prefix: temporaryDatabasePrefix, count: names.length, cleanup };
      if (cleanup.some(({ dropped }) => !dropped)) {
        operation.ok = false;
        operation.code = EXIT.EXECUTION;
        operation.stopped = true;
        operation.recovery = [...new Set([...(operation.recovery || []), "A protected temporary database could not be cleaned automatically; remove only the reported myserver_stage6_ database after verifying its name."])];
      }
    }
  }
}

export async function main(argv = process.argv.slice(2)) {
  let options;
  try {
    options = parseDeploymentArguments(argv);
  } catch (error) {
    const report = reportError("unknown", undefined, error, "argument-validation");
    process.stdout.write(`${JSON.stringify(report)}\n`);
    return report.code;
  }
  let report;
  if (options.command === "validate") report = runStaticValidation(options);
  else if (options.command === "preflight") report = await runPreflight(options);
  else if (options.command === "postflight") report = await runPostflight(options);
  else if (options.command === "apply") report = await runApply(options);
  else report = await runRebuildCheck(options);
  process.stdout.write(`${JSON.stringify(report)}\n`);
  return report.code;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().then((code) => { process.exitCode = code; }).catch((error) => {
    process.stdout.write(`${JSON.stringify(reportError("unknown", undefined, error, "unhandled"))}\n`);
    process.exitCode = EXIT.EXECUTION;
  });
}
