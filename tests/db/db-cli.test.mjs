import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdtempSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { EXIT, baselinePolicy, canonicalizeCatalog, catalogQuery, classifyFailure, executeDatabase, migrationMetricsEnvironment, migrationSafetyForDirectory, migrationSafetyForFile, migrationTimeoutBudget, parseArguments, redact, resolveDatabases, resolveSqlxBinary, sqlxMigrationMetadata, sqlxPostgresLockId, validateMigrationFiles } from "../../tools/db.js";

const authDatabase = {
  key: "auth",
  defaultDatabase: "myserver_auth",
  migrationDirectory: "db/migrations/auth",
  urlEnvironment: "TEST_DATABASE_URL",
  userEnvironment: "TEST_DATABASE_USER",
  passwordEnvironment: "TEST_DATABASE_PASSWORD"
};

const testEnvironment = { TEST_DATABASE_URL: "postgres://migration:secret@example.test:6543/myserver_auth?sslmode=require&application_name=db-test" };

function sqlxHistoryOutput(directory = "db/migrations/auth") {
  return sqlxMigrationMetadata(join(process.cwd(), directory))
    .map(({ version, description, checksum }) => `${version}|${description}|${checksum}|t`)
    .join("\n");
}

function withoutMigrationUrls() {
  const environment = { ...process.env };
  for (const key of Object.keys(environment)) {
    if (key.startsWith("MYSERVER_DB_MIGRATION_")) delete environment[key];
  }
  return environment;
}

function fakeBaselineClient(snapshotRows, failure = {}) {
  const calls = [];
  return {
    calls,
    async query(sql, parameters = []) {
      calls.push({ sql, parameters });
      if (failure.match && sql.includes(failure.match)) throw new Error(failure.message);
      if (sql.includes("row_to_json(c)::text")) return { rows: snapshotRows.map((row) => ({ row_to_json: row })) };
      if (sql.includes("SELECT to_regclass")) return { rows: [{ history: failure.history || null }] };
      return { rows: [] };
    },
    async end() { calls.push({ sql: "END" }); }
  };
}

function testOnlyAllowlist(fingerprint, entryOverrides = {}) {
  const target = sqlxMigrationMetadata(join(process.cwd(), "db/migrations/auth"))[0];
  return {
    schema: 2,
    fingerprintAlgorithm: "sha256",
    canonicalCatalogFormat: "myserver-postgresql-catalog-v1",
    databases: {
      auth: {
        fingerprints: [{
          sha256: fingerprint,
          version: target.version,
          description: target.description,
          kind: "test-fixture",
          ...entryOverrides
        }]
      },
      game: { fingerprints: [] },
      chat: { fingerprints: [] },
      announce: { fingerprints: [] },
      mail: { fingerprints: [] }
    }
  };
}

function transactionalSafetyHeader(overrides = {}) {
  return [
    `-- Logical owner: ${overrides.logicalOwner || "test-owner"}`,
    `-- Compatibility phase: ${overrides.compatibilityPhase || "expand"}`,
    `-- Irreversible risk: ${overrides.irreversibleRisk || "none"}`,
    `-- Transaction: ${overrides.transaction || "required"}`,
    `-- Lock timeout: ${overrides.lockTimeout || "5s"}`,
    `-- Statement timeout: ${overrides.statementTimeout || "60s"}`,
    `-- Backup point: ${overrides.backupPoint || "not-required"}`,
    `-- Recovery command: ${overrides.recoveryCommand || "SQLx rolls back the transaction; correct the migration and rerun db up."}`,
    ...(overrides.riskSummary ? [`-- Risk summary: ${overrides.riskSummary}`] : [])
  ].join("\n");
}

test("database CLI accepts only supported commands and options", () => {
  assert.deepEqual(parseArguments(["status", "--database", "auth"]), {
    command: "status",
    database: "auth",
    actor: undefined,
    expectedFingerprint: undefined,
    environment: undefined,
    task: undefined,
    maxBatches: undefined
  });
  assert.throws(() => parseArguments(["baseline", "--database", "auth"]), /--actor/);
  assert.throws(() => parseArguments(["status", "--database", "auth", "--force", "yes"]), /only --database/);
  assert.throws(() => parseArguments(["baseline", "--database", "all", "--actor", "deploy", "--expected-fingerprint", "a".repeat(64)]), /one database/);
  assert.throws(() => parseArguments(["baseline", "--database", "auth", "--actor", "deploy", "--expected-fingerprint", "invalid"]), /SHA-256/);
});

test("baseline rejects unknown fingerprints before catalog access or history writes", () => {
  const fingerprint = "a".repeat(64);
  assert.equal(baselinePolicy("auth", fingerprint).allowed, false);
  let called = false;
  const report = executeDatabase("baseline", authDatabase, "deploy", {
    expectedFingerprint: fingerprint,
    run() { called = true; return { status: 0, output: "" }; }
  });
  assert.equal(report.code, EXIT.BASELINE_OR_DRIFT);
  assert.equal(called, false);
  assert.match(report.error, /not a reviewed baseline variant/);
});

