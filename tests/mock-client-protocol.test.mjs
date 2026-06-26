import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";

import { parseArgs } from "../tools/mock-client/src/args.js";
import { MESSAGE_TYPE } from "../tools/mock-client/src/constants.js";
import {
  decodeByMessageType,
  encodeDebugCharacterTitleReq,
  encodeDebugApplyCharacterElementChangeReq,
  encodeEquipCharacterTitleReq,
  encodeGetCharacterDisciplinesReq,
  encodeGetCharacterTitlesReq,
  encodeGetCharacterElementsReq
} from "../tools/mock-client/src/messages.js";
import { runAnnounceGet } from "../tools/mock-client/src/scenarios/announce.js";
import { connectToChatServer } from "../tools/mock-client/src/scenarios/chat.js";
import { runMailGet } from "../tools/mock-client/src/scenarios/mail.js";
import {
  decodeFieldsWithRepeated,
  encodeBoolField,
  encodeInt64Field,
  encodeInt32Field,
  encodeMessageField,
  encodeStringField,
  encodeUInt32Field,
  encodeVarint,
  readBool,
  readInt32,
  readInt64,
  readString
} from "../tools/mock-client/src/protocol.js";

function encodePackedInt32Field(fieldNumber, values) {
  const payload = Buffer.concat(values.map((value) => encodeVarint(value)));
  return Buffer.concat([
    encodeVarint((fieldNumber << 3) | 2),
    encodeVarint(payload.length),
    payload
  ]);
}

function encodeElementValues(value) {
  return Buffer.concat([
    encodeInt32Field(1, value.earth),
    encodeInt32Field(2, value.fire),
    encodeInt32Field(3, value.water),
    encodeInt32Field(4, value.wind)
  ]);
}

function encodeCharacterElements(elements) {
  return Buffer.concat([
    encodeMessageField(1, encodeElementValues(elements.affinity)),
    encodeMessageField(2, encodeElementValues(elements.mastery))
  ]);
}

function encodeCharacterTitleDefinitionSummary(definition) {
  return Buffer.concat([
    encodeStringField(1, definition.id),
    encodeStringField(2, definition.name),
    encodeStringField(3, definition.type),
    encodeStringField(4, definition.rarity),
    encodeStringField(5, definition.icon),
    encodeStringField(6, definition.color),
    ...definition.tags.map((tag) => encodeStringField(7, tag)),
    encodeBoolField(8, definition.hidden),
    encodeBoolField(9, definition.limited),
    encodeInt32Field(10, definition.sortOrder)
  ]);
}

function encodeCharacterTitleSummary(title) {
  return Buffer.concat([
    encodeMessageField(1, encodeCharacterTitleDefinitionSummary(title.definition)),
    encodeBoolField(2, title.owned),
    encodeBoolField(3, title.equipped),
    encodeStringField(4, title.sourceType),
    encodeStringField(5, title.sourceId),
    encodeStringField(6, title.unlockedAt),
    encodeStringField(7, title.expiresAt),
    encodeBoolField(8, title.expired)
  ]);
}

function encodeCharacterDisciplineSummary(discipline) {
  return Buffer.concat([
    encodeStringField(1, discipline.disciplineId),
    encodeInt64Field(2, discipline.points),
    encodeStringField(3, discipline.tier),
    encodeBoolField(4, discipline.active),
    encodeStringField(5, discipline.learnedAt),
    encodeStringField(6, discipline.updatedAt)
  ]);
}

function decodeElementValues(buffer) {
  const fields = decodeFieldsWithRepeated(buffer);
  return {
    earth: readInt32(fields, 1),
    fire: readInt32(fields, 2),
    water: readInt32(fields, 3),
    wind: readInt32(fields, 4)
  };
}

test("mock-client defaults to public player entrypoints only", () => {
  const options = parseArgs([]);

  assert.equal(options.httpBaseUrl, "http://127.0.0.1:3000");
  assert.equal(options.host, "127.0.0.1");
  assert.equal(options.port, 14000);
  assert.equal(options.chatPort, 0);
  assert.equal(options.mailBaseUrl, "");
  assert.equal(options.announceBaseUrl, "");
});

