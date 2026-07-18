import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";

import {
  EXIT,
  classifyFailure,
  migrationSafetyForDirectory,
  sqlxMigrationMetadata
} from "../../tools/db.js";
import {
  buildMigrationMetricEvent,
  migrationMetricsConfig,
  publishMigrationMetric
} from "../../tools/db-migration-metrics.js";
import {
  runStage7Drill,
  temporaryBootstrapUrl,
  temporaryDatabaseName
} from "../../tools/db-stage7-drill.js";
import { writeMetrics } from "../../apps/metrics-collector/src/server.js";

const root = process.cwd();
const runPostgres = process.env.MYSERVER_STAGE7_RUN_POSTGRES === "1";

class MetricsRedisCapture {
  constructor() {
    this.hashes = new Map();
  }

  pipeline() {
    const redis = this;
    const commands = [];
    return {
      hset(key, fields) {
        commands.push(["hset", key, fields]);
        return this;
      },
      expire() { return this; },
      set() { return this; },
      async exec() {
        for (const [command, key, fields] of commands) {
          if (command === "hset") redis.hashes.set(key, { ...(redis.hashes.get(key) || {}), ...fields });
        }
      }
    };
  }
}

test("stage 7 drill requires explicit loopback opt-in and only guarded temporary names", () => {
  const environment = {
    MYSERVER_STAGE7_RUN_POSTGRES: "1",
    MYSERVER_STAGE7_POSTGRES_URL: "postgresql://postgres@localhost:5432/postgres",
    MYSERVER_STAGE7_POSTGRES_PASSWORD: "test-secret"
  };
  const url = temporaryBootstrapUrl(environment);
  assert.equal(new URL(url).hostname, "localhost");
  assert.equal(new URL(url).port, "5432");
  assert.equal(new URL(url).pathname, "/postgres");
  assert.equal(temporaryDatabaseName("abc12345", "current_auth"), "myserver_stage7_abc12345_current_auth");
  assert.throws(() => temporaryDatabaseName("abc12345", "AUTH"), /invalid/);
  assert.throws(() => temporaryBootstrapUrl({ ...environment, MYSERVER_STAGE7_RUN_POSTGRES: "0" }), /RUN_POSTGRES=1/);
  assert.throws(() => temporaryBootstrapUrl({ ...environment, MYSERVER_STAGE7_POSTGRES_URL: "postgresql://postgres@database.example:5432/postgres" }), /localhost/);
  assert.throws(() => temporaryBootstrapUrl({ ...environment, MYSERVER_STAGE7_POSTGRES_URL: "postgresql://postgres@localhost:5433/postgres" }), /localhost/);
  assert.throws(() => temporaryBootstrapUrl({ ...environment, MYSERVER_STAGE7_POSTGRES_URL: "postgresql://postgres@localhost:5432/postgres?options=-c%20statement_timeout%3D0" }), /must not set PostgreSQL options/);
});