test("baseline locks before catalog read and writes SQLx history in the same session", async () => {
  const fixture = [{ object_kind: "table", object_name: "public.simulated_baseline_fixture", definition: "r" }];
  const fingerprint = canonicalizeCatalog(fixture).sha256;
  const client = fakeBaselineClient(fixture);
  const report = await executeDatabase("baseline", authDatabase, "baseline-test", {
    expectedFingerprint: fingerprint,
    environment: testEnvironment,
    allowlist: testOnlyAllowlist(fingerprint),
    async connectBaseline() { return client; }
  });
  assert.equal(report.ok, true);
  const sql = client.calls.map(({ sql }) => sql);
  assert.equal(sql.findIndex((value) => value.startsWith("SELECT pg_advisory_lock")) < sql.findIndex((value) => value.includes("row_to_json(c)::text")), true);
  assert.equal(sql.findIndex((value) => value.includes("row_to_json(c)::text")) < sql.findIndex((value) => value.includes("CREATE TABLE IF NOT EXISTS _sqlx_migrations")), true);
  assert.equal(sql.findIndex((value) => value.includes("INSERT INTO _sqlx_migrations")) < sql.findIndex((value) => value.includes("INSERT INTO public._myserver_migration_audit")), true);
  assert.equal(sql.at(-2), "COMMIT");
  const migration = sqlxMigrationMetadata(join(process.cwd(), "db/migrations/auth"))[0];
  const insert = client.calls.find(({ sql: value }) => value.includes("INSERT INTO _sqlx_migrations"));
  assert.deepEqual(insert.parameters, [migration.version, "initial schema", migration.checksum]);
  const lock = client.calls.find(({ sql: value }) => value.startsWith("SELECT pg_advisory_lock"));
  assert.deepEqual(lock.parameters, [sqlxPostgresLockId("myserver_auth")]);
  const audit = client.calls.find(({ sql: value }) => value.includes("INSERT INTO public._myserver_migration_audit"));
  assert.match(audit.parameters[1], new RegExp(`target_version=${migration.version};target_description=${migration.description}`));
  assert.equal(report.audit.targetVersion, migration.version);
});

test("baseline allowlist requires a reviewed version and description in the local migration directory", () => {
  const fingerprint = "b".repeat(64);
  const migrations = sqlxMigrationMetadata(join(process.cwd(), "db/migrations/auth"));
  assert.equal(baselinePolicy("auth", fingerprint, testOnlyAllowlist(fingerprint), migrations).allowed, true);
  assert.match(
    baselinePolicy("auth", fingerprint, { ...testOnlyAllowlist(fingerprint), schema: 1 }, migrations).reason,
    /schema must be 2/
  );
  for (const [entryOverrides, expectedReason] of [
    [{ version: undefined }, /must bind/],
    [{ version: "invalid" }, /must bind/],
    [{ version: "20260718161349" }, /do not match a local migration/],
    [{ version: "20260718161351" }, /beyond the local migration directory/],
    [{ description: "different description" }, /do not match a local migration/]
  ]) {
    const policy = baselinePolicy("auth", fingerprint, testOnlyAllowlist(fingerprint, entryOverrides), migrations);
    assert.equal(policy.allowed, false);
    assert.match(policy.reason, expectedReason);
  }
});

