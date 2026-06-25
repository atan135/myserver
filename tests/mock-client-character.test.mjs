import assert from "node:assert/strict";
import net from "node:net";
import test from "node:test";

import { parseArgs } from "../tools/mock-client/src/args.js";
import {
  fetchTicket,
  formatLoginSummary,
  parseCharacterAppearance
} from "../tools/mock-client/src/auth.js";
import {
  runCharacterDuplicateName,
  runCharacterLoginAuth,
  runCharacterLimit,
  runCharacterList,
  runCharacterSelect
} from "../tools/mock-client/src/scenarios/character.js";
import { MESSAGE_TYPE, HEADER_LEN, MAGIC } from "../tools/mock-client/src/constants.js";
import { encodePacket } from "../tools/mock-client/src/packet.js";
import {
  decodeFieldsWithRepeated,
  encodeBoolField,
  encodeStringField,
  readString
} from "../tools/mock-client/src/protocol.js";

function createTicket(payload) {
  return `${Buffer.from(JSON.stringify(payload)).toString("base64url")}.sig`;
}

function clone(value) {
  return JSON.parse(JSON.stringify(value));
}

function response(status, payload) {
  return {
    ok: status >= 200 && status < 300,
    status,
    async json() {
      return clone(payload);
    }
  };
}

function createCharacter(index, overrides = {}) {
  return {
    character_id: `chr_${String(index).padStart(13, "0")}`,
    character_id_short: String(index).padStart(8, "0"),
    display_discriminator: String(index).padStart(8, "0"),
    name: overrides.name || `Role${index}`,
    world_id: overrides.worldId ?? 9,
    status: "active",
    appearance_json: overrides.appearance || { body: "default" },
    last_login_at: null,
    position: { scene_id: 100, x: 0, y: 0, dir_x: 0, dir_y: 1 },
    attributes: {
      affinity: { earth: 2500, fire: 2500, water: 2500, wind: 2500 },
      mastery: { earth: 0, fire: 0, water: 0, wind: 0 }
    }
  };
}

function installMockAuthFetch({
  initialCharacters = [],
  playerId = "player-001",
  ticketExpiresAt = "2026-06-25T12:15:00.000Z"
} = {}) {
  const calls = [];
  const characters = initialCharacters.map(clone);
  let nextCharacterIndex = characters.length + 1;

  globalThis.fetch = async (url, init = {}) => {
    const parsedUrl = new URL(String(url));
    const method = init.method || "GET";
    const body = init.body ? JSON.parse(init.body) : {};
    calls.push({ method, pathname: parsedUrl.pathname, body });

    if (parsedUrl.pathname === "/api/v1/auth/login" && method === "POST") {
      return response(201, {
        ok: true,
        playerId,
        loginName: body.loginName,
        guestId: null,
        accessToken: "access-token",
        ticket: null,
        ticketExpiresAt: null,
        services: { game: { host: "127.0.0.1", port: 14000, protocol: "tcp" } }
      });
    }

    if (parsedUrl.pathname === "/api/v1/auth/guest-login" && method === "POST") {
      return response(201, {
        ok: true,
        playerId,
        guestId: body.guestId || "guest-generated",
        loginName: null,
        accessToken: "access-token",
        ticket: null,
        ticketExpiresAt: null,
        services: { game: { host: "127.0.0.1", port: 14000, protocol: "tcp" } }
      });
    }

    if (parsedUrl.pathname === "/api/v1/characters" && method === "GET") {
      return response(200, {
        ok: true,
        playerId,
        characters
      });
    }

    if (parsedUrl.pathname === "/api/v1/characters" && method === "POST") {
      if (characters.length >= 6) {
        return response(403, {
          ok: false,
          error: "CHARACTER_LIMIT_EXCEEDED",
          message: "ordinary accounts can create at most 6 effective characters"
        });
      }

      const character = createCharacter(nextCharacterIndex, {
        name: body.name,
        appearance: body.appearance
      });
      nextCharacterIndex += 1;
      characters.push(character);
      return response(201, {
        ok: true,
        character
      });
    }

    if (parsedUrl.pathname === "/api/v1/characters/select" && method === "POST") {
      const characterId = body.character_id || body.characterId;
      const character = characters.find((candidate) => candidate.character_id === characterId);
      if (!character) {
        return response(403, {
          ok: false,
          error: "CHARACTER_NOT_FOUND",
          message: "character is not available to the current account"
        });
      }

      return response(200, {
        ok: true,
        playerId,
        character: {
          ...character,
          last_login_at: "2026-06-25T12:00:00.000Z"
        },
        ticket: createTicket({
          playerId,
          characterId,
          worldId: character.world_id,
          exp: ticketExpiresAt,
          ver: 1
        }),
        ticketExpiresAt,
        gameProxyHost: "127.0.0.1",
        gameProxyPort: 14000,
        services: { game: { host: "127.0.0.1", port: 14000, protocol: "tcp" } }
      });
    }

    return response(404, { ok: false, error: "NOT_FOUND", path: parsedUrl.pathname });
  };

  return {
    calls,
    characters
  };
}

