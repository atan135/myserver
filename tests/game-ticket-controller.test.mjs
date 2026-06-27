import assert from "node:assert/strict";
import crypto from "node:crypto";
import { register } from "node:module";
import { test } from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../apps/auth-http/tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const { AuthStore } = await import("../apps/auth-http/src/auth-store.js");
const { AuthService } = await import("../apps/auth-http/src/auth/auth.service.ts");
const { GameTicketController } = await import("../apps/auth-http/src/game-ticket/game-ticket.controller.ts");

class FakeRedis {
  constructor() {
    this.store = new Map();
  }

  async set(key, value) {
    this.store.set(key, String(value));
  }

  async get(key) {
    return this.store.get(key) ?? null;
  }

  async del(key) {
    this.store.delete(key);
  }

  async incr(key) {
    const next = Number.parseInt(this.store.get(key) ?? "0", 10) + 1;
    this.store.set(key, String(next));
    return next;
  }
}

function decodeTicketPayload(ticket) {
  return JSON.parse(Buffer.from(ticket.split(".")[0], "base64url").toString("utf8"));
}

function createLegacyTicket(secret) {
  const payload = {
    playerId: "player-001",
    nonce: "legacy-ticket",
    ver: 1,
    exp: new Date(Date.now() + 300000).toISOString()
  };
  const payloadB64 = Buffer.from(JSON.stringify(payload)).toString("base64url");
  const signature = crypto.createHmac("sha256", secret).update(payloadB64).digest("base64url");
  return `${payloadB64}.${signature}`;
}

function createRequest() {
  return {
    url: "/api/v1/game-ticket/issue",
    headers: { authorization: "Bearer access-token" },
    socket: { remoteAddress: "127.0.0.1" }
  };
}

function createContext(overrides = {}) {
  const config = {
    redisKeyPrefix: "test:",
    sessionTtlSeconds: 600,
    ticketTtlSeconds: 300,
    ticketSecret: "test-secret",
    trustProxy: false,
    trustedProxies: [],
    dbEnabled: true,
    accountLockEnabled: false,
    localDiscoveryFallbackEnabled: true,
    gameProxyHost: "127.0.0.1",
    gameProxyPort: 4000,
    ...overrides.config
  };
  const redis = new FakeRedis();
  const dbAudits = [];
  const authStore = new AuthStore(
    config,
    redis,
    {
      enabled: true,
      async appendAuthAudit(entry) {
        dbAudits.push(entry);
      }
    }
  );
  const originalGetSession = authStore.getSessionByAccessToken.bind(authStore);
  authStore.getSessionByAccessToken = async (token) => {
    if (token === "access-token") {
      return { playerId: "player-001", createdAt: "2026-06-25T12:00:00.000Z" };
    }
    return originalGetSession(token);
  };

  const characterStore = overrides.characterStore ?? {
    enabled: true,
    async getByCharacterId(characterId) {
      if (characterId !== "chr_0000000000001") {
        return null;
      }
      return {
        characterId,
        accountPlayerId: "player-001",
        worldId: 9,
        status: "active",
        deletedAt: null
      };
    }
  };
  const authService = new AuthService(
    config,
    authStore,
    null,
    { enabled: true },
    {
      async discoverClientServices() {
        return overrides.services ?? {
          game: { host: "127.0.0.1", port: 4000, protocol: "kcp" },
          chat: null,
          mail: null,
          announce: null
        };
      }
    },
    { async getStatus() { return { enabled: false }; } }
  );
  const controller = new GameTicketController(
    authStore,
    config,
    { async checkPlayer() { return { blocked: false, unavailable: false }; } },
    { async appendSecurityAudit() {} },
    characterStore,
    authService
  );

  return { controller, authStore, redis, dbAudits };
}

async function assertApiError(promise, status, errorCode) {
  await assert.rejects(
    promise,
    (error) => {
      assert.equal(error.getStatus(), status);
      assert.equal(error.getResponse().error, errorCode);
      return true;
    }
  );
}

test("game-ticket issue signs character-bound payload and keeps Redis owner account-scoped", async () => {
  const { controller, authStore, dbAudits } = createContext();

  const result = await controller.issue(createRequest(), { character_id: "chr_0000000000001" });

  assert.equal(result.ok, true);
  assert.equal(result.playerId, "player-001");
  assert.equal(result.characterId, "chr_0000000000001");
  assert.equal(result.worldId, 9);
  assert.ok(result.ticket);

  const payload = decodeTicketPayload(result.ticket);
  assert.equal(payload.playerId, "player-001");
  assert.equal(payload.characterId, "chr_0000000000001");
  assert.equal(payload.worldId, 9);
  assert.equal(payload.ver, 1);
  assert.equal(await authStore.getTicketOwner(result.ticket), "player-001");
  assert.deepEqual(dbAudits.at(-1).details, {
    expiresAt: result.ticketExpiresAt,
    characterId: "chr_0000000000001",
    worldId: 9
  });
});

test("game-ticket validate returns playerId, characterId, worldId for signed ticket", async () => {
  const { controller } = createContext();
  const issued = await controller.issue(createRequest(), { characterId: "chr_0000000000001" });

  const validated = await controller.validate({ ticket: issued.ticket });

  assert.equal(validated.ok, true);
  assert.equal(validated.playerId, "player-001");
  assert.equal(validated.characterId, "chr_0000000000001");
  assert.equal(validated.worldId, 9);
  assert.equal(validated.ver, 1);
});

test("game-ticket validate rejects legacy account-only ticket with explicit error", async () => {
  const { controller } = createContext();
  const legacyTicket = createLegacyTicket("test-secret");

  await assertApiError(controller.validate({ ticket: legacyTicket }), 401, "MISSING_CHARACTER_ID");
});

test("game-ticket issue rejects soft-deleted characters before signing", async () => {
  const { controller } = createContext({
    characterStore: {
      enabled: true,
      async getByCharacterId(characterId) {
        return {
          characterId,
          accountPlayerId: "player-001",
          worldId: 9,
          status: "deleted",
          deletedAt: "2026-06-25T12:00:00.000Z"
        };
      }
    }
  });

  await assertApiError(
    controller.issue(createRequest(), { character_id: "chr_0000000000001" }),
    403,
    "CHARACTER_NOT_FOUND"
  );
});
test("game-ticket issue rejects missing characterId before signing", async () => {
  const { controller } = createContext();

  await assertApiError(controller.issue(createRequest(), {}), 400, "MISSING_CHARACTER_ID");
});

test("game-ticket issue reports service discovery unavailable explicitly", async () => {
  const { controller } = createContext({
    config: { localDiscoveryFallbackEnabled: false },
    services: { game: null, chat: null, mail: null, announce: null }
  });

  await assertApiError(
    controller.issue(createRequest(), { character_id: "chr_0000000000001" }),
    503,
    "SERVICE_DISCOVERY_UNAVAILABLE"
  );
});
