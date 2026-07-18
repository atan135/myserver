import assert from "node:assert/strict";
import { join } from "node:path";
import test from "node:test";

import { EXIT, sqlxMigrationMetadata } from "../../tools/db.js";
import {
  parseDeploymentArguments,
  runApply,
  runPostflight,
  runPreflight,
  runRebuildCheck,
  runStaticValidation,
  temporaryBootstrapUrl,
  temporaryDatabaseName
} from "../../tools/db-deploy.js";

const root = process.cwd();

function migrationFor(database) {
  return sqlxMigrationMetadata(join(root, database.migrationDirectory)).at(-1);
}

function healthyClient(database, options = {}) {
  const migration = migrationFor(database);
  return {
    async query(sql) {
      if (sql.startsWith("SELECT current_database()")) {
        return {
          rows: [{
            database_name: database.defaultDatabase,
            history: options.history === false ? null : "_sqlx_migrations",
            managed_tables: options.managedTables === true
          }]
        };
      }
      if (sql.startsWith("SELECT version::text")) {
        return {
          rows: [{
            version: migration.version,
            description: migration.description,
            checksum: migration.checksum,
            success: options.success !== false
          }]
        };
      }
      if (sql.startsWith("SELECT pg_try_advisory_lock")) return { rows: [{ acquired: options.lockAvailable !== false }] };
      if (sql.startsWith("SELECT pg_advisory_unlock")) return { rows: [{ released: true }] };
      if (sql.startsWith("SELECT to_regclass($1)")) return { rows: [{ exists: options.keyTablesPresent !== false }] };
      throw new Error(`unexpected test query: ${sql}`);
    },
    async end() {}
  };
}

function fakeRuntime(options = {}) {
  return {
    environment: options.environment || {},
    connectionUrl: () => "postgresql://deployment-test@localhost:5432/deployment_test",
    connect: options.connect || (async (_url, database) => healthyClient(database, options.databaseOptions?.[database.key] || {})),
    executeDatabase: options.executeDatabase || (() => ({ ok: true, code: EXIT.OK })),
    executeDrift: options.executeDrift || (async () => ({
      ok: true,
      code: EXIT.OK,
      drift: {
        target: { object_count: 1 },
        actual: { object_count: 1 },
        differences: { unapproved: [] }
      }
    })),
    fetch: options.fetch || (async () => {
      throw new Error("fetch must not run without --check-readiness");
    }),
    randomToken: options.randomToken
  };
}

test("deployment CLI requires explicit environment, actor and temporary rebuild confirmation", () => {
  assert.deepEqual(parseDeploymentArguments(["preflight", "--environment", "ci"]), {
    command: "preflight",
    environment: "ci",
    actor: undefined,
    checkReadiness: false,
    requireReadiness: false,
    confirmTemporaryRebuild: undefined
  });
  assert.throws(() => parseDeploymentArguments(["apply", "--environment", "ci"]), /requires --actor/);
  assert.throws(() => parseDeploymentArguments(["postflight", "--environment", "ci", "--require-readiness"]), /requires --check-readiness/);
  assert.throws(() => parseDeploymentArguments(["rebuild-check", "--environment", "ci"]), /confirm-temporary-rebuild/);
  assert.equal(parseDeploymentArguments(["rebuild-check", "--environment", "ci", "--confirm-temporary-rebuild", "stage6-temporary-rebuild"]).confirmTemporaryRebuild, "stage6-temporary-rebuild");
});

test("static deployment validation binds every database target, key table and service range", () => {
  const report = runStaticValidation({ environment: "ci" });
  assert.equal(report.ok, true, report.error);
  assert.deepEqual(report.reports.map(({ database }) => database), ["auth", "game", "chat", "announce", "mail"]);
  assert.equal(report.reports.every(({ serviceCompatibility }) => serviceCompatibility.every(({ compatible, runtimeObserved }) => compatible && runtimeObserved === false)), true);
});

test("preflight stops at the first unavailable SQLx advisory lock", async () => {
  const report = await runPreflight({ environment: "ci" }, fakeRuntime({
    databaseOptions: { auth: { lockAvailable: false } }
  }));
  assert.equal(report.ok, false);
  assert.equal(report.code, EXIT.LOCK);
  assert.equal(report.reports.length, 1);
  assert.equal(report.reports[0].advisoryLock.available, false);
  assert.match(report.recovery.join(" "), /active migration holder/);
});