async function startFakeGameAuthServer() {
  const authRequests = [];
  const sockets = new Set();
  const server = net.createServer((socket) => {
    sockets.add(socket);
    let buffer = Buffer.alloc(0);

    socket.on("data", (chunk) => {
      buffer = Buffer.concat([buffer, chunk]);
      while (buffer.length >= HEADER_LEN) {
        assert.equal(buffer.readUInt16BE(0), MAGIC);
        const messageType = buffer.readUInt16BE(4);
        const seq = buffer.readUInt32BE(6);
        const bodyLen = buffer.readUInt32BE(10);
        const packetLen = HEADER_LEN + bodyLen;
        if (buffer.length < packetLen) {
          return;
        }

        const body = buffer.subarray(HEADER_LEN, packetLen);
        buffer = buffer.subarray(packetLen);
        assert.equal(messageType, MESSAGE_TYPE.AUTH_REQ);

        const fields = decodeFieldsWithRepeated(body);
        const ticket = readString(fields, 1);
        const ticketPayload = JSON.parse(Buffer.from(ticket.split(".")[0], "base64url").toString("utf8"));
        authRequests.push({ seq, ticket, ticketPayload });

        socket.write(
          encodePacket(
            MESSAGE_TYPE.AUTH_RES,
            seq,
            Buffer.concat([
              encodeBoolField(1, true),
              encodeStringField(2, ticketPayload.playerId),
              encodeStringField(3, "")
            ])
          )
        );
      }
    });

    socket.on("close", () => {
      sockets.delete(socket);
    });
  });

  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });

  return {
    port: server.address().port,
    authRequests,
    async close() {
      for (const socket of sockets) {
        socket.destroy();
      }
      await new Promise((resolve, reject) => {
        server.close((error) => {
          if (error) {
            reject(error);
            return;
          }
          resolve();
        });
      });
    }
  };
}

async function captureLogs(fn) {
  const originalLog = console.log;
  const logs = [];
  console.log = (...args) => {
    logs.push(args.join(" "));
  };
  try {
    const result = await fn();
    return { result, logs };
  } finally {
    console.log = originalLog;
  }
}

test("mock-client parses character flags", () => {
  const options = parseArgs([
    "--scenario", "character-create",
    "--character-id", "chr_0000000000001",
    "--character-name", "Echo",
    "--character-appearance-json", '{"body":"default","palette":"blue"}',
    "--auto-create-character",
    "--create-character-if-missing",
    "--character-name-prefix", "DebugRole",
    "--json-output"
  ]);

  assert.equal(options.scenario, "character-create");
  assert.equal(options.characterId, "chr_0000000000001");
  assert.equal(options.characterName, "Echo");
  assert.deepEqual(parseCharacterAppearance(options), { body: "default", palette: "blue" });
  assert.equal(options.autoCreateCharacter, true);
  assert.equal(options.createCharacterIfMissing, true);
  assert.equal(options.characterNamePrefix, "DebugRole");
  assert.equal(options.jsonOutput, true);
});