test("stage 7 fixture migrations are versioned, bounded and intentionally distinguish checksum sources", () => {
  const fixtures = [
    ["tests/fixtures/db/stage7/checksum/applied", "stage7-checksum-test", 1],
    ["tests/fixtures/db/stage7/checksum/tampered", "stage7-checksum-test", 1],
    ["tests/fixtures/db/stage7/sql-failure", "stage7-sql-failure-test", 3],
    ["tests/fixtures/db/stage7/connection-interruption", "stage7-interruption-test", 1],
    ["tests/fixtures/db/stage7/concurrent", "stage7-concurrent-test", 1],
    ["tests/fixtures/db/stage7/lock-timeout/base", "stage7-lock-timeout-test", 1],
    ["tests/fixtures/db/stage7/lock-timeout/blocked", "stage7-lock-timeout-test", 3]
  ];
  for (const [directory, owner, count] of fixtures) {
    const safety = migrationSafetyForDirectory(join(root, directory), { expectedOwner: owner });
    assert.equal(safety.length, count);
    assert.equal(safety.every(({ lockTimeoutMs, statementTimeoutMs }) => lockTimeoutMs === 500 && statementTimeoutMs === 5000), true);
  }
  const applied = sqlxMigrationMetadata(join(root, "tests/fixtures/db/stage7/checksum/applied"))[0];
  const tampered = sqlxMigrationMetadata(join(root, "tests/fixtures/db/stage7/checksum/tampered"))[0];
  assert.equal(applied.version, tampered.version);
  assert.notEqual(applied.checksum, tampered.checksum);
  const worker = readFileSync(join(root, "tools/db-stage7-worker.js"), "utf8");
  const lockRunner = readFileSync(join(root, "tools/db-lock-runner.js"), "utf8");
  const dbTool = readFileSync(join(root, "tools/db.js"), "utf8");
  const drill = readFileSync(join(root, "tools/db-stage7-drill.js"), "utf8");
  assert.match(worker, /myserver_stage7_/);
  assert.match(worker, /stage7\|stage4-rollout/);
  assert.match(lockRunner, /pg_try_advisory_lock/);
  assert.match(lockRunner, /pg_advisory_unlock/);
  assert.match(dbTool, /runLockedSqlx/);
  assert.match(drill, /runLockTimeoutDrill/);
  assert.match(drill, /LOCK TABLE stage7_lock_timeout_fixture IN ACCESS EXCLUSIVE MODE/);
  assert.match(drill, /firstExpand/);
  assert.match(drill, /repeatedExpand/);
});

test("database migration workflow triggers for stage 7 fixtures on pull requests and pushes", () => {
  const workflow = readFileSync(join(root, ".github/workflows/database-migration.yml"), "utf8");
  const fixturePathEntries = workflow.match(/^\s+- "tests\/fixtures\/db\/\*\*"$/gm) || [];
  const metricsEmitterEntries = workflow.match(/^\s+- "tools\/db-migration-metrics\.js"$/gm) || [];
  const collectorEntries = workflow.match(/^\s+- "apps\/metrics-collector\/(?:src\/\*\*|package\.json)"$/gm) || [];
  assert.equal(fixturePathEntries.length, 2);
  assert.equal(metricsEmitterEntries.length, 2);
  assert.equal(collectorEntries.length, 4);
});

test("migration metrics adapter publishes a collector-compatible, redacted version event", async () => {
  const event = buildMigrationMetricEvent({
    databaseKey: "auth",
    targetMigrationVersion: "20260718161350",
    appliedMigrationVersions: ["20260718161350"],
    attemptedMigrationVersions: ["20260718161350"],
    outcome: "failure",
    errorCategory: "lock"
  }, { timestamp: 1_700_000_001 });
  const messages = [];
  let closed = false;
  const nats = {
    StringCodec() {
      return { encode: (value) => Buffer.from(value, "utf8") };
    },
    async connect(options) {
      assert.equal(options.name, "db-migration-metrics");
      assert.equal(options.reconnect, false);
      return {
        publish(subject, data) { messages.push({ subject, data }); },
        async flush() {},
        close() { closed = true; }
      };
    }
  };
  const published = await publishMigrationMetric(event, {
    enabled: true,
    natsUrl: "nats://metrics.example.test:4222",
    timeoutMs: 100
  }, { nats });
  assert.deepEqual(published, { delivered: true, state: "delivered" });
  assert.equal(closed, true);
  assert.equal(messages.length, 1);
  assert.equal(messages[0].subject, event.subject);

  const payload = JSON.parse(messages[0].data.toString("utf8"));
  assert.equal(payload.service, "db-migration");
  assert.equal(payload.metrics.database_key, "auth");
  assert.equal(payload.metrics.target_migration_version, "20260718161350");
  assert.equal(payload.metrics.applied_migration_versions, "20260718161350");
  assert.equal(payload.metrics.outcome, "failure");
  assert.equal(payload.metrics.error_category, "lock");
  assert.equal(JSON.stringify(payload).includes("postgres://"), false);
  assert.equal(JSON.stringify(payload).includes("password"), false);

  const redis = new MetricsRedisCapture();
  await writeMetrics(redis, { metricsTtlSeconds: 60, heartbeatTtlSeconds: 30 }, { data: messages[0].data });
  const stored = [...redis.hashes.values()][0];
  assert.equal(stored.database_key, "auth");
  assert.equal(stored.target_migration_version, "20260718161350");
  assert.equal(stored.error_category, "lock");

  const unavailable = await publishMigrationMetric(event, { enabled: true, natsUrl: "nats://unavailable.test:4222", timeoutMs: 100 }, {
    nats: { async connect() { throw new Error("not reachable"); } }
  });
  assert.deepEqual(unavailable, { delivered: false, state: "unavailable" });
});

