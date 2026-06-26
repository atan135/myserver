import { MESSAGE_TYPE } from "../constants.js";
import { TcpProtocolClient } from "../client.js";
import {
  encodeDebugCharacterTitleReq,
  encodeDebugApplyCharacterElementChangeReq,
  encodeEquipCharacterTitleReq,
  encodeGetCharacterDisciplinesReq,
  encodeGetCharacterElementsReq,
  encodeGetCharacterTitlesReq,
  encodeRoomJoinReq
} from "../messages.js";
import {
  buildCharacterCreateInput,
  buildGeneratedCharacterName,
  createCharacter,
  fetchLoginSession,
  fetchTicket,
  formatLoginSummary,
  listCharacters,
  requestCreateCharacter,
  selectCharacter
} from "../auth.js";
import {
  authenticateClient,
  printResponse
} from "./room.js";

function shouldAutoGuestLogin(options) {
  return !options.ticket && !options.guestId && !options.loginName && !options.password;
}

function buildLoginOverrides(options, suffix) {
  return shouldAutoGuestLogin(options)
    ? { guestId: `${options.roomId}-${suffix}` }
    : {};
}

function getCharacterId(character) {
  return character?.character_id || character?.characterId || "";
}

function getCharacterWorldId(character) {
  return character?.world_id ?? character?.worldId ?? null;
}

function summarizeCharacter(character) {
  if (!character) {
    return null;
  }

  return {
    characterId: getCharacterId(character),
    name: character.name || "",
    worldId: getCharacterWorldId(character),
    status: character.status || "",
    displayDiscriminator: character.display_discriminator || character.displayDiscriminator || ""
  };
}

function summarizeCharacters(characters) {
  return (characters || []).map((character) => summarizeCharacter(character));
}

function buildEnvelope(scenario, ok, data = {}) {
  return {
    ok,
    scenario,
    ...data
  };
}

function printResult(label, envelope, options) {
  if (options.jsonOutput) {
    console.log(JSON.stringify(envelope, null, 2));
    return;
  }

  console.log(`${label}:`, JSON.stringify(envelope, null, 2));
}

function buildElementDeltaOptions(options, prefix) {
  return {
    earth: options[`${prefix}EarthDelta`] || 0,
    fire: options[`${prefix}FireDelta`] || 0,
    water: options[`${prefix}WaterDelta`] || 0,
    wind: options[`${prefix}WindDelta`] || 0
  };
}

function findTitleById(titlesRes, titleId) {
  return (titlesRes?.titles || []).find((title) => title.definition?.id === String(titleId)) || null;
}

function summarizeTitle(title) {
  if (!title) {
    return null;
  }
  return {
    id: title.definition?.id || "",
    name: title.definition?.name || "",
    type: title.definition?.type || "",
    rarity: title.definition?.rarity || "",
    owned: Boolean(title.owned),
    equipped: Boolean(title.equipped),
    sourceType: title.sourceType || "",
    sourceId: title.sourceId || "",
    expiresAt: title.expiresAt || "",
    expired: Boolean(title.expired),
    sortOrder: title.definition?.sortOrder ?? 0
  };
}

function summarizeTitlesResponse(response, titleId = "") {
  return {
    ok: Boolean(response?.ok),
    errorCode: response?.errorCode || "",
    characterId: response?.characterId || "",
    ownedCount: (response?.titles || []).filter((title) => title.owned).length,
    targetTitle: titleId ? summarizeTitle(findTitleById(response, titleId)) : null,
    equippedTitle: summarizeTitle(response?.equippedTitle)
  };
}

function summarizeDiscipline(discipline) {
  if (!discipline) {
    return null;
  }
  return {
    disciplineId: discipline.disciplineId || "",
    points: discipline.points ?? 0,
    tier: discipline.tier || "",
    active: Boolean(discipline.active),
    updatedAt: discipline.updatedAt || ""
  };
}

function findDisciplineById(disciplinesRes, disciplineId) {
  return (disciplinesRes?.disciplines || []).find((discipline) => discipline.disciplineId === disciplineId) || null;
}

function summarizeUnlockedTitles(titles) {
  return (titles || []).map(summarizeTitle).filter(Boolean);
}

async function queryTitles(client, options, seq, label) {
  await client.send(MESSAGE_TYPE.GET_CHARACTER_TITLES_REQ, seq, encodeGetCharacterTitlesReq());
  return client.readUntil(
    options.timeoutMs,
    (packet) => packet.messageType === MESSAGE_TYPE.GET_CHARACTER_TITLES_RES && packet.seq === seq,
    label
  );
}