test("mock-client rollout player examples stay on proxy TCP fallback", () => {
  const rolloutHelp = fs.readFileSync("tools/mock-client/help_rollout.txt", "utf8");

  assert.equal(rolloutHelp.includes("--port 7000"), false);
  assert.match(rolloutHelp, /--port 14000/);
  assert.match(rolloutHelp, /registry discovery/);
  assert.match(rolloutHelp, /本地 manual drill/);
});

test("mock-client side-service help marks local internal endpoints", () => {
  const help = fs.readFileSync("tools/mock-client/help.txt", "utf8");
  const readme = fs.readFileSync("tools/mock-client/README.md", "utf8");

  assert.match(help, /内部联调地址；本地示例通过 --chat-port 9001/);
  assert.match(help, /内部联调地址；本地示例通过 --mail-base-url 9003/);
  assert.match(help, /内部联调地址；本地示例通过 --announce-base-url 9004/);
  assert.match(readme, /9001 是本地内部联调地址示例/);
  assert.match(readme, /9003 是本地内部联调地址示例/);
  assert.match(readme, /9004 是本地内部联调地址示例/);
});

test("mock-client internal side-service scenarios require explicit endpoints", async () => {
  await assert.rejects(
    () => connectToChatServer(parseArgs([])),
    /chat scenarios are internal integration flows/
  );

  await assert.rejects(
    () => runMailGet({ ...parseArgs([]), mailId: "mail-test" }),
    /mail scenarios are internal integration flows/
  );

  await assert.rejects(
    () => runAnnounceGet({ ...parseArgs([]), announceId: "ann-test" }),
    /announce scenarios are internal integration flows/
  );
});

test("mock-client decodes proto3 packed repeated int32 fields", () => {
  const itemUseBody = Buffer.concat([
    encodeBoolField(1, true),
    encodeInt64Field(3, 25),
    encodePackedInt32Field(4, [101, 202])
  ]);

  assert.deepEqual(decodeByMessageType(MESSAGE_TYPE.ITEM_USE_RES, itemUseBody), {
    ok: true,
    errorCode: "",
    hpChange: 25,
    newBuffIds: [101, 202]
  });

  const visualChangeBody = Buffer.concat([
    encodeUInt32Field(1, 7),
    encodePackedInt32Field(2, [301, 302])
  ]);

  assert.deepEqual(decodeByMessageType(MESSAGE_TYPE.VISUAL_CHANGE_PUSH, visualChangeBody), {
    appearance: 7,
    activeBuffIds: [301, 302]
  });
});

test("mock-client encodes and decodes character element messages", () => {
  assert.equal(encodeGetCharacterElementsReq().length, 0);

  const changeReq = encodeDebugApplyCharacterElementChangeReq(
    { earth: -100, fire: 100, water: 0, wind: 0 },
    { earth: 0, fire: 10, water: 0, wind: 0 },
    "unit test",
    "debug-token"
  );
  const requestFields = decodeFieldsWithRepeated(changeReq);

  assert.deepEqual(decodeElementValues(requestFields.get(1)), {
    earth: -100,
    fire: 100,
    water: 0,
    wind: 0
  });
  assert.deepEqual(decodeElementValues(requestFields.get(2)), {
    earth: 0,
    fire: 10,
    water: 0,
    wind: 0
  });
  assert.equal(readString(requestFields, 3), "unit test");
  assert.equal(readString(requestFields, 4), "debug-token");

  const elements = {
    affinity: { earth: 2500, fire: 2500, water: 2500, wind: 2500 },
    mastery: { earth: 0, fire: 10, water: 0, wind: 0 }
  };
  const getResBody = Buffer.concat([
    encodeBoolField(1, true),
    encodeStringField(2, ""),
    encodeStringField(3, "chr_0000000000001"),
    encodeMessageField(4, encodeCharacterElements(elements))
  ]);

  assert.deepEqual(decodeByMessageType(MESSAGE_TYPE.GET_CHARACTER_ELEMENTS_RES, getResBody), {
    ok: true,
    errorCode: "",
    characterId: "chr_0000000000001",
    elements
  });

  const invalidChangeBody = Buffer.concat([
    encodeBoolField(1, false),
    encodeStringField(2, "INVALID_AFFINITY_TOTAL"),
    encodeStringField(3, "chr_0000000000001")
  ]);

  assert.deepEqual(
    decodeByMessageType(MESSAGE_TYPE.DEBUG_APPLY_CHARACTER_ELEMENT_CHANGE_RES, invalidChangeBody),
    {
      ok: false,
      errorCode: "INVALID_AFFINITY_TOTAL",
      characterId: "chr_0000000000001",
      before: null,
      after: null
    }
  );
});

