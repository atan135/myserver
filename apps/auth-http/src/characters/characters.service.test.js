import assert from "node:assert/strict";
import test from "node:test";

process.env.TS_NODE_TRANSPILE_ONLY ??= "true";

const { CharactersService } = await import("./characters.service.js");

function characterFixture(input) {
  return {
    characterId: "chr_0000000000001",
    accountPlayerId: input.accountPlayerId,
    worldId: input.worldId,
    name: input.name,
    status: "active",
    appearance: input.appearance,
    position: input.position,
    affinity: input.affinity,
    mastery: input.mastery,
    createdAt: "2026-07-30T00:00:00.000Z",
    lastLoginAt: null,
    deletedAt: null
  };
}

function createService({ account = { playerId: "plr_0000000000001" } } = {}) {
  const calls = [];
  const authStore = {
    async findPlayerAuthStateByPlayerId(playerId) {
      return account?.playerId === playerId ? account : null;
    }
  };
  const characterStore = {
    enabled: true,
    async createCharacterForAdmin(input, options) {
      calls.push({ input, options });
      return characterFixture(input);
    }
  };
  const config = {
    characterNameMinLength: 2,
    characterNameMaxLength: 16,
    characterAppearanceMaxJsonBytes: 4096,
    characterDefaultWorldId: 0,
    characterDefaultSceneId: 100,
    characterDefaultX: 0,
    characterDefaultY: 0,
    characterDefaultDirX: 0,
    characterDefaultDirY: 1
  };
  return {
    calls,
    service: new CharactersService(config, authStore, characterStore, {})
  };
}

test("CharactersService admin creation owns defaults, ID store call, and bypass audit context", async () => {
  const { calls, service } = createService();

  const result = await service.createForAdmin({
    accountPlayerId: "plr_0000000000001",
    name: "Echo",
    appearance: { body: "default" },
    adminActor: "operator",
    reason: "support request"
  });

  assert.equal(result.character.character_id, "chr_0000000000001");
  assert.equal(result.character.account_player_id, "plr_0000000000001");
  assert.deepEqual(calls, [{
    input: {
      accountPlayerId: "plr_0000000000001",
      worldId: 0,
      name: "Echo",
      appearance: { body: "default" },
      position: { sceneId: 100, x: 0, y: 0, dirX: 0, dirY: 1 },
      affinity: { earth: 2500, fire: 2500, water: 2500, wind: 2500 },
      mastery: { earth: 0, fire: 0, water: 0, wind: 0 }
    },
    options: {
      bypassCharacterLimit: true,
      adminActor: "operator",
      reason: "support request",
      targetAccountPlayerId: "plr_0000000000001",
      action: "admin_character_create"
    }
  }]);
});

test("CharactersService admin creation rejects a missing target account before writing", async () => {
  const { calls, service } = createService({ account: null });

  await assert.rejects(
    () => service.createForAdmin({
      accountPlayerId: "plr_0000000000001",
      name: "Echo",
      adminActor: "operator",
      reason: "support request"
    }),
    (error) => {
      assert.equal(error.getStatus(), 404);
      assert.equal(error.getResponse().error, "PLAYER_NOT_FOUND");
      return true;
    }
  );
  assert.equal(calls.length, 0);
});
