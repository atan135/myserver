import assert from "node:assert/strict";
import { test } from "node:test";

import { MESSAGE_TYPE } from "./constants.js";
import {
  decodeByMessageType,
  encodeActivityClaimReq,
  encodeActivityDetailReq,
  encodeActivityListReq
} from "./messages.js";
import { encodeBoolField, encodeInt64Field, encodeStringField, encodeUInt32Field } from "./protocol.js";
import { runActivityScenario } from "./scenarios/activity.js";
import { parseArgs } from "./args.js";

test("activity mock scenario sends list, detail, claim and same-request retry", async () => {
  const sent = [];
  const requests = await runActivityScenario(async (messageType, body) => {
    sent.push({ messageType, body });
  });

  assert.deepEqual(sent.map(({ messageType }) => messageType), [
    MESSAGE_TYPE.ACTIVITY_LIST_REQ,
    MESSAGE_TYPE.ACTIVITY_DETAIL_REQ,
    MESSAGE_TYPE.ACTIVITY_CLAIM_REQ,
    MESSAGE_TYPE.ACTIVITY_CLAIM_REQ
  ]);
  assert.equal(sent[0].body.length, 0);
  assert.deepEqual(sent[2].body, sent[3].body);
  assert.equal(requests.length, 4);
});

test("activity request encoders do not include client identity or reward fields", () => {
  const list = encodeActivityListReq();
  const detail = encodeActivityDetailReq("activity-demo", 3);
  const claim = encodeActivityClaimReq("activity-demo", 3, "stage-1", "retry-1");
  assert.equal(list.length, 0);
  assert.ok(detail.length > 0);
  assert.ok(claim.length > detail.length);
  assert.equal(claim.includes(Buffer.from("character-1")), false);
  assert.equal(claim.includes(Buffer.from("reward")), false);
});

test("activity response decoder preserves disabled and duplicate/processing contracts", () => {
  const stateRevision = 0x100000001;
  const detail = decodeByMessageType(
    MESSAGE_TYPE.ACTIVITY_DETAIL_RES,
    Buffer.concat([
      encodeBoolField(1, false),
      encodeStringField(2, "ACTIVITY_ENGINE_UNAVAILABLE"),
      encodeInt64Field(5, stateRevision)
    ])
  );
  assert.equal(detail.ok, false);
  assert.equal(detail.errorCode, "ACTIVITY_ENGINE_UNAVAILABLE");
  assert.equal(detail.stateRevision, stateRevision);

  const claim = decodeByMessageType(
    MESSAGE_TYPE.ACTIVITY_CLAIM_RES,
    Buffer.concat([
      encodeBoolField(1, false),
      encodeStringField(2, "ACTIVITY_PROCESSING"),
      encodeStringField(3, "activity-demo"),
      encodeUInt32Field(4, 1),
      encodeStringField(5, "stage-1"),
      encodeStringField(6, "retry-1"),
      encodeBoolField(7, true),
      encodeBoolField(8, true),
      encodeInt64Field(9, stateRevision)
    ])
  );
  assert.equal(claim.processing, true);
  assert.equal(claim.duplicate, true);
  assert.equal(claim.stateRevision, stateRevision);
});

test("activity scenario is accepted by the CLI parser", () => {
  assert.equal(parseArgs(["--scenario", "activity"]).scenario, "activity");
});
