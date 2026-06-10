import assert from "node:assert/strict";
import { test } from "node:test";

import { MySqlMailStore } from "../apps/mail-service/src/mysql-store.js";

async function createMail(store, overrides = {}) {
  const mail = {
    mail_id: "mail_001",
    sender_type: "system",
    sender_id: "system",
    sender_name: "系统",
    from_player_id: "system",
    to_player_id: "player_001",
    title: "Reward",
    content: "",
    attachments: [{ type: "item", id: 1001, count: 2 }],
    mail_type: "system",
    created_by_type: "system",
    created_by_id: "system",
    created_by_name: "系统",
    created_at: Date.now(),
    ...overrides
  };

  await store.createMail(mail);
  return mail;
}

test("mail attachment claim can only be reserved once before completion", async () => {
  const store = new MySqlMailStore(null);
  await createMail(store);

  const first = await store.beginClaimAttachments("mail_001");
  const second = await store.beginClaimAttachments("mail_001");

  assert.equal(first.reserved, true);
  assert.equal(first.inProgress, false);
  assert.equal(first.alreadyClaimed, false);
  assert.equal(first.mail.status, "claiming");

  assert.equal(second.reserved, false);
  assert.equal(second.inProgress, true);
  assert.equal(second.alreadyClaimed, false);
});

test("completed mail attachment claim is idempotent for later attempts", async () => {
  const store = new MySqlMailStore(null);
  await createMail(store);

  const reserved = await store.beginClaimAttachments("mail_001");
  assert.equal(reserved.reserved, true);

  const completed = await store.completeClaimAttachments("mail_001");
  const retry = await store.beginClaimAttachments("mail_001");

  assert.equal(completed.claimed, true);
  assert.equal(completed.mail.status, "claimed");
  assert.ok(completed.mail.claimed_at);

  assert.equal(retry.reserved, false);
  assert.equal(retry.inProgress, false);
  assert.equal(retry.alreadyClaimed, true);
  assert.equal(retry.mail.status, "claimed");
});

test("failed mail attachment claim can be released for retry", async () => {
  const store = new MySqlMailStore(null);
  await createMail(store);

  const reserved = await store.beginClaimAttachments("mail_001");
  assert.equal(reserved.reserved, true);

  const released = await store.releaseClaimAttachments("mail_001");
  const afterRelease = await store.getMailById("mail_001");
  const retry = await store.beginClaimAttachments("mail_001");

  assert.equal(released, true);
  assert.equal(afterRelease.status, "unread");
  assert.equal(afterRelease.claimed_at, null);

  assert.equal(retry.reserved, true);
  assert.equal(retry.mail.status, "claiming");
});

test("mail creation writes notification outbox in memory store", async () => {
  const store = new MySqlMailStore(null);
  await createMail(store);

  const outbox = await store.getMailNotificationOutboxByMailId("mail_001");

  assert.equal(outbox.mail_id, "mail_001");
  assert.equal(outbox.to_player_id, "player_001");
  assert.equal(outbox.status, "pending");
  assert.equal(outbox.attempts, 0);
  assert.equal(outbox.payload.to_player_id, "player_001");
  assert.equal(outbox.payload.mail.mail_id, "mail_001");
});

test("mail notification outbox can be reserved, failed, retried, and marked sent", async () => {
  const store = new MySqlMailStore(null);
  await createMail(store);

  const firstReserve = await store.reservePendingMailNotificationOutbox(10);
  assert.equal(firstReserve.length, 1);
  assert.equal(firstReserve[0].status, "sending");
  assert.equal(firstReserve[0].attempts, 1);

  await store.markMailNotificationOutboxFailed(firstReserve[0].id, "nats down");
  let outbox = await store.getMailNotificationOutboxByMailId("mail_001");
  assert.equal(outbox.status, "failed");
  assert.equal(outbox.attempts, 1);
  assert.equal(outbox.last_error, "nats down");

  outbox.next_attempt_at = new Date(Date.now() - 1);
  store.memoryOutbox.set(outbox.id, outbox);

  const retryReserve = await store.reservePendingMailNotificationOutbox(10);
  assert.equal(retryReserve.length, 1);
  assert.equal(retryReserve[0].attempts, 2);

  await store.markMailNotificationOutboxSent(retryReserve[0].id);
  outbox = await store.getMailNotificationOutboxByMailId("mail_001");
  assert.equal(outbox.status, "sent");
  assert.ok(outbox.sent_at);
});
