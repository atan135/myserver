import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { basename, extname, join } from "node:path";
import test from "node:test";

const migrationPath = "db/migrations/game/20260821120000_add_activity_schema.sql";
const migration = readFileSync(migrationPath, "utf8");
const recoveryMigration = readFileSync(
  "db/migrations/game/20260822130000_add_activity_recovery_runtime.sql",
  "utf8"
);
const controlMetadataMigration = readFileSync(
  "db/migrations/game/20260822150000_add_activity_control_metadata.sql",
  "utf8"
);
const init = readFileSync("db/init.sql", "utf8");
const deployGate = JSON.parse(readFileSync("db/config/deploy-gate.json", "utf8"));

function activityDdl(source) {
  const start = source.indexOf("CREATE TABLE IF NOT EXISTS activity (");
  const endMarker = "REVOKE UPDATE, DELETE, TRUNCATE ON activity_audit_log FROM PUBLIC;";
  const end = source.indexOf(endMarker, start);
  assert.ok(start >= 0 && end >= 0, "activity DDL section must be present");
  return source
    .slice(start, end + endMarker.length)
    .replace(/\r/g, "")
    .replace(/--.*$/gm, "")
    .replace(/\s+/g, "");
}

test("activity migration and compatibility init contain the same DDL", () => {
  assert.equal(activityDdl(migration), activityDdl(init));
});

test("activity schema protects semantic uniqueness and immutable versions", () => {
  for (const table of [
    "activity",
    "activity_version",
    "activity_reward_group",
    "activity_reward_item",
    "activity_stage",
    "player_activity_state",
    "activity_claim_record",
    "activity_draw_result",
    "reward_grant_ledger",
    "activity_audit_log"
  ]) {
    assert.match(migration, new RegExp(`CREATE TABLE IF NOT EXISTS ${table} \\(`));
  }
  for (const constraint of [
    "uk_activity_version_activity_version",
    "uk_activity_reward_group_key",
    "uk_activity_stage_id",
    "uk_player_activity_state_owner_version",
    "uk_activity_claim_semantic",
    "uk_activity_claim_client_request"
  ]) {
    assert.match(migration, new RegExp(`(?:CONSTRAINT|CREATE UNIQUE INDEX IF NOT EXISTS) ${constraint}\\b`));
  }
  assert.match(migration, /CREATE TRIGGER trg_activity_version_immutable/);
  assert.match(migration, /CREATE TRIGGER trg_activity_reward_group_immutable/);
  assert.match(migration, /CREATE TRIGGER trg_activity_reward_item_immutable/);
  assert.match(migration, /CREATE TRIGGER trg_activity_stage_immutable/);
});

test("activity audit and reward ledgers are append-only with controlled insert shape", () => {
  assert.match(migration, /event_type IN \('draft_created', 'draft_updated', 'published', 'offlined', 'archived', 'config_changed', 'reward_changed'\)/);
  assert.match(migration, /actor_type IN \('admin', 'system'\)/);
  assert.match(migration, /CREATE TRIGGER trg_reward_grant_ledger_no_truncate/);
  assert.match(migration, /CREATE TRIGGER trg_activity_audit_log_no_truncate/);
  assert.match(migration, /REVOKE UPDATE, DELETE, TRUNCATE ON reward_grant_ledger FROM PUBLIC/);
  assert.match(migration, /REVOKE UPDATE, DELETE, TRUNCATE ON activity_audit_log FROM PUBLIC/);
});

test("activity schema records versioned configuration, recovery and audit evidence", () => {
  for (const fragment of [
    "config_digest varchar(71) NOT NULL",
    "type_config_json jsonb NOT NULL",
    "published_at timestamptz NULL",
    "published_by varchar(128) NULL",
    "semantic_claim_key varchar(256) NOT NULL",
    "client_request_id varchar(128) NULL",
    "reward_snapshot_json jsonb NOT NULL",
    "cost_snapshot_json jsonb NOT NULL",
    "last_retry_at timestamptz NULL",
    "details_json jsonb NOT NULL"
  ]) {
    assert.ok(migration.includes(fragment), `activity migration must contain ${fragment}`);
  }
  for (const index of [
    "idx_activity_status_window",
    "idx_activity_type_status",
    "idx_activity_claim_owner_activity",
    "idx_activity_claim_stage_period",
    "idx_reward_grant_ledger_activity",
    "idx_activity_audit_activity"
  ]) {
    assert.match(migration, new RegExp(`CREATE (?:UNIQUE )?INDEX IF NOT EXISTS ${index}\\b`));
  }
});

