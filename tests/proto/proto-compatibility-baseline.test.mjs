import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import {
  buildBaseline,
  buildLocalBaseline,
  checkBaseline,
  parseProto,
  validateInventory,
  writeBaseline
} from "../../tools/proto-compatibility-baseline.js";

test("normalized baseline keeps wire declarations and ignores comments and declaration order", () => {
  const first = parseProto(`
    syntax = "proto3";
    package example.v1;
    // This comment is intentionally ignored.
    message Reply { string message = 2; reserved 1, 7 to 9, "legacy_name"; }
    enum Status { STATUS_UNKNOWN = 0; STATUS_READY = 1; }
    service Echo { rpc Get (Request) returns (stream Reply); }
    message Request { optional string id = 1; oneof value { string name = 2; bytes raw = 3; } }
  `);
  const second = parseProto(`
    syntax = "proto3";
    package example.v1;
    message Request { oneof value { bytes raw = 3; string name = 2; } optional string id = 1; }
    service Echo { rpc Get(Request) returns(stream Reply); }
    enum Status { STATUS_READY = 1; STATUS_UNKNOWN = 0; }
    message Reply { reserved "legacy_name", 7 to 9, 1; string message = 2; }
  `);

  assert.deepEqual(first, second);
  assert.deepEqual(first.messages.find((message) => message.name === "Request")?.fields, [
    { label: "optional", name: "id", number: 1, oneof: "", type: "string" },
    { label: "singular", name: "name", number: 2, oneof: "value", type: "string" },
    { label: "singular", name: "raw", number: 3, oneof: "value", type: "bytes" }
  ]);
  assert.deepEqual(first.services[0], {
    name: "Echo",
    rpcs: [{ name: "Get", request: { stream: false, type: "Request" }, response: { stream: true, type: "Reply" } }]
  });
});

test("baseline changes when a field wire type changes", () => {
  const directory = mkdtempSync(path.join(os.tmpdir(), "myserver-proto-baseline-"));
  try {
    mkdirSync(path.join(directory, "packages", "proto"), { recursive: true });
    writeFileSync(path.join(directory, "packages", "proto", "sample.proto"), 'syntax = "proto3"; package sample; message Value { string id = 1; }\n');
    const inventory = {
      baseline: { file: "packages/proto/compatibility/baseline.json" },
      protocols: [{ file: "packages/proto/sample.proto" }]
    };
    const original = buildBaseline(inventory, directory);
    writeFileSync(path.join(directory, "packages", "proto", "sample.proto"), 'syntax = "proto3"; package sample; message Value { uint64 id = 1; }\n');
    const changed = buildBaseline(inventory, directory);

    assert.notEqual(original.digest, changed.digest);
    assert.equal(changed.files[0].messages[0].fields[0].type, "uint64");
  } finally {
    rmSync(directory, { force: true, recursive: true });
  }
});

test("inventory validation rejects an active local proto outside packages/proto", () => {
  const directory = mkdtempSync(path.join(os.tmpdir(), "myserver-proto-inventory-"));
  try {
    mkdirSync(path.join(directory, "packages", "proto"), { recursive: true });
    mkdirSync(path.join(directory, "apps", "service"), { recursive: true });
    writeFileSync(path.join(directory, "packages", "proto", "sample.proto"), 'syntax = "proto3"; package sample;\n');
    writeFileSync(path.join(directory, "apps", "service", "local.proto"), 'syntax = "proto3"; package local;\n');

    assert.throws(
      () => validateInventory({ protocols: [{ file: "packages/proto/sample.proto" }] }, directory),
      /untracked local proto definitions/
    );
  } finally {
    rmSync(directory, { force: true, recursive: true });
  }
});

test("precisely registered private tool proto has a separate semantic baseline", () => {
  const directory = mkdtempSync(path.join(os.tmpdir(), "myserver-local-proto-baseline-"));
  try {
    mkdirSync(path.join(directory, "packages", "proto"), { recursive: true });
    mkdirSync(path.join(directory, "tools", "load-test", "proto"), { recursive: true });
    mkdirSync(path.join(directory, "tools", "load-test", "src"), { recursive: true });
    writeFileSync(path.join(directory, "packages", "proto", "sample.proto"), 'syntax = "proto3"; package sample;\n');
    const localProtoPath = path.join(directory, "tools", "load-test", "proto", "control.proto");
    writeFileSync(localProtoPath, 'syntax = "proto3"; package loadtest; message Session { uint32 protocol_version = 1; string id = 2; }\n');
    writeFileSync(path.join(directory, "tools", "load-test", "src", "control.rs"), "pub const CONTROL_PROTOCOL_VERSION: u32 = 1;\n");
    const localProtocol = {
      file: "tools/load-test/proto/control.proto",
      compatibility: {
        baseline: "tools/load-test/proto/compatibility/baseline.json",
        currentVersion: 1,
        versionField: "protocol_version",
        versionConstant: {
          path: "tools/load-test/src/control.rs",
          name: "CONTROL_PROTOCOL_VERSION"
        }
      }
    };
    const inventory = {
      protocols: [{ file: "packages/proto/sample.proto" }],
      localProtocols: [localProtocol]
    };

    assert.doesNotThrow(() => validateInventory(inventory, directory));
    const original = buildLocalBaseline(localProtocol, directory);
    writeFileSync(localProtoPath, 'syntax = "proto3"; package loadtest; message Session { uint32 protocol_version = 1; uint64 id = 2; }\n');
    const changed = buildLocalBaseline(localProtocol, directory);

    assert.notEqual(original.digest, changed.digest);
    assert.equal(changed.files[0].messages[0].fields[1].type, "uint64");
  } finally {
    rmSync(directory, { force: true, recursive: true });
  }
});

