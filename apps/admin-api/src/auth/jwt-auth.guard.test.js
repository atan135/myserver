import assert from "node:assert/strict";
import { register } from "node:module";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../../tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const { JwtAuthGuard } = await import("./jwt-auth.guard.ts");

function makeContext(req) {
  function handler() {}

  return {
    getHandler: () => handler,
    switchToHttp: () => ({
      getRequest: () => req
    })
  };
}

function makeRequest(headers = {}) {
  return {
    method: "GET",
    url: "/api/v1/auth/me",
    headers,
    socket: {
      remoteAddress: "198.51.100.20"
    }
  };
}

function makeRequestWithUrl(url, headers = {}) {
  return {
    ...makeRequest(headers),
    url
  };
}

function assertUnauthorized(errorCode) {
  return (error) => {
    assert.equal(error.getStatus(), 401);
    assert.equal(error.getResponse().error, errorCode);
    return true;
  };
}

test("JwtAuthGuard writes security audit for missing token", async () => {
  const audits = [];
  const guard = new JwtAuthGuard(
    { verifyAsync: async () => assert.fail("verifyAsync should not be called") },
    {},
    {
      async appendSecurityAuditLog(entry) {
        audits.push(entry);
      }
    },
    {}
  );

  await assert.rejects(() => guard.canActivate(makeContext(makeRequest())), assertUnauthorized("MISSING_TOKEN"));

  assert.equal(audits.length, 1);
  assert.equal(audits[0].eventType, "admin_auth_denied");
  assert.equal(audits[0].severity, "warning");
  assert.equal(audits[0].clientIp, "198.51.100.20");
  assert.equal(audits[0].details.errorCode, "MISSING_TOKEN");
  assert.equal(audits[0].details.hasJti, false);
  assert.equal(JSON.stringify(audits[0]).includes("Bearer"), false);
});

test("JwtAuthGuard writes security audit for invalid token without leaking token", async () => {
  const audits = [];
  const rawToken = "raw.jwt.value";
  const guard = new JwtAuthGuard(
    {
      async verifyAsync() {
        throw new Error("bad signature");
      }
    },
    { jwtSecret: "secret" },
    {
      async appendSecurityAuditLog(entry) {
        audits.push(entry);
      }
    },
    {}
  );

  await assert.rejects(
    () => guard.canActivate(makeContext(makeRequest({ authorization: `Bearer ${rawToken}` }))),
    assertUnauthorized("INVALID_TOKEN")
  );

  assert.equal(audits.length, 1);
  assert.equal(audits[0].eventType, "admin_auth_denied");
  assert.equal(audits[0].details.errorCode, "INVALID_TOKEN");
  assert.equal(JSON.stringify(audits[0]).includes(rawToken), false);
  assert.equal(JSON.stringify(audits[0]).includes("secret"), false);
});

test("JwtAuthGuard audit details strip query parameters", async () => {
  const audits = [];
  const guard = new JwtAuthGuard(
    { verifyAsync: async () => assert.fail("verifyAsync should not be called") },
    {},
    {
      async appendSecurityAuditLog(entry) {
        audits.push(entry);
      }
    },
    {}
  );

  await assert.rejects(
    () => guard.canActivate(makeContext(makeRequestWithUrl("/api/v1/auth/me?access_token=bad"))),
    assertUnauthorized("MISSING_TOKEN")
  );

  assert.equal(audits[0].details.path, "/api/v1/auth/me");
  assert.equal(JSON.stringify(audits[0]).includes("access_token"), false);
});

test("JwtAuthGuard audit write failure does not change unauthorized result", async () => {
  const guard = new JwtAuthGuard(
    { verifyAsync: async () => assert.fail("verifyAsync should not be called") },
    {},
    {
      async appendSecurityAuditLog() {
        throw new Error("database unavailable");
      }
    },
    {}
  );

  await assert.rejects(() => guard.canActivate(makeContext(makeRequest())), assertUnauthorized("MISSING_TOKEN"));
});
