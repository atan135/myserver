import assert from "node:assert/strict";
import { test } from "node:test";

import { DbMailStore } from "../../apps/mail-service/src/db-store.js";
import { MetricsCollector } from "../../apps/mail-service/src/metrics.js";
import {
  buildMailNotificationEvent,
  calculateOutboxBackoffMs,
  normalizeMailNotificationEvent,
  PermanentOutboxPayloadError
} from "../../apps/mail-service/src/notification-outbox.js";

function mail(overrides = {}) {
  return {
    mail_id: "mail_001",
    sender_id: "system",
    sender_name: "系统",
    from_player_id: "system",
    to_player_id: "player_001",
    title: "Reward",
    mail_type: "system",
    created_at: 1_783_931_896_000,
    ...overrides
  };
}

test("notification event ids remain stable and satisfy the v1 contract", () => {
  const event = buildMailNotificationEvent(mail(), {
    traceId: "0123456789abcdef0123456789abcdef"
  });
  assert.deepEqual(event, {
    event_id: "mail.notify:mail_001",
    event_type: "mail.created",
    version: 1,
    occurred_at: 1_783_931_896_000,
    player_id: "player_001",
    mail: {
      mail_id: "mail_001",
      title: "Reward",
      from_player_id: "system",
      from_name: "系统",
      mail_type: "system",
      created_at: 1_783_931_896_000
    },
    trace_id: "0123456789abcdef0123456789abcdef"
  });
});

test("legacy nested outbox payload is upgraded to a stable v1 envelope", () => {
  const entry = {
    mail_id: "mail_legacy",
    to_player_id: "player_legacy",
    created_at: new Date("2026-07-13T08:00:00.000Z"),
    payload: {
      to_player_id: "player_legacy",
      mail: {
        mail_id: "mail_legacy",
        sender_id: "system",
        sender_name: "系统",
        title: "Legacy",
        mail_type: "system",
        created_at: 1_783_929_600_000
      }
    }
  };
  const first = normalizeMailNotificationEvent(entry);
  const second = normalizeMailNotificationEvent(entry);
  assert.equal(first.event_id, "mail.notify:mail_legacy");
  assert.equal(first.trace_id, second.trace_id);
  assert.match(first.trace_id, /^[0-9a-f]{32}$/);
  assert.equal(first.player_id, "player_legacy");
});

test("invalid permanent payload is classified without retry", () => {
  assert.throws(
    () => normalizeMailNotificationEvent({
      mail_id: "mail_001",
      to_player_id: "player_001",
      payload: { event_type: "mail.deleted", mail: { mail_id: "mail_001" } }
    }),
    (error) => error instanceof PermanentOutboxPayloadError
      && error.code === "UNSUPPORTED_MAIL_NOTIFICATION_TYPE"
  );
});

test("bounded exponential backoff supports deterministic jitter injection", () => {
  assert.equal(calculateOutboxBackoffMs(1, { baseMs: 1000, maxMs: 5000, jitterRatio: 0.2 }, () => 0), 800);
  assert.equal(calculateOutboxBackoffMs(2, { baseMs: 1000, maxMs: 5000, jitterRatio: 0.2 }, () => 0.5), 2000);
  assert.equal(calculateOutboxBackoffMs(20, { baseMs: 1000, maxMs: 5000, jitterRatio: 0.2 }, () => 1), 5000);
});

test("expired lease can be taken over and stale holder cannot complete", async () => {
  const store = new DbMailStore(null, { outboxLeaseMs: 10_000 });
  await store.createMail(mail());

  const [first] = await store.reservePendingMailNotificationOutbox(1, {
    leaseOwner: "mail-a",
    leaseToken: "lease-a"
  });
  assert.equal((await store.reservePendingMailNotificationOutbox(1)).length, 0);

  const row = store.memoryOutbox.get(first.id);
  row.locked_until = new Date(Date.now() - 1);
  const [takenOver] = await store.reservePendingMailNotificationOutbox(1, {
    leaseOwner: "mail-b",
    leaseToken: "lease-b"
  });
  assert.equal(takenOver.lease_taken_over, true);
  assert.equal(await store.markMailNotificationOutboxSent(first.id, "lease-a"), false);
  assert.equal(await store.markMailNotificationOutboxSent(first.id, "lease-b"), true);
});

test("outbox state transitions require the current sending lease token", async () => {
  const cases = [
    {
      name: "sent",
      transition: (store, id, token) => store.markMailNotificationOutboxSent(id, token)
    },
    {
      name: "failed",
      transition: (store, id, token) => store.markMailNotificationOutboxFailed(id, "nats down", { leaseToken: token })
    },
    {
      name: "terminal",
      transition: (store, id, token) => store.markMailNotificationOutboxTerminal(id, "invalid", { leaseToken: token })
    }
  ];

  for (const item of cases) {
    const store = new DbMailStore(null);
    await store.createMail(mail({ mail_id: `mail_${item.name}` }));
    const [reserved] = await store.reservePendingMailNotificationOutbox(1, {
      leaseToken: `lease-${item.name}`
    });
    assert.equal(await item.transition(store, reserved.id, undefined), false);
    assert.equal(await item.transition(store, reserved.id, "wrong-token"), false);

    const row = store.memoryOutbox.get(reserved.id);
    row.status = "failed";
    assert.equal(await item.transition(store, reserved.id, reserved.lease_token), false);
    row.status = "sending";
    assert.equal(await item.transition(store, reserved.id, reserved.lease_token), true);
    assert.equal((await store.getMailNotificationOutboxByMailId(`mail_${item.name}`)).status, item.name);
  }
});