test("fetchTicket selects an existing character and summarizes ticket payload", async () => {
  const existing = createCharacter(1, { name: "Echo" });
  const mock = installMockAuthFetch({ initialCharacters: [existing] });
  const options = parseArgs([
    "--login-name", "test001",
    "--password", "Passw0rd!",
    "--character-id", existing.character_id
  ]);

  const login = await fetchTicket(options);
  const summary = formatLoginSummary(login);

  assert.equal(login.playerId, "player-001");
  assert.equal(login.characterId, existing.character_id);
  assert.equal(summary.characterId, existing.character_id);
  assert.equal(summary.worldId, 9);
  assert.equal(summary.ticketPayload.playerId, "player-001");
  assert.equal(summary.ticketPayload.characterId, existing.character_id);
  assert.equal(summary.ticketPayload.worldId, 9);
  assert.equal(summary.ticketPayload.exp, "2026-06-25T12:15:00.000Z");
  assert.deepEqual(
    mock.calls.map((call) => `${call.method} ${call.pathname}`),
    [
      "POST /api/v1/auth/login",
      "POST /api/v1/characters/select"
    ]
  );
});

test("fetchTicket prompts instead of entering game when account has no characters", async () => {
  installMockAuthFetch();
  const options = parseArgs(["--login-name", "test001", "--password", "Passw0rd!"]);

  await assert.rejects(
    () => fetchTicket(options),
    /has no characters; create one first/
  );
});

test("fetchTicket can auto-create a missing character before selecting it", async () => {
  const mock = installMockAuthFetch();
  const options = parseArgs([
    "--login-name", "test001",
    "--password", "Passw0rd!",
    "--auto-create-character",
    "--character-name", "Echo",
    "--character-appearance-json", '{"body":"default"}'
  ]);

  const login = await fetchTicket(options);

  assert.equal(login.character.name, "Echo");
  assert.equal(login.characterId, "chr_0000000000001");
  assert.deepEqual(mock.characters.map((character) => character.name), ["Echo"]);
  assert.deepEqual(
    mock.calls.map((call) => `${call.method} ${call.pathname}`),
    [
      "POST /api/v1/auth/login",
      "POST /api/v1/characters",
      "POST /api/v1/characters/select"
    ]
  );
});

test("character-list emits machine-readable JSON", async () => {
  installMockAuthFetch({ initialCharacters: [createCharacter(1, { name: "Echo" })] });
  const options = parseArgs([
    "--scenario", "character-list",
    "--login-name", "test001",
    "--password", "Passw0rd!",
    "--json-output"
  ]);

  const { logs } = await captureLogs(() => runCharacterList(options));
  const payload = JSON.parse(logs.at(-1));

  assert.equal(payload.ok, true);
  assert.equal(payload.scenario, "character-list");
  assert.equal(payload.characterCount, 1);
  assert.equal(payload.characters[0].characterId, "chr_0000000000001");
});

test("character-select can display selected characterId in JSON output", async () => {
  const existing = createCharacter(1, { name: "Echo" });
  installMockAuthFetch({ initialCharacters: [existing] });
  const options = parseArgs([
    "--scenario", "character-select",
    "--login-name", "test001",
    "--password", "Passw0rd!",
    "--character-id", existing.character_id,
    "--json-output"
  ]);

  const { logs } = await captureLogs(() => runCharacterSelect(options));
  const payload = JSON.parse(logs.at(-1));

  assert.equal(payload.ok, true);
  assert.equal(payload.login.characterId, existing.character_id);
  assert.equal(payload.login.ticketPayload.characterId, existing.character_id);
});