test("apply stops before later databases when one migration fails", async () => {
  const calls = [];
  const report = await runApply({ environment: "ci", actor: "stage6-test", checkReadiness: false, requireReadiness: false }, fakeRuntime({
    executeDatabase(command, database) {
      calls.push(`${command}:${database.key}`);
      if (command === "up" && database.key === "auth") return { ok: false, code: EXIT.EXECUTION, error: "fixture migration failure" };
      return { ok: true, code: EXIT.OK };
    }
  }));
  assert.equal(report.ok, false);
  assert.equal(report.phase, "migration");
  assert.deepEqual(calls.filter((call) => call.startsWith("up:")), ["up:auth"]);
  assert.equal(calls.includes("up:game"), false);
  assert.equal(report.postflight.state, "not-run");
});

test("postflight keeps readiness explicitly unknown until an operator asks to probe it", async () => {
  let fetchCalls = 0;
  const report = await runPostflight({ environment: "ci", checkReadiness: false, requireReadiness: false }, fakeRuntime({
    fetch: async () => {
      fetchCalls += 1;
      throw new Error("readiness must not be fetched");
    }
  }));
  assert.equal(report.ok, true, JSON.stringify(report));
  assert.equal(fetchCalls, 0);
  assert.equal(report.reports[0].readiness.every(({ state }) => state === "not-configured"), true);
});

test("explicit unhealthy readiness response blocks postflight without starting a service", async () => {
  const report = await runPostflight({ environment: "ci", checkReadiness: true, requireReadiness: false }, fakeRuntime({
    environment: {
      MYSERVER_DB_DEPLOY_AUTH_HTTP_READINESS_URL: "http://127.0.0.1:39999/healthz"
    },
    fetch: async () => ({
      ok: true,
      status: 200,
      headers: { get: () => "application/json" },
      text: async () => "{\"ok\":false}"
    })
  }));
  assert.equal(report.ok, false);
  assert.equal(report.code, EXIT.EXECUTION);
  assert.equal(report.reports.length, 1);
  assert.equal(report.reports[0].readiness[0].state, "unhealthy");
  assert.match(report.recovery.join(" "), /last compatible service version/);
});

test("temporary rebuild guards only allow explicit localhost stage6 database names", () => {
  assert.equal(temporaryDatabaseName("abc123", "auth"), "myserver_stage6_abc123_auth");
  assert.throws(() => temporaryDatabaseName("abc123", "AUTH"), /invalid/);
  const environment = {
    MYSERVER_DB_DEPLOY_TEMPORARY_REBUILD: "1",
    MYSERVER_DB_DEPLOY_TEMPORARY_POSTGRES_URL: "postgresql://postgres@localhost:5432/postgres",
    MYSERVER_DB_DEPLOY_TEMPORARY_POSTGRES_PASSWORD: "test-secret"
  };
  const url = temporaryBootstrapUrl(environment);
  assert.equal(new URL(url).hostname, "localhost");
  assert.equal(new URL(url).port, "5432");
  assert.equal(new URL(url).pathname, "/postgres");
  assert.equal(url.includes("test-secret"), true);
  assert.throws(() => temporaryBootstrapUrl({ ...environment, MYSERVER_DB_DEPLOY_TEMPORARY_POSTGRES_URL: "postgresql://postgres@database.example:5432/postgres" }), /localhost/);
  assert.throws(() => temporaryBootstrapUrl({ ...environment, MYSERVER_DB_DEPLOY_TEMPORARY_POSTGRES_URL: "postgresql://postgres@localhost:5433/postgres" }), /localhost/);
  assert.throws(() => temporaryBootstrapUrl({ ...environment, MYSERVER_DB_DEPLOY_TEMPORARY_POSTGRES_URL: "postgresql://postgres@localhost:5432/postgres?options%5Bstatement_timeout%5D=0" }), /must not set PostgreSQL options/);
  assert.throws(() => temporaryBootstrapUrl({ ...environment, MYSERVER_DB_DEPLOY_TEMPORARY_REBUILD: "0" }), /TEMPORARY_REBUILD=1/);
});

