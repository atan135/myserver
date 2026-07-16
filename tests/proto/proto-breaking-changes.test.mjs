import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  applyExemptions,
  checkBreakingChangesForRepository,
  compareProtocolBaselines
} from "../../tools/check-proto-breaking-changes.js";
import { buildBaseline, parseProto } from "../../tools/proto-compatibility-baseline.js";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const fixtureDirectory = path.join(__dirname, "fixtures", "proto-breaking");

function fileSnapshot(file, source) {
  return { path: file, ...parseProto(source, file) };
}

function snapshot(files) {
  return { files };
}

function inventory(protocols) {
  return { protocols };
}

function protocol(file, classification = ["client_gameplay"]) {
  return { classification, file };
}

const releasedSource = readFileSync(path.join(fixtureDirectory, "released.proto"), "utf8");
const breakingSource = readFileSync(path.join(fixtureDirectory, "breaking.proto"), "utf8");
const reservedSource = readFileSync(path.join(fixtureDirectory, "removed-field-reserved.proto"), "utf8");

test("deliberately breaking proto fixture reports field, enum, RPC, and player-client diagnostics", () => {
  const file = "packages/proto/game.proto";
  const diagnostics = compareProtocolBaselines(
    snapshot([fileSnapshot(file, releasedSource)]),
    snapshot([fileSnapshot(file, breakingSource)]),
    inventory([protocol(file)])
  );
  const rules = new Set(diagnostics.map((diagnostic) => diagnostic.rule));

  for (const rule of [
    "FIELD_NUMBER_REUSED",
    "FIELD_TYPE_CHANGED",
    "FIELD_LABEL_CHANGED",
    "FIELD_ONEOF_CHANGED",
    "ONEOF_REMOVED",
    "FIELD_REMOVED_NOT_RESERVED",
    "ENUM_NUMBER_REUSED",
    "ENUM_VALUE_DELETED",
    "RPC_REMOVED",
    "RPC_REQUEST_STREAM_CHANGED",
    "RPC_RESPONSE_TYPE_CHANGED",
    "RPC_RESPONSE_STREAM_CHANGED"
  ]) {
    assert.ok(rules.has(rule), `expected ${rule}`);
  }
  assert.ok(diagnostics.every((diagnostic) => diagnostic.file === file));
  assert.ok(diagnostics.every((diagnostic) => diagnostic.audience === "PLAYER_CLIENT"));
  assert.match(diagnostics.find((diagnostic) => diagnostic.rule === "FIELD_TYPE_CHANGED").detail, /Example\.text \(#1\)/);
});

test("removed field is accepted only when the same message reserves its historical number", () => {
  const file = "packages/proto/admin.proto";
  const diagnostics = compareProtocolBaselines(
    snapshot([fileSnapshot(file, releasedSource)]),
    snapshot([fileSnapshot(file, reservedSource)]),
    inventory([protocol(file, ["internal_control"])])
  );
  assert.deepEqual(diagnostics, []);
});

test("risk comes from inventory classification rather than a global severity", () => {
  const gameFile = "packages/proto/game.proto";
  const adminFile = "packages/proto/admin.proto";
  const released = snapshot([
    fileSnapshot(gameFile, releasedSource),
    fileSnapshot(adminFile, releasedSource)
  ]);
  const candidate = snapshot([
    fileSnapshot(gameFile, breakingSource),
    fileSnapshot(adminFile, breakingSource)
  ]);
  const diagnostics = compareProtocolBaselines(released, candidate, inventory([
    protocol(gameFile, ["client_gameplay", "internal_game_control"]),
    protocol(adminFile, ["internal_control"])
  ]));

  assert.equal(diagnostics.find((diagnostic) => diagnostic.file === gameFile).audience, "PLAYER_CLIENT");
  assert.equal(diagnostics.find((diagnostic) => diagnostic.file === adminFile).audience, "COORDINATED_INTERNAL");
});

test("controlled exemptions require exact, current, unexpired ownership records", () => {
  const diagnostic = {
    file: "packages/proto/game.proto",
    rule: "FIELD_TYPE_CHANGED",
    target: { fieldName: "text", fieldNumber: 1, file: "packages/proto/game.proto", message: "Example" }
  };
  const valid = {
    schemaVersion: 1,
    exemptions: [{
      expiresAt: "2099-01-01",
      id: "temporary-game-field-migration",
      owner: "game-server-owner",
      reason: "Temporary migration validated by release owner.",
      rule: "FIELD_TYPE_CHANGED",
      target: { fieldNumber: 1, file: "packages/proto/game.proto", message: "Example" }
    }]
  };
  const accepted = applyExemptions([diagnostic], valid, "2026-07-16");
  assert.deepEqual(accepted.diagnostics, []);
  assert.deepEqual(accepted.configDiagnostics, []);
  assert.deepEqual(accepted.exempted, ["temporary-game-field-migration"]);

  const expired = structuredClone(valid);
  expired.exemptions[0].expiresAt = "2026-07-15";
  const expiredResult = applyExemptions([diagnostic], expired, "2026-07-16");
  assert.equal(expiredResult.diagnostics.length, 1);
  assert.ok(expiredResult.configDiagnostics.some((item) => item.rule === "EXEMPTION_EXPIRED"));

  const invalid = structuredClone(valid);
  delete invalid.exemptions[0].owner;
  const invalidResult = applyExemptions([diagnostic], invalid, "2026-07-16");
  assert.ok(invalidResult.configDiagnostics.some((item) => item.rule === "EXEMPTION_INVALID"));

  const unused = structuredClone(valid);
  unused.exemptions[0].target.fieldNumber = 99;
  const unusedResult = applyExemptions([diagnostic], unused, "2026-07-16");
  assert.ok(unusedResult.configDiagnostics.some((item) => item.rule === "EXEMPTION_UNUSED"));
});

test("published reference catches a breaking source even after candidate baseline is updated", () => {
  const root = mkdtempSync(path.join(os.tmpdir(), "myserver-proto-breaking-test-"));
  try {
    const compatibilityDirectory = path.join(root, "packages", "proto", "compatibility");
    const protoPath = path.join(root, "packages", "proto", "game.proto");
    mkdirSync(compatibilityDirectory, { recursive: true });
    const fixtureInventory = {
      baseline: { file: "packages/proto/compatibility/baseline.json" },
      breakingExemptions: { file: "packages/proto/compatibility/breaking-exemptions.json" },
      protocols: [protocol("packages/proto/game.proto")],
      publishedReference: {
        baselineFile: "packages/proto/compatibility/release-baseline.json",
        manifestFile: "packages/proto/compatibility/release-reference.json"
      }
    };
    writeFileSync(protoPath, releasedSource);
    const released = buildBaseline(fixtureInventory, root);
    writeFileSync(path.join(compatibilityDirectory, "release-baseline.json"), `${JSON.stringify(released, null, 2)}\n`);
    writeFileSync(path.join(compatibilityDirectory, "release-reference.json"), `${JSON.stringify({
      baselineDigest: released.digest,
      baselineFile: "packages/proto/compatibility/release-baseline.json",
      release: {
        approvedAt: "2026-07-16",
        approvedBy: "protocol-owner",
        reason: "fixture release",
        releaseId: "fixture-v1"
      },
      schemaVersion: 1
    }, null, 2)}\n`);
    writeFileSync(path.join(compatibilityDirectory, "breaking-exemptions.json"), "{\n  \"schemaVersion\": 1,\n  \"exemptions\": []\n}\n");

    // This simulates a pull request updating proto and candidate baseline together.
    writeFileSync(protoPath, breakingSource);
    const candidate = buildBaseline(fixtureInventory, root);
    writeFileSync(path.join(compatibilityDirectory, "baseline.json"), `${JSON.stringify(candidate, null, 2)}\n`);

    const result = checkBreakingChangesForRepository(fixtureInventory, root, "2026-07-16");
    assert.ok(result.errors.some((diagnostic) => diagnostic.rule === "FIELD_TYPE_CHANGED"));
    assert.ok(result.errors.some((diagnostic) => diagnostic.rule === "FIELD_REMOVED_NOT_RESERVED"));
  } finally {
    rmSync(root, { force: true, recursive: true });
  }
});
