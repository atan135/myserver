import assert from "node:assert/strict";
import crypto from "node:crypto";
import test from "node:test";

import {
  ReplayCache,
  jcsCanonicalize,
  parseStrictJson,
  serializeMessage,
  signMessage,
  signingBytes,
  verifyMessageSignature
} from "./protocol.js";
import { MYFORGE_SIGNATURE_VECTOR } from "./signature-vector.js";

test("fixed Ed25519 vector pins exact JCS signing bytes and signature", () => {
  const privateKey = crypto.createPrivateKey(MYFORGE_SIGNATURE_VECTOR.privateKeyPem);
  const publicKey = crypto.createPublicKey(MYFORGE_SIGNATURE_VECTOR.publicKeyPem);
  const signed = signMessage(MYFORGE_SIGNATURE_VECTOR.unsignedMessage, privateKey);

  assert.equal(signingBytes(MYFORGE_SIGNATURE_VECTOR.unsignedMessage).toString("hex"), MYFORGE_SIGNATURE_VECTOR.signingBytesHex);
  assert.equal(signed.signature, MYFORGE_SIGNATURE_VECTOR.signature);
  assert.equal(verifyMessageSignature(signed, publicKey), true);
});

test("JCS signature is stable across field order, whitespace, escapes, and non-BMP Unicode", () => {
  const publicKey = crypto.createPublicKey(MYFORGE_SIGNATURE_VECTOR.publicKeyPem);
  const signature = MYFORGE_SIGNATURE_VECTOR.signature;
  const raw = ` {
    "type":"protocol.error", "meta":{"z":null,"a":["escaped",7]},
    "errorMessage":"\\u4e2d\\u6587\\ud83d\\ude00", "signature":"${signature}",
    "timestampMs":1783694421000,"fatal":true,"requestId":null,
    "nonce":"AAECAwQFBgcICQoLDA0ODw","protocolVersion":1,
    "projectId":"myforge-local","expiresAtMs":1783694481000,
    "errorCode":"MYFORGE_TEST_VECTOR","connectionId":"67da7da9-a653-4d6e-9e81-f5f8baf874bb",
    "agentId":"dev-pc-001"
  } `;
  const parsed = parseStrictJson(raw);

  assert.equal(verifyMessageSignature(parsed, publicKey), true);
  assert.equal(serializeMessage(parsed), jcsCanonicalize(parsed));
});

test("JCS property sorting uses UTF-16 code-unit order", () => {
  const canonical = jcsCanonicalize({ "\ue000": 2, "\ud83d\ude00": 1 });
  assert.equal(Buffer.from(canonical, "utf8").toString("hex"), "7b22f09f9880223a312c22ee8080223a327d");
});

test("strict parser rejects duplicate keys at every depth", () => {
  assert.throws(() => parseStrictJson('{"a":1,"a":2}'), { code: "MYFORGE_MESSAGE_IJSON_INVALID" });
  assert.throws(() => parseStrictJson('{"outer":{"a":1,"a":2}}'), { code: "MYFORGE_MESSAGE_IJSON_INVALID" });
});

test("strict parser rejects invalid UTF-8 and lone surrogates", () => {
  assert.throws(() => parseStrictJson(Buffer.from([0x7b, 0x22, 0x78, 0x22, 0x3a, 0x22, 0xc3, 0x28, 0x22, 0x7d])), {
    code: "MYFORGE_MESSAGE_IJSON_INVALID"
  });
  assert.throws(() => parseStrictJson('{"x":"\\uD800"}'), { code: "MYFORGE_MESSAGE_IJSON_INVALID" });
  assert.throws(() => parseStrictJson('{"x":"\\uDC00"}'), { code: "MYFORGE_MESSAGE_IJSON_INVALID" });
  assert.throws(() => parseStrictJson("[".repeat(66) + "null" + "]".repeat(66)), {
    code: "MYFORGE_MESSAGE_IJSON_INVALID"
  });
});

test("strict parser rejects floating, exponent, negative zero, and unsafe integers", () => {
  for (const value of ["1.0", "1e2", "-0", "9007199254740992"]) {
    assert.throws(() => parseStrictJson(`{"value":${value}}`), { code: "MYFORGE_MESSAGE_IJSON_INVALID" });
  }
  assert.equal(parseStrictJson('{"max":9007199254740991}').max, 9007199254740991);
});

test("changing any signed field invalidates the signature", () => {
  const privateKey = crypto.createPrivateKey(MYFORGE_SIGNATURE_VECTOR.privateKeyPem);
  const publicKey = crypto.createPublicKey(MYFORGE_SIGNATURE_VECTOR.publicKeyPem);
  const signed = signMessage(MYFORGE_SIGNATURE_VECTOR.unsignedMessage, privateKey);

  for (const mutation of [
    { ...signed, projectId: "other" },
    { ...signed, timestampMs: signed.timestampMs + 1 },
    { ...signed, expiresAtMs: signed.expiresAtMs + 1 },
    { ...signed, nonce: "AQECAwQFBgcICQoLDA0ODw" }
  ]) {
    assert.throws(() => verifyMessageSignature(mutation, publicKey), { code: "MYFORGE_AGENT_SIGNATURE_INVALID" });
  }
});

test("replay cache atomically retains nonce keys until their expiry", () => {
  const cache = new ReplayCache();
  cache.checkAndInsert("connection-agent-nonce", 2000, 1000);
  assert.throws(() => cache.checkAndInsert("connection-agent-nonce", 2000, 1001), { code: "MYFORGE_REPLAY_DETECTED" });
  cache.checkAndInsert("another", 3000, 2001);
  assert.equal(cache.size, 1);
});

test("replay cache never evicts live nonces and fails closed at capacity", () => {
  const cache = new ReplayCache(2);
  cache.checkAndInsert("first", 2000, 1000);
  cache.checkAndInsert("second", 3000, 1000);
  assert.throws(() => cache.checkAndInsert("third", 4000, 1500), { code: "MYFORGE_AGENT_BUSY" });
  assert.equal(cache.size, 2);
  assert.throws(() => cache.checkAndInsert("second", 3000, 1500), { code: "MYFORGE_REPLAY_DETECTED" });

  cache.checkAndInsert("third", 4000, 2001);
  assert.equal(cache.size, 2);
  assert.throws(() => cache.checkAndInsert("second", 3000, 2001), { code: "MYFORGE_REPLAY_DETECTED" });
});
