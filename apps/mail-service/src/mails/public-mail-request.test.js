import assert from "node:assert/strict";
import test from "node:test";

import { MetricsCollector } from "../metrics.js";
import { HttpExceptionFilter } from "../common/http-exception.filter.js";
import { serviceUnavailable } from "../common/http-exception.js";
import { MailPlayerRateLimiter } from "./mail-player-rate-limiter.js";
import {
  validateEmptyPlayerMutationBody,
  validateListQuery,
  validateMailId,
  validatePublicPlayerHeaders
} from "./public-mail-request.js";

class MemoryRateRedis {
  constructor() {
    this.values = new Map();
    this.ttls = new Map();
  }

  async incr(key) {
    const value = (this.values.get(key) || 0) + 1;
    this.values.set(key, value);
    return value;
  }

  async decr(key) {
    const value = (this.values.get(key) || 0) - 1;
    this.values.set(key, value);
    return value;
  }

  async pexpire(key, ttl) {
    this.ttls.set(key, ttl);
    return 1;
  }

  async pttl(key) {
    return this.ttls.get(key) || -1;
  }

  async del(key) {
    this.values.delete(key);
    this.ttls.delete(key);
    return 1;
  }
}

function trustedRequest(overrides = {}) {
  return {
    socket: { remoteAddress: "172.30.0.9" },
    rawHeaders: [
      "X-Game-Ticket", "payload.signature",
      "X-Forwarded-For", "203.0.113.8",
      "X-Real-IP", "203.0.113.8",
      "X-Forwarded-Proto", "https",
      "X-Request-ID", "5d6a6bf1-6a27-4bad-8e4e-3a51d9a7c0b5"
    ],
    ...overrides
  };
}

const trustedProxyConfig = {
  mailTrustProxy: true,
  mailTrustedProxyCidrs: ["172.30.0.0/24"]
};

test("public player request validation rejects client-controlled identity and malformed fields", () => {
  assert.deepEqual(validateListQuery({ status: "unread", limit: "20", offset: "5" }), {
    status: "unread",
    limit: 20,
    offset: 5
  });
  assert.throws(() => validateListQuery({ player_id: "player-attacker" }));
  assert.throws(() => validateListQuery({ limit: "51" }));
  assert.throws(() => validateMailId("mail/attacker"));
  assert.throws(() => validateEmptyPlayerMutationBody({ target_instance_id: "game-2" }));
  assert.throws(() => validateEmptyPlayerMutationBody({ attachments: [] }));
  assert.deepEqual(validateEmptyPlayerMutationBody({}), {});
  assert.doesNotThrow(() => validatePublicPlayerHeaders(
    { "accept-language": "zh-CN, en;q=0.9" },
    { socket: { remoteAddress: "127.0.0.1" } },
    "list",
    { mailTrustProxy: false }
  ));
  assert.doesNotThrow(() => validatePublicPlayerHeaders(
    { "accept-language": "*" },
    { socket: { remoteAddress: "127.0.0.1" } },
    "list",
    { mailTrustProxy: false }
  ));
  assert.throws(() => validatePublicPlayerHeaders(
    { "accept-language": "zh_CN" },
    { socket: { remoteAddress: "127.0.0.1" } },
    "list",
    { mailTrustProxy: false }
  ));
  assert.throws(() => validatePublicPlayerHeaders(
    { authorization: "Bearer player-ticket" },
    { socket: { remoteAddress: "127.0.0.1" } },
    "list",
    { mailTrustProxy: false }
  ));
});

test("public player request trusts forwarded identity only from configured Caddy CIDRs", () => {
  const trusted = validatePublicPlayerHeaders({}, trustedRequest(), "list", trustedProxyConfig);
  assert.deepEqual(trusted, {
    clientIp: "203.0.113.8",
    requestId: "5d6a6bf1-6a27-4bad-8e4e-3a51d9a7c0b5",
    trustedProxy: true
  });

  const untrusted = validatePublicPlayerHeaders({}, trustedRequest({
    socket: { remoteAddress: "198.51.100.99" }
  }), "list", trustedProxyConfig);
  assert.equal(untrusted.clientIp, "198.51.100.99");
  assert.equal(untrusted.trustedProxy, false);

  assert.throws(() => validatePublicPlayerHeaders({}, trustedRequest({
    rawHeaders: [
      "X-Forwarded-For", "203.0.113.8",
      "X-Real-IP", "203.0.113.9",
      "X-Forwarded-Proto", "https",
      "X-Request-ID", "5d6a6bf1-6a27-4bad-8e4e-3a51d9a7c0b5"
    ]
  }), "list", trustedProxyConfig));

  assert.throws(() => validatePublicPlayerHeaders({}, trustedRequest({
    rawHeaders: [
      "X-Game-Ticket", "one.signature",
      "X-Game-Ticket", "two.signature"
    ]
  }), "list", trustedProxyConfig));
});