test("mock-client defines contiguous character title and discipline message types", () => {
  assert.deepEqual(
    [
      MESSAGE_TYPE.GET_CHARACTER_TITLES_REQ,
      MESSAGE_TYPE.GET_CHARACTER_TITLES_RES,
      MESSAGE_TYPE.EQUIP_CHARACTER_TITLE_REQ,
      MESSAGE_TYPE.EQUIP_CHARACTER_TITLE_RES,
      MESSAGE_TYPE.GET_CHARACTER_DISCIPLINES_REQ,
      MESSAGE_TYPE.GET_CHARACTER_DISCIPLINES_RES,
      MESSAGE_TYPE.DEBUG_CHARACTER_TITLE_REQ,
      MESSAGE_TYPE.DEBUG_CHARACTER_TITLE_RES
    ],
    [1417, 1418, 1419, 1420, 1421, 1422, 1423, 1424]
  );
});

test("mock-client encodes character title and discipline requests", () => {
  assert.equal(encodeGetCharacterTitlesReq().length, 0);
  assert.equal(encodeGetCharacterDisciplinesReq().length, 0);

  const equipFields = decodeFieldsWithRepeated(encodeEquipCharacterTitleReq("9001"));
  assert.equal(readString(equipFields, 1), "9001");

  const debugFields = decodeFieldsWithRepeated(
    encodeDebugCharacterTitleReq({
      action: "set_discipline",
      titleId: "2001",
      disciplineId: "forging",
      disciplineTier: "apprentice",
      disciplinePoints: 120,
      disciplineActive: true,
      triggerUnlockCheck: true,
      reason: "unit test",
      debugToken: "debug-token",
      expiresAt: "2099-01-01T00:00:00Z"
    })
  );

  assert.equal(readString(debugFields, 1), "set_discipline");
  assert.equal(readString(debugFields, 2), "2001");
  assert.equal(readString(debugFields, 3), "forging");
  assert.equal(readString(debugFields, 4), "apprentice");
  assert.equal(readInt64(debugFields, 5), 120);
  assert.equal(readBool(debugFields, 6), true);
  assert.equal(readBool(debugFields, 7), true);
  assert.equal(readString(debugFields, 8), "unit test");
  assert.equal(readString(debugFields, 9), "debug-token");
  assert.equal(readString(debugFields, 10), "2099-01-01T00:00:00Z");
});

