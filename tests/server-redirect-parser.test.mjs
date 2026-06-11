import assert from "node:assert/strict";
import test from "node:test";

import { MESSAGE_TYPE } from "../tools/mock-client/src/constants.js";
import { decodeByMessageType } from "../tools/mock-client/src/messages.js";
import {
  encodeBoolField,
  encodeStringField,
  encodeUInt32Field
} from "../tools/mock-client/src/protocol.js";

test("mock-client decodes ServerRedirectPush reconnect target fields", () => {
  const body = Buffer.concat([
    encodeStringField(1, "rollout_redirect"),
    encodeStringField(2, "room-a"),
    encodeStringField(3, "epoch-42"),
    encodeBoolField(4, true),
    encodeUInt32Field(5, 300),
    encodeStringField(6, "proxy.example.internal"),
    encodeUInt32Field(7, 4000),
    encodeStringField(8, "game-server-new"),
    encodeStringField(9, "kcp")
  ]);

  assert.deepEqual(decodeByMessageType(MESSAGE_TYPE.SERVER_REDIRECT_PUSH, body), {
    reason: "rollout_redirect",
    roomId: "room-a",
    rolloutEpoch: "epoch-42",
    reconnectRequired: true,
    retryAfterMs: 300,
    targetHost: "proxy.example.internal",
    targetPort: 4000,
    targetServerId: "game-server-new",
    transport: "kcp"
  });
});