test("mail player rate limiting separates read, scan, claim, IP, and concurrent claim controls", async () => {
  const limiter = new MailPlayerRateLimiter(new MemoryRateRedis(), {
    mailPublicRateLimitEnabled: true,
    mailPublicRateLimitWindowMs: 60_000,
    mailReadRateLimitPerPlayer: 1,
    mailReadRateLimitPerIp: 10,
    mailListScanRateLimitPerPlayer: 10,
    mailClaimRateLimitPerPlayer: 10,
    mailClaimRateLimitPerIp: 10,
    mailClaimConcurrentPerPlayer: 1,
    mailClaimConcurrencyLeaseMs: 15_000
  });

  assert.equal((await limiter.check("list", "player-1", "203.0.113.8")).limited, false);
  const readLimited = await limiter.check("detail", "player-1", "203.0.113.8");
  assert.equal(readLimited.limited, true);
  assert.equal(readLimited.dimension, "read:player");

  const claimLimiter = new MailPlayerRateLimiter(new MemoryRateRedis(), {
    ...limiter.config,
    mailReadRateLimitPerPlayer: 10
  });
  const firstClaim = await claimLimiter.acquireClaim("player-1");
  const concurrentClaim = await claimLimiter.acquireClaim("player-1");
  assert.equal(firstClaim.acquired, true);
  assert.equal(concurrentClaim.acquired, false);
  assert.equal(concurrentClaim.dimension, "claim_concurrency");
  await firstClaim.release();
  assert.equal((await claimLimiter.acquireClaim("player-1")).acquired, true);
});

test("public mail metrics aggregate fixed route and outcome fields without player labels", async () => {
  const published = [];
  const metrics = new MetricsCollector({
    async publishJson(subject, body) {
      published.push({ subject, body });
    }
  }, "mail-service", "mail-test-001");

  metrics.recordMailPublicRequest("list", 200, 12);
  metrics.recordMailPublicRequest("claim", 202, 36);
  metrics.recordMailPublicRequest("claim", 429, 3);
  metrics.recordMailPublicRateLimited("claim_concurrency");
  await metrics.flush();

  const payload = published[0].body.metrics;
  assert.equal(payload.mail_public_requests, 3);
  assert.equal(payload.mail_public_list_requests, 1);
  assert.equal(payload.mail_public_claim_requests, 2);
  assert.equal(payload.mail_public_accepted, 1);
  assert.equal(payload.mail_public_rate_limited, 1);
  assert.equal(payload.mail_public_claim_concurrency_limited, 1);
  assert.equal(
    Object.keys(payload)
      .filter((key) => key.startsWith("mail_public_"))
      .some((key) => /player|ip|mail_id/i.test(key)),
    false
  );
});

test("public mail error mapping keeps the stable category and removes backend diagnostics", () => {
  const response = {
    statusCode: 0,
    body: null,
    status(value) {
      this.statusCode = value;
      return this;
    },
    send(value) {
      this.body = value;
      return value;
    }
  };
  const host = {
    switchToHttp() {
      return {
        getRequest() { return { url: "/api/v1/mails/mail-1?ticket=forbidden" }; },
        getResponse() { return response; }
      };
    }
  };

  new HttpExceptionFilter().catch(
    serviceUnavailable("MAIL_AUTH_UNAVAILABLE", "redis://secret@127.0.0.1:6379 timed out"),
    host
  );

  assert.equal(response.statusCode, 503);
  assert.deepEqual(response.body, {
    ok: false,
    error: "MAIL_AUTH_UNAVAILABLE",
    message: "Mail service is temporarily unavailable"
  });
  assert.doesNotMatch(JSON.stringify(response.body), /redis|127\.0\.0\.1|secret/);
});