test("migration metrics uses NATS_URL only as an explicit transport fallback", () => {
  const fallback = migrationMetricsConfig({
    MYSERVER_DB_MIGRATION_METRICS_ENABLED: "1",
    NATS_URL: "nats://fallback.example.test:4222",
    REDIS_URL: "redis://redis-user:redis-secret@redis.example.test:6379/0"
  });
  assert.equal(fallback.enabled, true);
  assert.equal(fallback.natsUrl, "nats://fallback.example.test:4222");

  const specific = migrationMetricsConfig({
    MYSERVER_DB_MIGRATION_METRICS_ENABLED: "1",
    NATS_URL: "nats://fallback.example.test:4222",
    MYSERVER_DB_MIGRATION_METRICS_NATS_URL: "nats://specific.example.test:4222"
  });
  assert.equal(specific.natsUrl, "nats://specific.example.test:4222");
});

test("connection interruption has a stable migration exit category", () => {
  assert.equal(classifyFailure("server closed the connection unexpectedly"), EXIT.CONNECTION);
  assert.equal(classifyFailure("terminating connection due to administrator command"), EXIT.CONNECTION);
  assert.equal(classifyFailure("\u7531\u4e8e\u7ba1\u7406\u5458\u547d\u4ee4\u4e2d\u65ad\u8054\u63a5"), EXIT.CONNECTION);
  assert.equal(classifyFailure("error communicating with database while receiving a PostgreSQL message"), EXIT.CONNECTION);
  assert.equal(classifyFailure("migration checksum mismatch"), EXIT.VALIDATION);
  assert.equal(classifyFailure("canceling statement due to lock timeout"), EXIT.LOCK);
  assert.equal(classifyFailure("\u53d6\u6d88\u8bed\u53e5\uff0c\u56e0\u4e3a\u9501\u7b49\u5f85\u8d85\u65f6"), EXIT.LOCK);
  assert.equal(classifyFailure("error while running migration: function stage7_missing_function() does not exist"), EXIT.EXECUTION);
});

test("stage 7 PostgreSQL verification drill covers guarded live failure and recovery paths", {
  skip: runPostgres ? false : "set MYSERVER_STAGE7_RUN_POSTGRES=1 with local PostgreSQL credentials to run",
  timeout: 180000
}, async () => {
  const report = await runStage7Drill();
  assert.equal(report.ok, true, report.error);
  assert.equal(report.code, EXIT.OK);
  assert.equal(report.services, "not-started-by-stage7-drill");
  assert.equal(report.temporaryDatabases.created.every((database) => /^myserver_stage7_[a-z0-9_]+$/.test(database)), true);
  assert.equal(report.temporaryDatabases.cleanup.length, report.temporaryDatabases.created.length);
  assert.equal(report.temporaryDatabases.cleanup.every(({ dropped }) => dropped), true);
  assert.deepEqual(report.scenarios.map(({ name }) => name), [
    "empty-current-repeat",
    "checksum-tamper",
    "sql-failure-stop",
    "connection-interruption",
    "concurrent-advisory-lock",
    "ddl-lock-timeout-retry",
    "expand-contract-recovery"
  ]);
  assert.equal(report.observability.runtimeMetrics.state, "implemented");
});