test("temporary rebuild uses only stage6 names and cleans every created database in finally", async () => {
  const created = [];
  const dropped = [];
  const migrated = new Set();
  const environment = {
    MYSERVER_DB_DEPLOY_TEMPORARY_REBUILD: "1",
    MYSERVER_DB_DEPLOY_TEMPORARY_POSTGRES_URL: "postgresql://postgres@localhost:5432/postgres",
    MYSERVER_DB_DEPLOY_TEMPORARY_POSTGRES_PASSWORD: "test-secret"
  };
  const runtime = fakeRuntime({
    environment,
    randomToken: () => "unit123",
    async connect(_url, database) {
      if (database.key === "stage6-bootstrap") {
        return {
          async query(sql) {
            const create = /^CREATE DATABASE "(myserver_stage6_[a-z0-9_]+)"$/.exec(sql);
            const drop = /^DROP DATABASE IF EXISTS "(myserver_stage6_[a-z0-9_]+)"$/.exec(sql);
            if (create) {
              created.push(create[1]);
              return { rows: [] };
            }
            if (drop) {
              dropped.push(drop[1]);
              return { rows: [] };
            }
            if (sql.startsWith("SELECT pg_terminate_backend")) return { rows: [] };
            throw new Error(`unexpected bootstrap query: ${sql}`);
          },
          async end() {}
        };
      }
      return healthyClient(database, { history: migrated.has(database.key) });
    },
    executeDatabase(command, database) {
      if (command === "up") migrated.add(database.key);
      return { ok: true, code: EXIT.OK };
    }
  });
  const report = await runRebuildCheck({ environment: "ci", checkReadiness: false, requireReadiness: false }, runtime);
  assert.equal(report.ok, true, JSON.stringify(report));
  assert.deepEqual(created, [
    "myserver_stage6_unit123_auth",
    "myserver_stage6_unit123_game",
    "myserver_stage6_unit123_chat",
    "myserver_stage6_unit123_announce",
    "myserver_stage6_unit123_mail"
  ]);
  assert.deepEqual(dropped, [...created].reverse());
  assert.equal(report.temporaryDatabases.cleanup.every(({ dropped: removed }) => removed), true);
  assert.equal(report.serviceDeployment, "not-started-by-temporary-rebuild");
});

test("temporary rebuild cleans only databases successfully created by this run", async () => {
  const attempted = [];
  const created = [];
  const dropped = [];
  const migrationCalls = [];
  const environment = {
    MYSERVER_DB_DEPLOY_TEMPORARY_REBUILD: "1",
    MYSERVER_DB_DEPLOY_TEMPORARY_POSTGRES_URL: "postgresql://postgres@localhost:5432/postgres",
    MYSERVER_DB_DEPLOY_TEMPORARY_POSTGRES_PASSWORD: "test-secret"
  };
  const runtime = fakeRuntime({
    environment,
    randomToken: () => "createfail",
    async connect(_url, database) {
      if (database.key !== "stage6-bootstrap") throw new Error("migration connection must not be attempted after create failure");
      return {
        async query(sql) {
          const create = /^CREATE DATABASE "(myserver_stage6_[a-z0-9_]+)"$/.exec(sql);
          const drop = /^DROP DATABASE IF EXISTS "(myserver_stage6_[a-z0-9_]+)"$/.exec(sql);
          if (create) {
            attempted.push(create[1]);
            if (create[1].endsWith("_game")) throw new Error("fixture create failure");
            created.push(create[1]);
            return { rows: [] };
          }
          if (drop) {
            dropped.push(drop[1]);
            return { rows: [] };
          }
          if (sql.startsWith("SELECT pg_terminate_backend")) return { rows: [] };
          throw new Error(`unexpected bootstrap query: ${sql}`);
        },
        async end() {}
      };
    },
    executeDatabase(command, database) {
      migrationCalls.push(`${command}:${database.key}`);
      return { ok: true, code: EXIT.OK };
    }
  });
  const report = await runRebuildCheck({ environment: "ci", checkReadiness: false, requireReadiness: false }, runtime);
  assert.equal(report.ok, false);
  assert.deepEqual(attempted, ["myserver_stage6_createfail_auth", "myserver_stage6_createfail_game"]);
  assert.deepEqual(created, ["myserver_stage6_createfail_auth"]);
  assert.deepEqual(dropped, ["myserver_stage6_createfail_auth"]);
  assert.equal(attempted.some((name) => name.endsWith("_chat")), false);
  assert.deepEqual(migrationCalls, []);
  assert.deepEqual(report.temporaryDatabases.cleanup, [{ database: "myserver_stage6_createfail_auth", dropped: true }]);
});
