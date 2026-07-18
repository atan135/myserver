import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdtempSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { EXIT, baselinePolicy, canonicalizeCatalog, catalogQuery, classifyFailure, executeDatabase, parseArguments, redact, resolveDatabases, resolveSqlxBinary, sqlxMigrationMetadata, sqlxPostgresLockId, validateMigrationFiles } from "../../tools/db.js";

const authDatabase = {
  key: "auth",
  defaultDatabase: "myserver_auth",
  migrationDirectory: "db/migrations/auth",
  urlEnvironment: "TEST_DATABASE_URL",
  userEnvironment: "TEST_DATABASE_USER",
  passwordEnvironment: "TEST_DATABASE_PASSWORD"
};

const testEnvironment = { TEST_DATABASE_URL: "postgres://migration:secret@example.test:6543/myserver_auth?sslmode=require&application_name=db-test" };

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

function testOnlyAllowlist(fingerprint) {
  return {
    schema: 1,
    fingerprintAlgorithm: "sha256",
    canonicalCatalogFormat: "myserver-postgresql-catalog-v1",
    databases: {
      auth: { fingerprints: [{ sha256: fingerprint, kind: "test-fixture" }] },
      game: { fingerprints: [] },
      chat: { fingerprints: [] },
      announce: { fingerprints: [] },
      mail: { fingerprints: [] }
    }
  };
}

test("database CLI accepts only supported commands and options", () => {
  assert.deepEqual(parseArguments(["status", "--database", "auth"]), { command: "status", database: "auth", actor: undefined, expectedFingerprint: undefined });
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
  writeFileSync(join(directory, "20260718120000_first.sql"), "SELECT 1;");
  writeFileSync(join(directory, "20260718120001_second_step.sql"), "SELECT 1;");
  assert.deepEqual(validateMigrationFiles(directory), ["20260718120000_first.sql", "20260718120001_second_step.sql"]);
  const invalid = join(directory, "invalid");
  mkdirSync(invalid);
  writeFileSync(join(invalid, "1_bad.sql"), "SELECT 1;");
  assert.throws(() => validateMigrationFiles(invalid), /invalid migration filename/);
  writeFileSync(join(invalid, "20260718120002__double.sql"), "SELECT 1;");
  assert.throws(() => validateMigrationFiles(invalid), /invalid migration filename/);
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
  const report = executeDatabase("up", authDatabase, "deploy", {
    environment: testEnvironment,
    resolveSqlxBinary: () => ({ binary: "sqlx.exe", version: "0.8.6" }),
    run(command, args, environment) {
      calls.push({ command, args, environment });
      if (command === "psql" && args.includes("--tuples-only")) return { status: 0, output: "t,f" };
      if (command === "sqlx.exe" && args[0] === "--version") return { status: 0, output: "sqlx-cli 0.8.6" };
      if (command === "sqlx.exe" && args[1] === "info") return { status: 0, output: "migration info" };
      if (command === "sqlx.exe" && args[1] === "run") return { status: 0, output: "migration run" };
      if (command === "psql") return { status: 0, output: "" };
      throw new Error(`unexpected command: ${command}`);
    }
  });
  assert.equal(report.ok, true);
  const psqlCalls = calls.filter(({ command }) => command === "psql");
  assert.equal(psqlCalls.length, 2);
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
    assert.equal(environment.PGAPPNAME, "db-test");
  }
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
      if (args.some((argument) => String(argument).includes("_myserver_migration_audit"))) return { status: 1, output: "permission denied" };
      return { status: 0, output: "t,f" };
    }
  });
  assert.equal(report.ok, false);
  assert.equal(report.code, EXIT.EXECUTION);
  assert.match(report.error, /audit write failed/);
  assert.equal(calls.some(([command, args]) => command === "sqlx.exe" && args.includes("run")), true);
});