test("character-login-auth auto-creates, selects, and authenticates with a character-bound ticket", async () => {
  const ticketExpiresAt = new Date(Date.now() + 300000).toISOString();
  const mock = installMockAuthFetch({ ticketExpiresAt });
  const gameServer = await startFakeGameAuthServer();
  const options = parseArgs([
    "--scenario", "character-login-auth",
    "--login-name", "test001",
    "--password", "Passw0rd!",
    "--auto-create-character",
    "--character-name", "Echo",
    "--character-appearance-json", '{"body":"default"}',
    "--game-host", "127.0.0.1",
    "--port", String(gameServer.port),
    "--no-service-discovery",
    "--json-output",
    "--timeout-ms", "1000"
  ]);

  try {
    const { logs, result } = await captureLogs(() => runCharacterLoginAuth(options));
    const payload = JSON.parse(logs.at(-1));

    assert.equal(result.ok, true);
    assert.equal(payload.ok, true);
    assert.equal(payload.scenario, "character-login-auth");
    assert.equal(payload.login.playerId, "player-001");
    assert.equal(payload.login.characterId, "chr_0000000000001");
    assert.equal(payload.login.ticketPayload.playerId, "player-001");
    assert.equal(payload.login.ticketPayload.characterId, "chr_0000000000001");
    assert.equal(payload.login.ticketPayload.worldId, 9);
    assert.deepEqual(
      mock.calls.map((call) => `${call.method} ${call.pathname}`),
      [
        "POST /api/v1/auth/login",
        "POST /api/v1/characters",
        "POST /api/v1/characters/select"
      ]
    );
    assert.equal(gameServer.authRequests.length, 1);
    assert.deepEqual(gameServer.authRequests[0].ticketPayload, {
      playerId: "player-001",
      characterId: "chr_0000000000001",
      worldId: 9,
      exp: ticketExpiresAt,
      ver: 1
    });
  } finally {
    await gameServer.close();
  }
});

test("duplicate-name scenario creates two characters with the same name", async () => {
  installMockAuthFetch();
  const options = parseArgs([
    "--scenario", "character-duplicate-name",
    "--login-name", "test001",
    "--password", "Passw0rd!",
    "--character-name", "Echo",
    "--json-output"
  ]);

  const { result } = await captureLogs(() => runCharacterDuplicateName(options));

  assert.equal(result.ok, true);
  assert.deepEqual(result.characters.map((character) => character.name), ["Echo", "Echo"]);
  assert.equal(new Set(result.characters.map((character) => character.characterId)).size, 2);
});

test("character-limit scenario treats the 7th ordinary character failure as success", async () => {
  installMockAuthFetch();
  const options = parseArgs([
    "--scenario", "character-limit",
    "--login-name", "test001",
    "--password", "Passw0rd!",
    "--character-name-prefix", "Limit",
    "--json-output"
  ]);

  const { logs, result } = await captureLogs(() => runCharacterLimit(options));
  const payload = JSON.parse(logs.at(-1));

  assert.equal(result.ok, true);
  assert.equal(payload.ok, true);
  assert.equal(payload.createdCount, 6);
  assert.equal(payload.failure.attempt, 7);
  assert.equal(payload.failure.overallCharacterNumber, 7);
  assert.equal(payload.failure.error, "CHARACTER_LIMIT_EXCEEDED");
});

test("character-limit counts existing characters before probing the 7th failure", async () => {
  installMockAuthFetch({
    initialCharacters: Array.from({ length: 5 }, (_, index) => createCharacter(index + 1))
  });
  const options = parseArgs([
    "--scenario", "character-limit",
    "--login-name", "test001",
    "--password", "Passw0rd!",
    "--character-name-prefix", "Limit",
    "--json-output"
  ]);

  const { result } = await captureLogs(() => runCharacterLimit(options));

  assert.equal(result.ok, true);
  assert.equal(result.existingCount, 5);
  assert.equal(result.createdCount, 1);
  assert.equal(result.failure.attempt, 2);
  assert.equal(result.failure.overallCharacterNumber, 7);
  assert.equal(result.failure.error, "CHARACTER_LIMIT_EXCEEDED");
});
