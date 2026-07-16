import assert from "node:assert/strict";
import test from "node:test";

import {
  negotiateClientProtocolVersion,
  readClientProtocolVersionPolicy,
  verifyClientProtocolVersionImplementation
} from "../../tools/client-protocol-version-policy.js";
import { encodeAuthReq, decodeByMessageType } from "../../tools/mock-client/src/messages.js";
import { MESSAGE_TYPE } from "../../tools/mock-client/src/constants.js";
import { decodeFieldsWithRepeated, encodeBoolField, encodeStringField, encodeUInt32Field } from "../../tools/mock-client/src/protocol.js";

test("legacy omitted AuthReq version remains accepted by the current v1 policy", () => {
  assert.deepEqual(negotiateClientProtocolVersion(0), {
    accepted: true,
    effectiveVersion: 1,
    source: "legacy_implicit"
  });
});

test("policy accepts the current version and rejects both old and future versions", () => {
  const policy = structuredClone(readClientProtocolVersionPolicy());
  policy.applicationProtocol.currentClientProtocolVersion = 2;
  policy.applicationProtocol.minimumClientProtocolVersion = 2;

  assert.deepEqual(negotiateClientProtocolVersion(2, policy), {
    accepted: true,
    effectiveVersion: 2,
    source: "explicit"
  });
  assert.equal(negotiateClientProtocolVersion(0, policy).errorCode, "CLIENT_PROTOCOL_VERSION_TOO_OLD");
  assert.equal(negotiateClientProtocolVersion(3, policy).errorCode, "CLIENT_PROTOCOL_VERSION_TOO_NEW");
});

test("mock-client emits explicit current version while preserving a legacy fixture mode", () => {
  const explicit = decodeFieldsWithRepeated(encodeAuthReq("fixture_ticket"));
  const legacy = decodeFieldsWithRepeated(encodeAuthReq("fixture_ticket", 0));
  assert.equal(Number(explicit.get(2)), 1);
  assert.equal(legacy.has(2), false);
});

test("mock-client decodes AuthRes policy and upgrade fields", () => {
  const body = Buffer.concat([
    encodeBoolField(1, false),
    encodeStringField(3, "CLIENT_PROTOCOL_VERSION_TOO_OLD"),
    encodeUInt32Field(4, 2),
    encodeUInt32Field(5, 2),
    encodeStringField(6, "Update required"),
    encodeStringField(7, "https://updates.example.invalid/game")
  ]);
  assert.deepEqual(decodeByMessageType(MESSAGE_TYPE.AUTH_RES, body), {
    ok: false,
    accountPlayerId: "",
    errorCode: "CLIENT_PROTOCOL_VERSION_TOO_OLD",
    serverProtocolVersion: 2,
    minimumClientProtocolVersion: 2,
    upgradeMessage: "Update required",
    upgradeUrl: "https://updates.example.invalid/game"
  });
});

test("shared Rust policy and protobuf declarations match policy metadata", () => {
  assert.deepEqual(verifyClientProtocolVersionImplementation(), []);
});
