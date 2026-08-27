import assert from "node:assert/strict";
import test from "node:test";

import { formatLogPayload } from "./logger.js";

test("announce logger redacts sensitive fields and bounds nested data", () => {
  const payload = formatLogPayload("announce failed", {
    payload: "raw-body",
    nested: { token: "raw-token", message: "raw-error" },
    endpoint: "nats://user:pw@nats:4222"
  });
  assert.doesNotMatch(payload, /raw-body|raw-token|raw-error|user:pw/);
  assert.match(payload, /REDACTED/);
  assert.match(payload, /nats:\/\/\[REDACTED_ENDPOINT\]/);
});