test("activity recovery migration persists replay, retry and manual-review state", () => {
  for (const fragment of [
    "reward_request_id varchar(128) NULL",
    "runtime_key varchar(320) NULL",
    "order_snapshot_json jsonb NOT NULL",
    "result_json jsonb NOT NULL",
    "notification_failed boolean NOT NULL",
    "attempt_count integer NOT NULL",
    "updated_at timestamptz NOT NULL"
  ]) {
    assert.ok(recoveryMigration.includes(fragment), `activity recovery migration must contain ${fragment}`);
  }
  for (const status of ["reconciliation_pending", "blocked_capacity", "manual_review"]) {
    assert.ok(recoveryMigration.includes(`'${status}'`), `activity recovery migration must support ${status}`);
  }
  for (const index of [
    "idx_activity_claim_recovery",
    "uk_activity_claim_runtime_key",
    "uk_activity_claim_active_draw",
    "idx_reward_mail_outbox_dispatch",
    "idx_activity_claim_review_lookup",
    "uk_activity_claim_review_request_reason"
  ]) {
    assert.match(recoveryMigration, new RegExp(`CREATE (?:UNIQUE )?INDEX IF NOT EXISTS ${index}\\b`));
  }
  assert.match(recoveryMigration, /CREATE TABLE IF NOT EXISTS activity_claim_review \(/);
});

test("activity control metadata migration records display and audit evidence", () => {
  assert.match(controlMetadataMigration, /ADD COLUMN IF NOT EXISTS change_reason varchar\(512\) NOT NULL/);
  assert.match(controlMetadataMigration, /ADD COLUMN IF NOT EXISTS display_json jsonb NOT NULL/);
  assert.match(controlMetadataMigration, /ADD CONSTRAINT ck_activity_stage_display CHECK \(jsonb_typeof\(display_json\) = 'object'\)/);
  assert.match(controlMetadataMigration, /DROP INDEX IF EXISTS uk_activity_version_digest/);
  assert.match(controlMetadataMigration, /CREATE INDEX IF NOT EXISTS idx_activity_version_digest\b/);
  assert.doesNotMatch(controlMetadataMigration, /CREATE UNIQUE INDEX IF NOT EXISTS idx_activity_version_digest\b/);
  for (const eventType of ["draft_forked", "preflight", "records_read"]) {
    assert.ok(controlMetadataMigration.includes(`'${eventType}'`), `activity audit must support ${eventType}`);
  }
});

test("game database deployment gate admits and probes all activity migrations", () => {
  assert.ok(deployGate.databases.game.keyTables.includes("public.activity"));
  for (const service of deployGate.databases.game.services) {
    assert.ok(
      service.maximumMigrationVersion >= "20260822150000",
      `${service.name} must admit the latest activity migration`
    );
  }
});

test("each activity type has one sibling implementation file in every application layer", () => {
  const expected = ["login_reward", "lottery"];
  const layers = [
    ["apps/game-server/src/activity/types", ".rs"],
    ["apps/admin-api/src/modules/activity/types", ".ts"],
    ["apps/admin-web/src/modules/activity/types", ".ts"]
  ];
  for (const [directory, extension] of layers) {
    const typeFiles = readdirSync(directory)
      .filter((file) => extname(file) === extension && file !== "mod.rs")
      .map((file) => basename(file, extension))
      .sort();
    assert.deepEqual(typeFiles, expected, `${directory} must keep one sibling file per registered type`);
  }

  const rustRegistry = readFileSync(join("apps/game-server/src/activity/types/mod.rs"), "utf8");
  assert.match(rustRegistry, /mod login_reward;/);
  assert.match(rustRegistry, /mod lottery;/);
  const webRegistry = readFileSync(join("apps/admin-web/src/modules/activity/type-registry.js"), "utf8");
  assert.match(webRegistry, /\.\/types\/login_reward\.ts/);
  assert.match(webRegistry, /\.\/types\/lottery\.ts/);

  const publicView = readFileSync(join("apps/admin-web/src/views/Activities.vue"), "utf8");
  assert.doesNotMatch(publicView, /\b(?:login_reward|lottery)\b/);
});
