import { MESSAGE_TYPE } from "../constants.js";
import { TcpProtocolClient } from "../client.js";
import {
  encodeDebugCharacterTitleReq,
  encodeDebugApplyCharacterElementChangeReq,
  encodeEquipCharacterTitleReq,
  encodeGetCharacterDisciplinesReq,
  encodeGetCharacterElementsReq,
  encodeGetCharacterTitlesReq,
  encodeAddCharacterDisciplinePointsReq,
  encodeApplyCharacterProgressReq,
  encodeLearnCharacterDisciplineReq,
  encodeSetCharacterDisciplineActiveReq,
  encodeSwitchCharacterDisciplineReq,
  encodeRoomJoinReq
} from "../messages.js";
import {
  buildCharacterCreateInput,
  buildGeneratedCharacterName,
  createCharacter,
  deleteCharacter,
  fetchLoginSession,
  fetchTicket,
  formatLoginSummary,
  getCharacterProfile,
  listCharacters,
  requestCreateCharacter,
  restoreCharacter,
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
    displayDiscriminator: character.display_discriminator || character.displayDiscriminator || "",
    deletedAt: character.deleted_at || character.deletedAt || null
  };
}

function summarizeLifecycle(lifecycle) {
  if (!lifecycle) {
    return null;
  }

  return {
    state: lifecycle.state || "",
    deletedAt: lifecycle.deleted_at || null,
    restoreWindowSeconds: lifecycle.restore_window_seconds ?? null,
    restoreExpiresAt: lifecycle.restore_expires_at || null,
    deleteCooldownSeconds: lifecycle.delete_cooldown_seconds ?? null,
    hardDeleteEligibleAt: lifecycle.hard_delete_eligible_at || null
  };
}

