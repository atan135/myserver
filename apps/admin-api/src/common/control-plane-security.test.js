import assert from "node:assert/strict";
import { register } from "node:module";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../../tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const {
  getClientIp,
  ipMatchesEntry,
  ipMatchesAny,
  isRequestSecure,
  isTrustedProxy
} = await import("./client-ip.ts");
const { evaluateControlPlaneSecurity } = await import("./control-plane-security.ts");

function req(remoteAddress, headers = {}, extra = {}) {
  return {
    headers,
    socket: { remoteAddress, encrypted: false },
    ...extra
  };
}

test("getClientIp ignores forwarded IPs when proxy is not trusted", () => {
  const request = req("10.0.0.10", { "x-forwarded-for": "203.0.113.8" });

  assert.equal(getClientIp(request, { trustProxy: false, trustedProxies: ["10.0.0.10"] }), "10.0.0.10");
  assert.equal(getClientIp(request, { trustProxy: true, trustedProxies: ["10.0.0.11"] }), "10.0.0.10");
});

test("trusted proxy check uses socket remote address before request ip", () => {
  const request = req("198.51.100.9", {
    "x-forwarded-for": "203.0.113.8",
    "x-forwarded-proto": "https"
  }, {
    ip: "10.0.0.10"
  });
  const config = { trustProxy: true, trustedProxies: ["10.0.0.10"] };

  assert.equal(isTrustedProxy(request, config), false);
  assert.equal(getClientIp(request, config), "198.51.100.9");
  assert.equal(isRequestSecure(request, config), false);
});

test("getClientIp uses forwarded IPs only from trusted proxies", () => {
  const request = req("10.0.0.10", { "x-forwarded-for": "203.0.113.8, 10.0.0.10" });

  assert.equal(getClientIp(request, { trustProxy: true, trustedProxies: ["10.0.0.10"] }), "203.0.113.8");
  assert.equal(isTrustedProxy(request, { trustProxy: true, trustedProxies: ["10.0.0.0/24"] }), true);
});

test("ip matching supports exact IP and IPv4 CIDR allowlist entries", () => {
  assert.equal(ipMatchesEntry("203.0.113.8", "203.0.113.8"), true);
  assert.equal(ipMatchesEntry("203.0.113.8", "203.0.113.0/24"), true);
  assert.equal(ipMatchesEntry("203.0.114.8", "203.0.113.0/24"), false);
  assert.equal(ipMatchesAny("::1", ["127.0.0.1", "::1"]), true);
});

test("isRequestSecure trusts forwarded proto only from trusted proxies", () => {
  const request = req("10.0.0.10", { "x-forwarded-proto": "https" });

  assert.equal(isRequestSecure(request, { trustProxy: false, trustedProxies: ["10.0.0.10"] }), false);
  assert.equal(isRequestSecure(request, { trustProxy: true, trustedProxies: ["10.0.0.11"] }), false);
  assert.equal(isRequestSecure(request, { trustProxy: true, trustedProxies: ["10.0.0.10"] }), true);
  assert.equal(isRequestSecure(req("203.0.113.8", {}, { secure: true }), {}), true);
});

test("evaluateControlPlaneSecurity rejects non-TLS requests when required", () => {
  const result = evaluateControlPlaneSecurity(req("203.0.113.8"), {
    adminApiRequireTls: true,
    adminApiRequireIpAllowlist: false
  });

  assert.equal(result.ok, false);
  assert.equal(result.statusCode, 426);
  assert.equal(result.error, "ADMIN_API_TLS_REQUIRED");
});

test("evaluateControlPlaneSecurity allows and rejects by client IP allowlist", () => {
  const config = {
    adminApiRequireTls: false,
    adminApiRequireIpAllowlist: true,
    adminApiIpAllowlist: ["203.0.113.0/24"]
  };

  assert.equal(evaluateControlPlaneSecurity(req("203.0.113.8"), config).ok, true);

  const rejected = evaluateControlPlaneSecurity(req("198.51.100.8"), config);
  assert.equal(rejected.ok, false);
  assert.equal(rejected.statusCode, 403);
  assert.equal(rejected.error, "ADMIN_API_IP_NOT_ALLOWED");
});
