import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";

import pg from "pg";

import {
  EXIT,
  canonicalizeCatalog,
  catalogQuery,
  compareDriftCatalog,
  executeDatabase,
  executeBackfill,
  executeDrift,
  loadBackfillTask,
  loadDriftTarget,
  normalizeDriftCatalog,
  parseArguments,
  resolveDatabases,
  sqlxMigrationMetadata,
  validateBackfillTaskIdentity,
  validateBackfillTask,
  validateDriftPolicy
} from "../../tools/db.js";

const { Client } = pg;
const root = process.cwd();
const migration = sqlxMigrationMetadata(join(root, "db/migrations/auth")).at(-1);
const authDatabase = {
  key: "auth",
  defaultDatabase: "myserver_auth",
  migrationDirectory: "db/migrations/auth",
  urlEnvironment: "TEST_DATABASE_URL",
  userEnvironment: "TEST_DATABASE_USER",
  passwordEnvironment: "TEST_DATABASE_PASSWORD"
};
const testEnvironment = {
  TEST_DATABASE_URL: "postgresql://migration@example.test:6543/myserver_auth?sslmode=require",
  TEST_DATABASE_PASSWORD: "secret"
};

function catalogRows() {
  return [
    { object_kind: "table", object_name: "public.stage5_widget", object_identity: "public.stage5_widget", definition: "r" },
    { object_kind: "column", object_name: "public.stage5_widget", object_identity: "public.stage5_widget.id", definition: "id bigint not null " },
    { object_kind: "column", object_name: "public.stage5_widget", object_identity: "public.stage5_widget.marker", definition: "marker text null " },
    { object_kind: "constraint", object_name: "public.stage5_widget", object_identity: "public.stage5_widget.stage5_widget_pkey", definition: "PRIMARY KEY (id)" },
    { object_kind: "index", object_name: "public.stage5_widget_pkey", object_identity: "public.stage5_widget_pkey", definition: "CREATE UNIQUE INDEX stage5_widget_pkey ON public.stage5_widget USING btree (id)" },
    { object_kind: "trigger", object_name: "public.stage5_widget", object_identity: "public.stage5_widget.stage5_widget_audit", definition: "CREATE TRIGGER stage5_widget_audit BEFORE INSERT ON public.stage5_widget FOR EACH ROW EXECUTE FUNCTION public.stage5_widget_stamp()" },
    { object_kind: "function", object_name: "public.stage5_widget_stamp()", object_identity: "public.stage5_widget_stamp()", definition: "CREATE OR REPLACE FUNCTION public.stage5_widget_stamp() RETURNS trigger LANGUAGE plpgsql AS $function$ BEGIN RETURN NEW; END; $function$" }
  ];
}

function targetFromRows(rows) {
  return {
    migration,
    catalog_sha256: canonicalizeCatalog(rows).sha256,
    objects: normalizeDriftCatalog(rows).objects,
    manifest_sha256: normalizeDriftCatalog(rows).manifest_sha256
  };
}

function driftPolicy(database, allowances = []) {
  return {
    schema: 1,
    canonicalCatalogFormat: "myserver-postgresql-catalog-v1",
    targets: {
      [database]: {
        file: "tests/fixtures/db/stage5-drift-target.json",
        version: migration.version,
        description: migration.description,
        checksum: migration.checksum
      }
    },
    allowances
  };
}

function fakeCatalogClient(rows) {
  const calls = [];
  return {
    calls,
    async query(sql) {
      calls.push(sql);
      if (sql.startsWith("SET search_path")) return { rows: [] };
      if (sql.includes("row_to_json(c)::text")) return { rows: rows.map((row) => ({ row_to_json: JSON.stringify(row) })) };
      throw new Error(`unexpected fake drift query: ${sql}`);
    },
    async end() { calls.push("END"); }
  };
}

test("drift arguments require an exact environment and backfill commands require task audit inputs", () => {
  assert.deepEqual(parseArguments(["drift", "--database", "auth", "--environment", "ci"]), {
    command: "drift",
    database: "auth",
    actor: undefined,
    expectedFingerprint: undefined,
    environment: "ci",
    task: undefined,
    maxBatches: undefined
  });
  assert.throws(() => parseArguments(["drift", "--database", "auth"]), /--environment/);
  assert.throws(() => parseArguments(["drift", "--database", "auth", "--environment", "*"]), /lower-case/);
  assert.throws(() => parseArguments(["backfill-run", "--database", "auth", "--task", "stage5-copy-marker"]), /--actor/);
  assert.throws(() => parseArguments(["backfill-status", "--database", "all", "--task", "stage5-copy-marker"]), /one database/);
});