test("mock-client decodes character title, equip, discipline, and debug responses", () => {
  const title9001 = {
    definition: {
      id: "9001",
      name: "系统观察员",
      type: "system",
      rarity: "epic",
      icon: "icon_title_system_observer",
      color: "#B46CFF",
      tags: ["system", "gm"],
      hidden: true,
      limited: false,
      sortOrder: 900
    },
    owned: true,
    equipped: true,
    sourceType: "gm",
    sourceId: "debug-character-titles",
    unlockedAt: "2026-06-26T10:00:00Z",
    expiresAt: "",
    expired: false
  };
  const title2001 = {
    definition: {
      id: "2001",
      name: "见习锻造者",
      type: "discipline",
      rarity: "common",
      icon: "icon_title_forging_novice",
      color: "#D98B45",
      tags: ["discipline", "forging"],
      hidden: false,
      limited: false,
      sortOrder: 200
    },
    owned: true,
    equipped: false,
    sourceType: "discipline",
    sourceId: "forging",
    unlockedAt: "2026-06-26T10:01:00Z",
    expiresAt: "",
    expired: false
  };
  const discipline = {
    disciplineId: "forging",
    points: 120,
    tier: "apprentice",
    active: true,
    learnedAt: "2026-06-26T10:00:00Z",
    updatedAt: "2026-06-26T10:01:00Z"
  };

  const titlesResBody = Buffer.concat([
    encodeBoolField(1, true),
    encodeStringField(2, ""),
    encodeStringField(3, "chr_0000000000001"),
    encodeMessageField(4, encodeCharacterTitleSummary(title2001)),
    encodeMessageField(4, encodeCharacterTitleSummary(title9001)),
    encodeMessageField(5, encodeCharacterTitleSummary(title9001))
  ]);
  assert.deepEqual(decodeByMessageType(MESSAGE_TYPE.GET_CHARACTER_TITLES_RES, titlesResBody), {
    ok: true,
    errorCode: "",
    characterId: "chr_0000000000001",
    titles: [title2001, title9001],
    equippedTitle: title9001
  });

  const equipResBody = Buffer.concat([
    encodeBoolField(1, true),
    encodeStringField(2, ""),
    encodeStringField(3, "chr_0000000000001"),
    encodeMessageField(4, encodeCharacterTitleSummary(title9001))
  ]);
  assert.deepEqual(decodeByMessageType(MESSAGE_TYPE.EQUIP_CHARACTER_TITLE_RES, equipResBody), {
    ok: true,
    errorCode: "",
    characterId: "chr_0000000000001",
    equippedTitle: title9001
  });

  const disciplinesResBody = Buffer.concat([
    encodeBoolField(1, true),
    encodeStringField(2, ""),
    encodeStringField(3, "chr_0000000000001"),
    encodeMessageField(4, encodeCharacterDisciplineSummary(discipline))
  ]);
  assert.deepEqual(
    decodeByMessageType(MESSAGE_TYPE.GET_CHARACTER_DISCIPLINES_RES, disciplinesResBody),
    {
      ok: true,
      errorCode: "",
      characterId: "chr_0000000000001",
      disciplines: [discipline]
    }
  );

  const debugResBody = Buffer.concat([
    encodeBoolField(1, true),
    encodeStringField(2, ""),
    encodeStringField(3, "chr_0000000000001"),
    encodeStringField(4, "set_discipline"),
    encodeMessageField(6, encodeCharacterDisciplineSummary(discipline)),
    encodeMessageField(7, encodeCharacterTitleSummary(title2001))
  ]);
  assert.deepEqual(decodeByMessageType(MESSAGE_TYPE.DEBUG_CHARACTER_TITLE_RES, debugResBody), {
    ok: true,
    errorCode: "",
    characterId: "chr_0000000000001",
    action: "set_discipline",
    title: null,
    discipline,
    unlockedTitles: [title2001]
  });
});

test("mock-client character title scenarios document async response matching and JSON shape", () => {
  const scenarioSource = fs.readFileSync("tools/mock-client/src/scenarios/character.js", "utf8");
  const help = fs.readFileSync("tools/mock-client/help.txt", "utf8");
  const readme = fs.readFileSync("tools/mock-client/README.md", "utf8");

  assert.match(
    scenarioSource,
    /packet\.messageType === MESSAGE_TYPE\.GET_CHARACTER_TITLES_RES && packet\.seq === seq/
  );
  assert.match(
    scenarioSource,
    /packet\.messageType === MESSAGE_TYPE\.DEBUG_CHARACTER_TITLE_RES && packet\.seq === 3/
  );
  assert.match(
    scenarioSource,
    /packet\.messageType === MESSAGE_TYPE\.EQUIP_CHARACTER_TITLE_RES && packet\.seq === 4/
  );
  assert.match(scenarioSource, /action: "grant_title"/);
  assert.match(scenarioSource, /action: "set_discipline"/);
  assert.match(scenarioSource, /triggerUnlockCheck: true/);
  assert.match(scenarioSource, /before: summarizeTitlesResponse/);
  assert.match(scenarioSource, /unlockedTitles: summarizeUnlockedTitles/);
  assert.match(scenarioSource, /equippedTitle: summarizeTitle/);
  assert.match(scenarioSource, /disciplinePoints: options\.disciplinePoints/);

  assert.match(help, /--discipline-points <n>/);
  assert.match(help, /character-titles-debug/);
  assert.match(help, /character-disciplines-debug/);
  assert.match(readme, /--discipline-points/);
  assert.match(readme, /messageType \+ seq/);
});
