import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";

const initSql = fs.readFileSync("db/init.sql", "utf8");

function sectionBetween(startMarker, endMarker) {
  const start = initSql.indexOf(startMarker);
  assert.notEqual(start, -1, `${startMarker} should exist in db/init.sql`);
  const end = initSql.indexOf(endMarker, start + startMarker.length);
  assert.notEqual(end, -1, `${endMarker} should exist after ${startMarker}`);
  return initSql.slice(start, end);
}

function compact(sql) {
  return sql.replace(/\s+/g, " ").trim();
}

const gameSection = sectionBetween("\\connect myserver_game", "\\connect myserver_chat");
const charactersTableMatch = gameSection.match(
  /CREATE TABLE IF NOT EXISTS characters \([\s\S]*?\n\);/
);
const connectionAuditTableMatch = gameSection.match(
  /CREATE TABLE IF NOT EXISTS game_connection_audit_logs \([\s\S]*?\n\);/
);
const roomEventTableMatch = gameSection.match(
  /CREATE TABLE IF NOT EXISTS room_event_logs \([\s\S]*?\n\);/
);
const characterElementLogsTableMatch = gameSection.match(
  /CREATE TABLE IF NOT EXISTS character_element_logs \([\s\S]*?\n\);/
);
const characterDisciplinesTableMatch = gameSection.match(
  /CREATE TABLE IF NOT EXISTS character_disciplines \([\s\S]*?\n\);/
);
const characterDisciplineLogsTableMatch = gameSection.match(
  /CREATE TABLE IF NOT EXISTS character_discipline_logs \([\s\S]*?\n\);/
);
const characterTitlesTableMatch = gameSection.match(
  /CREATE TABLE IF NOT EXISTS character_titles \([\s\S]*?\n\);/
);
const characterTitleLogsTableMatch = gameSection.match(
  /CREATE TABLE IF NOT EXISTS character_title_logs \([\s\S]*?\n\);/
);
const characterInventoryTableMatch = gameSection.match(
  /CREATE TABLE IF NOT EXISTS character_inventory \([\s\S]*?\n\);/
);
const characterInventoryGrantsTableMatch = gameSection.match(
  /CREATE TABLE IF NOT EXISTS character_inventory_grants \([\s\S]*?\n\);/
);