async function queryDisciplines(client, options, seq, label) {
  await client.send(MESSAGE_TYPE.GET_CHARACTER_DISCIPLINES_REQ, seq, encodeGetCharacterDisciplinesReq());
  return client.readUntil(
    options.timeoutMs,
    (packet) => packet.messageType === MESSAGE_TYPE.GET_CHARACTER_DISCIPLINES_RES && packet.seq === seq,
    label
  );
}

async function withJsonQuiet(options, fn) {
  if (!options.jsonOutput) {
    return fn();
  }

  const originalLog = console.log;
  console.log = () => {};
  try {
    return await fn();
  } finally {
    console.log = originalLog;
  }
}

async function loginSession(options, suffix = "character") {
  const loginOptions = {
    ...options,
    characterId: "",
    autoCreateCharacter: false,
    createCharacterIfMissing: false
  };

  const session = await fetchLoginSession(loginOptions, buildLoginOverrides(options, suffix));
  if (!session.accessToken) {
    throw new Error("character scenarios require an access token from auth-http");
  }
  return session;
}

function createCharacterInputForIndex(options, index) {
  return buildCharacterCreateInput(options, {
    name: options.characterName || buildGeneratedCharacterName(options, String(index + 1).padStart(2, "0")),
    suffix: String(index + 1).padStart(2, "0")
  });
}

export async function runCharacterList(options) {
  const session = await loginSession(options, "character-list");
  const payload = await listCharacters(options, session.accessToken);
  const envelope = buildEnvelope("character-list", true, {
    playerId: payload.playerId || session.playerId,
    characterCount: payload.characters.length,
    characters: summarizeCharacters(payload.characters)
  });
  printResult("character.list", envelope, options);
  return envelope;
}

export async function runCharacterCreate(options) {
  const session = await loginSession(options, "character-create");
  const input = buildCharacterCreateInput(options);
  const character = await createCharacter(options, session.accessToken, input);
  const envelope = buildEnvelope("character-create", true, {
    playerId: session.playerId,
    character: summarizeCharacter(character)
  });
  printResult("character.create", envelope, options);
  return envelope;
}

export async function runCharacterSelect(options) {
  const session = await loginSession(options, "character-select");
  let characterId = options.characterId;
  if (!characterId) {
    const payload = await listCharacters(options, session.accessToken);
    const selected = payload.characters[0];
    if (!selected) {
      throw new Error("account has no characters; create one first with --scenario character-create");
    }
    characterId = getCharacterId(selected);
  }

  const login = await selectCharacter(options, session, characterId);
  const envelope = buildEnvelope("character-select", true, {
    login: formatLoginSummary(login)
  });
  printResult("character.select", envelope, options);
  return envelope;
}

export async function runCharacterDuplicateName(options) {
  const session = await loginSession(options, "character-duplicate-name");
  const name = options.characterName || buildGeneratedCharacterName(options, "dup");
  const input = buildCharacterCreateInput(options, { name });
  const first = await createCharacter(options, session.accessToken, input);
  const second = await createCharacter(options, session.accessToken, input);

  const envelope = buildEnvelope("character-duplicate-name", true, {
    playerId: session.playerId,
    duplicateName: name,
    characters: [summarizeCharacter(first), summarizeCharacter(second)]
  });
  printResult("character.duplicateName", envelope, options);
  return envelope;
}

export async function runCharacterLimit(options) {
  const session = await loginSession(options, "character-limit");
  const listed = await listCharacters(options, session.accessToken);
  const existingCount = listed.characters.length;
  const created = [];
  let failure = null;

  for (let index = existingCount; index < 7; index += 1) {
    const input = createCharacterInputForIndex(options, index);
    const response = await requestCreateCharacter(options, session.accessToken, input);
    if (response.ok) {
      created.push(summarizeCharacter(response.payload.character));
      continue;
    }

    failure = {
      status: response.status,
      error: response.payload?.error || response.payload?.errorCode || "REQUEST_FAILED",
      message: response.payload?.message || null,
      attempt: created.length + 1,
      overallCharacterNumber: index + 1
    };
    break;
  }

  const ok = Boolean(
    failure &&
    failure.overallCharacterNumber === 7 &&
    failure.error === "CHARACTER_LIMIT_EXCEEDED" &&
    existingCount + created.length === 6
  );
  const envelope = buildEnvelope("character-limit", ok, {
    playerId: session.playerId,
    existingCount,
    createdCount: created.length,
    created,
    failure
  });
  printResult("character.limit", envelope, options);
  if (!ok && !options.jsonOutput) {
    throw new Error(`expected 7th character creation to fail with CHARACTER_LIMIT_EXCEEDED, got ${JSON.stringify(failure)}`);
  }
  return envelope;
}

