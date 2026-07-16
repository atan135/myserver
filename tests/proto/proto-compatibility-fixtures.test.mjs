import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  DEFAULT_FIXTURE_DIRECTORY,
  DEFAULT_MANIFEST_PATH,
  scanFixtureContent,
  validateProtoCompatibilityFixtures
} from "../../tools/check-proto-compatibility-fixtures.js";
import { decodeByMessageType } from "../../tools/mock-client/src/messages.js";
import { decodeMovementSnapshotV1 } from "./fixtures/compatibility/legacy-movement-snapshot-v1.mjs";

const manifest = JSON.parse(readFileSync(DEFAULT_MANIFEST_PATH, "utf8"));
const fixturesByFile = new Map(manifest.fixtures.map((fixture) => [fixture.file, fixture]));

function readFixture(file) {
  return readFileSync(path.join(DEFAULT_FIXTURE_DIRECTORY, file));
}

test("binary protobuf fixtures are complete, bounded, and synthetic", () => {
  const result = validateProtoCompatibilityFixtures();
  assert.deepEqual(result.diagnostics, []);
  assert.equal(result.fixtures.length, 6);
});

test("current mock-client decodes historical fixture bodies", () => {
  for (const fixture of manifest.fixtures) {
    const decoded = decodeByMessageType(fixture.messageType, readFixture(fixture.file));
    if (fixture.expectations.kind === "exact") {
      assert.deepEqual(decoded, fixture.expectations.decoded, fixture.file);
      continue;
    }

    assert.equal(fixture.expectations.kind, "large_payload", fixture.file);
    assert.equal(decoded.event, fixture.expectations.decoded.event);
    assert.equal(decoded.roomId, fixture.expectations.decoded.roomId);
    assert.equal(decoded.characterId, fixture.expectations.decoded.characterId);
    assert.equal(decoded.action, fixture.expectations.decoded.action);
    assert.equal(Buffer.byteLength(decoded.payloadJson), fixture.expectations.decoded.payloadUtf8Bytes);
    assert.ok(decoded.payloadJson.startsWith(fixture.expectations.decoded.payloadPrefix));
    assert.ok(decoded.payloadJson.endsWith(fixture.expectations.decoded.payloadSuffix));
    assert.match(decoded.payloadJson, /^\{"fixture":"x+"\}$/);
  }
});

test("historical v1 projection ignores future protobuf fields and unknown enum values", () => {
  const oldBody = readFixture("movement-snapshot-v1.bin");
  const futureBody = readFixture("movement-snapshot-future-fields.bin");
  const oldProjection = decodeMovementSnapshotV1(oldBody);

  assert.deepEqual(decodeMovementSnapshotV1(futureBody), oldProjection);
  assert.deepEqual(oldProjection, {
    roomId: "fixture_room_legacy",
    frameId: 0xffff_ffff,
    entities: [{
      entityId: Number.MAX_SAFE_INTEGER,
      characterId: "fixture_character_legacy",
      sceneId: 7,
      x: 1.25,
      y: -2.5,
      dirX: 0,
      dirY: 0,
      moving: true,
      lastInputFrame: 0xffff_ffff
    }],
    fullSync: false,
    reason: ""
  });

  const current = decodeByMessageType(fixturesByFile.get("movement-snapshot-future-fields.bin").messageType, futureBody);
  assert.equal(current.correctionKind, 99);
  assert.equal(current.reasonCode, 77);
  assert.equal(current.roomId, oldProjection.roomId);
  assert.equal(current.frameId, oldProjection.frameId);
});

test("sensitive-content scanner rejects credential and non-synthetic account patterns", () => {
  const sensitiveCodes = scanFixtureContent(
    Buffer.from('fixture=eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.signature_value'),
    "test-body.bin"
  ).map((entry) => entry.code);
  assert.ok(sensitiveCodes.includes("FIXTURE_SENSITIVE_JWT"));
  assert.ok(scanFixtureContent("game_ticket=abcdEFGH_1234", "test-manifest.json").some((entry) => entry.code === "FIXTURE_SENSITIVE_TICKET"));
  assert.ok(scanFixtureContent("account=person@example.com", "test-manifest.json").some((entry) => entry.code === "FIXTURE_SENSITIVE_EMAIL"));

  const directory = mkdtempSync(path.join(os.tmpdir(), "myserver-proto-fixture-sensitive-test-"));
  try {
    const fixture = structuredClone(fixturesByFile.get("get-character-elements-int32-boundaries.bin"));
    fixture.source.fields.character_id = "production_character_123";
    writeFileSync(path.join(directory, fixture.file), readFixture(fixture.file));
    writeFileSync(path.join(directory, "manifest.json"), JSON.stringify({
      ...manifest,
      fixtures: [fixture]
    }));
    const diagnostics = validateProtoCompatibilityFixtures({
      fixtureDirectory: directory,
      manifestPath: path.join(directory, "manifest.json")
    }).diagnostics;
    assert.ok(diagnostics.some((entry) => entry.code === "FIXTURE_IDENTITY_NOT_SYNTHETIC"));
  } finally {
    rmSync(directory, { force: true, recursive: true });
  }
});
