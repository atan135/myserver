import assert from "node:assert/strict";
import test from "node:test";

import { formatLogPayload } from "./logger.js";

test("auth logger redacts credentials, endpoint credentials and error details", () => {
  const payload = formatLogPayload("request failed", {
    token: "raw-token",
    endpoint: "postgres://user:pw@db:5432/auth",
    error: new Error("password=raw-secret")
  });
  assert.doesNotMatch(payload, /raw-token|raw-secret|user:pw/);
  assert.match(payload, /REDACTED/);
  assert.match(payload, /postgres:\/\/\[REDACTED_ENDPOINT\]/);
});
