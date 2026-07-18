import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdtempSync, mkdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { EXIT, classifyFailure, executeDatabase, parseArguments, redact, resolveDatabases, resolveSqlxBinary, validateMigrationFiles } from "../../tools/db.js";

const authDatabase = {
  key: "auth",
  defaultDatabase: "myserver_auth",
  migrationDirectory: "db/migrations/auth",
  urlEnvironment: "TEST_DATABASE_URL",
  userEnvironment: "TEST_DATABASE_USER",
  passwordEnvironment: "TEST_DATABASE_PASSWORD"
};

const testEnvironment = { TEST_DATABASE_URL: "postgres://migration:secret@example.test/myserver_auth" };

function withoutMigrationUrls() {
  const environment = { ...process.env };
  for (const key of Object.keys(environment)) {
    if (key.startsWith("MYSERVER_DB_MIGRATION_")) delete environment[key];
  }
  return environment;
}

test("database CLI accepts only supported commands and options", () => {
  assert.deepEqual(parseArguments(["status", "--database", "auth"]), { command: "status", database: "auth", actor: undefined });
  assert.throws(() => parseArguments(["baseline", "--database", "auth"]), /usage/);
  assert.throws(() => parseArguments(["status", "--database", "auth", "--force", "yes"]), /only --database/);
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