export async function runCharacterLoginAuth(options) {
  const login = await fetchTicket(options);
  const envelope = await withJsonQuiet(options, async () => {
    const client = new TcpProtocolClient(options, "characterClient");
    await client.connect();

    try {
      await authenticateClient(client, options, login, 1);
      return buildEnvelope("character-login-auth", true, {
        login: formatLoginSummary(login)
      });
    } finally {
      client.close();
    }
  });
  printResult("character.loginAuth", envelope, options);
  return envelope;
}

export async function runCharacterRoomJoin(options) {
  const login = await fetchTicket(options);
  const envelope = await withJsonQuiet(options, async () => {
    const client = new TcpProtocolClient(options, "characterClient");
    await client.connect();

    try {
      await authenticateClient(client, options, login, 1);
      await client.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(options.roomId, options.policyId || ""));
      const joinRes = await client.readUntil(
        options.timeoutMs,
        (packet) => packet.messageType === MESSAGE_TYPE.ROOM_JOIN_RES && packet.seq === 2,
        "roomJoin"
      );
      if (!joinRes.ok) {
        throw new Error(`room join failed: ${joinRes.errorCode}`);
      }
      const push = await client.readUntil(
        options.timeoutMs,
        (packet) => packet.messageType === MESSAGE_TYPE.ROOM_STATE_PUSH,
        "roomStatePush(join)"
      );
      return buildEnvelope("character-room-join", true, {
        login: formatLoginSummary(login),
        room: {
          roomId: joinRes.roomId,
          event: push.event || null,
          memberCount: push.snapshot?.members?.length ?? null,
          ownerPlayerId: push.snapshot?.ownerPlayerId || null
        }
      });
    } finally {
      client.close();
    }
  });
  printResult("character.roomJoin", envelope, options);
  return envelope;
}

export async function runCharacterElementsDebug(options) {
  const login = await fetchTicket(options);
  const envelope = await withJsonQuiet(options, async () => {
    const client = new TcpProtocolClient(options, "characterClient");
    await client.connect();

    try {
      await authenticateClient(client, options, login, 1);

      await client.send(
        MESSAGE_TYPE.GET_CHARACTER_ELEMENTS_REQ,
        2,
        encodeGetCharacterElementsReq()
      );
      const before = await client.readUntil(
        options.timeoutMs,
        (packet) => packet.messageType === MESSAGE_TYPE.GET_CHARACTER_ELEMENTS_RES && packet.seq === 2,
        "getCharacterElements(before)"
      );

      const affinityDelta = buildElementDeltaOptions(options, "elementAffinity");
      const masteryDelta = buildElementDeltaOptions(options, "elementMastery");
      await client.send(
        MESSAGE_TYPE.DEBUG_APPLY_CHARACTER_ELEMENT_CHANGE_REQ,
        3,
        encodeDebugApplyCharacterElementChangeReq(
          affinityDelta,
          masteryDelta,
          options.elementChangeReason,
          options.elementDebugToken
        )
      );
      const change = await client.readUntil(
        options.timeoutMs,
        (packet) => packet.messageType === MESSAGE_TYPE.DEBUG_APPLY_CHARACTER_ELEMENT_CHANGE_RES && packet.seq === 3,
        "debugApplyCharacterElementChange"
      );

      await client.send(
        MESSAGE_TYPE.GET_CHARACTER_ELEMENTS_REQ,
        4,
        encodeGetCharacterElementsReq()
      );
      const after = await client.readUntil(
        options.timeoutMs,
        (packet) => packet.messageType === MESSAGE_TYPE.GET_CHARACTER_ELEMENTS_RES && packet.seq === 4,
        "getCharacterElements(after)"
      );

      const ok = Boolean(before.ok && change.ok && after.ok);
      return buildEnvelope("character-elements-debug", ok, {
        login: formatLoginSummary(login),
        request: {
          affinityDelta,
          masteryDelta,
          reason: options.elementChangeReason || "",
          debugTokenProvided: Boolean(options.elementDebugToken)
        },
        before,
        change,
        after
      });
    } finally {
      client.close();
    }
  });

  printResult("character.elementsDebug", envelope, options);
  return envelope;
}