test("version-controlled drift targets bind all five reviewed migration baselines", () => {
  const policy = validateDriftPolicy();
  assert.deepEqual(policy.allowances, []);
  const targets = resolveDatabases("all").map((database) => {
    const target = loadDriftTarget(database, policy);
    return [database.key, target.migration.version, target.objects.length];
  });
  assert.deepEqual(targets, [
    ["auth", "20260719100000", 373],
    ["game", "20260718161350", 316],
    ["chat", "20260718161350", 38],
    ["announce", "20260718161350", 24],
    ["mail", "20260718161350", 150]
  ]);
});

test("drift report separates definition changes, missing targets and actual extras", () => {
  const expected = catalogRows();
  const actual = expected
    .filter(({ object_kind }) => object_kind !== "trigger")
    .map((row) => row.object_identity === "public.stage5_widget.marker" ? { ...row, definition: "marker text not null " } : row)
    .concat({ object_kind: "index", object_name: "public.idx_stage5_manual", object_identity: "public.idx_stage5_manual", definition: "CREATE INDEX idx_stage5_manual ON public.stage5_widget USING btree (marker)" });
  const differences = compareDriftCatalog(targetFromRows(expected), normalizeDriftCatalog(actual));
  assert.deepEqual(differences.map(({ direction }) => direction), ["actual-extra", "definition-change", "target-missing"]);
  assert.equal(differences.find(({ direction }) => direction === "definition-change").object_identity, "public.stage5_widget.marker");
  assert.equal(differences.find(({ direction }) => direction === "target-missing").object_identity, "public.stage5_widget.stage5_widget_audit");
});

test("clean target and actual drift manifests share the semantic digest while changes remain visible", () => {
  const expected = catalogRows();
  // Compact targets expand object_name from object_identity, while live catalog rows retain a display label.
  const target = normalizeDriftCatalog(expected.map((row) => ({ ...row, object_name: row.object_identity })));
  const actual = normalizeDriftCatalog(expected);
  assert.equal(target.manifest_sha256, actual.manifest_sha256);
  assert.deepEqual(compareDriftCatalog({ objects: target.objects }, actual), []);

  const changed = normalizeDriftCatalog(expected.map((row) => row.object_identity === "public.stage5_widget.marker" ? { ...row, definition: "marker text not null " } : row));
  assert.notEqual(changed.manifest_sha256, target.manifest_sha256);
  assert.equal(compareDriftCatalog({ objects: target.objects }, changed).some(({ direction }) => direction === "definition-change"), true);

  const missing = normalizeDriftCatalog(expected.filter(({ object_kind }) => object_kind !== "trigger"));
  assert.notEqual(missing.manifest_sha256, target.manifest_sha256);
  assert.equal(compareDriftCatalog({ objects: target.objects }, missing).some(({ direction }) => direction === "target-missing"), true);

  const extra = normalizeDriftCatalog([...expected, { object_kind: "index", object_name: "public.idx_stage5_manual", object_identity: "public.idx_stage5_manual", definition: "CREATE INDEX idx_stage5_manual ON public.stage5_widget USING btree (marker)" }]);
  assert.notEqual(extra.manifest_sha256, target.manifest_sha256);
  assert.equal(compareDriftCatalog({ objects: target.objects }, extra).some(({ direction }) => direction === "actual-extra"), true);
});

test("drift accepts only an exact reviewed allowance and reports unapproved manual drift", async () => {
  const expected = catalogRows();
  const extra = { object_kind: "index", object_name: "public.idx_stage5_allowed", object_identity: "public.idx_stage5_allowed", definition: "CREATE INDEX idx_stage5_allowed ON public.stage5_widget USING btree (marker)" };
  const actual = [...expected, extra];
  const extraDigest = normalizeDriftCatalog([extra]).objects[0].definition_sha256;
  const allowance = {
    id: "stage5-ci-extra-index",
    database: "auth",
    direction: "actual-extra",
    object_kind: "index",
    object_identity: "public.idx_stage5_allowed",
    actual_definition_sha256: extraDigest,
    scope: { environment: "ci" },
    reason: "CI fixture keeps a reviewed diagnostic index."
  };
  const allowedClient = fakeCatalogClient(actual);
  const allowed = await executeDrift(authDatabase, "ci", {
    environment: testEnvironment,
    driftPolicy: driftPolicy("auth", [allowance]),
    driftTarget: targetFromRows(expected),
    async connectDrift() { return allowedClient; }
  });
  assert.equal(allowed.code, EXIT.OK);
  assert.equal(allowed.drift.differences.allowed.length, 1);
  assert.equal(allowed.drift.differences.unapproved.length, 0);

  const manual = await executeDrift(authDatabase, "ci", {
    environment: testEnvironment,
    driftPolicy: driftPolicy("auth", [allowance]),
    driftTarget: targetFromRows(expected),
    async connectDrift() { return fakeCatalogClient([...actual, { object_kind: "column", object_name: "public.stage5_widget", object_identity: "public.stage5_widget.manual_change", definition: "manual_change text null " }]); }
  });
  assert.equal(manual.code, EXIT.BASELINE_OR_DRIFT);
  assert.equal(manual.drift.differences.unapproved.length, 1);
  assert.equal(manual.drift.differences.actual_extra[0].object_identity, "public.stage5_widget.manual_change");

  assert.throws(() => validateDriftPolicy(driftPolicy("auth", [{ ...allowance, id: "stage5-wildcard", object_identity: "public.*" }])), /exact/);
});

