import assert from "node:assert/strict";
import test from "node:test";

import { formatLogPayload } from "./logger.js";

test("admin logger redacts credentials and endpoint details", () => {
  const payload = formatLogPayload("[admin.request]", {
    authorization: "Bearer raw-token",
    databaseUrl: "postgresql://user:pw@db:5432/admin",
    error: new Error("ticket=raw-ticket")
  });
  assert.doesNotMatch(payload, /raw-token|raw-ticket|user:pw/);
  assert.match(payload, /REDACTED/);
  assert.match(payload, /postgresql:\/\/\[REDACTED_ENDPOINT\]/);
});
