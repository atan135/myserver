import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const migrationPath = "db/migrations/game/20260821120000_add_activity_schema.sql";
const migration = readFileSync(migrationPath, "utf8");
const init = readFileSync("db/init.sql", "utf8");

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