test("PostgreSQL outbox transitions reject missing tokens and constrain valid updates", async () => {
  const calls = [];
  const store = new DbMailStore({
    async query(sql, params) {
      calls.push({ sql, params });
      return { rowCount: 0 };
    }
  });
  assert.equal(await store.markMailNotificationOutboxSent(1), false);
  assert.equal(await store.markMailNotificationOutboxFailed(1, "failed"), false);
  assert.equal(await store.markMailNotificationOutboxTerminal(1, "terminal"), false);
  assert.equal(calls.length, 0);

  assert.equal(await store.markMailNotificationOutboxSent(1, "lease-current"), false);
  assert.equal(calls.length, 1);
  assert.match(calls[0].sql, /status = 'sending'/);
  assert.match(calls[0].sql, /lease_token = \$2/);
  assert.deepEqual(calls[0].params, [1, "lease-current"]);
});

test("sent and terminal rows are cleaned using separate retention windows", async () => {
  const store = new DbMailStore(null);
  await store.createMail(mail({ mail_id: "mail_sent" }));
  await store.createMail(mail({ mail_id: "mail_terminal" }));
  const [sent] = await store.reservePendingMailNotificationOutbox(1, { leaseToken: "sent-lease" });
  await store.markMailNotificationOutboxSent(sent.id, "sent-lease");
  const [terminal] = await store.reservePendingMailNotificationOutbox(1, { leaseToken: "terminal-lease" });
  await store.markMailNotificationOutboxTerminal(terminal.id, "invalid payload", { leaseToken: "terminal-lease" });
  store.memoryOutbox.get(sent.id).sent_at = new Date(0);
  store.memoryOutbox.get(terminal.id).terminal_at = new Date(0);

  const deleted = await store.cleanupMailNotificationOutbox({
    now: new Date(10_000),
    sentRetentionMs: 1000,
    terminalRetentionMs: 5000,
    limit: 10
  });
  assert.equal(deleted, 2);
  assert.equal(store.memoryOutbox.size, 0);
});

test("mail metrics expose outbox and claim routing failures separately", async () => {
  const published = [];
  const collector = new MetricsCollector({
    async publishJson(subject, payload) {
      published.push({ subject, payload });
    }
  }, "mail-service", "mail-001");
  collector.setOutboxSnapshot({ backlog: 3, oldestAgeMs: 9000 });
  collector.recordOutboxPublished(500);
  collector.recordOutboxRetry();
  collector.recordOutboxTerminal();
  collector.recordOutboxLeaseTakeover();
  collector.recordMailClaimRouteUnavailable();
  collector.recordMailClaimGrantFailure();
  collector.recordMailClaimRecoveryAcquired();
  collector.recordMailClaimRecovered();
  collector.recordMailClaimRecoveryUnknownAge(12_000);
  collector.recordMailClaimRecoveryQueryResult("succeeded");
  collector.recordMailClaimRecoveryQueryResult("not_seen");
  collector.recordMailClaimRecoveryQueryResult("conflict");
  collector.recordMailClaimRecoveryQueryResult("result_unavailable");
  collector.recordMailClaimRecoveryGrantRetry();
  collector.recordMailClaimRecoveryLeaseTakeover();
  collector.recordMailClaimRecoveryManualReview();
  collector.recordMailClaimRecoveryDuration(2500);
  await collector.flush();

  const metrics = published[0].payload.metrics;
  assert.equal(metrics.mail_outbox_backlog, 3);
  assert.equal(metrics.mail_outbox_oldest_age_ms, 9000);
  assert.equal(metrics.mail_outbox_publish_latency_ms, 500);
  assert.equal(metrics.mail_outbox_retries, 1);
  assert.equal(metrics.mail_outbox_terminal, 1);
  assert.equal(metrics.mail_outbox_lease_takeovers, 1);
  assert.equal(metrics.mail_claim_route_unavailable, 1);
  assert.equal(metrics.mail_claim_grant_failures, 1);
  assert.equal(metrics.mail_claim_recovery_acquired, 1);
  assert.equal(metrics.mail_claim_recovered, 1);
  assert.equal(metrics.mail_claim_recovery_unknown_age_ms, 12000);
  assert.equal(metrics.mail_claim_recovery_query_succeeded, 1);
  assert.equal(metrics.mail_claim_recovery_query_not_seen, 1);
  assert.equal(metrics.mail_claim_recovery_query_conflict, 1);
  assert.equal(metrics.mail_claim_recovery_query_unavailable, 1);
  assert.equal(metrics.mail_claim_recovery_grant_retries, 1);
  assert.equal(metrics.mail_claim_recovery_lease_takeovers, 1);
  assert.equal(metrics.mail_claim_recovery_manual_reviews, 1);
  assert.equal(metrics.mail_claim_recovery_duration_ms, 2500);
});
