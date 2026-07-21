import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import test from "node:test";

import pg from "pg";

import { executeDatabase, EXIT } from "../../tools/db.js";

const { Client } = pg;
const runPostgres = process.env.MYSERVER_STAGE4_RUN_POSTGRES === "1";
const host = process.env.MYSERVER_STAGE4_POSTGRES_HOST || "localhost";
const port = Number(process.env.MYSERVER_STAGE4_POSTGRES_PORT || "5432");
const user = process.env.MYSERVER_STAGE4_POSTGRES_USER || "postgres";
const password = process.env.MYSERVER_STAGE4_POSTGRES_PASSWORD;

function quoteIdentifier(identifier) {
  if (!/^myserver_stage4_[a-z0-9_]+$/.test(identifier)) throw new Error("stage 4 temporary database name is invalid");
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
  if (!/^myserver_stage4_[a-z0-9_]+$/.test(database)) return;
  await withClient("postgres", async (client) => {
    await client.query(
      "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = $1 AND pid <> pg_backend_pid()",
      [database]
    );
    await client.query(`DROP DATABASE IF EXISTS ${quoteIdentifier(database)}`);
  });
}

test("stage 4 rollout keeps old callers compatible through expand and restores contract data", {
  skip: runPostgres ? false : "set MYSERVER_STAGE4_RUN_POSTGRES=1 with local PostgreSQL credentials to run",
  timeout: 120000
}, async () => {
  assert.ok(password, "MYSERVER_STAGE4_POSTGRES_PASSWORD is required for the PostgreSQL rollout exercise");
  const database = `myserver_stage4_rollout_${process.pid}_${randomUUID().replaceAll("-", "").slice(0, 12)}`;
  const environment = {
    ...process.env,
    MYSERVER_STAGE4_ROLLOUT_URL: `postgresql://${encodeURIComponent(user)}@${host}:${port}/${database}?sslmode=disable`,
    MYSERVER_STAGE4_ROLLOUT_PASSWORD: password
  };
  const databaseConfig = {
    key: "stage4-rollout",
    defaultDatabase: database,
    logicalOwner: "stage4-rollout-test",
    migrationDirectory: "tests/fixtures/db/stage4-rollout/legacy",
    urlEnvironment: "MYSERVER_STAGE4_ROLLOUT_URL",
    userEnvironment: "MYSERVER_STAGE4_ROLLOUT_USER",
    passwordEnvironment: "MYSERVER_STAGE4_ROLLOUT_PASSWORD"
  };
  let created = false;

  try {
    await withClient("postgres", async (client) => {
      await client.query(`CREATE DATABASE ${quoteIdentifier(database)}`);
    });
    created = true;

    let report = executeDatabase("up", databaseConfig, "stage4-rollout-test", { environment });
    assert.equal(report.code, EXIT.OK, report.error);

    await withClient(database, async (client) => {
      await client.query("INSERT INTO stage4_rollout_accounts (legacy_name) VALUES ($1)", ["old-before-expand"]);
    });

    databaseConfig.migrationDirectory = "tests/fixtures/db/stage4-rollout/expand";
    report = executeDatabase("up", databaseConfig, "stage4-rollout-test", { environment });
    assert.equal(report.code, EXIT.OK, report.error);

    await withClient(database, async (client) => {
      const oldRead = await client.query("SELECT legacy_name FROM stage4_rollout_accounts WHERE legacy_name = $1", ["old-before-expand"]);
      assert.equal(oldRead.rowCount, 1);
      await client.query("INSERT INTO stage4_rollout_accounts (legacy_name) VALUES ($1)", ["old-after-expand"]);
      await client.query("INSERT INTO stage4_rollout_accounts (legacy_name, display_name) VALUES ($1, $2)", ["dual-write", "dual-write"]);
      const newRead = await client.query("SELECT display_name FROM stage4_rollout_accounts WHERE legacy_name = $1", ["dual-write"]);
      assert.equal(newRead.rows[0]?.display_name, "dual-write");
      const index = await client.query("SELECT i.indisvalid FROM pg_index AS i JOIN pg_class AS c ON c.oid = i.indexrelid WHERE c.relname = 'idx_stage4_rollout_display_name'");
      assert.equal(index.rows[0]?.indisvalid, true);
      await client.query("CREATE TABLE stage4_rollout_accounts_backup AS TABLE stage4_rollout_accounts");
    });

    databaseConfig.migrationDirectory = "tests/fixtures/db/stage4-rollout/contract";
    report = executeDatabase("up", databaseConfig, "stage4-rollout-test", { environment });
    assert.equal(report.code, EXIT.OK, report.error);

    await withClient(database, async (client) => {
      await assert.rejects(
        () => client.query("SELECT legacy_name FROM stage4_rollout_accounts"),
        (error) => error?.code === "42703"
      );
      await client.query("ALTER TABLE stage4_rollout_accounts ADD COLUMN legacy_name text");
      await client.query("UPDATE stage4_rollout_accounts AS target SET legacy_name = backup.legacy_name FROM stage4_rollout_accounts_backup AS backup WHERE backup.id = target.id");
      await client.query("ALTER TABLE stage4_rollout_accounts ALTER COLUMN legacy_name SET NOT NULL");
      await client.query("INSERT INTO stage4_rollout_accounts (legacy_name, display_name) VALUES ($1, $2)", ["old-after-recovery", "recovered"]);
      const recovered = await client.query("SELECT legacy_name FROM stage4_rollout_accounts WHERE legacy_name = $1", ["old-after-recovery"]);
      assert.equal(recovered.rowCount, 1);
    });
  } finally {
    if (created) await dropTemporaryDatabase(database);
  }
});
