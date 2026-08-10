import assert from "node:assert/strict";
import test from "node:test";

process.env.TS_NODE_TRANSPILE_ONLY ??= "true";

const { AuthService } = await import("./auth/auth.service.js");
const { CharactersService } = await import("./characters/characters.service.js");
const { GameTicketController } = await import("./game-ticket/game-ticket.controller.js");

const CLIENT_SERVICES = {
  game: {
    host: "game.game.example",
    port: 4000,
    protocol: "kcp"
  },
  chat: {
    host: "chat.game.example",
    port: 443,
    protocol: "wss"
  },
  mail: {
    host: "api.bevy.zergzerg.cn",
    port: 443,
    protocol: "https"
  },
  announce: null
};

const CHARACTER = {
  characterId: "chr_0000000000001",
  accountPlayerId: "plr_0000000000001",
  worldId: 7,
  name: "Echo",
  status: "active",
  appearance: { body: "default" },
  position: { sceneId: 100, x: 0, y: 0, dirX: 0, dirY: 1 },
  affinity: { earth: 2500, fire: 2500, water: 2500, wind: 2500 },
  mastery: { earth: 0, fire: 0, water: 0, wind: 0 },
  createdAt: "2026-07-30T00:00:00.000Z",
  lastLoginAt: null,
  deletedAt: null
};

function gameTicket() {
  return {
    value: "ticket-value",
    expiresAt: "2026-07-30T00:15:00.000Z"
  };
}

function publicServiceAuth() {
  return {
    async assertNotInMaintenance() {},
    async buildServicePayload() {
      return CLIENT_SERVICES;
    },
    getGameProxyDescriptor(services) {
      return services.game;
    }
  };
}

test("login response returns configured public chat and mail descriptors", async () => {
  const authService = new AuthService(
    { localDiscoveryFallbackEnabled: false },
    {},
    {},
    {},
    { async discoverClientServices() { return CLIENT_SERVICES; } }
  );

  const result = await authService.buildLoginSuccess({
    playerId: "plr_0000000000001",
    accessToken: "access-token",
    gameTicket: gameTicket()
  });

  assert.deepEqual(result.services, CLIENT_SERVICES);
  assert.deepEqual(result.services.chat, CLIENT_SERVICES.chat);
  assert.deepEqual(result.services.mail, CLIENT_SERVICES.mail);
});

test("character selection response keeps configured public chat and mail descriptors", async () => {
  const authStore = {
    async getSessionByAccessToken() {
      return { playerId: CHARACTER.accountPlayerId };
    },
    async assertPlayerCanIssueTicket() {},
    async issueGameTicket() {
      return gameTicket();
    }
  };
  const characterStore = {
    enabled: true,
    async getByCharacterId() {
      return CHARACTER;
    },
    async updateLastLoginAt() {
      return true;
    }
  };
  const service = new CharactersService({ trustProxy: false }, authStore, characterStore, publicServiceAuth());

  const result = await service.select(
    { headers: { authorization: "Bearer access-token" }, ip: "127.0.0.1" },
    { character_id: CHARACTER.characterId }
  );

  assert.deepEqual(result.services, CLIENT_SERVICES);
  assert.deepEqual(result.services.chat, CLIENT_SERVICES.chat);
  assert.deepEqual(result.services.mail, CLIENT_SERVICES.mail);
});

test("game-ticket issue response keeps configured public chat and mail descriptors", async () => {
  const authStore = {
    async getSessionByAccessToken() {
      return { playerId: CHARACTER.accountPlayerId };
    },
    async assertPlayerCanIssueTicket() {},
    async issueGameTicket() {
      return gameTicket();
    }
  };
  const controller = new GameTicketController(
    authStore,
    { trustProxy: false },
    { async checkPlayer() { return { unavailable: false, blocked: false }; } },
    null,
    { enabled: true, async getByCharacterId() { return CHARACTER; } },
    publicServiceAuth()
  );

  const result = await controller.issue(
    { headers: { authorization: "Bearer access-token" }, ip: "127.0.0.1", url: "/api/v1/game-ticket/issue" },
    { character_id: CHARACTER.characterId }
  );

  assert.deepEqual(result.services, CLIENT_SERVICES);
  assert.deepEqual(result.services.chat, CLIENT_SERVICES.chat);
  assert.deepEqual(result.services.mail, CLIENT_SERVICES.mail);
});
