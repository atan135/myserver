import assert from "node:assert/strict";
import { mkdir, readFile, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { test } from "node:test";
import ts from "typescript";
import { pathToFileURL } from "node:url";

import "reflect-metadata";
import { MySqlMailStore } from "../apps/mail-service/src/mysql-store.js";

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
    .replaceAll("../logger.js", "./logger.js")
    .replaceAll("../tokens.js", "./tokens.js");

  await writeFile(outPath, outputText, "utf8");
  await writeFile(
    path.join(outDir, "common/http-exception.js"),
    "export * from '../../../../apps/mail-service/src/common/http-exception.ts';\n",
    "utf8"
  );
  await writeFile(
    path.join(outDir, "logger.js"),
    "export function log() {}\n",
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

function createService() {
  const MailsService = createService.MailsService;
  const mailStore = new MySqlMailStore(null);
  const pubsubClient = {
    publishMailNotification: async () => {}
  };
  const gameAdminClient = {
    grants: [],
    async grantMailAttachments(playerId, requestId, attachments, reason) {
      this.grants.push({ playerId, requestId, attachments, reason });
      return { ok: true };
    }
  };

  return {
    mailStore,
    gameAdminClient,
    service: new MailsService(mailStore, pubsubClient, gameAdminClient)
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

  const first = await service.claim("mail_001", { player_id: "player_001" });
  const second = await service.claim("mail_001", { player_id: "player_001" });

  assert.equal(first.ok, true);
  assert.equal(first.claimed, true);
  assert.equal(first.already_claimed, false);
  assert.equal(first.status, "claimed");

  assert.equal(second.ok, true);
  assert.equal(second.claimed, false);
  assert.equal(second.already_claimed, true);
  assert.equal(second.status, "claimed");

  assert.equal(gameAdminClient.grants.length, 1);
  assert.equal(gameAdminClient.grants[0].playerId, "player_001");
  assert.equal(gameAdminClient.grants[0].requestId, "mail_claim:mail_001");
});
