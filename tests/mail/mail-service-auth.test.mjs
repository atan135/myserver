import assert from "node:assert/strict";
import crypto from "node:crypto";
import { mkdir, readFile, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { test } from "node:test";
import { pathToFileURL } from "node:url";

import "reflect-metadata";
import ts from "typescript";

import {
  hashTicket,
  MailPlayerAuthService,
  ticketKey,
  ticketVersionKey,
  verifyTicketSignature
} from "../../apps/mail-service/src/mail-auth.js";
import { DbMailStore } from "../../apps/mail-service/src/db-store.js";

const TICKET_SECRET = "test-ticket-secret";
const SERVICE_TOKEN = "test-mail-service-token";

function createTicket({
  playerId = "player_001",
  characterId = "character_001",
  secret = TICKET_SECRET,
  exp = new Date(Date.now() + 60_000).toISOString(),
  ver = 1
} = {}) {
  const payload = {
    playerId,
    characterId,
    nonce: "test-nonce",
    ver,
    exp
  };
  const payloadB64 = Buffer.from(JSON.stringify(payload)).toString("base64url");
  const signature = crypto
    .createHmac("sha256", secret)
    .update(payloadB64)
    .digest("base64url");
  return `${payloadB64}.${signature}`;
}

class MemoryRedis {
  constructor(entries = []) {
    this.values = new Map(entries);
  }

  async get(key) {
    return this.values.get(key) ?? null;
  }
}

async function transpileTypeScriptModule(sourcePath, outPath, replacements = {}) {
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

  let outputText = compiled.outputText;
  for (const [from, to] of Object.entries(replacements)) {
    outputText = outputText.replaceAll(from, to);
  }

  await writeFile(outPath, outputText, "utf8");
}

async function loadMailControllerAndService() {
  const outDir = path.resolve("tests/.tmp/mail-service-auth");
  const serviceSource = path.resolve("apps/mail-service/src/mails/mails.service.ts");
  const controllerSource = path.resolve("apps/mail-service/src/mails/mails.controller.ts");

  await rm(outDir, { recursive: true, force: true });
  await mkdir(path.join(outDir, "common"), { recursive: true });

  await transpileTypeScriptModule(serviceSource, path.join(outDir, "mails.service.mjs"), {
    "../common/": "./common/",
    "../global-id.js": "./global-id.js",
    "../logger.js": "./logger.js",
    "../notification-outbox.js": "./notification-outbox.js",
    "../tokens.js": "./tokens.js"
  });
  await transpileTypeScriptModule(controllerSource, path.join(outDir, "mails.controller.mjs"), {
    "../common/": "./common/",
    "../mail-auth.js": "./mail-auth.js",
    "../tokens.js": "./tokens.js",
    "./mails.service.js": "./mails.service.mjs"
  });

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
  await writeFile(path.join(outDir, "logger.js"), "export function log() {}\n", "utf8");
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
  await writeFile(
    path.join(outDir, "mail-auth.js"),
    "export * from '../../../apps/mail-service/src/mail-auth.js';\n",
    "utf8"
  );

  const serviceModule = await import(`${pathToFileURL(path.join(outDir, "mails.service.mjs")).href}?v=${Date.now()}`);
  const controllerModule = await import(`${pathToFileURL(path.join(outDir, "mails.controller.mjs")).href}?v=${Date.now()}`);
  return {
    MailsService: serviceModule.MailsService,
    MailsController: controllerModule.MailsController
  };
}

function createService(MailsService) {
  const mailStore = new DbMailStore(null);
  const pubsubClient = {
    async publishMailNotification() {}
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
    sender_name: "system",
    from_player_id: "system",
    to_player_id: "player_001",
    title: "Reward",
    content: "",
    attachments: [{ type: "item", id: 1001, count: 2 }],
    mail_type: "system",
    created_by_type: "system",
    created_by_id: "system",
    created_by_name: "system",
    created_at: Date.now(),
    ...overrides
  };

  await store.createMail(mail);
  return mail;
}

function createController(MailsController, service, playerAuth) {
  return new MailsController(
    service,
    {
      mailPlayerAuthRequired: true,
      mailServiceToken: SERVICE_TOKEN
    },
    playerAuth
  );
}

function assertApiError(error, statusCode, code) {
  assert.equal(error.getStatus?.(), statusCode);
  assert.equal(error.getResponse?.().ok, false);
  assert.equal(error.getResponse?.().error, code);
}

test("mail ticket helper parses a valid ticket", () => {
  const ticket = createTicket({ playerId: "player_001", ver: 7 });
  const payload = verifyTicketSignature(TICKET_SECRET, ticket);

  assert.equal(payload.playerId, "player_001");
  assert.equal(payload.ver, 7);
});

test("mail ticket helper rejects invalid signature", () => {
  const ticket = createTicket({ secret: "other-secret" });

  assert.throws(
    () => verifyTicketSignature(TICKET_SECRET, ticket),
    (error) => {
      assert.equal(error.code, "INVALID_TICKET_SIGNATURE");
      return true;
    }
  );
});

test("mail ticket helper rejects expired ticket", () => {
  const ticket = createTicket({ exp: new Date(Date.now() - 1000).toISOString() });

  assert.throws(
    () => verifyTicketSignature(TICKET_SECRET, ticket),
    (error) => {
      assert.equal(error.code, "TICKET_EXPIRED");
      return true;
    }
  );
});

test("mail player auth validates Redis ticket owner and version", async () => {
  const ticket = createTicket({ playerId: "player_001", ver: 3 });
  const redis = new MemoryRedis([
    [ticketKey("test:", ticket), "player_001"],
    [ticketVersionKey("test:", "player_001"), "3"]
  ]);
  const auth = new MailPlayerAuthService(
    {
      ticketSecret: TICKET_SECRET,
      redisKeyPrefix: "test:"
    },
    redis
  );

  const result = await auth.authenticateTicket(ticket);

  assert.equal(result.playerId, "player_001");
  assert.equal(result.ticketVersion, 3);
  assert.equal(hashTicket(ticket).length, 64);
});

test("mail player auth rejects Redis ticket owner mismatch", async () => {
  const ticket = createTicket({ playerId: "player_001", ver: 1 });
  const redis = new MemoryRedis([
    [ticketKey("test:", ticket), "player_002"],
    [ticketVersionKey("test:", "player_001"), "1"]
  ]);
  const auth = new MailPlayerAuthService(
    {
      ticketSecret: TICKET_SECRET,
      redisKeyPrefix: "test:"
    },
    redis
  );

  await assert.rejects(
    () => auth.authenticateTicket(ticket),
    (error) => {
      assert.equal(error.code, "TICKET_REVOKED");
      return true;
    }
  );
});

test("mail player auth rejects missing Redis ticket version", async () => {
  const ticket = createTicket({ playerId: "player_001", ver: 1 });
  const redis = new MemoryRedis([
    [ticketKey("test:", ticket), "player_001"]
  ]);
  const auth = new MailPlayerAuthService(
    {
      ticketSecret: TICKET_SECRET,
      redisKeyPrefix: "test:"
    },
    redis
  );

  await assert.rejects(
    () => auth.authenticateTicket(ticket),
    (error) => {
      assert.equal(error.code, "TICKET_REVOKED");
      return true;
    }
  );
});

test("mail list uses authenticated player id", async () => {
  const { MailsController, MailsService } = await loadMailControllerAndService();
  const { service, mailStore } = createService(MailsService);
  await createMail(mailStore, { mail_id: "own_mail", to_player_id: "player_001" });
  await createMail(mailStore, { mail_id: "other_mail", to_player_id: "player_002" });

  const controller = createController(MailsController, service, {
    async authenticateTicket() {
      return { playerId: "player_001", characterId: "character_001" };
    }
  });

  const result = await controller.list(
    { authorization: "Bearer ticket-value" },
    {}
  );

  assert.deepEqual(result.mails.map((mail) => mail.mail_id), ["own_mail"]);
  assert.equal(result.unread_count, 1);
});

test("mail detail rejects reading another player's mail", async () => {
  const { MailsController, MailsService } = await loadMailControllerAndService();
  const { service, mailStore } = createService(MailsService);
  await createMail(mailStore, { mail_id: "other_mail", to_player_id: "player_002" });

  const controller = createController(MailsController, service, {
    async authenticateTicket() {
      return { playerId: "player_001", characterId: "character_001" };
    }
  });

  await assert.rejects(
    () => controller.get("other_mail", { authorization: "Bearer ticket-value" }, {}),
    (error) => {
      assertApiError(error, 403, "FORBIDDEN");
      return true;
    }
  );
});

test("mail claim rejects body player_id mismatch", async () => {
  const { MailsController, MailsService } = await loadMailControllerAndService();
  const { service, mailStore, gameAdminClient } = createService(MailsService);
  await createMail(mailStore);

  const controller = createController(MailsController, service, {
    async authenticateTicket() {
      return { playerId: "player_001", characterId: "character_001" };
    }
  });

  await assert.rejects(
    () =>
      controller.claim(
        "mail_001",
        { authorization: "Bearer ticket-value" },
        { player_id: "player_002" }
      ),
    (error) => {
      assertApiError(error, 403, "PLAYER_ID_MISMATCH");
      return true;
    }
  );
  assert.deepEqual(gameAdminClient.grants, []);
});

test("mail create rejects missing service token", async () => {
  const { MailsController, MailsService } = await loadMailControllerAndService();
  const { service } = createService(MailsService);
  const controller = createController(MailsController, service, {
    async authenticateTicket() {
      return { playerId: "player_001" };
    }
  });

  assert.throws(
    () => controller.create({}, { to_player_id: "player_001", title: "Reward" }),
    (error) => {
      assertApiError(error, 401, "MAIL_SERVICE_TOKEN_REQUIRED");
      return true;
    }
  );
});

test("mail create accepts service token", async () => {
  const { MailsController, MailsService } = await loadMailControllerAndService();
  const { service, mailStore } = createService(MailsService);
  const controller = createController(MailsController, service, {
    async authenticateTicket() {
      return { playerId: "player_001" };
    }
  });

  const result = await controller.create(
    { authorization: `Bearer ${SERVICE_TOKEN}` },
    { to_player_id: "player_001", title: "Reward" }
  );
  const mail = await mailStore.getMailById(result.mail_id);

  assert.equal(result.ok, true);
  assert.equal(mail.to_player_id, "player_001");
  assert.equal(mail.title, "Reward");
});

async function withEnv(overrides, fn) {
  const previousEnv = new Map(Object.entries(process.env));
  try {
    for (const [key, value] of Object.entries(overrides)) {
      if (value === undefined) {
        delete process.env[key];
      } else {
        process.env[key] = value;
      }
    }
    return await fn();
  } finally {
    for (const key of Object.keys(process.env)) {
      if (!previousEnv.has(key)) {
        delete process.env[key];
      }
    }
    for (const [key, value] of previousEnv.entries()) {
      process.env[key] = value;
    }
  }
}

test("mail-service production config rejects weak auth settings", async () => {
  await withEnv(
    {
      NODE_ENV: "production",
      APP_ENV: undefined,
      TICKET_SECRET: "dev-only-change-this-ticket-secret",
      MAIL_PLAYER_AUTH_REQUIRED: "false",
      MAIL_SERVICE_TOKEN: "dev-only-change-this-mail-service-token"
    },
    async () => {
      const { getConfig } = await import(`../../apps/mail-service/src/config.js?v=${Date.now()}`);

      assert.throws(
        () => getConfig(),
        (error) => {
          assert.match(error.message, /MAIL_PLAYER_AUTH_REQUIRED/);
          assert.match(error.message, /TICKET_SECRET/);
          assert.match(error.message, /MAIL_SERVICE_TOKEN/);
          return true;
        }
      );
    }
  );
});

test("mail-service production config accepts strong auth settings", async () => {
  await withEnv(
    {
      NODE_ENV: "production",
      APP_ENV: undefined,
      TICKET_SECRET: "prod-ticket-secret-123",
      MAIL_PLAYER_AUTH_REQUIRED: "true",
      MAIL_SERVICE_TOKEN: "prod-mail-service-token-123",
      REGISTRY_ENABLED: "true"
    },
    async () => {
      const { getConfig } = await import(`../../apps/mail-service/src/config.js?v=${Date.now()}`);
      const config = getConfig();

      assert.equal(config.mailPlayerAuthRequired, true);
      assert.equal(config.ticketSecret, "prod-ticket-secret-123");
      assert.equal(config.mailServiceToken, "prod-mail-service-token-123");
    }
  );
});

test("mail-service production config rejects short auth secrets", async () => {
  await withEnv(
    {
      NODE_ENV: "production",
      APP_ENV: undefined,
      TICKET_SECRET: "short",
      MAIL_PLAYER_AUTH_REQUIRED: "true",
      MAIL_SERVICE_TOKEN: "also-short"
    },
    async () => {
      const { getConfig } = await import(`../../apps/mail-service/src/config.js?v=${Date.now()}`);

      assert.throws(
        () => getConfig(),
        (error) => {
          assert.match(error.message, /TICKET_SECRET/);
          assert.match(error.message, /MAIL_SERVICE_TOKEN/);
          return true;
        }
      );
    }
  );
});
