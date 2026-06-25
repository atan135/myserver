import assert from "node:assert/strict";
import crypto from "node:crypto";
import { register } from "node:module";
import { test } from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../apps/auth-http/tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const { AuthService } = await import("../apps/auth-http/src/auth/auth.service.ts");
const { CharactersService } = await import("../apps/auth-http/src/characters/characters.service.ts");

function decodeTicketPayload(ticket) {
  return JSON.parse(Buffer.from(ticket.split(".")[0], "base64url").toString("utf8"));
}

function createTicket(playerId, secret, options = {}) {
  const payload = {
    playerId,
    nonce: "test-nonce",
    ver: 1,
    exp: "2026-06-25T12:15:00.000Z",
    ...options
  };
  const payloadB64 = Buffer.from(JSON.stringify(payload)).toString("base64url");
  const signature = crypto.createHmac("sha256", secret).update(payloadB64).digest("base64url");
  return `${payloadB64}.${signature}`;
}

class FakeCharacterStore {
  constructor() {
    this.enabled = true;
    this.rows = [];
    this.nextId = 1;
  }

  async listByAccountPlayerId(accountPlayerId, { includeDeleted = false } = {}) {
    return this.rows
      .filter(
        (row) =>
          row.accountPlayerId === accountPlayerId &&
          (includeDeleted || row.deletedAt === null)
      )
      .map((row) => structuredClone(row));
  }

  async getByCharacterId(characterId, { includeDeleted = false } = {}) {
    const row = this.rows.find(
      (candidate) =>
        candidate.characterId === characterId &&
        (includeDeleted || candidate.deletedAt === null)
    );
    return row ? structuredClone(row) : null;
  }

  async createCharacter(input) {
    const effectiveCount = this.rows.filter(
      (row) => row.accountPlayerId === input.accountPlayerId && row.deletedAt === null
    ).length;
    if (effectiveCount >= 6) {
      const error = new Error("effective character limit exceeded");
      error.code = "CHARACTER_LIMIT_EXCEEDED";
      error.current = effectiveCount;
      error.limit = 6;
      throw error;
    }

    const character = createCharacter({
      characterId: `chr_${String(this.nextId++).padStart(13, "0")}`,
      accountPlayerId: input.accountPlayerId,
      name: input.name,
      appearance: input.appearance,
      worldId: input.worldId,
      position: input.position,
      affinity: input.affinity,
      mastery: input.mastery
    });
    this.rows.push(character);
    return structuredClone(character);
  }

  async updateLastLoginAt(characterId) {
    const row = this.rows.find(
      (candidate) => candidate.characterId === characterId && candidate.deletedAt === null
    );
    if (!row) {
      return false;
    }
    row.lastLoginAt = "2026-06-25T12:00:00.000Z";
    return true;
  }
}

function createCharacter(overrides = {}) {
  return {
    characterId: overrides.characterId ?? "chr_0000000000001",
    accountPlayerId: overrides.accountPlayerId ?? "player-001",
    worldId: overrides.worldId ?? 0,
    name: overrides.name ?? "Echo",
    status: overrides.status ?? "active",
    appearance: overrides.appearance ?? { body: "default" },
    position: overrides.position ?? {
      sceneId: 100,
      x: 0,
      y: 0,
      dirX: 0,
      dirY: 1
    },
    affinity: overrides.affinity ?? {
      earth: 2500,
      fire: 2500,
      water: 2500,
      wind: 2500
    },
    mastery: overrides.mastery ?? {
      earth: 0,
      fire: 0,
      water: 0,
      wind: 0
    },
    createdAt: "2026-06-25T11:00:00.000Z",
    lastLoginAt: overrides.lastLoginAt ?? null,
    deletedAt: overrides.deletedAt ?? null
  };
}

function createRequest(token = "access-001") {
  return {
    url: "/api/v1/characters",
    headers: token ? { authorization: `Bearer ${token}` } : {},
    socket: { remoteAddress: "127.0.0.1" }
  };
}

