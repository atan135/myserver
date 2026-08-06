import assert from "node:assert/strict";
import test from "node:test";

import { RedisBlocklistMiddleware } from "./redis-blocklist.middleware.js";

function request(path: string) {
  return {
    url: path,
    headers: {},
    socket: { remoteAddress: "127.0.0.1" }
  };
}

test("RedisBlocklistMiddleware rejects blocked auth IPs with the stable error code", async () => {
  const audits: any[] = [];
  const middleware = new RedisBlocklistMiddleware(
    { trustProxy: false, trustedProxies: [] },
    { async checkIp() { return { blocked: true }; } },
    { async appendSecurityAudit(entry: any) { audits.push(entry); } }
  );

  await assert.rejects(
    () => middleware.use(request("/api/v1/auth/login"), {}, () => assert.fail("next must not run")),
    (error: any) => error.getStatus() === 403 && error.getResponse().error === "IP_BLOCKED"
  );
  assert.deepEqual(audits.map((entry) => entry.eventType), ["ip_blocked"]);
});

test("RedisBlocklistMiddleware leaves routes outside its public guard untouched", async () => {
  let checked = false;
  let continued = false;
  const middleware = new RedisBlocklistMiddleware(
    { trustProxy: false, trustedProxies: [] },
    { async checkIp() { checked = true; return { blocked: true }; } },
    null
  );

  await middleware.use(request("/healthz"), {}, () => { continued = true; });
  assert.equal(checked, false);
  assert.equal(continued, true);
});