export async function runCharacterTitlesDebug(options) {
  const login = await fetchTicket(options);
  const envelope = await withJsonQuiet(options, async () => {
    const client = new TcpProtocolClient(options, "characterClient");
    await client.connect();

    try {
      await authenticateClient(client, options, login, 1);

      const before = await queryTitles(client, options, 2, "getCharacterTitles(before)");

      await client.send(
        MESSAGE_TYPE.DEBUG_CHARACTER_TITLE_REQ,
        3,
        encodeDebugCharacterTitleReq({
          action: "grant_title",
          titleId: options.titleId,
          reason: options.titleChangeReason,
          debugToken: options.titleDebugToken
        })
      );
      const action = await client.readUntil(
        options.timeoutMs,
        (packet) => packet.messageType === MESSAGE_TYPE.DEBUG_CHARACTER_TITLE_RES && packet.seq === 3,
        "debugCharacterTitle(grant)"
      );

      await client.send(
        MESSAGE_TYPE.EQUIP_CHARACTER_TITLE_REQ,
        4,
        encodeEquipCharacterTitleReq(options.titleId)
      );
      const equip = await client.readUntil(
        options.timeoutMs,
        (packet) => packet.messageType === MESSAGE_TYPE.EQUIP_CHARACTER_TITLE_RES && packet.seq === 4,
        "equipCharacterTitle"
      );

      const after = await queryTitles(client, options, 5, "getCharacterTitles(after)");
      const disciplines = await queryDisciplines(client, options, 6, "getCharacterDisciplines");

      const ok = Boolean(before.ok && action.ok && equip.ok && after.ok && disciplines.ok);
      return buildEnvelope("character-titles-debug", ok, {
        login: formatLoginSummary(login),
        before: summarizeTitlesResponse(before, options.titleId),
        action: {
          ok: Boolean(action.ok),
          errorCode: action.errorCode || "",
          action: action.action || "grant_title",
          title: summarizeTitle(action.title),
          equip: {
            ok: Boolean(equip.ok),
            errorCode: equip.errorCode || ""
          }
        },
        after: summarizeTitlesResponse(after, options.titleId),
        unlockedTitles: summarizeUnlockedTitles(action.unlockedTitles),
        equippedTitle: summarizeTitle(after.equippedTitle || equip.equippedTitle),
        discipline: summarizeDiscipline(findDisciplineById(disciplines, options.disciplineId)),
        request: {
          titleId: options.titleId,
          reason: options.titleChangeReason || "",
          debugTokenProvided: Boolean(options.titleDebugToken)
        }
      });
    } finally {
      client.close();
    }
  });

  printResult("character.titlesDebug", envelope, options);
  return envelope;
}

export async function runCharacterDisciplinesDebug(options) {
  const login = await fetchTicket(options);
  const envelope = await withJsonQuiet(options, async () => {
    const client = new TcpProtocolClient(options, "characterClient");
    await client.connect();

    try {
      await authenticateClient(client, options, login, 1);

      const before = await queryTitles(client, options, 2, "getCharacterTitles(before)");

      await client.send(
        MESSAGE_TYPE.DEBUG_CHARACTER_TITLE_REQ,
        3,
        encodeDebugCharacterTitleReq({
          action: "set_discipline",
          disciplineId: options.disciplineId,
          disciplineTier: options.disciplineTier,
          disciplinePoints: options.disciplinePoints,
          disciplineActive: true,
          triggerUnlockCheck: true,
          reason: options.titleChangeReason,
          debugToken: options.titleDebugToken
        })
      );
      const action = await client.readUntil(
        options.timeoutMs,
        (packet) => packet.messageType === MESSAGE_TYPE.DEBUG_CHARACTER_TITLE_RES && packet.seq === 3,
        "debugCharacterTitle(setDiscipline)"
      );

      const after = await queryTitles(client, options, 4, "getCharacterTitles(after)");
      const disciplines = await queryDisciplines(client, options, 5, "getCharacterDisciplines(after)");
      const discipline = findDisciplineById(disciplines, options.disciplineId) || action.discipline;
      const equippedTitle = after.equippedTitle || null;

      const ok = Boolean(before.ok && action.ok && after.ok && disciplines.ok);
      return buildEnvelope("character-disciplines-debug", ok, {
        login: formatLoginSummary(login),
        before: summarizeTitlesResponse(before),
        action: {
          ok: Boolean(action.ok),
          errorCode: action.errorCode || "",
          action: action.action || "set_discipline",
          discipline: summarizeDiscipline(action.discipline)
        },
        after: summarizeTitlesResponse(after),
        unlockedTitles: summarizeUnlockedTitles(action.unlockedTitles),
        equippedTitle: summarizeTitle(equippedTitle),
        discipline: summarizeDiscipline(discipline),
        request: {
          disciplineId: options.disciplineId,
          disciplineTier: options.disciplineTier,
          disciplinePoints: options.disciplinePoints,
          reason: options.titleChangeReason || "",
          debugTokenProvided: Boolean(options.titleDebugToken)
        }
      });
    } finally {
      client.close();
    }
  });

  printResult("character.disciplinesDebug", envelope, options);
  return envelope;
}
