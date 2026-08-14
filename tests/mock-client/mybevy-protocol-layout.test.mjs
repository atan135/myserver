import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { checkMybevyClientProtocolAtRoot } from "../../tools/check-mock-client-protocol.js";

const MESSAGE_TYPES = Object.freeze({ AUTH_REQ: 1001, PING_RES: 1004 });
const GAME_PROTO = 'syntax = "proto3";\nmessage GameSnapshot {}\n';
const CHAT_PROTO = 'syntax = "proto3";\nmessage ChatSnapshot {}\n';

function makeTempRoot(t) {
  const root = mkdtempSync(path.join(os.tmpdir(), "myserver-mybevy-layout-"));
  t.after(() => rmSync(root, { force: true, recursive: true }));
  return root;
}

function writeFile(root, relativePath, contents) {
  const target = path.join(root, relativePath);
  mkdirSync(path.dirname(target), { recursive: true });
  writeFileSync(target, contents, "utf8");
}

function messageTypeSource() {
  return `
pub enum MessageType {
    AuthReq = 1001,
    PingRes = 1004,
}

impl MessageType {
    pub fn from_u16(value: u16) -> Option<Self> {
        match value {
            1001 => Some(Self::AuthReq),
            1004 => Some(Self::PingRes),
            _ => None,
        }
    }
}
`;
}

function legacyBuildSource() {
  return `
fn main() {
    let proto_dir = root.join("MyServer").join("packages").join("proto");
    let game_proto = proto_dir.join("game.proto");
    prost_build::Config::new().compile_protos(&[game_proto], &[proto_dir]).unwrap();
}
`;
}

function crateBuildSource({ compileChat = true } = {}) {
  const inputs = compileChat ? "&[game_proto, chat_proto]" : "&[game_proto]";
  return `
fn main() {
    let proto_dir = project_dir.join("vendor").join("myserver").join("proto");
    let game_proto = proto_dir.join("game.proto");
    let chat_proto = proto_dir.join("chat.proto");
    prost_build::Config::new().compile_protos(${inputs}, &[proto_dir]).unwrap();
}
`;
}

function writeCanonicalProtos(root) {
  writeFile(root, "canonical/proto/game.proto", GAME_PROTO);
  writeFile(root, "canonical/proto/chat.proto", CHAT_PROTO);
  return path.join(root, "canonical", "proto");
}

function writeCrateLayout(root, options = {}) {
  writeFile(root, "project/crates/myserver-protocol/src/lib.rs", messageTypeSource());
  writeFile(
    root,
    "project/crates/myserver-protocol/Cargo.toml",
    '[package]\nname = "myserver-protocol"\nversion = "0.1.0"\n'
  );
  writeFile(
    root,
    "project/crates/myserver-protocol/build.rs",
    crateBuildSource({ compileChat: options.compileChat !== false })
  );
  writeFile(
    root,
    "project/vendor/myserver/proto/game.proto",
    options.vendoredGameProto ?? GAME_PROTO
  );
  writeFile(
    root,
    "project/vendor/myserver/proto/chat.proto",
    options.vendoredChatProto ?? CHAT_PROTO
  );
}

test("legacy mybevy layout remains supported when it owns MessageType", (t) => {
  const root = makeTempRoot(t);
  const canonicalProtoDirectory = writeCanonicalProtos(root);
  writeFile(root, "project/src/game/myserver/protocol.rs", messageTypeSource());
  writeFile(root, "project/build.rs", legacyBuildSource());

  const result = checkMybevyClientProtocolAtRoot(MESSAGE_TYPES, {
    clientRoot: root,
    canonicalProtoDirectory
  });

  assert.equal(result.layout, "legacy-project");
  assert.deepEqual(result.errors, []);
});

test("crate layout selects the authoritative source instead of the legacy facade", (t) => {
  const root = makeTempRoot(t);
  const canonicalProtoDirectory = writeCanonicalProtos(root);
  writeCrateLayout(root);
  writeFile(root, "project/src/game/myserver/protocol.rs", "pub use myserver_protocol::*;\n");

  const result = checkMybevyClientProtocolAtRoot(MESSAGE_TYPES, {
    clientRoot: root,
    canonicalProtoDirectory
  });

  assert.equal(result.layout, "crate");
  assert.deepEqual(result.errors, []);
  assert(result.checkedFiles.includes("project/crates/myserver-protocol/src/lib.rs"));
  assert(result.checkedFiles.includes("project/vendor/myserver/proto/game.proto"));
  assert(result.checkedFiles.includes("project/vendor/myserver/proto/chat.proto"));
});

test("crate layout rejects an incomplete shared proto build or stale vendor snapshot", (t) => {
  const root = makeTempRoot(t);
  const canonicalProtoDirectory = writeCanonicalProtos(root);
  writeCrateLayout(root, {
    compileChat: false,
    vendoredGameProto: 'syntax = "proto3";\nmessage StaleGameSnapshot {}\n'
  });

  const result = checkMybevyClientProtocolAtRoot(MESSAGE_TYPES, {
    clientRoot: root,
    canonicalProtoDirectory
  });

  assert.equal(result.layout, "crate");
  assert(result.errors.some((error) => error.includes("must compile the vendored game.proto and chat.proto")));
  assert(result.errors.some((error) => error.includes("vendored game.proto does not match packages/proto/game.proto")));
});

test("missing or conflicting MessageType layouts fail closed", (t) => {
  const missingRoot = makeTempRoot(t);
  const canonicalProtoDirectory = writeCanonicalProtos(missingRoot);
  const missing = checkMybevyClientProtocolAtRoot(MESSAGE_TYPES, {
    clientRoot: missingRoot,
    canonicalProtoDirectory
  });
  assert.equal(missing.layout, null);
  assert(missing.errors.some((error) => error.includes("MessageType source not found")));

  const ambiguousRoot = makeTempRoot(t);
  const ambiguousProtoDirectory = writeCanonicalProtos(ambiguousRoot);
  writeCrateLayout(ambiguousRoot);
  writeFile(ambiguousRoot, "project/src/game/myserver/protocol.rs", messageTypeSource());
  writeFile(ambiguousRoot, "project/build.rs", legacyBuildSource());
  const ambiguous = checkMybevyClientProtocolAtRoot(MESSAGE_TYPES, {
    clientRoot: ambiguousRoot,
    canonicalProtoDirectory: ambiguousProtoDirectory
  });
  assert.equal(ambiguous.layout, null);
  assert(ambiguous.errors.some((error) => error.includes("protocol layout is ambiguous")));
});