test("baseline writes only the reviewed target version when later local migrations exist", async () => {
  const fixture = [{ object_kind: "table", object_name: "public.simulated_baseline_fixture", definition: "r" }];
  const fingerprint = canonicalizeCatalog(fixture).sha256;
  const relativeDirectory = join("tests", `.tmp-baseline-future-${process.pid}-${Date.now()}`);
  const directory = join(process.cwd(), relativeDirectory);
  const initialFilename = "20260718161350_initial_schema.sql";
  const futureFilename = "20260718161351_future_additive_column.sql";
  mkdirSync(directory, { recursive: true });
  writeFileSync(join(directory, initialFilename), readFileSync(join(process.cwd(), "db/migrations/auth", initialFilename)));
  writeFileSync(join(directory, futureFilename), `${transactionalSafetyHeader({ logicalOwner: "auth-http" })}\nCREATE TABLE baseline_future_fixture (id bigint PRIMARY KEY);\n`);
  const client = fakeBaselineClient(fixture);
  try {
    const initial = sqlxMigrationMetadata(directory)[0];
    const report = await executeDatabase("baseline", { ...authDatabase, migrationDirectory: relativeDirectory }, "baseline-test", {
      expectedFingerprint: fingerprint,
      environment: testEnvironment,
      allowlist: testOnlyAllowlist(fingerprint),
      async connectBaseline() { return client; }
    });
    assert.equal(report.ok, true);
    assert.equal(report.output, `baseline recorded 1 SQLx migration(s) through version ${initial.version}`);
    const historyInserts = client.calls.filter(({ sql }) => sql.includes("INSERT INTO _sqlx_migrations"));
    assert.equal(historyInserts.length, 1);
    assert.deepEqual(historyInserts[0].parameters, [initial.version, initial.description, initial.checksum]);
    const audit = client.calls.find(({ sql }) => sql.includes("INSERT INTO public._myserver_migration_audit"));
    assert.match(audit.parameters[1], new RegExp(`target_version=${initial.version};target_description=${initial.description}`));
    assert.doesNotMatch(audit.parameters[1], /20260718161351/);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test("baseline rejects catalog mismatch, existing history, lock and audit transaction failures", async () => {
  const fixture = [{ object_kind: "table", object_name: "public.simulated_baseline_fixture", definition: "r" }];
  const fingerprint = canonicalizeCatalog(fixture).sha256;
  const options = {
    expectedFingerprint: fingerprint,
    environment: testEnvironment,
    allowlist: testOnlyAllowlist(fingerprint)
  };
  const mismatch = await executeDatabase("baseline", authDatabase, "baseline-test", {
    ...options,
    async connectBaseline() { return fakeBaselineClient([{ ...fixture[0], definition: "different" }]); }
  });
  assert.equal(mismatch.code, EXIT.BASELINE_OR_DRIFT);
  for (const [failure, expectedCode] of [[{ history: "_sqlx_migrations" }, EXIT.BASELINE_OR_DRIFT], [{ match: "pg_advisory_lock", message: "could not obtain advisory lock" }, EXIT.LOCK], [{ match: "INSERT INTO public._myserver_migration_audit", message: "permission denied for table _myserver_migration_audit" }, EXIT.EXECUTION]]) {
    const report = await executeDatabase("baseline", authDatabase, "baseline-test", {
      ...options,
      async connectBaseline() { return fakeBaselineClient(fixture, failure); }
    });
    assert.equal(report.code, expectedCode);
  }
});

test("baseline schema is split from bootstrap and development seed", () => {
  const root = process.cwd();
  const init = readFileSync(join(root, "db/init.sql"), "utf8").replaceAll("\r\n", "\n");
  const bootstrap = readFileSync(join(root, "db/bootstrap/development.sql"), "utf8");
  const seed = readFileSync(join(root, "db/seeds/development/auth-local-world.sql"), "utf8");
  const owners = {
    auth: "auth-http",
    game: "game-server",
    chat: "chat-server",
    announce: "announce-service",
    mail: "mail-service"
  };
  assert.match(bootstrap, /CREATE DATABASE/);
  assert.match(seed, /local-dev/);
  for (const database of ["auth", "game", "chat", "announce", "mail"]) {
    const directory = join(root, "db/migrations", database);
    const files = readdirSync(directory).filter((file) => file.endsWith(".sql"));
    assert.deepEqual(files, ["20260718161350_initial_schema.sql"]);
    const schema = readFileSync(join(directory, files[0]), "utf8");
    assert.match(schema, new RegExp(`^-- Logical owner: ${owners[database]}\\r?\\n-- Compatibility phase: expand\\r?\\n-- Irreversible risk: `));
    assert.doesNotMatch(schema, /\\connect|CREATE DATABASE/i);
    assert.doesNotMatch(schema, /local-dev|INSERT INTO id_origins|INSERT INTO worlds/i);
    const next = database === "mail" ? "$" : `\\n\\connect myserver_${database === "auth" ? "game" : database === "game" ? "chat" : database === "chat" ? "announce" : "mail"}`;
    const source = new RegExp(`\\\\connect myserver_${database}\\n([\\s\\S]*?)(?=${next})`).exec(init)?.[1] || "";
    const sourceSchema = database === "auth"
      ? source.replace(/INSERT INTO id_origins[\s\S]*?\n\);\n\n/, "")
      : source;
    assert.equal(schema.replaceAll("\r\n", "\n").trim().endsWith(sourceSchema.trim()), true, `${database} baseline must preserve init.sql DDL`);
  }
});

test("catalog fingerprint inputs cover the reviewed schema object kinds", () => {
  const query = readFileSync(join(process.cwd(), "db/schema/catalog-snapshot.sql"), "utf8");
  for (const objectKind of ["table", "column", "constraint", "index", "trigger", "function"]) {
    assert.match(query, new RegExp(`'${objectKind}' AS object_kind`));
  }
  assert.match(query, /_sqlx_migrations/);
  assert.match(query, /_myserver_migration_audit/);
  const allowlist = JSON.parse(readFileSync(join(process.cwd(), "db/schema/baseline-allowlist.json"), "utf8"));
  assert.equal(allowlist.schema, 2);
  assert.deepEqual(allowlist.reviewedFingerprintEntry.required, ["sha256", "version", "description"]);
  assert.deepEqual(Object.keys(allowlist.databases), ["auth", "game", "chat", "announce", "mail"]);
  assert.equal(Object.values(allowlist.databases).every(({ fingerprints }) => fingerprints.length === 0), true);
  const fixture = JSON.parse(readFileSync(join(process.cwd(), "tests/fixtures/db/simulated-auth-catalog.json"), "utf8"));
  assert.equal(canonicalizeCatalog(fixture).sha256, "dae48e4fcba60d6258712b1ddb8acdffa486e7b0cedb7df706f21642b6d6732a");
  assert.match(catalogQuery(), /^SELECT row_to_json\(c\)::text FROM \(/m);
  assert.match(catalogQuery(), /SELECT 'table' AS object_kind/);
});

test("reset script requires confirmation, development and localhost migration URLs", () => {
  const reset = readFileSync(join(process.cwd(), "scripts/reset-dev-data.ps1"), "utf8");
  assert.match(reset, /-not \$Confirm/);
  assert.match(reset, /ValidateSet\("development"\)/);
  assert.match(reset, /must target localhost/);
  assert.match(reset, /must target \$\(\$connection\.Database\)/);
  assert.match(reset, /same local PostgreSQL endpoint/);
  assert.match(reset, /\$psqlConnectionArguments = @\("--host", \$bootstrapHost, "--port", \$bootstrapPort/);
  assert.match(reset, /db\/bootstrap\/development\.sql/);
  assert.match(reset, /tools\/db\.js/);
});

test("Node CLI emits one JSON line and propagates configuration failure", () => {
  const result = spawnSync(process.execPath, ["tools/db.js", "status", "--database", "auth"], {
    cwd: process.cwd(),
    env: withoutMigrationUrls(),
    encoding: "utf8"
  });
  assert.equal(result.status, EXIT.CONFIG);
  assert.equal(result.stdout.includes("\\\\n"), false);
  assert.equal(result.stderr, "");
  const lines = result.stdout.trim().split("\n");
  assert.equal(lines.length, 1);
  assert.equal(JSON.parse(lines[0]).reports[0].code, EXIT.CONFIG);
});

test("PowerShell entry point propagates the CLI exit code and JSON line", { skip: process.platform !== "win32" }, () => {
  const result = spawnSync("powershell", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "scripts/db.ps1", "-Command", "status", "-Database", "auth"], {
    cwd: process.cwd(),
    env: withoutMigrationUrls(),
    encoding: "utf8"
  });
  assert.equal(result.status, EXIT.CONFIG);
  assert.equal(result.stdout.includes("\\\\n"), false);
  const lines = result.stdout.trim().split("\n");
  assert.equal(lines.length, 1);
  assert.equal(JSON.parse(lines[0]).reports[0].code, EXIT.CONFIG);
});

test("database selection preserves the deployment order", () => {
  const config = { databases: { auth: {}, game: {}, chat: {}, announce: {}, mail: {} } };
  assert.deepEqual(resolveDatabases("all", config).map(({ key }) => key), ["auth", "game", "chat", "announce", "mail"]);
  assert.throws(() => resolveDatabases("unknown", config), /unknown database/);
});

test("redaction removes PostgreSQL userinfo and password-like values", () => {
  const value = redact("postgres://admin:super-secret@example.test/db password=other-secret");
  assert.equal(value.includes("super-secret"), false);
  assert.equal(value.includes("other-secret"), false);
  assert.match(value, /postgres:\/\/\*\*\*:\*\*\*@example\.test/);
});

test("migration metrics child uses an explicit runtime and NATS-only environment allowlist", () => {
  const environment = migrationMetricsEnvironment({
    Path: "C:\\Windows\\System32",
    SystemRoot: "C:\\Windows",
    WINDIR: "C:\\Windows",
    ComSpec: "C:\\Windows\\System32\\cmd.exe",
    PATHEXT: ".COM;.EXE;.BAT;.CMD",
    NATS_URL: "nats://metrics.example.test:4222",
    MYSERVER_DB_MIGRATION_METRICS_ENABLED: "1",
    MYSERVER_DB_MIGRATION_METRICS_NATS_URL: "nats://metrics.example.test:4222",
    MYSERVER_DB_MIGRATION_METRICS_TIMEOUT_MS: "750",
    MYSERVER_DB_MIGRATION_AUTH_URL: "postgres://migration:secret@database.example.test/myserver_auth",
    MYSERVER_DB_MIGRATION_AUTH_PASSWORD: "secret",
    MYSERVER_STAGE7_POSTGRES_PASSWORD: "stage7-secret",
    MYSERVER_DB_DEPLOY_TEMPORARY_POSTGRES_URL: "postgres://postgres:secret@localhost/postgres",
    DATABASE_URL: "postgres://migration:secret@database.example.test/myserver_auth",
    REDIS_URL: "redis://redis-user:redis-secret@redis.example.test:6379/0",
    AUTH_HTTP_URL: "https://service-user:service-secret@auth.example.test",
    SERVICE_DISCOVERY_URL: "https://discovery.example.test",
    PGHOST: "database.example.test",
    PGPASSWORD: "pg-secret",
    API_TOKEN: "irrelevant-secret",
    EXTERNAL_SECRET: "external-secret"
  }, { subject: "myserver.metrics.db-migration.dGVzdA", payload: { service: "db-migration", metrics: { event_type: "migration" } } });
  assert.equal(environment.PATH, "C:\\Windows\\System32");
  assert.equal(environment.SystemRoot, "C:\\Windows");
  assert.equal(environment.WINDIR, "C:\\Windows");
  assert.equal(environment.ComSpec, "C:\\Windows\\System32\\cmd.exe");
  assert.equal(environment.PATHEXT, ".COM;.EXE;.BAT;.CMD");
  assert.equal(environment.NATS_URL, "nats://metrics.example.test:4222");
  assert.equal(environment.MYSERVER_DB_MIGRATION_METRICS_ENABLED, "1");
  assert.equal(environment.MYSERVER_DB_MIGRATION_METRICS_NATS_URL, "nats://metrics.example.test:4222");
  assert.equal(environment.MYSERVER_DB_MIGRATION_METRICS_TIMEOUT_MS, "750");
  assert.equal(environment.MYSERVER_DB_MIGRATION_AUTH_URL, undefined);
  assert.equal(environment.MYSERVER_DB_MIGRATION_AUTH_PASSWORD, undefined);
  assert.equal(environment.MYSERVER_STAGE7_POSTGRES_PASSWORD, undefined);
  assert.equal(environment.MYSERVER_DB_DEPLOY_TEMPORARY_POSTGRES_URL, undefined);
  assert.equal(environment.DATABASE_URL, undefined);
  assert.equal(environment.REDIS_URL, undefined);
  assert.equal(environment.AUTH_HTTP_URL, undefined);
  assert.equal(environment.SERVICE_DISCOVERY_URL, undefined);
  assert.equal(environment.PGHOST, undefined);
  assert.equal(environment.PGPASSWORD, undefined);
  assert.equal(environment.API_TOKEN, undefined);
  assert.equal(environment.EXTERNAL_SECRET, undefined);
  assert.deepEqual(Object.keys(environment).sort(), [
    "ComSpec",
    "MYSERVER_DB_MIGRATION_METRICS_ENABLED",
    "MYSERVER_DB_MIGRATION_METRICS_NATS_URL",
    "MYSERVER_DB_MIGRATION_METRICS_TIMEOUT_MS",
    "MYSERVER_DB_MIGRATION_METRIC_EVENT",
    "NATS_URL",
    "PATH",
    "PATHEXT",
    "SystemRoot",
    "WINDIR"
  ].sort());
  assert.equal(JSON.stringify(environment).includes("secret"), false);
});

test("known sqlx failure classes map to stable exit codes", () => {
  assert.equal(classifyFailure("migration checksum mismatch"), EXIT.VALIDATION);
  assert.equal(classifyFailure("could not obtain advisory lock"), EXIT.LOCK);
  assert.equal(classifyFailure("password authentication failed"), EXIT.CONNECTION);
  assert.equal(classifyFailure("syntax error at or near SELECT"), EXIT.EXECUTION);
});

test("sqlx binary requires the configured SHA-256", () => {
  const directory = mkdtempSync(join(tmpdir(), "myserver-sqlx-"));
  const binary = join(directory, "sqlx.exe");
  writeFileSync(binary, "approved binary");
  const hash = createHash("sha256").update("approved binary").digest("hex");
  const config = {
    version: "0.8.6",
    platforms: {
      "win32-x64": { binary, artifactUrl: "https://example.invalid/sqlx.exe", sha256: hash, provisioned: true }
    }
  };
  assert.throws(() => resolveSqlxBinary({ ...config, platforms: { "win32-x64": { ...config.platforms["win32-x64"], provisioned: false } } }), /not provisioned/);
  assert.throws(() => resolveSqlxBinary({ ...config, platforms: { "win32-x64": { ...config.platforms["win32-x64"], sha256: "0".repeat(64) } } }), /mismatch/);
  assert.equal(resolveSqlxBinary(config).binary, binary);
});

test("sqlx binary accepts the pinned local cargo-install artifact", () => {
  const directory = mkdtempSync(join(tmpdir(), "myserver-sqlx-artifact-"));
  const binary = join(directory, "sqlx.exe");
  writeFileSync(binary, "approved local sqlx fixture");
  const hash = createHash("sha256").update(readFileSync(binary)).digest("hex");
  const config = {
    version: "0.8.6",
    platforms: {
      "win32-x64": {
        binary,
        artifactUrl: "local://cargo-install/sqlx-cli-0.8.6?locked=true&features=postgres%2Crustls",
        sha256: hash,
        provisioned: true
      }
    }
  };
  assert.equal(resolveSqlxBinary(config).binary, binary);
});

test("migration files require monotonic UTC timestamp names", () => {
  const directory = mkdtempSync(join(tmpdir(), "myserver-migrations-"));
  writeFileSync(join(directory, "20260718120000_first.sql"), `${transactionalSafetyHeader()}\nSELECT 1;\n`);
  writeFileSync(join(directory, "20260718120001_second_step.sql"), `${transactionalSafetyHeader()}\nSELECT 1;\n`);
  assert.deepEqual(validateMigrationFiles(directory), ["20260718120000_first.sql", "20260718120001_second_step.sql"]);
  const invalid = join(directory, "invalid");
  mkdirSync(invalid);
  writeFileSync(join(invalid, "1_bad.sql"), `${transactionalSafetyHeader()}\nSELECT 1;\n`);
  assert.throws(() => validateMigrationFiles(invalid), /invalid migration filename/);
  writeFileSync(join(invalid, "20260718120002__double.sql"), `${transactionalSafetyHeader()}\nSELECT 1;\n`);
  assert.throws(() => validateMigrationFiles(invalid), /invalid migration filename/);
});

test("migration safety metadata enforces transaction, rollback and irreversible change rules", () => {
  const legacyInitial = readFileSync(join(process.cwd(), "db/migrations/auth/20260718161350_initial_schema.sql"), "utf8");
  assert.equal(migrationSafetyForFile("20260718161350_initial_schema.sql", legacyInitial, { expectedOwner: "auth-http" }).legacy, true);
  assert.throws(
    () => migrationSafetyForFile("20260718161350_initial_schema.sql", `${legacyInitial}\n-- altered`, { expectedOwner: "auth-http" }),
    /Transaction metadata is required/
  );

  const normal = `${transactionalSafetyHeader()}\nALTER TABLE example_table ADD COLUMN new_column text;\n`;
  const normalPolicy = migrationSafetyForFile("20260718120000_expand_column.sql", normal, { expectedOwner: "test-owner" });
  assert.equal(normalPolicy.transaction, "required");
  assert.equal(normalPolicy.lockTimeoutMs, 5000);
  assert.equal(normalPolicy.statementTimeoutMs, 60000);

  const concurrentIndex = [
    "-- no-transaction",
    transactionalSafetyHeader({ transaction: "no-transaction", statementTimeout: "5min", recoveryCommand: "Inspect pg_index.indisvalid; drop the invalid index concurrently, then rerun db up." }),
    "-- Non-transaction reason: create-index-concurrently",
    "CREATE INDEX CONCURRENTLY idx_example_table_new_column ON example_table (new_column);"
  ].join("\n");
  const noTransactionPolicy = migrationSafetyForFile("20260718120001_add_index.sql", concurrentIndex, { expectedOwner: "test-owner" });
  assert.equal(noTransactionPolicy.transaction, "no-transaction");
  assert.equal(noTransactionPolicy.nonTransactionReason, "create-index-concurrently");

  assert.throws(
    () => migrationSafetyForFile("20260718120002_missing_directive.sql", transactionalSafetyHeader({ transaction: "no-transaction" }), { expectedOwner: "test-owner" }),
    /must agree/
  );
  assert.throws(
    () => migrationSafetyForFile("20260718120003_bad_reason.sql", concurrentIndex.replace("create-index-concurrently", "vacuum"), { expectedOwner: "test-owner" }),
    /approved Non-transaction reason/
  );
  assert.throws(
    () => migrationSafetyForFile("20260718120003_wrong_operation.sql", concurrentIndex.replace("CREATE INDEX CONCURRENTLY", "CREATE INDEX"), { expectedOwner: "test-owner" }),
    /does not contain the declared approved operation/
  );
  assert.throws(
    () => migrationSafetyForFile("20260718120004_manual_commit.sql", `${normal}\nCOMMIT;`, { expectedOwner: "test-owner" }),
    /must not issue BEGIN, COMMIT or ROLLBACK/
  );
  assert.throws(
    () => migrationSafetyForFile("20260718120004_expand_drop.sql", `${normal}\nALTER TABLE example_table DROP COLUMN old_column;`, { expectedOwner: "test-owner" }),
    /expand migrations must be additive/
  );
  assert.throws(
    () => migrationSafetyForFile("20260718120005_unbacked_contract.sql", `${transactionalSafetyHeader({ compatibilityPhase: "contract", irreversibleRisk: "data-loss" })}\nALTER TABLE example_table DROP COLUMN old_column;`, { expectedOwner: "test-owner" }),
    /named backup point/
  );
  const irreversible = migrationSafetyForFile(
    "20260718120006_backed_contract.sql",
    `${transactionalSafetyHeader({ compatibilityPhase: "contract", irreversibleRisk: "data-loss", backupPoint: "stage4-backup-20260718", recoveryCommand: "Restore the verified backup point using the approved runbook.", riskSummary: "Dropping old_column removes values used by an obsolete service." })}\nALTER TABLE example_table DROP COLUMN old_column;`,
    { expectedOwner: "test-owner" }
  );
  assert.equal(irreversible.irreversibleRisk, "data-loss");
});

test("stage 4 rollout fixtures and templates have a machine-valid safety policy", () => {
  const root = process.cwd();
  for (const phase of ["legacy", "expand", "contract"]) {
    const migrations = migrationSafetyForDirectory(join(root, "tests/fixtures/db/stage4-rollout", phase), { expectedOwner: "stage4-rollout-test" });
    assert.equal(migrations.length > 0, true);
    const budget = migrationTimeoutBudget(migrations);
    assert.deepEqual(budget, {
      lockTimeoutMs: 5000,
      statementTimeoutMs: phase === "legacy" ? 60000 : 300000
    });
  }
  const templates = readdirSync(join(root, "db/migrations/templates")).sort();
  assert.deepEqual(templates, ["contract-drop-column.sql.example", "expand-add-column.sql.example", "no-transaction-create-index.sql.example"]);
  const noTransactionTemplate = readFileSync(join(root, "db/migrations/templates/no-transaction-create-index.sql.example"), "utf8");
  assert.equal(noTransactionTemplate.startsWith("-- no-transaction\n"), true);
});

test("mixed migration budgets use the bounded approved maximum for one SQLx batch", () => {
  const defaultBudgetMigration = {
    lockTimeoutMs: 5000,
    statementTimeoutMs: 60000
  };
  const approvedLongMigration = {
    lockTimeoutMs: 5000,
    statementTimeoutMs: 300000
  };
  assert.deepEqual(
    migrationTimeoutBudget([defaultBudgetMigration, approvedLongMigration]),
    { lockTimeoutMs: 5000, statementTimeoutMs: 300000 }
  );
  assert.deepEqual(
    migrationTimeoutBudget([{ lockTimeoutMs: 999999, statementTimeoutMs: 999999 }]),
    { lockTimeoutMs: 15000, statementTimeoutMs: 300000 }
  );
});

test("migration connection URLs cannot override the CLI timeout budget", () => {
  const report = executeDatabase("status", authDatabase, undefined, {
    environment: { ...testEnvironment, TEST_DATABASE_URL: "postgres://migration:secret@example.test:6543/myserver_auth?options=-c%20statement_timeout%3D0" },
    run() { throw new Error("connection preflight must not run"); }
  });
  assert.equal(report.code, EXIT.CONFIG);
  assert.match(report.error, /must not set PostgreSQL options/);
});

test("up rejects unbaselined user tables before SQLx is resolved", () => {
  let sqlxResolved = false;
  const report = executeDatabase("up", authDatabase, "deploy", {
    environment: testEnvironment,
    run(command) {
      assert.equal(command, "psql");
      return { status: 0, output: "f,t" };
    },
    resolveSqlxBinary() {
      sqlxResolved = true;
      throw new Error("should not resolve SQLx");
    }
  });
  assert.equal(report.code, EXIT.BASELINE_OR_DRIFT);
  assert.equal(sqlxResolved, false);
});

test("psql preflight and audit receive the resolved PostgreSQL connection through child environment", () => {
  const calls = [];
  const emitted = [];
  const report = executeDatabase("up", authDatabase, "deploy", {
    environment: testEnvironment,
    resolveSqlxBinary: () => ({ binary: "sqlx.exe", version: "0.8.6" }),
    run(command, args, environment) {
      calls.push({ command, args, environment });
      if (command === "psql" && args.includes("--tuples-only") && args.includes("--field-separator=|")) return { status: 0, output: sqlxHistoryOutput() };
      if (command === "psql" && args.includes("--tuples-only")) return { status: 0, output: "t,f" };
      if (command === "sqlx.exe" && args[0] === "--version") return { status: 0, output: "sqlx-cli 0.8.6" };
      if (command === "sqlx.exe" && args[1] === "info") return { status: 0, output: "migration info" };
      if (command === "sqlx.exe" && args[1] === "run") return { status: 0, output: "migration run" };
      if (command === "psql") return { status: 0, output: "" };
      throw new Error(`unexpected command: ${command}`);
    },
    emitMigrationMetric(event) {
      emitted.push(event);
      return { delivered: true, state: "delivered" };
    }
  });
  assert.equal(report.ok, true);
  const psqlCalls = calls.filter(({ command }) => command === "psql");
  assert.equal(psqlCalls.length, 3);
  for (const { args, environment } of psqlCalls) {
    assert.equal(args.includes("--dbname"), false);
    assert.equal(args.includes(testEnvironment.TEST_DATABASE_URL), false);
    assert.equal(args.includes("secret"), false);
    assert.equal(environment.PGHOST, "example.test");
    assert.equal(environment.PGPORT, "6543");
    assert.equal(environment.PGUSER, "migration");
    assert.equal(environment.PGPASSWORD, "secret");
    assert.equal(environment.PGDATABASE, "myserver_auth");
    assert.equal(environment.PGSSLMODE, "require");
    assert.equal(environment.PGAPPNAME, "myserver-db-migrate-auth");
    assert.equal(environment.PGOPTIONS, "-c lock_timeout=5000ms -c statement_timeout=60000ms");
  }
  const sqlxCalls = calls.filter(({ command, args }) => command === "sqlx.exe" && args[0] === "migrate");
  assert.equal(sqlxCalls.every(({ environment }) => environment.PGOPTIONS === "-c lock_timeout=5000ms -c statement_timeout=60000ms"), true);
  const audit = psqlCalls.find(({ args }) => args.some((argument) => String(argument).includes("_myserver_migration_audit")));
  assert.match(audit.args.join(" "), /versions=/);
  assert.deepEqual(report.audit.migrationVersions, ["20260718161350"]);
  assert.equal(report.audit.targetMigrationVersion, "20260718161350");
  assert.deepEqual(report.metrics, { delivered: true, state: "delivered" });
  assert.equal(emitted.length, 1);
  assert.deepEqual(emitted[0].payload.metrics, {
    event_type: "migration",
    database_key: "auth",
    target_migration_version: "20260718161350",
    applied_migration_versions: "20260718161350",
    attempted_migration_versions: "none",
    outcome: "success",
    error_category: "none"
  });
  assert.equal(JSON.stringify(emitted[0]).includes("secret"), false);
});

test("a mixed migration batch injects the approved long timeout into SQLx", () => {
  const calls = [];
  const database = {
    ...authDatabase,
    key: "stage4-rollout",
    logicalOwner: "stage4-rollout-test",
    migrationDirectory: "tests/fixtures/db/stage4-rollout/expand"
  };
  const report = executeDatabase("up", database, "deploy", {
    environment: testEnvironment,
    resolveSqlxBinary: () => ({ binary: "sqlx.exe", version: "0.8.6" }),
    run(command, args, environment) {
      calls.push({ command, args, environment });
      if (command === "psql" && args.includes("--tuples-only") && args.includes("--field-separator=|")) return { status: 0, output: sqlxHistoryOutput("tests/fixtures/db/stage4-rollout/expand") };
      if (command === "psql" && args.includes("--tuples-only")) return { status: 0, output: "t,f" };
      if (command === "sqlx.exe" && args[0] === "--version") return { status: 0, output: "sqlx-cli 0.8.6" };
      if (command === "sqlx.exe" && args[1] === "info") return { status: 0, output: "migration info" };
      if (command === "sqlx.exe" && args[1] === "run") return { status: 0, output: "migration run" };
      if (command === "psql") return { status: 0, output: "" };
      throw new Error(`unexpected command: ${command}`);
    }
  });
  assert.equal(report.ok, true);
  const sqlxCalls = calls.filter(({ command, args }) => command === "sqlx.exe" && args[0] === "migrate");
  assert.equal(sqlxCalls.length, 2);
  assert.equal(sqlxCalls.every(({ environment }) => environment.PGOPTIONS === "-c lock_timeout=5000ms -c statement_timeout=300000ms"), true);
});

test("up delegates SQLx run through the migration lock runner hook", () => {
  const calls = [];
  const emitted = [];
  const report = executeDatabase("up", authDatabase, "deploy", {
    environment: testEnvironment,
    resolveSqlxBinary: () => ({ binary: "sqlx.exe", version: "0.8.6" }),
    run(command, args) {
      calls.push([command, args]);
      if (command === "psql" && args.includes("--field-separator=|")) return { status: 0, output: sqlxHistoryOutput() };
      if (command === "psql") return { status: 0, output: "t,f" };
      if (command === "sqlx.exe" && args[0] === "--version") return { status: 0, output: "sqlx-cli 0.8.6" };
      if (command === "sqlx.exe" && args[1] === "info") return { status: 0, output: "migration info" };
      throw new Error(`unexpected command: ${command}`);
    },
    runMigration(binary, args, environment, database) {
      calls.push(["lock-runner", [binary, ...args, database.defaultDatabase, environment.PGOPTIONS]]);
      return { status: 1, output: "could not obtain advisory lock" };
    },
    emitMigrationMetric(event) {
      emitted.push(event);
      return { delivered: false, state: "unavailable" };
    }
  });
  assert.equal(report.ok, false);
  assert.equal(report.code, EXIT.LOCK);
  const lockRunner = calls.find(([command]) => command === "lock-runner");
  assert.deepEqual(lockRunner[1].slice(0, 4), ["sqlx.exe", "migrate", "run", "--source"]);
  assert.equal(lockRunner[1].at(-2), "myserver_auth");
  assert.deepEqual(report.metrics, { delivered: false, state: "unavailable" });
  assert.equal(emitted.length, 1);
  assert.equal(emitted[0].payload.metrics.outcome, "failure");
  assert.equal(emitted[0].payload.metrics.error_category, "lock");
});

test("history checksum drift is rejected before SQLx migration execution", () => {
  const calls = [];
  const report = executeDatabase("validate", authDatabase, undefined, {
    environment: testEnvironment,
    resolveSqlxBinary: () => ({ binary: "sqlx.exe", version: "0.8.6" }),
    run(command, args) {
      calls.push([command, args]);
      if (command === "psql" && args.includes("--field-separator=|")) {
        return { status: 0, output: "20260718161350|initial schema|deadbeef|t" };
      }
      if (command === "psql") return { status: 0, output: "t,f" };
      if (command === "sqlx.exe" && args[0] === "--version") return { status: 0, output: "sqlx-cli 0.8.6" };
      throw new Error(`unexpected command: ${command}`);
    }
  });
  assert.equal(report.ok, false);
  assert.equal(report.code, EXIT.VALIDATION);
  assert.equal(calls.some(([command, args]) => command === "sqlx.exe" && args[0] === "migrate"), false);
});

test("initialized database rejects an unapproved SQLx artifact", () => {
  const report = executeDatabase("validate", authDatabase, undefined, {
    environment: testEnvironment,
    run(command) {
      assert.equal(command, "psql");
      return { status: 0, output: "t,f" };
    },
    resolveSqlxBinary() {
      throw new Error("not provisioned");
    }
  });
  assert.equal(report.code, EXIT.SQLX);
});

test("uninitialized status reports missing history before checking SQLx", () => {
  let sqlxResolved = false;
  const report = executeDatabase("status", authDatabase, undefined, {
    environment: testEnvironment,
    run: () => ({ status: 0, output: "f,f" }),
    resolveSqlxBinary() {
      sqlxResolved = true;
      throw new Error("not provisioned");
    }
  });
  assert.equal(report.ok, true);
  assert.equal(report.output, "_sqlx_migrations is absent");
  assert.equal(sqlxResolved, false);
});

test("audit write failure prevents an up command from succeeding", () => {
  const calls = [];
  const report = executeDatabase("up", authDatabase, "deploy", {
    environment: testEnvironment,
    now: () => "2026-07-18T00:00:00.000Z",
    resolveSqlxBinary: () => ({ binary: "sqlx.exe", version: "0.8.6" }),
    run(command, args) {
      calls.push([command, args]);
      if (command === "sqlx.exe" && args[0] === "--version") return { status: 0, output: "sqlx-cli 0.8.6" };
      if (command === "sqlx.exe") return { status: 0, output: "ok" };
      if (args.includes("--field-separator=|")) return { status: 0, output: sqlxHistoryOutput() };
      if (args.some((argument) => String(argument).includes("_myserver_migration_audit"))) return { status: 1, output: "permission denied" };
      return { status: 0, output: "t,f" };
    }
  });
  assert.equal(report.ok, false);
  assert.equal(report.code, EXIT.EXECUTION);
  assert.match(report.error, /audit write failed/);
  assert.equal(calls.some(([command, args]) => command === "sqlx.exe" && args.includes("run")), true);
});
