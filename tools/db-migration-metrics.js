import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const MIGRATION_METRICS_SERVICE = "db-migration";
export const MIGRATION_METRICS_SUBJECT_PREFIX = `myserver.metrics.${MIGRATION_METRICS_SERVICE}.`;

const ERROR_CATEGORIES = new Set([
  "none",
  "config",
  "connection",
  "validation",
  "lock",
  "execution",
  "baseline-or-drift",
  "sqlx",
  "unknown"
]);

function normalizeVersion(value, fallback = "unknown") {
  return /^\d{14}$/.test(String(value || "")) ? String(value) : fallback;
}

function normalizeVersions(values) {
  if (!Array.isArray(values)) return [];
  return [...new Set(values.map((value) => normalizeVersion(value, "")).filter(Boolean))];
}

function normalizeTimestamp(value) {
  const timestamp = Number.isFinite(value) ? Math.floor(value) : Math.floor(Date.now() / 1000);
  return timestamp > 0 ? timestamp : Math.floor(Date.now() / 1000);
}

function encodeSubjectToken(value) {
  return Buffer.from(String(value), "utf8").toString("base64url");
}

export function migrationMetricErrorCategory(exitCode) {
  return {
    0: "none",
    2: "config",
    3: "connection",
    4: "validation",
    5: "lock",
    6: "execution",
    7: "baseline-or-drift",
    8: "sqlx"
  }[exitCode] || "unknown";
}

export function buildMigrationMetricEvent(input, options = {}) {
  const databaseKey = String(input?.databaseKey || "");
  if (!/^[a-z][a-z0-9-]{0,63}$/.test(databaseKey)) {
    throw new Error("migration metric database key is invalid");
  }
  const outcome = input?.outcome === "success" ? "success" : input?.outcome === "failure" ? "failure" : null;
  if (!outcome) throw new Error("migration metric outcome is invalid");

  const targetMigrationVersion = normalizeVersion(input?.targetMigrationVersion);
  const appliedMigrationVersions = normalizeVersions(input?.appliedMigrationVersions);
  const attemptedMigrationVersions = normalizeVersions(input?.attemptedMigrationVersions);
  const errorCategory = outcome === "success"
    ? "none"
    : ERROR_CATEGORIES.has(input?.errorCategory) && input.errorCategory !== "none"
      ? input.errorCategory
      : "unknown";
  const timestamp = normalizeTimestamp(options.timestamp);
  const bucket = Math.floor(timestamp / 5) * 5;
  const instanceId = `migration-${databaseKey}-${targetMigrationVersion}`;

  return {
    subject: `${MIGRATION_METRICS_SUBJECT_PREFIX}${encodeSubjectToken(instanceId)}`,
    payload: {
      service: MIGRATION_METRICS_SERVICE,
      instance_id: instanceId,
      bucket,
      timestamp,
      metrics: {
        event_type: "migration",
        database_key: databaseKey,
        target_migration_version: targetMigrationVersion,
        applied_migration_versions: appliedMigrationVersions.join(",") || "none",
        attempted_migration_versions: attemptedMigrationVersions.join(",") || "none",
        outcome,
        error_category: errorCategory
      }
    }
  };
}

function boundedTimeout(value) {
  const parsed = Number.parseInt(String(value || ""), 10);
  if (!Number.isFinite(parsed)) return 1000;
  return Math.max(100, Math.min(parsed, 5000));
}

export function migrationMetricsConfig(environment = process.env) {
  return {
    enabled: environment.MYSERVER_DB_MIGRATION_METRICS_ENABLED === "1",
    natsUrl: environment.MYSERVER_DB_MIGRATION_METRICS_NATS_URL || environment.NATS_URL || "nats://127.0.0.1:4222",
    timeoutMs: boundedTimeout(environment.MYSERVER_DB_MIGRATION_METRICS_TIMEOUT_MS)
  };
}

function validEvent(event) {
  return Boolean(
    event &&
    typeof event === "object" &&
    typeof event.subject === "string" &&
    event.subject.startsWith(MIGRATION_METRICS_SUBJECT_PREFIX) &&
    event.payload?.service === MIGRATION_METRICS_SERVICE &&
    event.payload?.metrics?.event_type === "migration"
  );
}

async function withTimeout(promise, timeoutMs) {
  let timer;
  try {
    return await Promise.race([
      Promise.resolve(promise),
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error("migration metrics publish timed out")), timeoutMs);
      })
    ]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

export async function publishMigrationMetric(event, config, options = {}) {
  if (!config?.enabled) return { delivered: false, state: "disabled" };
  if (!validEvent(event)) return { delivered: false, state: "invalid-event" };

  let connection;
  try {
    const nats = options.nats || await import("nats");
    const codec = options.codec || nats.StringCodec();
    connection = await withTimeout(
      nats.connect({
        servers: config.natsUrl,
        name: "db-migration-metrics",
        timeout: config.timeoutMs,
        reconnect: false,
        maxReconnectAttempts: 0
      }),
      config.timeoutMs
    );
    connection.publish(event.subject, codec.encode(JSON.stringify(event.payload)));
    await withTimeout(connection.flush(), config.timeoutMs);
    return { delivered: true, state: "delivered" };
  } catch {
    return { delivered: false, state: "unavailable" };
  } finally {
    try { connection?.close(); } catch { /* best-effort metric connection cleanup */ }
  }
}

export async function emitMigrationMetricFromEnvironment(environment = process.env, options = {}) {
  const config = migrationMetricsConfig(environment);
  if (!config.enabled) return { delivered: false, state: "disabled" };
  try {
    const event = JSON.parse(environment.MYSERVER_DB_MIGRATION_METRIC_EVENT || "");
    return publishMigrationMetric(event, config, options);
  } catch {
    return { delivered: false, state: "invalid-event" };
  }
}

export async function main(environment = process.env) {
  return emitMigrationMetricFromEnvironment(environment);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().then((report) => {
    process.stdout.write(`${JSON.stringify(report)}\n`);
  }).catch(() => {
    process.stdout.write('{"delivered":false,"state":"unavailable"}\n');
  });
}
