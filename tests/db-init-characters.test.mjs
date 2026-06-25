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

test("characters table enforces character_id uniqueness and affinity total only", () => {
  assert.notEqual(charactersTableMatch, null);
  const tableSql = charactersTableMatch[0];
  const compactTableSql = compact(tableSql);

  assert.match(tableSql, /CONSTRAINT uk_characters_character_id UNIQUE \(character_id\)/);
  assert.equal(/UNIQUE\s*\(\s*world_id\s*,\s*name\s*\)/i.test(tableSql), false);
  assert.match(
    compactTableSql,
    /CONSTRAINT ck_characters_affinity_total CHECK \( affinity_earth \+ affinity_fire \+ affinity_water \+ affinity_wind = 10000 \)/
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
