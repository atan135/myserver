import assert from "node:assert/strict";
import test from "node:test";

import { HttpException, NotFoundException } from "@nestjs/common";

import { HttpExceptionFilter } from "./http-exception.filter.js";
import { TlsRequiredMiddleware } from "./tls-required.middleware.js";

function createMiddleware(auditEvents = []) {
  return new TlsRequiredMiddleware(
    { authRequireTls: true },
    {
      appendSecurityAudit: async (event) => {
        auditEvents.push(event);
      }
    }
  );
}

test("TLS middleware allows the exact internal Docker healthcheck path", async () => {
  const auditEvents = [];
  const middleware = createMiddleware(auditEvents);
  let nextCalled = false;

  await middleware.use(
    { url: "/", raw: { url: "/healthz" }, socket: { remoteAddress: "127.0.0.1" } },
    {},
    () => {
      nextCalled = true;
    }
  );

  assert.equal(nextCalled, true);
  assert.deepEqual(auditEvents, []);
});

test("TLS middleware keeps non-exact healthcheck paths behind HTTPS", async () => {
  const auditEvents = [];
  const middleware = createMiddleware(auditEvents);

  await assert.rejects(
    middleware.use(
      { url: "/healthz?verbose=1", socket: { remoteAddress: "127.0.0.1" } },
      {},
      () => {}
    ),
    (error) => error instanceof HttpException && error.getStatus() === 426
  );

  assert.equal(auditEvents.length, 1);
  assert.equal(auditEvents[0].eventType, "auth_tls_required");
});

test("TLS middleware allows token-protected internal service routes on the private application port", async () => {
  const auditEvents = [];
  const middleware = createMiddleware(auditEvents);
  let nextCalled = false;

  await middleware.use(
    {
      url: "/api/v1/internal/characters",
      raw: { url: "/api/v1/internal/characters" },
      socket: { remoteAddress: "172.30.0.20" }
    },
    {},
    () => {
      nextCalled = true;
    }
  );

  assert.equal(nextCalled, true);
  assert.deepEqual(auditEvents, []);
});

test("auth HTTP exception filter writes JSON through a Fastify raw response", () => {
  const response = {
    headers: {},
    setHeader(name, value) {
      this.headers[name] = value;
    },
    end(payload) {
      this.payload = payload;
    }
  };
  const host = {
    switchToHttp: () => ({
      getRequest: () => ({ url: "/healthz" }),
      getResponse: () => response
    })
  };

  new HttpExceptionFilter().catch(
    new HttpException({ ok: false, error: "AUTH_TLS_REQUIRED" }, 426),
    host
  );

  assert.equal(response.statusCode, 426);
  assert.equal(response.headers["content-type"], "application/json");
  assert.deepEqual(JSON.parse(response.payload), { ok: false, error: "AUTH_TLS_REQUIRED" });
});

test("auth HTTP exception filter does not echo an unknown request URL", () => {
  const response = {
    status(statusCode) {
      this.statusCode = statusCode;
      return this;
    },
    send(payload) {
      this.payload = payload;
    }
  };
  const host = {
    switchToHttp: () => ({
      getRequest: () => ({ url: "/login?redirect=/" }),
      getResponse: () => response
    })
  };

  new HttpExceptionFilter().catch(new NotFoundException(), host);

  assert.equal(response.statusCode, 404);
  assert.deepEqual(response.payload, { ok: false, error: "NOT_FOUND" });
});