function summarizeProfile(profile) {
  if (!profile) {
    return null;
  }

  return {
    character: summarizeCharacter(profile),
    sameName: profile.same_name || null,
    attributes: profile.attributes || null,
    equippedTitle: profile.equipped_title || null,
    discipline: profile.discipline || null,
    profileSources: profile.profile_sources || null,
    lifecycle: summarizeLifecycle(profile.lifecycle)
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

function summarizeDisciplineDefinition(definition) {
  if (!definition) {
    return null;
  }
  return {
    disciplineId: definition.disciplineId || "",
    name: definition.name || "",
    initialTier: definition.initialTier || "",
    initialPoints: definition.initialPoints ?? 0,
    skillPool: definition.skillPool || [],
    interactionPermissions: definition.interactionPermissions || [],
    displayFieldsJson: definition.displayFieldsJson || ""
  };
}

function findDisciplineById(disciplinesRes, disciplineId) {
  return (disciplinesRes?.disciplines || []).find((discipline) => discipline.disciplineId === disciplineId) || null;
}

function summarizeUnlockedTitles(titles) {
  return (titles || []).map(summarizeTitle).filter(Boolean);
}

function summarizeProgressReward(reward) {
  return {
    rewardType: reward?.rewardType || "",
    rewardId: reward?.rewardId || "",
    status: reward?.status || "",
    title: summarizeTitle(reward?.title),
    discipline: summarizeDiscipline(reward?.discipline),
    eligibility: reward?.eligibility || ""
  };
}

function summarizeCharacterPush(push) {
  if (!push?.meta) {
    return null;
  }
  return {
    messageType: push.messageType,
    characterId: push.meta.characterId || "",
    sequence: push.meta.sequence ?? 0,
    revision: push.meta.revision ?? 0,
    sourceType: push.meta.sourceType || "",
    sourceId: push.meta.sourceId || "",
    action: push.meta.action || "",
    summary: push.meta.summary || "",
    snapshotCompensation: Boolean(push.meta.snapshotCompensation)
  };
}

function isCharacterPush(packet) {
  return [
    MESSAGE_TYPE.CHARACTER_ELEMENTS_CHANGE_PUSH,
    MESSAGE_TYPE.CHARACTER_TITLE_CHANGE_PUSH,
    MESSAGE_TYPE.CHARACTER_DISCIPLINE_CHANGE_PUSH
  ].includes(packet.messageType);
}

async function readCharacterPush(client, options, label, expected = {}) {
  const push = await client.readUntil(
    options.timeoutMs,
    (packet, decoded) => {
      if (!isCharacterPush(packet)) {
        return false;
      }
      if (expected.messageType && packet.messageType !== expected.messageType) {
        return false;
      }
      if (expected.action && decoded.meta?.action !== expected.action) {
        return false;
      }
      if (expected.characterId && decoded.meta?.characterId !== expected.characterId) {
        return false;
      }
      return true;
    },
    label
  );
  const decodedPush = { ...push, messageType: expected.messageType || push.messageType };
  return {
    ...decodedPush,
    push: summarizeCharacterPush(decodedPush)
  };
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
    accountPlayerId: payload.playerId || session.playerId,
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
    accountPlayerId: session.playerId,
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

export async function runCharacterProfile(options) {
  const session = await loginSession(options, "character-profile");
  let characterId = options.characterId;
  if (!characterId) {
    const payload = await listCharacters(options, session.accessToken);
    const selected = payload.characters[0];
    if (!selected) {
      throw new Error("account has no characters; create one first with --scenario character-create");
    }
    characterId = getCharacterId(selected);
  }

  const payload = await getCharacterProfile(options, session.accessToken, characterId);
  const envelope = buildEnvelope("character-profile", true, {
    accountPlayerId: session.playerId,
    profile: summarizeProfile(payload.profile)
  });
  printResult("character.profile", envelope, options);
  return envelope;
}

export async function runCharacterDelete(options) {
  const session = await loginSession(options, "character-delete");
  let characterId = options.characterId;
  if (!characterId) {
    const payload = await listCharacters(options, session.accessToken);
    const selected = payload.characters[0];
    if (!selected) {
      throw new Error("account has no characters; create one first with --scenario character-create");
    }
    characterId = getCharacterId(selected);
  }

  const payload = await deleteCharacter(options, session.accessToken, characterId);
  const envelope = buildEnvelope("character-delete", true, {
    accountPlayerId: session.playerId,
    character: summarizeCharacter(payload.character),
    lifecycle: summarizeLifecycle(payload.lifecycle)
  });
  printResult("character.delete", envelope, options);
  return envelope;
}

export async function runCharacterRestore(options) {
  const session = await loginSession(options, "character-restore");
  if (!options.characterId) {
    throw new Error("character-restore requires --character-id because deleted characters are hidden from ordinary list");
  }

  const payload = await restoreCharacter(options, session.accessToken, options.characterId);
  const envelope = buildEnvelope("character-restore", true, {
    accountPlayerId: session.playerId,
    character: summarizeCharacter(payload.character),
    lifecycle: summarizeLifecycle(payload.lifecycle)
  });
  printResult("character.restore", envelope, options);
  return envelope;
}

export async function runCharacterDuplicateName(options) {
  const session = await loginSession(options, "character-duplicate-name");
  const name = options.characterName || buildGeneratedCharacterName(options, "dup");
  const input = buildCharacterCreateInput(options, { name });
  const first = await createCharacter(options, session.accessToken, input);
  const second = await createCharacter(options, session.accessToken, input);

  const envelope = buildEnvelope("character-duplicate-name", true, {
    accountPlayerId: session.playerId,
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
    accountPlayerId: session.playerId,
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
          ownerCharacterId: push.snapshot?.ownerCharacterId || null
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
      const elementPush = change.ok
        ? await readCharacterPush(client, options, "characterElementsChangePush", {
          messageType: MESSAGE_TYPE.CHARACTER_ELEMENTS_CHANGE_PUSH,
          action: "element_change",
          characterId: login.characterId
        })
        : null;

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
        push: elementPush?.push || null,
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
      const grantPush = action.ok
        ? await readCharacterPush(client, options, "characterTitleChangePush(grant)", {
          messageType: MESSAGE_TYPE.CHARACTER_TITLE_CHANGE_PUSH,
          action: "grant",
          characterId: login.characterId
        })
        : null;

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
      const equipPush = equip.ok
        ? await readCharacterPush(client, options, "characterTitleChangePush(equip)", {
          messageType: MESSAGE_TYPE.CHARACTER_TITLE_CHANGE_PUSH,
          action: "equip",
          characterId: login.characterId
        })
        : null;

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
        pushes: [grantPush?.push, equipPush?.push].filter(Boolean),
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
      const disciplinePush = action.ok
        ? await readCharacterPush(client, options, "characterDisciplineChangePush(debug)", {
          messageType: MESSAGE_TYPE.CHARACTER_DISCIPLINE_CHANGE_PUSH,
          action: "upsert",
          characterId: login.characterId
        })
        : null;

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
        push: disciplinePush?.push || null,
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

export async function runCharacterDisciplineLearn(options) {
  const login = await fetchTicket(options);
  const envelope = await withJsonQuiet(options, async () => {
    const client = new TcpProtocolClient(options, "characterClient");
    await client.connect();

    try {
      await authenticateClient(client, options, login, 1);

      const before = await queryDisciplines(client, options, 2, "getCharacterDisciplines(before)");

      await client.send(
        MESSAGE_TYPE.LEARN_CHARACTER_DISCIPLINE_REQ,
        3,
        encodeLearnCharacterDisciplineReq(options.disciplineId)
      );
      const learn = await client.readUntil(
        options.timeoutMs,
        (packet) => packet.messageType === MESSAGE_TYPE.LEARN_CHARACTER_DISCIPLINE_RES && packet.seq === 3,
        "learnCharacterDiscipline"
      );
      const learnPush = learn.ok
        ? await readCharacterPush(client, options, "characterDisciplineChangePush(learn)", {
          messageType: MESSAGE_TYPE.CHARACTER_DISCIPLINE_CHANGE_PUSH,
          action: "learn",
          characterId: login.characterId
        })
        : null;

      const after = await queryDisciplines(client, options, 4, "getCharacterDisciplines(after)");
      const discipline = findDisciplineById(after, options.disciplineId) || learn.discipline;

      const ok = Boolean(before.ok && learn.ok && after.ok);
      return buildEnvelope("character-discipline-learn", ok, {
        login: formatLoginSummary(login),
        before: {
          ok: Boolean(before.ok),
          errorCode: before.errorCode || "",
          discipline: summarizeDiscipline(findDisciplineById(before, options.disciplineId))
        },
        learn: {
          ok: Boolean(learn.ok),
          errorCode: learn.errorCode || "",
          discipline: summarizeDiscipline(learn.discipline),
          definition: summarizeDisciplineDefinition(learn.definition),
          consumedItems: learn.consumedItems || [],
          activeSkillPool: learn.activeSkillPool || [],
          unlockedTitles: summarizeUnlockedTitles(learn.unlockedTitles)
        },
        push: learnPush?.push || null,
        after: {
          ok: Boolean(after.ok),
          errorCode: after.errorCode || "",
          discipline: summarizeDiscipline(discipline)
        },
        request: {
          disciplineId: options.disciplineId
        }
      });
    } finally {
      client.close();
    }
  });

  printResult("character.disciplineLearn", envelope, options);
  return envelope;
}

async function runCharacterDisciplineActiveChange(options, active) {
  const scenario = active ? "character-discipline-activate" : "character-discipline-deactivate";
  const login = await fetchTicket(options);
  const envelope = await withJsonQuiet(options, async () => {
    const client = new TcpProtocolClient(options, "characterClient");
    await client.connect();

    try {
      await authenticateClient(client, options, login, 1);

      const before = await queryDisciplines(client, options, 2, "getCharacterDisciplines(before)");
      await client.send(
        MESSAGE_TYPE.SET_CHARACTER_DISCIPLINE_ACTIVE_REQ,
        3,
        encodeSetCharacterDisciplineActiveReq(options.disciplineId, active)
      );
      const action = await client.readUntil(
        options.timeoutMs,
        (packet) => packet.messageType === MESSAGE_TYPE.SET_CHARACTER_DISCIPLINE_ACTIVE_RES && packet.seq === 3,
        "setCharacterDisciplineActive"
      );
      const actionPush = action.ok
        ? await readCharacterPush(client, options, `characterDisciplineChangePush(${active ? "activate" : "deactivate"})`, {
          messageType: MESSAGE_TYPE.CHARACTER_DISCIPLINE_CHANGE_PUSH,
          action: active ? "activate" : "deactivate",
          characterId: login.characterId
        })
        : null;
      const after = await queryDisciplines(client, options, 4, "getCharacterDisciplines(after)");
      const discipline = findDisciplineById(after, options.disciplineId) || action.discipline;

      const ok = Boolean(before.ok && action.ok && after.ok);
      return buildEnvelope(scenario, ok, {
        login: formatLoginSummary(login),
        before: {
          ok: Boolean(before.ok),
          errorCode: before.errorCode || "",
          discipline: summarizeDiscipline(findDisciplineById(before, options.disciplineId))
        },
        action: {
          ok: Boolean(action.ok),
          errorCode: action.errorCode || "",
          discipline: summarizeDiscipline(action.discipline),
          activeSkillPool: action.activeSkillPool || [],
          unlockedTitles: summarizeUnlockedTitles(action.unlockedTitles)
        },
        push: actionPush?.push || null,
        after: {
          ok: Boolean(after.ok),
          errorCode: after.errorCode || "",
          discipline: summarizeDiscipline(discipline)
        },
        request: {
          disciplineId: options.disciplineId,
          active
        }
      });
    } finally {
      client.close();
    }
  });

  printResult(`character.discipline${active ? "Activate" : "Deactivate"}`, envelope, options);
  return envelope;
}

export async function runCharacterDisciplineActivate(options) {
  return runCharacterDisciplineActiveChange(options, true);
}

export async function runCharacterDisciplineDeactivate(options) {
  return runCharacterDisciplineActiveChange(options, false);
}

export async function runCharacterDisciplineSwitch(options) {
  const login = await fetchTicket(options);
  const envelope = await withJsonQuiet(options, async () => {
    const client = new TcpProtocolClient(options, "characterClient");
    await client.connect();

    try {
      await authenticateClient(client, options, login, 1);

      const before = await queryDisciplines(client, options, 2, "getCharacterDisciplines(before)");
      await client.send(
        MESSAGE_TYPE.SWITCH_CHARACTER_DISCIPLINE_REQ,
        3,
        encodeSwitchCharacterDisciplineReq(options.disciplineId)
      );
      const action = await client.readUntil(
        options.timeoutMs,
        (packet) => packet.messageType === MESSAGE_TYPE.SWITCH_CHARACTER_DISCIPLINE_RES && packet.seq === 3,
        "switchCharacterDiscipline"
      );
      const switchPush = action.ok
        ? await readCharacterPush(client, options, "characterDisciplineChangePush(switch)", {
          messageType: MESSAGE_TYPE.CHARACTER_DISCIPLINE_CHANGE_PUSH,
          action: "switch",
          characterId: login.characterId
        })
        : null;
      const after = await queryDisciplines(client, options, 4, "getCharacterDisciplines(after)");
      const discipline = findDisciplineById(after, options.disciplineId) || action.discipline;

      const ok = Boolean(before.ok && action.ok && after.ok);
      return buildEnvelope("character-discipline-switch", ok, {
        login: formatLoginSummary(login),
        before: {
          ok: Boolean(before.ok),
          errorCode: before.errorCode || "",
          activeCount: (before.disciplines || []).filter((discipline) => discipline.active).length
        },
        action: {
          ok: Boolean(action.ok),
          errorCode: action.errorCode || "",
          discipline: summarizeDiscipline(action.discipline),
          activeSkillPool: action.activeSkillPool || [],
          unlockedTitles: summarizeUnlockedTitles(action.unlockedTitles)
        },
        push: switchPush?.push || null,
        after: {
          ok: Boolean(after.ok),
          errorCode: after.errorCode || "",
          activeCount: (after.disciplines || []).filter((discipline) => discipline.active).length,
          discipline: summarizeDiscipline(discipline)
        },
        request: {
          disciplineId: options.disciplineId
        }
      });
    } finally {
      client.close();
    }
  });

  printResult("character.disciplineSwitch", envelope, options);
  return envelope;
}

export async function runCharacterDisciplinePoints(options) {
  const login = await fetchTicket(options);
  const envelope = await withJsonQuiet(options, async () => {
    const client = new TcpProtocolClient(options, "characterClient");
    await client.connect();

    try {
      await authenticateClient(client, options, login, 1);

      const before = await queryDisciplines(client, options, 2, "getCharacterDisciplines(before)");
      await client.send(
        MESSAGE_TYPE.ADD_CHARACTER_DISCIPLINE_POINTS_REQ,
        3,
        encodeAddCharacterDisciplinePointsReq(options.disciplineId, options.disciplinePoints)
      );
      const action = await client.readUntil(
        options.timeoutMs,
        (packet) => packet.messageType === MESSAGE_TYPE.ADD_CHARACTER_DISCIPLINE_POINTS_RES && packet.seq === 3,
        "addCharacterDisciplinePoints"
      );
      const pointsPush = action.ok
        ? await readCharacterPush(client, options, "characterDisciplineChangePush(points)", {
          messageType: MESSAGE_TYPE.CHARACTER_DISCIPLINE_CHANGE_PUSH,
          action: "points_change",
          characterId: login.characterId
        })
        : null;
      const after = await queryDisciplines(client, options, 4, "getCharacterDisciplines(after)");
      const discipline = findDisciplineById(after, options.disciplineId) || action.discipline;

      const ok = Boolean(before.ok && action.ok && after.ok);
      return buildEnvelope("character-discipline-points", ok, {
        login: formatLoginSummary(login),
        before: {
          ok: Boolean(before.ok),
          errorCode: before.errorCode || "",
          discipline: summarizeDiscipline(findDisciplineById(before, options.disciplineId))
        },
        action: {
          ok: Boolean(action.ok),
          errorCode: action.errorCode || "",
          discipline: summarizeDiscipline(action.discipline),
          activeSkillPool: action.activeSkillPool || [],
          unlockedTitles: summarizeUnlockedTitles(action.unlockedTitles)
        },
        push: pointsPush?.push || null,
        after: {
          ok: Boolean(after.ok),
          errorCode: after.errorCode || "",
          discipline: summarizeDiscipline(discipline)
        },
        request: {
          disciplineId: options.disciplineId,
          pointsDelta: options.disciplinePoints
        }
      });
    } finally {
      client.close();
    }
  });

  printResult("character.disciplinePoints", envelope, options);
  return envelope;
}

export async function runCharacterProgressApply(options) {
  const login = await fetchTicket(options);
  const envelope = await withJsonQuiet(options, async () => {
    const client = new TcpProtocolClient(options, "characterClient");
    await client.connect();

    try {
      await authenticateClient(client, options, login, 1);

      await client.send(
        MESSAGE_TYPE.APPLY_CHARACTER_PROGRESS_REQ,
        2,
        encodeApplyCharacterProgressReq(options.progressId)
      );
      const action = await client.readUntil(
        options.timeoutMs,
        (packet) => packet.messageType === MESSAGE_TYPE.APPLY_CHARACTER_PROGRESS_RES && packet.seq === 2,
        "applyCharacterProgress"
      );
      const progressPush = action.ok && action.applied
        ? await readCharacterPush(client, options, "characterProgressRewardPush", {
          characterId: login.characterId
        })
        : null;
      const titles = await queryTitles(client, options, 3, "getCharacterTitles(afterProgress)");
      const disciplines = await queryDisciplines(client, options, 4, "getCharacterDisciplines(afterProgress)");

      const ok = Boolean(action.ok && titles.ok && disciplines.ok);
      return buildEnvelope("character-progress-apply", ok, {
        login: formatLoginSummary(login),
        action: {
          ok: Boolean(action.ok),
          errorCode: action.errorCode || "",
          applied: Boolean(action.applied),
          progressId: action.progressId || "",
          sourceType: action.sourceType || "",
          sourceId: action.sourceId || "",
          rewards: (action.rewards || []).map(summarizeProgressReward)
        },
        push: progressPush?.push || null,
        titles: summarizeTitlesResponse(titles),
        disciplines: (disciplines.disciplines || []).map(summarizeDiscipline),
        request: {
          progressId: options.progressId
        }
      });
    } finally {
      client.close();
    }
  });

  printResult("character.progressApply", envelope, options);
  return envelope;
}