test("private proto wire changes require a recorded version upgrade before writing a new baseline", () => {
  const directory = mkdtempSync(path.join(os.tmpdir(), "myserver-local-proto-version-"));
  try {
    mkdirSync(path.join(directory, "packages", "proto", "compatibility"), { recursive: true });
    mkdirSync(path.join(directory, "tools", "load-test", "proto"), { recursive: true });
    mkdirSync(path.join(directory, "tools", "load-test", "src"), { recursive: true });
    writeFileSync(path.join(directory, "packages", "proto", "sample.proto"), 'syntax = "proto3"; package sample;\n');
    const localProtoPath = path.join(directory, "tools", "load-test", "proto", "control.proto");
    const constantPath = path.join(directory, "tools", "load-test", "src", "control.rs");
    writeFileSync(localProtoPath, 'syntax = "proto3"; package loadtest; message Session { uint32 protocol_version = 1; string id = 2; }\n');
    writeFileSync(constantPath, "pub const CONTROL_PROTOCOL_VERSION: u32 = 1;\n");
    const localProtocol = {
      file: "tools/load-test/proto/control.proto",
      compatibility: {
        baseline: "tools/load-test/proto/compatibility/baseline.json",
        currentVersion: 1,
        versionField: "protocol_version",
        versionConstant: {
          path: "tools/load-test/src/control.rs",
          name: "CONTROL_PROTOCOL_VERSION"
        }
      }
    };
    const inventory = {
      baseline: { file: "packages/proto/compatibility/baseline.json" },
      protocols: [{ file: "packages/proto/sample.proto" }],
      localProtocols: [localProtocol]
    };
    const metadata = { approvedBy: "test owner", reason: "test version boundary" };

    assert.doesNotThrow(() => writeBaseline(inventory, directory, metadata));
    writeFileSync(localProtoPath, 'syntax = "proto3"; package loadtest; message Session { uint32 protocol_version = 1; uint64 id = 2; }\n');
    assert.throws(
      () => checkBaseline(inventory, directory),
      /wire snapshot changed without increasing compatibility\.currentVersion/
    );
    assert.throws(
      () => writeBaseline(inventory, directory, metadata),
      /wire snapshot changed without increasing compatibility\.currentVersion/
    );

    localProtocol.compatibility.currentVersion = 2;
    writeFileSync(constantPath, "pub const CONTROL_PROTOCOL_VERSION: u32 = 2;\n");
    assert.throws(
      () => checkBaseline(inventory, directory),
      /local protocol baseline is stale/
    );
    assert.doesNotThrow(() => writeBaseline(inventory, directory, metadata));
    assert.doesNotThrow(() => checkBaseline(inventory, directory));
  } finally {
    rmSync(directory, { force: true, recursive: true });
  }
});

test("private proto inventory rejects a missing protocol version field", () => {
  const directory = mkdtempSync(path.join(os.tmpdir(), "myserver-local-proto-version-field-"));
  try {
    mkdirSync(path.join(directory, "packages", "proto"), { recursive: true });
    mkdirSync(path.join(directory, "tools", "load-test", "proto"), { recursive: true });
    mkdirSync(path.join(directory, "tools", "load-test", "src"), { recursive: true });
    writeFileSync(path.join(directory, "packages", "proto", "sample.proto"), 'syntax = "proto3"; package sample;\n');
    writeFileSync(path.join(directory, "tools", "load-test", "proto", "control.proto"), 'syntax = "proto3"; package loadtest; message Session { uint32 version = 1; }\n');
    writeFileSync(path.join(directory, "tools", "load-test", "src", "control.rs"), "pub const CONTROL_PROTOCOL_VERSION: u32 = 1;\n");
    const inventory = {
      protocols: [{ file: "packages/proto/sample.proto" }],
      localProtocols: [{
        file: "tools/load-test/proto/control.proto",
        compatibility: {
          baseline: "tools/load-test/proto/compatibility/baseline.json",
          currentVersion: 1,
          versionField: "protocol_version",
          versionConstant: {
            path: "tools/load-test/src/control.rs",
            name: "CONTROL_PROTOCOL_VERSION"
          }
        }
      }]
    };

    assert.throws(() => validateInventory(inventory, directory), /does not declare versionField protocol_version/);
    writeFileSync(path.join(directory, "tools", "load-test", "proto", "control.proto"), 'syntax = "proto3"; package loadtest; message Session { string protocol_version = 1; }\n');
    assert.throws(() => validateInventory(inventory, directory), /versionField protocol_version must use uint32/);
  } finally {
    rmSync(directory, { force: true, recursive: true });
  }
});

test("checked-in compatibility baseline is current", () => {
  execFileSync(process.execPath, ["tools/proto-compatibility-baseline.js", "--check"], {
    cwd: process.cwd(),
    stdio: "pipe"
  });
});
