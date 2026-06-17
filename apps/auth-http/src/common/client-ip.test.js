import assert from "node:assert/strict";
import test from "node:test";

import { getClientIp, isRequestSecure, isTrustedProxy } from "./client-ip.js";

function req(remoteAddress, headers = {}, extra = {}) {
  return {
    headers,
    socket: { remoteAddress, encrypted: false },
    ...extra
  };
}

test("auth-http client IP ignores forwarded IPs when proxy is not trusted", () => {
  const request = req("10.0.0.10", { "x-forwarded-for": "203.0.113.8" });

  assert.equal(getClientIp(request, { trustProxy: false, trustedProxies: ["10.0.0.10"] }), "10.0.0.10");
  assert.equal(getClientIp(request, { trustProxy: true, trustedProxies: ["10.0.0.11"] }), "10.0.0.10");
});

test("auth-http client IP accepts forwarded IPs from trusted proxy CIDR", () => {
  const request = req("10.0.0.10", { "x-forwarded-for": "203.0.113.8, 10.0.0.10" });

  assert.equal(getClientIp(request, { trustProxy: true, trustedProxies: ["10.0.0.0/24"] }), "203.0.113.8");
  assert.equal(isTrustedProxy(request, { trustProxy: true, trustedProxies: ["10.0.0.0/24"] }), true);
});

test("auth-http TLS check trusts forwarded proto only from trusted proxies", () => {
  const request = req("10.0.0.10", { "x-forwarded-proto": "https" });

  assert.equal(isRequestSecure(request, { trustProxy: false, trustedProxies: ["10.0.0.10"] }), false);
  assert.equal(isRequestSecure(request, { trustProxy: true, trustedProxies: ["10.0.0.11"] }), false);
  assert.equal(isRequestSecure(request, { trustProxy: true, trustedProxies: ["10.0.0.10"] }), true);
});

test("auth-http TLS check accepts direct encrypted socket", () => {
  const request = req("127.0.0.1", {}, { socket: { remoteAddress: "127.0.0.1", encrypted: true } });

  assert.equal(isRequestSecure(request, { trustProxy: false }), true);
});