function createContext(overrides = {}) {
  const config = {
    trustProxy: false,
    trustedProxies: [],
    dbEnabled: true,
    accountLockEnabled: false,
    localDiscoveryFallbackEnabled: true,
    gameProxyHost: "127.0.0.1",
    gameProxyPort: 4000,
    characterNameMinLength: 2,
    characterNameMaxLength: 16,
    characterNameForbiddenWords: ["reserved"],
    characterDefaultWorldId: 9,
    characterDefaultSceneId: 300,
    characterDefaultX: 10,
    characterDefaultY: 20,
    characterDefaultDirX: 0,
    characterDefaultDirY: 1,
    characterAppearanceMaxJsonBytes: 4096,
    ticketSecret: "test-secret",
    ...overrides.config
  };
  const sessions = new Map([
    ["access-001", { playerId: "player-001", loginName: "test001" }],
    ["access-002", { playerId: "player-002", loginName: "test002" }],
    ["disabled-token", { playerId: "disabled-player", loginName: "disabled" }],
    ["blocked-token", { playerId: "blocked-player", loginName: "blocked" }]
  ]);
  const issuedTickets = [];
  const blocklistChecks = [];
  const authStore = {
    async getSessionByAccessToken(token) {
      return sessions.get(token) ?? null;
    },
    async assertPlayerCanIssueTicket(playerId) {
      if (playerId === "disabled-player") {
        const error = new Error("account disabled");
        error.code = "ACCOUNT_DISABLED";
        throw error;
      }
    },
    async assertPlayerNotBlocked(playerId, clientIp, source) {
      blocklistChecks.push({ playerId, clientIp, source });
      if (playerId === "blocked-player") {
        const error = new Error("player is blocked");
        error.code = "PLAYER_BLOCKED";
        throw error;
      }
    },
    async issueGameTicket(playerId, clientIp, options) {
      issuedTickets.push({ playerId, clientIp, options });
      return {
        value: createTicket(playerId, config.ticketSecret, options),
        expiresAt: "2026-06-25T12:15:00.000Z"
      };
    }
  };
  const characterStore = overrides.characterStore ?? new FakeCharacterStore();
  const authService = new AuthService(
    config,
    authStore,
    null,
    { enabled: true },
    {
      async discoverClientServices() {
        return {
          game: { host: "127.0.0.1", port: 4000, protocol: "kcp" },
          chat: null,
          mail: null,
          announce: null
        };
      }
    },
    { async getStatus() { return { enabled: false }; } }
  );
  const service = new CharactersService(config, authStore, characterStore, authService);

  return { service, characterStore, issuedTickets, blocklistChecks };
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

test("normal account creates first character with server defaults and balanced attributes", async () => {
  const { service, blocklistChecks } = createContext();

  const result = await service.create(createRequest(), {
    name: "  Echo  ",
    appearance: { body: "default", palette: "blue" },
    world_id: 999,
    position: { scene_id: 999, x: 99 },
    affinity: { fire: 10000 }
  });

  assert.equal(result.ok, true);
  assert.equal(result.character.name, "Echo");
  assert.match(result.character.character_id, /^chr_[0-9a-hjkmnp-tv-z]+$/);
  assert.equal(result.character.world_id, 9);
  assert.deepEqual(result.character.position, {
    scene_id: 300,
    x: 10,
    y: 20,
    dir_x: 0,
    dir_y: 1
  });
  assert.deepEqual(result.character.attributes.affinity, {
    earth: 2500,
    fire: 2500,
    water: 2500,
    wind: 2500
  });
  assert.deepEqual(result.character.attributes.mastery, {
    earth: 0,
    fire: 0,
    water: 0,
    wind: 0
  });
  assert.deepEqual(blocklistChecks, [
    { playerId: "player-001", clientIp: "127.0.0.1", source: "character_create" }
  ]);
});

test("same account and different accounts can create duplicate names up to ordinary limit", async () => {
  const { service, characterStore } = createContext();

  const created = [];
  for (let index = 0; index < 6; index += 1) {
    created.push(await service.create(createRequest(), {
      name: "Echo",
      appearance: { body: "default", slot: String(index) }
    }));
  }

  assert.equal(new Set(created.map((item) => item.character.character_id)).size, 6);
  assert.equal((await service.list(createRequest())).characters.length, 6);

  await assertApiError(
    service.create(createRequest(), { name: "Echo", appearance: { body: "default" } }),
    403,
    "CHARACTER_LIMIT_EXCEEDED"
  );

  const other = await service.create(createRequest("access-002"), {
    name: "Echo",
    appearance: { body: "default" }
  });
  assert.equal(other.character.name, "Echo");
  assert.equal(characterStore.rows.filter((row) => row.name === "Echo").length, 7);
});

test("character name and appearance validation reject invalid input", async () => {
  const { service } = createContext();

  await assertApiError(service.create(createRequest(), { name: " ", appearance: {} }), 400, "INVALID_CHARACTER_NAME");
  await assertApiError(service.create(createRequest(), { name: "A B", appearance: {} }), 400, "INVALID_CHARACTER_NAME");
  await assertApiError(service.create(createRequest(), { name: "bad!", appearance: {} }), 400, "INVALID_CHARACTER_NAME");
  await assertApiError(service.create(createRequest(), { name: "reservedName", appearance: {} }), 400, "CHARACTER_NAME_RESERVED");
  await assertApiError(service.create(createRequest(), { name: "Echo", appearance: [] }), 400, "INVALID_APPEARANCE");
  await assertApiError(service.create(createRequest(), { name: "Echo", appearance: { "bad key": "x" } }), 400, "INVALID_APPEARANCE");
  await assertApiError(service.create(createRequest(), { name: "Echo", appearance: { body: "<script>" } }), 400, "INVALID_APPEARANCE");
});

test("list only returns current account characters and hides soft-deleted rows", async () => {
  const characterStore = new FakeCharacterStore();
  characterStore.rows.push(
    createCharacter({ characterId: "chr_0000000000001", accountPlayerId: "player-001", name: "Echo" }),
    createCharacter({ characterId: "chr_0000000000002", accountPlayerId: "player-001", name: "Echo" }),
    createCharacter({ characterId: "chr_0000000000003", accountPlayerId: "player-001", name: "Deleted", deletedAt: "2026-06-25T00:00:00.000Z" }),
    createCharacter({ characterId: "chr_0000000000004", accountPlayerId: "player-002", name: "Other" })
  );
  const { service } = createContext({ characterStore });

  const result = await service.list(createRequest());

  assert.equal(result.ok, true);
  assert.deepEqual(
    result.characters.map((character) => [character.character_id, character.name, character.character_id_short]),
    [
      ["chr_0000000000001", "Echo", "00000001"],
      ["chr_0000000000002", "Echo", "00000002"]
    ]
  );
  assert.deepEqual(result.characters[0].attributes.affinity, {
    earth: 2500,
    fire: 2500,
    water: 2500,
    wind: 2500
  });
  assert.deepEqual(result.characters[0].position, {
    scene_id: 100,
    x: 0,
    y: 0,
    dir_x: 0,
    dir_y: 1
  });
});

test("empty account list returns empty array without auto creation", async () => {
  const { service, characterStore } = createContext();

  const result = await service.list(createRequest());

  assert.deepEqual(result.characters, []);
  assert.equal(characterStore.rows.length, 0);
});

test("select own active character updates last login and issues character-bound ticket", async () => {
  const characterStore = new FakeCharacterStore();
  characterStore.rows.push(
    createCharacter({
      characterId: "chr_0000000000001",
      accountPlayerId: "player-001",
      worldId: 9,
      name: "Echo"
    })
  );
  const { service, issuedTickets, blocklistChecks } = createContext({ characterStore });

  const result = await service.select(createRequest(), { character_id: "chr_0000000000001" });

  assert.equal(result.ok, true);
  assert.equal(result.character.character_id, "chr_0000000000001");
  assert.equal(result.character.last_login_at, "2026-06-25T12:00:00.000Z");
  assert.equal(result.gameProxyHost, "127.0.0.1");
  assert.equal(result.gameProxyPort, 4000);
  assert.deepEqual(issuedTickets, [
    {
      playerId: "player-001",
      clientIp: "127.0.0.1",
      options: { characterId: "chr_0000000000001", worldId: 9 }
    }
  ]);
  assert.deepEqual(blocklistChecks, [
    { playerId: "player-001", clientIp: "127.0.0.1", source: "character_select" }
  ]);

  const ticketPayload = decodeTicketPayload(result.ticket);
  assert.equal(ticketPayload.playerId, "player-001");
  assert.equal(ticketPayload.characterId, "chr_0000000000001");
  assert.equal(ticketPayload.worldId, 9);
});

test("select rejects other account, soft-deleted, disabled, invalid id, and disabled account", async () => {
  const characterStore = new FakeCharacterStore();
  characterStore.rows.push(
    createCharacter({ characterId: "chr_0000000000001", accountPlayerId: "player-002" }),
    createCharacter({ characterId: "chr_0000000000002", accountPlayerId: "player-001", deletedAt: "2026-06-25T00:00:00.000Z" }),
    createCharacter({ characterId: "chr_0000000000003", accountPlayerId: "player-001", status: "disabled" }),
    createCharacter({ characterId: "chr_0000000000004", accountPlayerId: "disabled-player" }),
    createCharacter({ characterId: "chr_0000000000005", accountPlayerId: "blocked-player" })
  );
  const { service } = createContext({ characterStore });

  await assertApiError(service.select(createRequest(), { character_id: "bad-id" }), 400, "INVALID_CHARACTER_ID");
  await assertApiError(service.select(createRequest(), { character_id: "chr_0000000000001" }), 403, "CHARACTER_OWNER_MISMATCH");
  await assertApiError(service.select(createRequest(), { character_id: "chr_0000000000002" }), 403, "CHARACTER_NOT_FOUND");
  await assertApiError(service.select(createRequest(), { character_id: "chr_0000000000003" }), 403, "CHARACTER_NOT_LOGINABLE");
  await assertApiError(service.select(createRequest("disabled-token"), { character_id: "chr_0000000000004" }), 403, "ACCOUNT_DISABLED");
  await assertApiError(service.select(createRequest("blocked-token"), { character_id: "chr_0000000000005" }), 403, "PLAYER_BLOCKED");
});

test("authenticated character endpoints reject missing and invalid bearer tokens", async () => {
  const { service } = createContext();

  await assertApiError(service.list(createRequest(null)), 401, "MISSING_BEARER_TOKEN");
  await assertApiError(service.create(createRequest("invalid-token"), { name: "Echo", appearance: {} }), 401, "INVALID_ACCESS_TOKEN");
});
