import assert from "node:assert/strict";
import crypto from "node:crypto";
import { mkdir, readFile, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { test } from "node:test";
import { pathToFileURL } from "node:url";
import ts from "typescript";

import "reflect-metadata";

import {
  AnnounceReadAuthService,
  ticketKey,
  ticketVersionKey
} from "../../apps/announce-service/src/announce-auth.js";

const ADMIN_TOKEN = "test-announce-admin-token";
const READ_TOKEN = "test-announce-read-token";
const TICKET_SECRET = "test-ticket-secret";

function createTicket({
  playerId = "player_001",
  secret = TICKET_SECRET,
  exp = new Date(Date.now() + 60_000).toISOString(),
  ver = 1
} = {}) {
  const payload = {
    playerId,
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

async function loadAnnouncementsController() {
  const sourcePath = path.resolve(
    "apps/announce-service/src/announcements/announcements.controller.ts"
  );
  const outDir = path.resolve("tests/.tmp/announce-service-security");
  const outPath = path.join(outDir, "announcements.controller.mjs");

  await rm(outDir, { recursive: true, force: true });
  await mkdir(path.join(outDir, "common"), { recursive: true });
  await transpileTypeScriptModule(sourcePath, outPath, {
    "../announce-auth.js": "./announce-auth.js",
    "../common/": "./common/",
    "../tokens.js": "./tokens.js",
    "./announcements.service.js": "./announcements.service.js"
  });
  await writeFile(
    path.join(outDir, "common/http-exception.js"),
    "export * from '../../../../apps/announce-service/src/common/http-exception.ts';\n",
    "utf8"
  );
  await writeFile(
    path.join(outDir, "tokens.js"),
    "export * from '../../../apps/announce-service/src/tokens.ts';\n",
    "utf8"
  );
  await writeFile(
    path.join(outDir, "announce-auth.js"),
    "export * from '../../../apps/announce-service/src/announce-auth.js';\n",
    "utf8"
  );
  await writeFile(
    path.join(outDir, "announcements.service.js"),
    "export class AnnouncementsService {}\n",
    "utf8"
  );

  const imported = await import(`${pathToFileURL(outPath).href}?v=${Date.now()}`);
  return imported.AnnouncementsController;
}

async function loadRequestLogSanitizer() {
  const sourcePath = path.resolve(
    "apps/announce-service/src/common/request-log.middleware.ts"
  );
  const outDir = path.resolve("tests/.tmp/announce-service-request-log");
  const outPath = path.join(outDir, "request-log.middleware.mjs");

  await rm(outDir, { recursive: true, force: true });
  await mkdir(outDir, { recursive: true });
  await transpileTypeScriptModule(sourcePath, outPath, {
    "../logger.js": "./logger.js"
  });
  await writeFile(path.join(outDir, "logger.js"), "export function log() {}\n", "utf8");

  const imported = await import(`${pathToFileURL(outPath).href}?v=${Date.now()}`);
  return imported.sanitizeRequestPath;
}

function createServiceStub() {
  const calls = [];
  return {
    calls,
    list(query) {
      calls.push(["list", query]);
      return { ok: true, announcements: [] };
    },
    get(announceId) {
      calls.push(["get", announceId]);
      return { ok: true, announcement: { announce_id: announceId } };
    },
    create(body) {
      calls.push(["create", body]);
      return { ok: true, announcement: body };
    },
    update(announceId, body) {
      calls.push(["update", announceId, body]);
      return { ok: true, announcement: { announce_id: announceId, ...body } };
    },
    delete(announceId) {
      calls.push(["delete", announceId]);
      return { ok: true, deleted: true };
    }
  };
}

function createController(ControllerClass, overrides = {}) {
  const service = createServiceStub();
  const config = {
    announceAdminToken: ADMIN_TOKEN,
    announceReadAuthRequired: true,
    announceReadToken: READ_TOKEN,
    ticketSecret: TICKET_SECRET,
    redisKeyPrefix: "test:",
    ...overrides.config
  };
  const readAuth =
    overrides.readAuth ||
    new AnnounceReadAuthService(config, new MemoryRedis());
  const controller = new ControllerClass(service, config, readAuth);
  return { controller, service };
}

function assertApiError(error, statusCode, code) {
  assert.equal(error.getStatus?.(), statusCode);
  assert.equal(error.getResponse?.().ok, false);
  assert.equal(error.getResponse?.().error, code);
}

function withEnv(overrides, fn) {
  const previousEnv = new Map(Object.entries(process.env));
  try {
    for (const [key, value] of Object.entries(overrides)) {
      if (value === undefined) {
        delete process.env[key];
      } else {
        process.env[key] = value;
      }
    }
    return fn();
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

test("announce write APIs reject missing admin token", async () => {
  const AnnouncementsController = await loadAnnouncementsController();
  const { controller, service } = createController(AnnouncementsController);

  assert.throws(
    () => controller.create({}, { title: "Title" }),
    (error) => {
      assertApiError(error, 401, "ANNOUNCE_ADMIN_TOKEN_REQUIRED");
      return true;
    }
  );
  assert.deepEqual(service.calls, []);
});

test("announce write APIs do not accept token outside supported headers", async () => {
  const AnnouncementsController = await loadAnnouncementsController();
  const { controller, service } = createController(AnnouncementsController);

  assert.throws(
    () =>
      controller.create(
        {},
        {
          title: "Title",
          admin_token: ADMIN_TOKEN,
          announce_admin_token: ADMIN_TOKEN
        }
      ),
    (error) => {
      assertApiError(error, 401, "ANNOUNCE_ADMIN_TOKEN_REQUIRED");
      return true;
    }
  );
  assert.deepEqual(service.calls, []);
});

test("announce write APIs reject invalid admin token", async () => {
  const AnnouncementsController = await loadAnnouncementsController();
  const { controller, service } = createController(AnnouncementsController);

  assert.throws(
    () =>
      controller.update(
        "ann_001",
        { authorization: "Bearer wrong-token" },
        { title: "Updated" }
      ),
    (error) => {
      assertApiError(error, 403, "ANNOUNCE_ADMIN_TOKEN_INVALID");
      return true;
    }
  );
  assert.deepEqual(service.calls, []);
});

test("announce write APIs accept Bearer and X-Admin-Token headers", async () => {
  const AnnouncementsController = await loadAnnouncementsController();
  const { controller, service } = createController(AnnouncementsController);

  const created = controller.create(
    { authorization: `Bearer ${ADMIN_TOKEN}` },
    { title: "Title" }
  );
  const deleted = controller.delete("ann_001", {
    "X-Admin-Token": ADMIN_TOKEN
  });

  assert.equal(created.ok, true);
  assert.equal(deleted.ok, true);
  assert.deepEqual(service.calls, [
    ["create", { title: "Title" }],
    ["delete", "ann_001"]
  ]);
});

test("announce read APIs require read auth by default", async () => {
  const AnnouncementsController = await loadAnnouncementsController();
  const { controller, service } = createController(AnnouncementsController);

  await assert.rejects(
    () => controller.list({}, { locale: "zh-CN" }),
    (error) => {
      assertApiError(error, 401, "ANNOUNCE_READ_AUTH_REQUIRED");
      return true;
    }
  );

  await assert.rejects(
    () => controller.get("ann_001", {}),
    (error) => {
      assertApiError(error, 401, "ANNOUNCE_READ_AUTH_REQUIRED");
      return true;
    }
  );
  assert.deepEqual(service.calls, []);
});

test("announce read APIs reject query token without echoing token", async () => {
  const AnnouncementsController = await loadAnnouncementsController();
  const { controller, service } = createController(AnnouncementsController);

  await assert.rejects(
    () =>
      controller.list(
        {},
        {
          locale: "zh-CN",
          token: READ_TOKEN,
          read_token: READ_TOKEN,
          announce_read_token: READ_TOKEN
        }
      ),
    (error) => {
      assertApiError(error, 401, "ANNOUNCE_READ_AUTH_REQUIRED");
      assert.equal(JSON.stringify(error.getResponse?.()).includes(READ_TOKEN), false);
      return true;
    }
  );
  assert.deepEqual(service.calls, []);
});

test("announce request logging strips query strings before logging paths", async () => {
  const sanitizeRequestPath = await loadRequestLogSanitizer();

  assert.equal(
    sanitizeRequestPath(`/api/v1/announcements?token=${READ_TOKEN}&locale=zh-CN`),
    "/api/v1/announcements"
  );
  assert.equal(
    sanitizeRequestPath(`/api/v1/announcements/ann_001?read_token=${READ_TOKEN}`),
    "/api/v1/announcements/ann_001"
  );
  assert.equal(sanitizeRequestPath(`?token=${READ_TOKEN}`), "/");
});

test("announce read token allows list and detail", async () => {
  const AnnouncementsController = await loadAnnouncementsController();
  const { controller, service } = createController(AnnouncementsController);

  const listResult = await controller.list(
    { authorization: `Bearer ${READ_TOKEN}` },
    { locale: "zh-CN" }
  );
  const detailResult = await controller.get("ann_001", {
    "X-Read-Token": READ_TOKEN
  });

  assert.equal(listResult.ok, true);
  assert.equal(detailResult.ok, true);
  assert.deepEqual(service.calls, [
    ["list", { locale: "zh-CN" }],
    ["get", "ann_001"]
  ]);
});

test("announce read token does not grant write access", async () => {
  const AnnouncementsController = await loadAnnouncementsController();
  const { controller, service } = createController(AnnouncementsController);

  assert.throws(
    () =>
      controller.create(
        { authorization: `Bearer ${READ_TOKEN}` },
        { title: "Title" }
      ),
    (error) => {
      assertApiError(error, 403, "ANNOUNCE_ADMIN_TOKEN_INVALID");
      return true;
    }
  );
  assert.deepEqual(service.calls, []);
});

test("announce game ticket allows list and detail after Redis owner and version validation", async () => {
  const AnnouncementsController = await loadAnnouncementsController();
  const ticket = createTicket({ playerId: "player_001", ver: 3 });
  const config = {
    announceAdminToken: ADMIN_TOKEN,
    announceReadAuthRequired: true,
    announceReadToken: READ_TOKEN,
    ticketSecret: TICKET_SECRET,
    redisKeyPrefix: "test:"
  };
  const readAuth = new AnnounceReadAuthService(
    config,
    new MemoryRedis([
      [ticketKey("test:", ticket), "player_001"],
      [ticketVersionKey("test:", "player_001"), "3"]
    ])
  );
  const { controller, service } = createController(AnnouncementsController, {
    config,
    readAuth
  });

  const listResult = await controller.list(
    { authorization: `Bearer ${ticket}` },
    { locale: "zh-CN" }
  );
  const detailResult = await controller.get("ann_001", {
    "X-Game-Ticket": ticket
  });

  assert.equal(listResult.ok, true);
  assert.equal(detailResult.ok, true);
  assert.deepEqual(service.calls, [
    ["list", { locale: "zh-CN" }],
    ["get", "ann_001"]
  ]);
});

test("announce game ticket rejects Redis owner or version mismatch", async () => {
  const AnnouncementsController = await loadAnnouncementsController();
  const ticket = createTicket({ playerId: "player_001", ver: 3 });
  const config = {
    announceAdminToken: ADMIN_TOKEN,
    announceReadAuthRequired: true,
    announceReadToken: READ_TOKEN,
    ticketSecret: TICKET_SECRET,
    redisKeyPrefix: "test:"
  };
  const readAuth = new AnnounceReadAuthService(
    config,
    new MemoryRedis([
      [ticketKey("test:", ticket), "player_001"],
      [ticketVersionKey("test:", "player_001"), "2"]
    ])
  );
  const { controller, service } = createController(AnnouncementsController, {
    config,
    readAuth
  });

  await assert.rejects(
    () => controller.list({ authorization: `Bearer ${ticket}` }, {}),
    (error) => {
      assertApiError(error, 401, "TICKET_REVOKED");
      return true;
    }
  );
  assert.deepEqual(service.calls, []);
});

test("announce read auth can be disabled outside production", async () => {
  const AnnouncementsController = await loadAnnouncementsController();
  const { controller, service } = createController(AnnouncementsController, {
    config: {
      announceReadAuthRequired: false
    }
  });

  assert.equal((await controller.list({}, { locale: "zh-CN" })).ok, true);
  assert.equal((await controller.get("ann_001", {})).ok, true);
  assert.deepEqual(service.calls, [
    ["list", { locale: "zh-CN" }],
    ["get", "ann_001"]
  ]);
});

test("announce-service production config rejects default admin token", async () => {
  const { getConfig } = await import(
    `../../apps/announce-service/src/config.js?v=${Date.now()}`
  );

  withEnv(
    {
      NODE_ENV: "production",
      APP_ENV: undefined,
      ANNOUNCE_ADMIN_TOKEN: undefined,
      ANNOUNCE_READ_AUTH_REQUIRED: "true",
      ANNOUNCE_READ_TOKEN: undefined,
      TICKET_SECRET: "prod-ticket-secret-123456"
    },
    () => {
      assert.throws(
        () => getConfig(),
        /ANNOUNCE_ADMIN_TOKEN must be set to a non-default value in production/
      );
    }
  );

  withEnv(
    {
      NODE_ENV: "development",
      APP_ENV: "production",
      ANNOUNCE_ADMIN_TOKEN: "dev-only-change-this-announce-admin-token",
      ANNOUNCE_READ_AUTH_REQUIRED: "true",
      ANNOUNCE_READ_TOKEN: undefined,
      TICKET_SECRET: "prod-ticket-secret-123456"
    },
    () => {
      assert.throws(
        () => getConfig(),
        /ANNOUNCE_ADMIN_TOKEN must be set to a non-default value in production/
      );
    }
  );

  withEnv(
    {
      NODE_ENV: "production",
      APP_ENV: undefined,
      ANNOUNCE_ADMIN_TOKEN: "prod-announce-admin-token",
      ANNOUNCE_READ_AUTH_REQUIRED: "true",
      ANNOUNCE_READ_TOKEN: undefined,
      TICKET_SECRET: "prod-ticket-secret-123456",
      REGISTRY_ENABLED: "true"
    },
    () => {
      const config = getConfig();
      assert.equal(config.env, "production");
      assert.equal(config.announceAdminToken, "prod-announce-admin-token");
    }
  );
});

test("announce-service production config rejects weak read settings", async () => {
  const { getConfig } = await import(
    `../../apps/announce-service/src/config.js?v=${Date.now()}`
  );

  withEnv(
    {
      NODE_ENV: "production",
      APP_ENV: undefined,
      ANNOUNCE_ADMIN_TOKEN: "prod-announce-admin-token",
      ANNOUNCE_READ_AUTH_REQUIRED: "false",
      ANNOUNCE_READ_TOKEN: "dev-only-change-this-announce-read-token",
      TICKET_SECRET: "dev-only-change-this-ticket-secret"
    },
    () => {
      assert.throws(
        () => getConfig(),
        (error) => {
          assert.match(error.message, /ANNOUNCE_READ_AUTH_REQUIRED/);
          assert.match(error.message, /ANNOUNCE_READ_TOKEN/);
          assert.match(error.message, /TICKET_SECRET/);
          return true;
        }
      );
    }
  );

  withEnv(
    {
      NODE_ENV: "production",
      APP_ENV: undefined,
      ANNOUNCE_ADMIN_TOKEN: "prod-announce-admin-token",
      ANNOUNCE_READ_AUTH_REQUIRED: "true",
      ANNOUNCE_READ_TOKEN: "short",
      TICKET_SECRET: "short"
    },
    () => {
      assert.throws(
        () => getConfig(),
        (error) => {
          assert.match(error.message, /ANNOUNCE_READ_TOKEN/);
          assert.match(error.message, /TICKET_SECRET/);
          return true;
        }
      );
    }
  );

  withEnv(
    {
      NODE_ENV: "production",
      APP_ENV: undefined,
      ANNOUNCE_ADMIN_TOKEN: "shared-prod-token-123",
      ANNOUNCE_READ_AUTH_REQUIRED: "true",
      ANNOUNCE_READ_TOKEN: "shared-prod-token-123",
      TICKET_SECRET: "prod-ticket-secret-123456"
    },
    () => {
      assert.throws(
        () => getConfig(),
        /ANNOUNCE_READ_TOKEN must not match ANNOUNCE_ADMIN_TOKEN/
      );
    }
  );
});