test("backfill task contract pins cursor limits and immutable revision input", () => {
  const manifest = JSON.parse(readFileSync(join(root, "tests/fixtures/db/stage5-backfill/task.json"), "utf8"));
  const batchSql = readFileSync(join(root, "tests/fixtures/db/stage5-backfill/batch.sql"), "utf8");
  const task = validateBackfillTask(manifest, batchSql, authDatabase);
  assert.equal(task.id, "stage5-copy-marker");
  assert.equal(task.batch_size, 2);
  assert.equal(task.max_batches_per_run, 4);
  assert.match(task.revision, /^[a-f0-9]{64}$/);
  assert.throws(() => validateBackfillTaskIdentity({ ...manifest, id: "stage5-other-task" }, "stage5-copy-marker"), /must match/);
  assert.throws(() => loadBackfillTask(authDatabase, "stage5-copy-marker", { taskRoot: join(root, "tests/fixtures/db/stage5-backfill-mismatch") }), /must match/);
  assert.throws(() => validateBackfillTask({ ...manifest, max_batch_size: 1 }, batchSql, authDatabase), /reviewed limit/);
  assert.throws(() => validateBackfillTask(manifest, "SELECT $1, $2", authDatabase), /WITH statement/);
});

const runPostgres = process.env.MYSERVER_STAGE5_RUN_POSTGRES === "1";
const host = process.env.MYSERVER_STAGE5_POSTGRES_HOST || "localhost";
const port = Number(process.env.MYSERVER_STAGE5_POSTGRES_PORT || "5432");
const user = process.env.MYSERVER_STAGE5_POSTGRES_USER || "postgres";
const password = process.env.MYSERVER_STAGE5_POSTGRES_PASSWORD;

function quoteIdentifier(identifier) {
  if (!/^myserver_stage5_[a-z0-9_]+$/.test(identifier)) throw new Error("stage 5 temporary database name is invalid");
  return `"${identifier}"`;
}

function connectionConfig(database) {
  return { host, port, user, password, database };
}

async function withClient(database, callback) {
  const client = new Client(connectionConfig(database));
  await client.connect();
  try {
    return await callback(client);
  } finally {
    await client.end();
  }
}

async function dropTemporaryDatabase(database) {
  if (!/^myserver_stage5_[a-z0-9_]+$/.test(database)) return;
  await withClient("postgres", async (client) => {
    await client.query("SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = $1 AND pid <> pg_backend_pid()", [database]);
    await client.query(`DROP DATABASE IF EXISTS ${quoteIdentifier(database)}`);
  });
}

async function readLiveCatalog(database) {
  return withClient(database, async (client) => {
    const result = await client.query(catalogQuery());
    return result.rows.map(({ row_to_json }) => typeof row_to_json === "string" ? JSON.parse(row_to_json) : row_to_json);
  });
}