test("db init creates characters table in the game database section", () => {
  assert.notEqual(charactersTableMatch, null, "characters table should be created in myserver_game");

  const beforeGameSection = initSql.slice(0, initSql.indexOf("\\connect myserver_game"));
  assert.equal(
    /CREATE TABLE IF NOT EXISTS characters \(/.test(beforeGameSection),
    false,
    "characters table should not be created before myserver_game"
  );
});

test("characters table contains P0 identity split base fields and defaults", () => {
  assert.notEqual(charactersTableMatch, null);
  const tableSql = charactersTableMatch[0];

  for (const pattern of [
    /character_id varchar\(64\) NOT NULL/,
    /account_player_id varchar\(64\) NOT NULL/,
    /world_id bigint NOT NULL DEFAULT 0/,
    /name varchar\(64\) NOT NULL/,
    /status varchar\(32\) NOT NULL DEFAULT 'active'/,
    /appearance_json jsonb NOT NULL/,
    /scene_id integer NOT NULL/,
    /x real NOT NULL DEFAULT 0/,
    /y real NOT NULL DEFAULT 0/,
    /dir_x real NOT NULL DEFAULT 0/,
    /dir_y real NOT NULL DEFAULT 1/,
    /affinity_earth integer NOT NULL DEFAULT 2500/,
    /affinity_fire integer NOT NULL DEFAULT 2500/,
    /affinity_water integer NOT NULL DEFAULT 2500/,
    /affinity_wind integer NOT NULL DEFAULT 2500/,
    /mastery_earth integer NOT NULL DEFAULT 0/,
    /mastery_fire integer NOT NULL DEFAULT 0/,
    /mastery_water integer NOT NULL DEFAULT 0/,
    /mastery_wind integer NOT NULL DEFAULT 0/,
    /created_at timestamptz NOT NULL DEFAULT current_timestamp/,
    /last_login_at timestamptz NULL/,
    /deleted_at timestamptz NULL/
  ]) {
    assert.match(tableSql, pattern);
  }
});

test("characters table enforces character_id uniqueness and element value boundaries", () => {
  assert.notEqual(charactersTableMatch, null);
  const tableSql = charactersTableMatch[0];
  const compactTableSql = compact(tableSql);

  assert.match(tableSql, /CONSTRAINT uk_characters_character_id UNIQUE \(character_id\)/);
  assert.equal(/UNIQUE\s*\(\s*world_id\s*,\s*name\s*\)/i.test(tableSql), false);
  assert.match(
    compactTableSql,
    /CONSTRAINT ck_characters_affinity_non_negative CHECK \( affinity_earth >= 0 AND affinity_fire >= 0 AND affinity_water >= 0 AND affinity_wind >= 0 \)/
  );
  assert.match(
    compactTableSql,
    /CONSTRAINT ck_characters_affinity_total CHECK \( affinity_earth \+ affinity_fire \+ affinity_water \+ affinity_wind = 10000 \)/
  );
  assert.match(
    compactTableSql,
    /CONSTRAINT ck_characters_mastery_non_negative CHECK \( mastery_earth >= 0 AND mastery_fire >= 0 AND mastery_water >= 0 AND mastery_wind >= 0 \)/
  );
});

test("characters table has lookup indexes required by P0", () => {
  for (const [indexName, columnName] of [
    ["idx_characters_account_player_id", "account_player_id"],
    ["idx_characters_world_id", "world_id"],
    ["idx_characters_status", "status"],
    ["idx_characters_created_at", "created_at"]
  ]) {
    assert.match(
      gameSection,
      new RegExp(`CREATE INDEX IF NOT EXISTS ${indexName}\\s+ON characters \\(${columnName}\\);`)
    );
  }
});

test("character element logs capture P1 source, operator, deltas, snapshots, and reason", () => {
  assert.notEqual(
    characterElementLogsTableMatch,
    null,
    "character_element_logs table should be created in myserver_game"
  );
  const tableSql = characterElementLogsTableMatch[0];

  for (const pattern of [
    /character_id varchar\(64\) NOT NULL/,
    /source_type varchar\(32\) NOT NULL/,
    /source_id varchar\(128\) NULL/,
    /operator_type varchar\(32\) NULL/,
    /operator_id varchar\(128\) NULL/,
    /affinity_earth_delta integer NOT NULL DEFAULT 0/,
    /affinity_fire_delta integer NOT NULL DEFAULT 0/,
    /affinity_water_delta integer NOT NULL DEFAULT 0/,
    /affinity_wind_delta integer NOT NULL DEFAULT 0/,
    /mastery_earth_delta integer NOT NULL DEFAULT 0/,
    /mastery_fire_delta integer NOT NULL DEFAULT 0/,
    /mastery_water_delta integer NOT NULL DEFAULT 0/,
    /mastery_wind_delta integer NOT NULL DEFAULT 0/,
    /before_json jsonb NOT NULL/,
    /after_json jsonb NOT NULL/,
    /reason varchar\(255\) NULL/,
    /created_at timestamptz NOT NULL DEFAULT current_timestamp/
  ]) {
    assert.match(tableSql, pattern);
  }

  assert.match(
    compact(tableSql),
    /CONSTRAINT fk_character_element_logs_character_id FOREIGN KEY \(character_id\) REFERENCES characters\(character_id\)/
  );
});

test("character element logs have lookup and reverse-time indexes", () => {
  for (const [indexName, indexColumns] of [
    ["idx_character_element_logs_character_id", "character_id"],
    ["idx_character_element_logs_created_at_desc", "created_at DESC"],
    ["idx_character_element_logs_character_created_at_desc", "character_id, created_at DESC"],
    ["idx_character_element_logs_source", "source_type, source_id"]
  ]) {
    assert.match(
      gameSection,
      new RegExp(
        `CREATE INDEX IF NOT EXISTS ${indexName}\\s+ON character_element_logs \\(${indexColumns}\\);`
      )
    );
  }
});

test("character disciplines table captures P2 discipline tier state", () => {
  assert.notEqual(
    characterDisciplinesTableMatch,
    null,
    "character_disciplines table should be created in myserver_game"
  );
  const tableSql = characterDisciplinesTableMatch[0];

  for (const pattern of [
    /id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY/,
    /character_id varchar\(64\) NOT NULL/,
    /discipline_id varchar\(64\) NOT NULL/,
    /points bigint NOT NULL DEFAULT 0 CHECK \(points >= 0\)/,
    /tier varchar\(32\) NOT NULL DEFAULT 'novice'/,
    /active boolean NOT NULL DEFAULT false/,
    /learned_at timestamptz NOT NULL DEFAULT current_timestamp/,
    /updated_at timestamptz NOT NULL DEFAULT current_timestamp/,
    /CONSTRAINT uk_character_disciplines_discipline UNIQUE \(character_id, discipline_id\)/
  ]) {
    assert.match(tableSql, pattern);
  }

  assert.match(
    compact(tableSql),
    /CONSTRAINT fk_character_disciplines_character_id FOREIGN KEY \(character_id\) REFERENCES characters\(character_id\)/
  );
});

test("character disciplines table has lookup indexes and update trigger", () => {
  for (const [indexName, indexColumns] of [
    ["idx_character_disciplines_character_id", "character_id"],
    ["idx_character_disciplines_character_active", "character_id, active"],
    ["idx_character_disciplines_updated_at", "updated_at"]
  ]) {
    assert.match(
      gameSection,
      new RegExp(
        `CREATE INDEX IF NOT EXISTS ${indexName}\\s+ON character_disciplines \\(${indexColumns}\\);`
      )
    );
  }

  assert.match(
    gameSection,
    /CREATE TRIGGER trg_character_disciplines_updated_at\s+BEFORE UPDATE ON character_disciplines[\s\S]*?EXECUTE FUNCTION set_current_timestamp_updated_at\(\);/
  );
});

test("character discipline logs capture source, operator, action, snapshots, and reason", () => {
  assert.notEqual(
    characterDisciplineLogsTableMatch,
    null,
    "character_discipline_logs table should be created in myserver_game"
  );
  const tableSql = characterDisciplineLogsTableMatch[0];

  for (const pattern of [
    /id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY/,
    /character_id varchar\(64\) NOT NULL/,
    /discipline_id varchar\(64\) NOT NULL/,
    /action varchar\(32\) NOT NULL/,
    /source_type varchar\(32\) NULL/,
    /source_id varchar\(128\) NULL/,
    /operator_type varchar\(32\) NULL/,
    /operator_id varchar\(128\) NULL/,
    /before_json jsonb NULL/,
    /after_json jsonb NULL/,
    /reason varchar\(255\) NULL/,
    /created_at timestamptz NOT NULL DEFAULT current_timestamp/
  ]) {
    assert.match(tableSql, pattern);
  }

  assert.match(
    compact(tableSql),
    /CONSTRAINT fk_character_discipline_logs_character_id FOREIGN KEY \(character_id\) REFERENCES characters\(character_id\)/
  );
});

test("character discipline logs have reverse-time, discipline, and source indexes", () => {
  for (const [indexName, indexColumns] of [
    ["idx_character_discipline_logs_character_id", "character_id"],
    ["idx_character_discipline_logs_created_at_desc", "created_at DESC"],
    ["idx_character_discipline_logs_character_created_at_desc", "character_id, created_at DESC"],
    ["idx_character_discipline_logs_discipline_id", "discipline_id"],
    ["idx_character_discipline_logs_source", "source_type, source_id"]
  ]) {
    assert.match(
      gameSection,
      new RegExp(
        `CREATE INDEX IF NOT EXISTS ${indexName}\\s+ON character_discipline_logs \\(${indexColumns}\\);`
      )
    );
  }
});

test("character titles table captures P2 ownership and one-equipped service boundary", () => {
  assert.notEqual(
    characterTitlesTableMatch,
    null,
    "character_titles table should be created in myserver_game"
  );
  const tableSql = characterTitlesTableMatch[0];

  for (const pattern of [
    /id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY/,
    /character_id varchar\(64\) NOT NULL/,
    /title_id varchar\(64\) NOT NULL/,
    /source_type varchar\(32\) NOT NULL/,
    /source_id varchar\(128\) NULL/,
    /is_equipped boolean NOT NULL DEFAULT false/,
    /unlocked_at timestamptz NOT NULL DEFAULT current_timestamp/,
    /expires_at timestamptz NULL/,
    /created_at timestamptz NOT NULL DEFAULT current_timestamp/,
    /updated_at timestamptz NOT NULL DEFAULT current_timestamp/,
    /CONSTRAINT uk_character_titles_title UNIQUE \(character_id, title_id\)/
  ]) {
    assert.match(tableSql, pattern);
  }

  assert.match(
    compact(tableSql),
    /CONSTRAINT fk_character_titles_character_id FOREIGN KEY \(character_id\) REFERENCES characters\(character_id\)/
  );
  assert.match(
    gameSection,
    /Equip only one main display title by service transaction: unequip old title, then equip the new title/
  );
});

test("character titles table has ownership, equipped, expiry indexes and update trigger", () => {
  for (const [indexName, indexColumns] of [
    ["idx_character_titles_character_id", "character_id"],
    ["idx_character_titles_character_equipped", "character_id, is_equipped"],
    ["idx_character_titles_is_equipped", "is_equipped"],
    ["idx_character_titles_expires_at", "expires_at"]
  ]) {
    assert.match(
      gameSection,
      new RegExp(`CREATE INDEX IF NOT EXISTS ${indexName}\\s+ON character_titles \\(${indexColumns}\\);`)
    );
  }

  assert.match(
    gameSection,
    /CREATE TRIGGER trg_character_titles_updated_at\s+BEFORE UPDATE ON character_titles[\s\S]*?EXECUTE FUNCTION set_current_timestamp_updated_at\(\);/
  );
});

test("character title logs capture P2 title audit context and snapshots", () => {
  assert.notEqual(
    characterTitleLogsTableMatch,
    null,
    "character_title_logs table should be created in myserver_game"
  );
  const tableSql = characterTitleLogsTableMatch[0];

  for (const pattern of [
    /id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY/,
    /character_id varchar\(64\) NOT NULL/,
    /title_id varchar\(64\) NOT NULL/,
    /action varchar\(32\) NOT NULL/,
    /source_type varchar\(32\) NULL/,
    /source_id varchar\(128\) NULL/,
    /operator_type varchar\(32\) NULL/,
    /operator_id varchar\(128\) NULL/,
    /before_json jsonb NULL/,
    /after_json jsonb NULL/,
    /reason varchar\(255\) NULL/,
    /created_at timestamptz NOT NULL DEFAULT current_timestamp/
  ]) {
    assert.match(tableSql, pattern);
  }

  assert.match(
    compact(tableSql),
    /CONSTRAINT fk_character_title_logs_character_id FOREIGN KEY \(character_id\) REFERENCES characters\(character_id\)/
  );
});

test("character title logs have reverse-time and title lookup indexes", () => {
  for (const [indexName, indexColumns] of [
    ["idx_character_title_logs_character_id", "character_id"],
    ["idx_character_title_logs_created_at_desc", "created_at DESC"],
    ["idx_character_title_logs_character_created_at_desc", "character_id, created_at DESC"],
    ["idx_character_title_logs_title_id", "title_id"]
  ]) {
    assert.match(
      gameSection,
      new RegExp(
        `CREATE INDEX IF NOT EXISTS ${indexName}\\s+ON character_title_logs \\(${indexColumns}\\);`
      )
    );
  }
});

test("character inventory table is character-scoped and has no legacy level column", () => {
  assert.notEqual(
    characterInventoryTableMatch,
    null,
    "character_inventory table should be created in myserver_game"
  );
  const tableSql = characterInventoryTableMatch[0];

  for (const pattern of [
    /character_id varchar\(64\) NOT NULL/,
    /hp bigint NOT NULL DEFAULT 0/,
    /inventory_data jsonb NOT NULL/,
    /warehouse_data jsonb NOT NULL/,
    /equipment_data jsonb NOT NULL/,
    /attr_base_data jsonb NOT NULL/,
    /visual_data jsonb NOT NULL/,
    /buffs_data jsonb NOT NULL/,
    /CONSTRAINT uk_character_inventory_character_id UNIQUE \(character_id\)/
  ]) {
    assert.match(tableSql, pattern);
  }

  assert.equal(/\blevel\b/i.test(tableSql), false);
  assert.equal(/\bplayer_id\b/i.test(tableSql), false);
  assert.equal(/\baccount_player_id\b/i.test(tableSql), false);
});

test("character inventory grants are idempotent per request and target character ids", () => {
  assert.notEqual(
    characterInventoryGrantsTableMatch,
    null,
    "character_inventory_grants table should be created in myserver_game"
  );
  const tableSql = characterInventoryGrantsTableMatch[0];

  for (const pattern of [
    /request_id varchar\(128\) NOT NULL/,
    /character_id varchar\(64\) NOT NULL/,
    /source varchar\(64\) NOT NULL/,
    /items_json jsonb NOT NULL/,
    /CONSTRAINT uk_character_inventory_grants_request_id UNIQUE \(request_id\)/
  ]) {
    assert.match(tableSql, pattern);
  }

  assert.match(
    gameSection,
    /CREATE INDEX IF NOT EXISTS idx_character_inventory_grants_character_id\s+ON character_inventory_grants \(character_id\);/
  );
});

test("game audit tables directly include account and character identity fields", () => {
  assert.notEqual(
    connectionAuditTableMatch,
    null,
    "game_connection_audit_logs table should be created in myserver_game"
  );
  assert.notEqual(roomEventTableMatch, null, "room_event_logs table should be created in myserver_game");

  for (const tableSql of [connectionAuditTableMatch[0], roomEventTableMatch[0]]) {
    assert.match(tableSql, /account_player_id varchar\(64\) NULL/);
    assert.match(tableSql, /character_id varchar\(64\) NULL/);
  }
});

test("game audit tables have account and character lookup indexes", () => {
  for (const [indexName, tableName, columnName] of [
    [
      "idx_game_connection_audit_logs_account_player_id",
      "game_connection_audit_logs",
      "account_player_id"
    ],
    ["idx_game_connection_audit_logs_character_id", "game_connection_audit_logs", "character_id"],
    ["idx_room_event_logs_account_player_id", "room_event_logs", "account_player_id"],
    ["idx_room_event_logs_character_id", "room_event_logs", "character_id"]
  ]) {
    assert.match(
      gameSection,
      new RegExp(`CREATE INDEX IF NOT EXISTS ${indexName}\\s+ON ${tableName} \\(${columnName}\\);`)
    );
  }
});
