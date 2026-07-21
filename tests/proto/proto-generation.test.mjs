import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import {
  RUST_PROTO_TARGETS,
  compareGeneratedRustDirectories,
  formatGeneratorFailure,
  parseMode
} from "../../tools/proto-generate.js";

test("proto generator accepts exactly one explicit mode", () => {
  assert.equal(parseMode(["--check"]), "--check");
  assert.equal(parseMode(["--write"]), "--write");
  assert.throws(() => parseMode([]), /Usage:/);
  assert.throws(() => parseMode(["--check", "--write"]), /Usage:/);
});

test("generated Rust comparison reports missing, stale, and changed files", () => {
  const directory = mkdtempSync(path.join(os.tmpdir(), "myserver-proto-generation-test-"));
  try {
    const expected = path.join(directory, "expected");
    const actual = path.join(directory, "actual");
    mkdirSync(expected);
    mkdirSync(actual);
    writeFileSync(path.join(expected, "myserver.game.rs"), "current\n");
    writeFileSync(path.join(expected, "myserver.chat.rs"), "removed\n");
    writeFileSync(path.join(actual, "myserver.game.rs"), "stale\n");
    writeFileSync(path.join(actual, "myserver.admin.rs"), "old\n");
    writeFileSync(path.join(actual, "mod.rs"), "hand-written\n");

    const differences = compareGeneratedRustDirectories(expected, actual);
    assert.equal(differences.length, 3);
    assert.match(differences.join("\n"), /myserver\.game\.rs differs/);
    assert.match(differences.join("\n"), /myserver\.chat\.rs is missing/);
    assert.match(differences.join("\n"), /myserver\.admin\.rs is stale/);
    assert.doesNotMatch(differences.join("\n"), /mod\.rs/);
  } finally {
    rmSync(directory, { force: true, recursive: true });
  }
});

test("failure text identifies tools, inputs, and output target", () => {
  const text = formatGeneratorFailure(RUST_PROTO_TARGETS[0], "simulated cargo failure");
  assert.match(text, /protoc-bin-vendored 3\.2\.0/);
  assert.match(text, /prost-build 0\.13\.5/);
  assert.match(text, /tonic-build 0\.12\.3/);
  assert.match(text, /packages\/proto\/game\.proto/);
  assert.match(text, /apps\/game-server\/src\/proto/);
});
