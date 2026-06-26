import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";

import { parseArgs } from "../tools/mock-client/src/args.js";
import { MESSAGE_TYPE } from "../tools/mock-client/src/constants.js";
import {
  decodeByMessageType,
  encodeDebugApplyCharacterElementChangeReq,
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
  readInt32,
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
