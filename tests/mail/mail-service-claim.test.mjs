import assert from "node:assert/strict";
import { mkdir, readFile, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { test } from "node:test";
import ts from "typescript";
import { pathToFileURL } from "node:url";

import "reflect-metadata";
import { DbMailStore } from "../../apps/mail-service/src/db-store.js";

async function loadMailsService() {
  const sourcePath = path.resolve("apps/mail-service/src/mails/mails.service.ts");
  const outDir = path.resolve("tests/.tmp/mail-service-claim");
  const outPath = path.join(outDir, "mails.service.mjs");
  const source = await readFile(sourcePath, "utf8");
  const compiled = ts.transpileModule(source, {
    compilerOptions: {
      target: ts.ScriptTarget.ES2022,
      module: ts.ModuleKind.ES2022,
      moduleResolution: ts.ModuleResolutionKind.NodeNext,
      experimentalDecorators: true,
      emitDecoratorMetadata: true
    },
    fileName: sourcePath
  });

  await rm(outDir, { recursive: true, force: true });
  await mkdir(path.join(outDir, "common"), { recursive: true });
  const outputText = compiled.outputText
    .replaceAll("../common/", "./common/")
    .replaceAll("../global-id.js", "./global-id.js")
    .replaceAll("../logger.js", "./logger.js")
    .replaceAll("../notification-outbox.js", "./notification-outbox.js")
    .replaceAll("../tokens.js", "./tokens.js");

  await writeFile(outPath, outputText, "utf8");
  await writeFile(
    path.join(outDir, "common/http-exception.js"),
    "export * from '../../../../apps/mail-service/src/common/http-exception.ts';\n",
    "utf8"
  );
  await writeFile(
    path.join(outDir, "global-id.js"),
    "let nextId = 1; export function generateMailId() { return `mail_generated_${nextId++}`; }\n",
    "utf8"
  );
  await writeFile(
    path.join(outDir, "logger.js"),
    "export function log() {}\n",
    "utf8"
  );
  await writeFile(
    path.join(outDir, "notification-outbox.js"),
    "export * from '../../../apps/mail-service/src/notification-outbox.js';\n",
    "utf8"
  );
  await writeFile(
    path.join(outDir, "tokens.js"),
    "export * from '../../../apps/mail-service/src/tokens.ts';\n",
    "utf8"
  );

  const imported = await import(`${pathToFileURL(outPath).href}?v=${Date.now()}`);
  return imported.MailsService;
}

function createService(options = {}) {
  const MailsService = createService.MailsService;
  const mailStore = new DbMailStore(null, options.storeOptions);
  const pubsubClient = {
    publishes: [],
    failuresRemaining: 0,
    async publishMailNotification(playerId, eventOrMail) {
      const mail = eventOrMail.event_type === "mail.created" ? eventOrMail.mail : eventOrMail;
      this.publishes.push({ playerId, mailId: mail.mail_id, event: eventOrMail });
      if (this.failuresRemaining > 0) {
        this.failuresRemaining -= 1;
        throw new Error("nats down");
      }
    }
  };
  const gameAdminClient = {
    grants: [],
    async grantMailAttachments(characterId, requestId, attachments, reason, options) {
      this.grants.push({ characterId, requestId, attachments, reason, options });
      return { ok: true };
    }
  };

  return {
    mailStore,
    pubsubClient,
    gameAdminClient,
    service: new MailsService(mailStore, pubsubClient, gameAdminClient, options.config || {}, options.metrics || null)
  };
}

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

test("MailsService claim grants once with stable requestId and repeats idempotently", async () => {
  createService.MailsService = await loadMailsService();
  const { service, mailStore, gameAdminClient } = createService();
  await createMail(mailStore);

  const first = await service.claim("mail_001", "player_001", "chr_0000000000001", { player_id: "player_001" });
  const second = await service.claim("mail_001", "player_001", "chr_0000000000001", { player_id: "player_001" });

  assert.equal(first.ok, true);
  assert.equal(first.claimed, true);
  assert.equal(first.already_claimed, false);
  assert.equal(first.status, "claimed");

  assert.equal(second.ok, true);
  assert.equal(second.claimed, false);
  assert.equal(second.already_claimed, true);
  assert.equal(second.status, "claimed");

  assert.equal(gameAdminClient.grants.length, 1);
  assert.equal(gameAdminClient.grants[0].characterId, "chr_0000000000001");
  assert.equal(gameAdminClient.grants[0].requestId, "mail_claim:mail_001");
});

test("MailsService claim passes camelCase targetInstanceId to game admin grant", async () => {
  createService.MailsService = await loadMailsService();
  const { service, mailStore, gameAdminClient } = createService({
    config: { localDiscoveryFallbackEnabled: true, registryDiscoveryRequired: false }
  });
  await createMail(mailStore);

  await service.claim("mail_001", "player_001", "chr_0000000000001", {
    player_id: "player_001",
    targetInstanceId: "game-server-b"
  });

  assert.equal(gameAdminClient.grants.length, 1);
  assert.deepEqual(gameAdminClient.grants[0].options, { targetInstanceId: "game-server-b" });
});

test("MailsService claim accepts snake_case target_instance_id for game admin grant", async () => {
  createService.MailsService = await loadMailsService();
  const { service, mailStore, gameAdminClient } = createService({
    config: { localDiscoveryFallbackEnabled: true, registryDiscoveryRequired: false }
  });
  await createMail(mailStore);

  await service.claim("mail_001", "player_001", "chr_0000000000001", {
    player_id: "player_001",
    target_instance_id: "game-server-c"
  });

  assert.equal(gameAdminClient.grants.length, 1);
  assert.deepEqual(gameAdminClient.grants[0].options, { targetInstanceId: "game-server-c" });
});

test("MailsService create keeps mail and outbox when notification publish fails", async () => {
  createService.MailsService = await loadMailsService();
  const { service, mailStore, pubsubClient } = createService();
  pubsubClient.failuresRemaining = 1;

  const result = await service.create({
    to_player_id: "player_001",
    title: "Reward",
    content: "claim it"
  });

  assert.equal(result.ok, true);
  const mail = await mailStore.getMailById(result.mail_id);
  const outbox = await mailStore.getMailNotificationOutboxByMailId(result.mail_id);

  assert.equal(mail.title, "Reward");
  assert.equal(outbox.status, "failed");
  assert.equal(outbox.attempts, 1);
  assert.equal(outbox.last_error, "nats down");
});

test("MailsService retry sends pending notification outbox and marks it sent", async () => {
  createService.MailsService = await loadMailsService();
  const { service, mailStore, pubsubClient } = createService();
  pubsubClient.failuresRemaining = 1;

  const result = await service.create({
    to_player_id: "player_001",
    title: "Reward"
  });

  let outbox = await mailStore.getMailNotificationOutboxByMailId(result.mail_id);
  outbox.next_attempt_at = new Date(Date.now() - 1);
  mailStore.memoryOutbox.set(outbox.id, outbox);

  const retry = await service.processPendingNotificationOutbox();
  outbox = await mailStore.getMailNotificationOutboxByMailId(result.mail_id);

  assert.equal(retry.processed, 1);
  assert.equal(retry.sent, 1);
  assert.equal(retry.failed, 0);
  assert.equal(outbox.status, "sent");
  assert.equal(outbox.attempts, 2);
  assert.deepEqual(pubsubClient.publishes.map((publish) => publish.mailId), [result.mail_id, result.mail_id]);
});

test("MailsService create marks outbox sent when immediate publish succeeds", async () => {
  createService.MailsService = await loadMailsService();
  const { service, mailStore, pubsubClient } = createService();

  const result = await service.create({
    to_player_id: "player_001",
    title: "Reward"
  });

  const outbox = await mailStore.getMailNotificationOutboxByMailId(result.mail_id);

  assert.equal(outbox.status, "sent");
  assert.equal(outbox.attempts, 1);
  assert.equal(pubsubClient.publishes.length, 1);
});

test("MailsService terminates notification after configured maximum attempts", async () => {
  createService.MailsService = await loadMailsService();
  const { service, mailStore, pubsubClient } = createService({
    storeOptions: { outboxMaxAttempts: 1 },
    config: { outboxBackoffJitterRatio: 0 }
  });
  pubsubClient.failuresRemaining = 1;

  const result = await service.create({ to_player_id: "player_001", title: "Reward" });
  const outbox = await mailStore.getMailNotificationOutboxByMailId(result.mail_id);
  assert.equal(result.ok, true);
  assert.equal(outbox.status, "terminal");
  assert.ok(outbox.terminal_at);
  assert.match(outbox.last_error, /nats down/);
});

test("MailsService terminates permanently invalid notification payload without publishing", async () => {
  createService.MailsService = await loadMailsService();
  const { service, mailStore, pubsubClient } = createService();
  await createMail(mailStore, { mail_id: "mail_invalid_event" });
  const outbox = await mailStore.getMailNotificationOutboxByMailId("mail_invalid_event");
  outbox.payload.event_type = "mail.deleted";
  mailStore.memoryOutbox.set(outbox.id, outbox);

  const result = await service.processPendingNotificationOutbox();
  const terminal = await mailStore.getMailNotificationOutboxByMailId("mail_invalid_event");
  assert.equal(result.terminal, 1);
  assert.equal(result.failed, 0);
  assert.equal(terminal.status, "terminal");
  assert.match(terminal.last_error, /UNSUPPORTED_MAIL_NOTIFICATION_TYPE/);
  assert.equal(pubsubClient.publishes.length, 0);
});

test("MailsService never starts a publish beyond max attempts after repeated lease expiry", async () => {
  createService.MailsService = await loadMailsService();
  const { service, mailStore, pubsubClient } = createService({
    storeOptions: { outboxMaxAttempts: 2, outboxLeaseMs: 1000 },
    config: { outboxLeaseMs: 1000 }
  });
  await createMail(mailStore, { mail_id: "mail_publish_limit" });

  mailStore.markMailNotificationOutboxSent = async () => false;
  await service.processPendingNotificationOutbox();
  let row = await mailStore.getMailNotificationOutboxByMailId("mail_publish_limit");
  assert.equal(row.attempts, 1);
  mailStore.memoryOutbox.get(row.id).locked_until = new Date(Date.now() - 1);

  await service.processPendingNotificationOutbox();
  row = await mailStore.getMailNotificationOutboxByMailId("mail_publish_limit");
  assert.equal(row.attempts, 2);
  mailStore.memoryOutbox.get(row.id).locked_until = new Date(Date.now() - 1);

  const exhausted = await service.processPendingNotificationOutbox();
  row = await mailStore.getMailNotificationOutboxByMailId("mail_publish_limit");
  assert.equal(exhausted.terminal, 1);
  assert.equal(row.status, "terminal");
  assert.equal(row.attempts, 2);
  assert.equal(pubsubClient.publishes.length, 2);
});

test("MailsService returns mail success when immediate outbox scan fails", async () => {
  createService.MailsService = await loadMailsService();
  const { service, mailStore } = createService();
  mailStore.reservePendingMailNotificationOutbox = async () => {
    throw new Error("outbox database unavailable");
  };

  const result = await service.create({ to_player_id: "player_001", title: "Reward" });
  assert.equal(result.ok, true);
  assert.ok(await mailStore.getMailById(result.mail_id));
  assert.equal((await mailStore.getMailNotificationOutboxByMailId(result.mail_id)).status, "pending");
});
