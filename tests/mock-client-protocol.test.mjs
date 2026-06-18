import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";

import { parseArgs } from "../tools/mock-client/src/args.js";
import { MESSAGE_TYPE } from "../tools/mock-client/src/constants.js";
import { decodeByMessageType } from "../tools/mock-client/src/messages.js";
import { runAnnounceGet } from "../tools/mock-client/src/scenarios/announce.js";
import { connectToChatServer } from "../tools/mock-client/src/scenarios/chat.js";
import { runMailGet } from "../tools/mock-client/src/scenarios/mail.js";
import {
  encodeBoolField,
  encodeInt64Field,
  encodeUInt32Field,
  encodeVarint
} from "../tools/mock-client/src/protocol.js";

function encodePackedInt32Field(fieldNumber, values) {
  const payload = Buffer.concat(values.map((value) => encodeVarint(value)));
  return Buffer.concat([
    encodeVarint((fieldNumber << 3) | 2),
    encodeVarint(payload.length),
    payload
  ]);
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