test("stage 5 drift and backfill exercise has clean, allowlisted and unapproved outcomes with pause/resume", {
  skip: runPostgres ? false : "set MYSERVER_STAGE5_RUN_POSTGRES=1 with local PostgreSQL credentials to run",
  timeout: 120000
}, async () => {
  assert.ok(password, "MYSERVER_STAGE5_POSTGRES_PASSWORD is required for the PostgreSQL stage 5 exercise");
  const database = `myserver_stage5_drift_backfill_${process.pid}_${randomUUID().replaceAll("-", "").slice(0, 12)}`;
  const environment = {
    ...process.env,
    STAGE5_DATABASE_URL: `postgresql://${encodeURIComponent(user)}@${host}:${port}/${database}?sslmode=disable`,
    STAGE5_DATABASE_PASSWORD: password
  };
  const stageDatabase = {
    key: "stage5",
    defaultDatabase: database,
    migrationDirectory: "db/migrations/auth",
    logicalOwner: "auth-http",
    urlEnvironment: "STAGE5_DATABASE_URL",
    userEnvironment: "STAGE5_DATABASE_USER",
    passwordEnvironment: "STAGE5_DATABASE_PASSWORD"
  };
  const backfillDatabase = { ...stageDatabase, key: "auth" };
  const fixtureManifest = JSON.parse(readFileSync(join(root, "tests/fixtures/db/stage5-backfill/task.json"), "utf8"));
  const fixtureBatchSql = readFileSync(join(root, "tests/fixtures/db/stage5-backfill/batch.sql"), "utf8");
  const task = validateBackfillTask(fixtureManifest, fixtureBatchSql, backfillDatabase);
  let created = false;

  try {
    await withClient("postgres", async (client) => {
      await client.query(`CREATE DATABASE ${quoteIdentifier(database)}`);
    });
    created = true;
    const beforeTargetMigration = await executeBackfill("backfill-run", backfillDatabase, task.id, "stage5-test", { environment, backfillTask: task, maxBatches: 1 });
    assert.equal(beforeTargetMigration.code, EXIT.VALIDATION);
    const migration = executeDatabase("up", backfillDatabase, "stage5-test", { environment });
    assert.equal(migration.code, EXIT.OK, migration.error);
    await withClient(database, async (client) => {
      await client.query("CREATE TABLE stage5_drift_items (id bigint PRIMARY KEY, marker text NOT NULL, checked_at timestamptz)");
      await client.query("CREATE INDEX idx_stage5_drift_marker ON stage5_drift_items (marker)");
      await client.query("ALTER TABLE stage5_drift_items ADD CONSTRAINT ck_stage5_drift_marker CHECK (length(marker) > 0)");
      await client.query("CREATE FUNCTION stage5_drift_stamp() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NEW.checked_at = clock_timestamp(); RETURN NEW; END; $$");
      await client.query("CREATE TRIGGER trg_stage5_drift_stamp BEFORE INSERT ON stage5_drift_items FOR EACH ROW EXECUTE FUNCTION stage5_drift_stamp()");
      await client.query("CREATE TABLE stage5_backfill_items (id bigint PRIMARY KEY, copied_at timestamptz)");
      await client.query("INSERT INTO stage5_backfill_items (id) SELECT generate_series(1, 5)");
    });

    const targetRows = await readLiveCatalog(database);
    const target = targetFromRows(targetRows);
    const clean = await executeDrift(stageDatabase, "stage5-test", {
      environment,
      driftPolicy: driftPolicy("stage5"),
      driftTarget: target
    });
    assert.equal(clean.code, EXIT.OK, clean.error);

    await withClient(database, (client) => client.query("CREATE INDEX idx_stage5_allowed ON stage5_drift_items (checked_at)"));
    const allowedRows = await readLiveCatalog(database);
    const allowedObject = normalizeDriftCatalog(allowedRows).objects.find(({ object_identity }) => object_identity === "public.idx_stage5_allowed");
    const allowance = {
      id: "stage5-reviewed-diagnostic-index",
      database: "stage5",
      direction: "actual-extra",
      object_kind: "index",
      object_identity: "public.idx_stage5_allowed",
      actual_definition_sha256: allowedObject.definition_sha256,
      scope: { environment: "stage5-test" },
      reason: "Temporary stage 5 diagnostic index is deliberately reviewed."
    };
    const allowlisted = await executeDrift(stageDatabase, "stage5-test", {
      environment,
      driftPolicy: driftPolicy("stage5", [allowance]),
      driftTarget: target
    });
    assert.equal(allowlisted.code, EXIT.OK, allowlisted.error);
    assert.equal(allowlisted.drift.differences.allowed.length, 1);

    await withClient(database, (client) => client.query("ALTER TABLE stage5_drift_items ADD COLUMN manual_change text"));
    const manual = await executeDrift(stageDatabase, "stage5-test", {
      environment,
      driftPolicy: driftPolicy("stage5", [allowance]),
      driftTarget: target
    });
    assert.equal(manual.code, EXIT.BASELINE_OR_DRIFT);
    assert.equal(manual.drift.differences.actual_extra.some(({ object_identity }) => object_identity === "public.stage5_drift_items.manual_change"), true);

    let report = await executeBackfill("backfill-run", backfillDatabase, task.id, "stage5-test", { environment, backfillTask: task, maxBatches: 1 });
    assert.equal(report.code, EXIT.OK, report.error);
    assert.equal(report.backfill.state.cursor, "2");
    assert.equal(report.backfill.state.status, "pending");

    report = await executeBackfill("backfill-pause", backfillDatabase, task.id, "stage5-test", { environment, backfillTask: task });
    assert.equal(report.backfill.state.status, "paused");
    report = await executeBackfill("backfill-run", backfillDatabase, task.id, "stage5-test", { environment, backfillTask: task, maxBatches: 1 });
    assert.equal(report.backfill.batches[0].reason, "paused");
    report = await executeBackfill("backfill-resume", backfillDatabase, task.id, "stage5-test", { environment, backfillTask: task });
    assert.equal(report.backfill.state.cursor, "2");
    report = await executeBackfill("backfill-run", backfillDatabase, task.id, "stage5-test", { environment, backfillTask: task, maxBatches: 4 });
    assert.equal(report.backfill.state.status, "completed");

    const recoverableTask = validateBackfillTask({ ...fixtureManifest, id: "stage5-recoverable-copy" }, [
      "WITH selected AS (",
      "  SELECT id FROM stage5_recoverable_items WHERE id > $1::bigint ORDER BY id LIMIT $2 FOR UPDATE SKIP LOCKED",
      "), updated AS (",
      "  UPDATE stage5_recoverable_items AS target SET copied_at = clock_timestamp() FROM selected WHERE target.id = selected.id RETURNING target.id",
      ")",
      "SELECT coalesce(max(id), $1::bigint)::text AS next_cursor, count(*)::integer AS processed_rows FROM updated"
    ].join("\n"), backfillDatabase);
    const failed = await executeBackfill("backfill-run", backfillDatabase, recoverableTask.id, "stage5-test", { environment, backfillTask: recoverableTask, maxBatches: 1 });
    assert.equal(failed.code, EXIT.EXECUTION);
    const failedStatus = await executeBackfill("backfill-status", backfillDatabase, recoverableTask.id, undefined, { environment, backfillTask: recoverableTask });
    assert.equal(failedStatus.backfill.state.status, "failed");
    const blockedRetry = await executeBackfill("backfill-run", backfillDatabase, recoverableTask.id, "stage5-test", { environment, backfillTask: recoverableTask, maxBatches: 1 });
    assert.equal(blockedRetry.code, EXIT.EXECUTION);
    assert.equal(blockedRetry.ok, false);
    assert.equal(blockedRetry.backfill.batches[0].reason, "failed");
    await withClient(database, async (client) => {
      await client.query("CREATE TABLE stage5_recoverable_items (id bigint PRIMARY KEY, copied_at timestamptz)");
      await client.query("INSERT INTO stage5_recoverable_items (id) VALUES (1)");
    });
    const resumedFailure = await executeBackfill("backfill-resume", backfillDatabase, recoverableTask.id, "stage5-test", { environment, backfillTask: recoverableTask });
    assert.equal(resumedFailure.code, EXIT.OK);
    const recovered = await executeBackfill("backfill-run", backfillDatabase, recoverableTask.id, "stage5-test", { environment, backfillTask: recoverableTask, maxBatches: 1 });
    assert.equal(recovered.code, EXIT.OK, recovered.error);
    assert.equal(recovered.backfill.batches[0].executed, true);

    await withClient(database, async (client) => {
      const copied = await client.query("SELECT count(*)::integer AS count FROM stage5_backfill_items WHERE copied_at IS NOT NULL");
      assert.equal(copied.rows[0].count, 5);
      const audit = await client.query("SELECT action, outcome FROM public._myserver_backfill_audit ORDER BY id");
      assert.equal(audit.rows.some(({ action }) => action === "pause"), true);
      assert.equal(audit.rows.some(({ action }) => action === "resume"), true);
      assert.equal(audit.rows.some(({ action, outcome }) => action === "failure" && outcome === "failed"), true);
      const sqlxHistory = await client.query("SELECT to_regclass('public._sqlx_migrations') AS history");
      assert.equal(sqlxHistory.rows[0].history, "_sqlx_migrations");
      const historyRows = await client.query("SELECT count(*)::integer AS count FROM public._sqlx_migrations");
      assert.equal(historyRows.rows[0].count, 1);
    });
  } finally {
    if (created) await dropTemporaryDatabase(database);
  }
});
