import assert from "node:assert/strict";
import { createPublicKey, verify } from "node:crypto";
import { readFileSync } from "node:fs";
import { register } from "node:module";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../../tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const { canonicalAdminOperationAssertionPayload } = await import("./admin-operation-assertion.service.ts");

const fixture = JSON.parse(readFileSync(new URL("../../../../tests/fixtures/admin-operation-assertion-v1.json", import.meta.url)));

function fixturePublicKey(rawBase64url) {
  const raw = Buffer.from(rawBase64url, "base64url");
  const spkiPrefix = Buffer.from("302a300506032b6570032100", "hex");
  return createPublicKey({
    key: Buffer.concat([spkiPrefix, raw]),
    format: "der",
    type: "spki"
  });
}

test("AdminOperationAssertion golden fixture remains Node-canonical and verifiable", () => {
  const publicKey = fixturePublicKey(fixture.key.publicKeyBase64url);

  for (const entry of Object.values(fixture.cases)) {
    const { assertion, expectedCanonicalPayloadUtf8, assertionHeaderValue } = entry;
    const { signature, ...unsigned } = assertion;
    const canonical = canonicalAdminOperationAssertionPayload(unsigned);

    assert.equal(canonical.toString("utf8"), expectedCanonicalPayloadUtf8);
    assert.equal(Buffer.from(JSON.stringify(assertion), "utf8").toString("base64url"), assertionHeaderValue);
    assert.equal(verify(null, canonical, publicKey, Buffer.from(signature, "base64url")), true);
  }
});
