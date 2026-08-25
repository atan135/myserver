import assert from "node:assert/strict";
import { test } from "node:test";

import { parseArgs } from "./args.js";
import { MESSAGE_TYPE } from "./constants.js";
import {
  decodeByMessageType,
  encodeActivityActionReq,
  encodeActivityClaimHistoryReq,
  encodeActivityClaimReq,
  encodeActivityProgressReq
} from "./messages.js";
import {
  decodeFieldsWithRepeated,
  encodeBoolField,
  encodeInt64Field,
  encodeMessageField,
  encodeStringField,
  encodeUInt32Field,
  readString,
  readUInt32
} from "./protocol.js";
import {
  buildActivityRequestSequence,
  describeActivityTcpEndpoint,
  runActivityScenario,
  validateActivityOptions
} from "./scenarios/activity.js";

function activityOptions(action = "claim") {
  const args = [
    "--scenario", "activity",
    "--activity-id", "activity-demo",
    "--activity-version", "3",
    "--activity-action", action,
    "--activity-request-id", "activity-retry-1",
    "--activity-pacing-ms", "0",
    "--timeout-ms", "50"
  ];
  if (action === "claim") {
    args.push("--activity-stage-id", "stage-1");
  }
  return parseArgs(args);
}

function encodeActivitySummary(activityId, version, activityType = "login_reward") {
  return Buffer.concat([
    encodeStringField(1, activityId),
    encodeUInt32Field(2, version),
    encodeStringField(3, activityType),
    encodeStringField(4, "running"),
    encodeInt64Field(5, 1),
    encodeInt64Field(6, 2),
    encodeInt64Field(7, 3),
    encodeStringField(8, "Asia/Shanghai")
  ]);
}

function responsePacket(messageType, seq, body) {
  return { messageType, seq, body };
}

function listResponse(seq, target, ok = true, errorCode = "") {
  const fields = [encodeBoolField(1, ok), encodeStringField(2, errorCode)];
  if (ok) {
    fields.push(encodeInt64Field(3, 100), encodeMessageField(4, encodeActivitySummary(target.activityId, target.version)));
  }
  return responsePacket(MESSAGE_TYPE.ACTIVITY_LIST_RES, seq, Buffer.concat(fields));
}

function detailResponse(seq, target, progress = {}) {
  return responsePacket(
    MESSAGE_TYPE.ACTIVITY_DETAIL_RES,
    seq,
    Buffer.concat([
      encodeBoolField(1, true),
      encodeMessageField(3, encodeActivitySummary(target.activityId, target.version)),
      encodeStringField(4, JSON.stringify(progress)),
      encodeInt64Field(5, 9)
    ])
  );
}

function progressResponse(seq, target, progress = {}) {
  return responsePacket(
    MESSAGE_TYPE.ACTIVITY_PROGRESS_RES,
    seq,
    Buffer.concat([
      encodeBoolField(1, true),
      encodeStringField(3, target.activityId),
      encodeUInt32Field(4, target.version),
      encodeStringField(5, JSON.stringify(progress)),
      encodeInt64Field(6, 9)
    ])
  );
}

function actionResponse(seq, target, duplicate) {
  const claim = target.action === "claim";
  const fields = [
    encodeBoolField(1, true),
    encodeStringField(3, target.activityId),
    encodeUInt32Field(4, target.version),
    encodeStringField(5, target.stageId)
  ];
  if (claim) {
    fields.push(
      encodeStringField(6, target.clientRequestId),
      encodeBoolField(7, false),
      encodeBoolField(8, duplicate),
      encodeInt64Field(9, 10)
    );
  } else {
    fields.push(
      encodeStringField(6, target.action),
      encodeStringField(7, target.clientRequestId),
      encodeBoolField(8, false),
      encodeBoolField(9, duplicate),
      encodeInt64Field(10, 10)
    );
  }
  return responsePacket(
    claim ? MESSAGE_TYPE.ACTIVITY_CLAIM_RES : MESSAGE_TYPE.ACTIVITY_ACTION_RES,
    seq,
    Buffer.concat(fields)
  );
}

function failedActionResponse(seq, target, errorCode) {
  return responsePacket(
    target.action === "claim" ? MESSAGE_TYPE.ACTIVITY_CLAIM_RES : MESSAGE_TYPE.ACTIVITY_ACTION_RES,
    seq,
    Buffer.concat([encodeBoolField(1, false), encodeStringField(2, errorCode)])
  );
}

function successfulPackets(options) {
  const { target } = buildActivityRequestSequence(options);
  return [
    listResponse(2, target),
    detailResponse(3, target, { phase: "before" }),
    progressResponse(4, target, { phase: "before" }),
    actionResponse(5, target, false),
    actionResponse(6, target, true),
    detailResponse(7, target, { phase: "after", result: "server-owned" }),
    progressResponse(8, target, { phase: "after" })
  ];
}

class FakeClient {
  constructor(packets, events) {
    this.packets = [...packets];
    this.events = events;
    this.sent = [];
  }

  async connect() {
    this.events.push("connect");
  }

  async send(messageType, seq, body) {
    this.events.push(`send:${messageType}:${seq}`);
    this.sent.push({ messageType, seq, body });
  }

  async readNextPacket() {
    const packet = this.packets.shift();
    if (!packet) {
      throw new Error("fake client has no response packet");
    }
    return packet;
  }

  close() {
    this.events.push("close");
  }
}

async function runWithFakeClient(options, packets = successfulPackets(options), { sleep } = {}) {
  const events = [];
  const client = new FakeClient(packets, events);
  const result = await runActivityScenario(options, {
    fetchTicket: async (liveOptions) => {
      events.push("fetchTicket");
      liveOptions.gameHost = "discovered-proxy";
      liveOptions.port = 14000;
      return {
        ticket: "test-ticket-not-logged",
        accountPlayerId: "account-1",
        characterId: "character-1",
        services: {
          game: { protocol: "tcp", host: "discovered-proxy", port: 14000 }
        }
      };
    },
    createClient: () => {
      events.push("createClient");
      return client;
    },
    authenticateClient: async (_client, liveOptions, login, seq) => {
      assert.equal(liveOptions.gameHost, "discovered-proxy");
      assert.equal(login.ticket, "test-ticket-not-logged");
      assert.equal(seq, 1);
      events.push("authenticate");
    },
    sleep: sleep ? (delayMs) => sleep(delayMs, events) : undefined
  });
  return { client, events, result };
}

test("activity CLI parser accepts explicit target/action/retry and dry-run parameters", () => {
  const parsed = parseArgs([
    "--scenario", "activity",
    "--activity-id", "lottery-1",
    "--activity-version", "7",
    "--activity-stage-id", "pool-1",
    "--activity-action", "draw",
    "--activity-request-id", "draw-1",
    "--activity-pacing-ms", "250",
    "--activity-dry-run"
  ]);
  assert.equal(parsed.activityId, "lottery-1");
  assert.equal(parsed.activityVersion, 7);
  assert.equal(parsed.activityStageId, "pool-1");
  assert.equal(parsed.activityAction, "draw");
  assert.equal(parsed.activityRequestId, "draw-1");
  assert.equal(parsed.activityPacingMs, 250);
  assert.equal(parsed.activityDryRun, true);
  assert.equal(parseArgs([]).activityPacingMs, 150);

  assert.throws(() => validateActivityOptions(parseArgs(["--scenario", "activity"])), /--activity-id/);
  assert.throws(() => validateActivityOptions({ ...parsed, activityVersion: 0 }), /--activity-version/);
  assert.throws(
    () => validateActivityOptions(parseArgs([
      "--scenario", "activity", "--activity-id", "a", "--activity-version", "3junk",
      "--activity-action", "draw", "--activity-request-id", "r"
    ])),
    /--activity-version/
  );
  assert.throws(() => validateActivityOptions({ ...parsed, activityAction: "grant" }), /claim or draw/);
  assert.throws(() => validateActivityOptions({ ...parsed, activityPacingMs: -1 }), /--activity-pacing-ms/);
  assert.throws(() => validateActivityOptions({ ...parsed, activityPacingMs: 5001 }), /--activity-pacing-ms/);
  assert.throws(
    () => validateActivityOptions({ ...parsed, activityAction: "claim", activityStageId: "" }),
    /--activity-stage-id/
  );
  assert.doesNotThrow(() => validateActivityOptions({
    ...parsed,
    activityRequestId: "界".repeat(42)
  }));
  assert.throws(
    () => validateActivityOptions({ ...parsed, activityRequestId: "界".repeat(43) }),
    /128 UTF-8 bytes/
  );
  assert.doesNotThrow(() => validateActivityOptions({
    ...parsed,
    activityId: "界".repeat(43),
    activityStageId: "界".repeat(43)
  }));
});

test("activity TCP endpoint report distinguishes configured fallback from a tcp descriptor", () => {
  const configured = parseArgs([]);
  const kcp = describeActivityTcpEndpoint(configured, {
    services: { game: { protocol: "kcp", host: "127.0.0.1", port: 4000 } }
  });
  assert.deepEqual(kcp, {
    address: "127.0.0.1:14000",
    transport: "tcp",
    source: "configured TCP endpoint",
    descriptorProtocol: "kcp"
  });

  const discovered = describeActivityTcpEndpoint(
    { ...configured, gameHost: "proxy.test", port: 14123 },
    { services: { game: { protocol: "tcp", host: "proxy.test", port: 14123 } } }
  );
  assert.equal(discovered.source, "services.game tcp descriptor");
  assert.equal(discovered.address, "proxy.test:14123");
});

test("activity request encoders include only server-issued routing and opaque retry fields", () => {
  const progress = decodeFieldsWithRepeated(encodeActivityProgressReq("activity-demo", 3));
  assert.equal(readString(progress, 1), "activity-demo");
  assert.equal(readUInt32(progress, 2), 3);

  const claim = decodeFieldsWithRepeated(
    encodeActivityClaimReq("activity-demo", 3, "stage-1", "retry-1")
  );
  assert.equal(readString(claim, 1), "activity-demo");
  assert.equal(readUInt32(claim, 2), 3);
  assert.equal(readString(claim, 3), "stage-1");
  assert.equal(readString(claim, 4), "retry-1");

  const drawBody = encodeActivityActionReq("activity-demo", 3, "", "draw", "retry-1");
  const draw = decodeFieldsWithRepeated(drawBody);
  assert.equal(readString(draw, 4), "draw");
  assert.equal(readString(draw, 5), "retry-1");
  for (const forbidden of ["character-1", "account-1", "reward", "weight", "probability", "progress", "token"]) {
    assert.equal(drawBody.includes(Buffer.from(forbidden)), false, `${forbidden} must remain server-owned`);
  }
});

test("activity history request and response preserve the character-bound paging contract", () => {
  const request = decodeFieldsWithRepeated(encodeActivityClaimHistoryReq("opaque-cursor", 25));
  assert.equal(readString(request, 1), "opaque-cursor");
  assert.equal(readUInt32(request, 2), 25);
  for (const forbidden of ["character_id", "character-1", "ticket", "reward_snapshot"]) {
    assert.equal(encodeActivityClaimHistoryReq("opaque-cursor", 25).includes(Buffer.from(forbidden)), false);
  }

  const reward = Buffer.concat([
    encodeStringField(1, "item"),
    encodeStringField(2, "1001"),
    encodeInt64Field(3, 2)
  ]);
  const granted = Buffer.concat([
    encodeStringField(1, "activity-demo"),
    encodeUInt32Field(2, 3),
    encodeStringField(3, "login_reward"),
    encodeStringField(4, "claim"),
    encodeStringField(5, "stage-1"),
    encodeInt64Field(6, 1000),
    encodeInt64Field(7, 1100),
    encodeStringField(8, "granted"),
    encodeMessageField(9, reward)
  ]);
  const manualReview = Buffer.concat([
    encodeStringField(1, "activity-demo"),
    encodeUInt32Field(2, 3),
    encodeStringField(4, "draw"),
    encodeStringField(8, "manual_review")
  ]);
  const response = decodeByMessageType(
    MESSAGE_TYPE.ACTIVITY_CLAIM_HISTORY_RES,
    Buffer.concat([
      encodeBoolField(1, true),
      encodeMessageField(3, granted),
      encodeMessageField(3, manualReview),
      encodeStringField(4, "next-cursor"),
      encodeBoolField(5, true)
    ])
  );
  assert.equal(response.ok, true);
  assert.equal(response.nextCursor, "next-cursor");
  assert.equal(response.hasMore, true);
  assert.equal(response.records.length, 2);
  assert.deepEqual(response.records[0].rewards, [{ rewardType: "item", assetId: "1001", quantity: 2 }]);
  assert.deepEqual(response.records[1].rewards, []);
  assert.equal(response.records[1].status, "manual_review");
  assert.equal(response.records[1].errorCode, undefined);

  const empty = decodeByMessageType(
    MESSAGE_TYPE.ACTIVITY_CLAIM_HISTORY_RES,
    Buffer.concat([encodeBoolField(1, true), encodeBoolField(5, false)])
  );
  assert.deepEqual(empty.records, []);
  assert.equal(empty.hasMore, false);

  const unavailable = decodeByMessageType(
    MESSAGE_TYPE.ACTIVITY_CLAIM_HISTORY_RES,
    Buffer.concat([
      encodeBoolField(1, false),
      encodeStringField(2, "ACTIVITY_STORAGE_UNAVAILABLE")
    ])
  );
  assert.equal(unavailable.ok, false);
  assert.equal(unavailable.errorCode, "ACTIVITY_STORAGE_UNAVAILABLE");
});

test("activity response decoder covers progress and generic action contracts", () => {
  const target = validateActivityOptions(activityOptions("draw"));
  const progress = decodeByMessageType(
    MESSAGE_TYPE.ACTIVITY_PROGRESS_RES,
    progressResponse(4, target, { draws: 1 }).body
  );
  assert.equal(progress.activityId, target.activityId);
  assert.equal(progress.version, target.version);
  assert.deepEqual(JSON.parse(progress.progressJson), { draws: 1 });

  const action = decodeByMessageType(
    MESSAGE_TYPE.ACTIVITY_ACTION_RES,
    actionResponse(5, target, true).body
  );
  assert.equal(action.actionType, "draw");
  assert.equal(action.clientRequestId, target.clientRequestId);
  assert.equal(action.duplicate, true);
  assert.equal(action.processing, false);
});

test("activity dry-run builds claim sequence without login or sockets", async () => {
  const options = { ...activityOptions("claim"), activityDryRun: true };
  let dependencyCalled = false;
  const result = await runActivityScenario(options, {
    fetchTicket: async () => {
      dependencyCalled = true;
    },
    createClient: () => {
      dependencyCalled = true;
    }
  });
  assert.equal(result.dryRun, true);
  assert.equal(dependencyCalled, false);
  assert.deepEqual(result.requests.map((request) => request.messageType), [
    MESSAGE_TYPE.ACTIVITY_LIST_REQ,
    MESSAGE_TYPE.ACTIVITY_DETAIL_REQ,
    MESSAGE_TYPE.ACTIVITY_PROGRESS_REQ,
    MESSAGE_TYPE.ACTIVITY_CLAIM_REQ,
    MESSAGE_TYPE.ACTIVITY_CLAIM_REQ,
    MESSAGE_TYPE.ACTIVITY_DETAIL_REQ,
    MESSAGE_TYPE.ACTIVITY_PROGRESS_REQ
  ]);
  assert.deepEqual(result.requests[3].body, result.requests[4].body);
});

for (const action of ["claim", "draw"]) {
  test(`activity live ${action} flow logs in, matches responses and replays the same request id`, async () => {
    const options = activityOptions(action);
    const { client, events, result } = await runWithFakeClient(options);
    const actionRequestType = action === "claim"
      ? MESSAGE_TYPE.ACTIVITY_CLAIM_REQ
      : MESSAGE_TYPE.ACTIVITY_ACTION_REQ;

    assert.deepEqual(events.slice(0, 4), ["fetchTicket", "createClient", "connect", "authenticate"]);
    assert.equal(events.at(-1), "close");
    assert.deepEqual(client.sent.map(({ messageType, seq }) => ({ messageType, seq })), [
      { messageType: MESSAGE_TYPE.ACTIVITY_LIST_REQ, seq: 2 },
      { messageType: MESSAGE_TYPE.ACTIVITY_DETAIL_REQ, seq: 3 },
      { messageType: MESSAGE_TYPE.ACTIVITY_PROGRESS_REQ, seq: 4 },
      { messageType: actionRequestType, seq: 5 },
      { messageType: actionRequestType, seq: 6 },
      { messageType: MESSAGE_TYPE.ACTIVITY_DETAIL_REQ, seq: 7 },
      { messageType: MESSAGE_TYPE.ACTIVITY_PROGRESS_REQ, seq: 8 }
    ]);
    assert.deepEqual(client.sent[3].body, client.sent[4].body);
    assert.equal(result.firstAction.duplicate, false);
    assert.equal(result.replayAction.duplicate, true);
    assert.equal(result.resultQuery.interface, "ActivityDetailRes.progress_json + ActivityProgressRes.progress_json");
    assert.deepEqual(result.resultQuery.detail, { phase: "after", result: "server-owned" });
  });
}

test("activity live pacing stays outside the read limiter window but keeps replay adjacent", async () => {
  const options = { ...activityOptions("claim"), activityPacingMs: 150 };
  const { events } = await runWithFakeClient(options, successfulPackets(options), {
    sleep: async (delayMs, flowEvents) => flowEvents.push(`sleep:${delayMs}`)
  });
  assert.equal(events.filter((event) => event === "sleep:150").length, 5);
  const firstActionIndex = events.indexOf(`send:${MESSAGE_TYPE.ACTIVITY_CLAIM_REQ}:5`);
  assert.equal(events[firstActionIndex + 1], `send:${MESSAGE_TYPE.ACTIVITY_CLAIM_REQ}:6`);
});

test("activity live flow stops on the first failed response and closes the client", async () => {
  const options = activityOptions("claim");
  const target = validateActivityOptions(options);
  const events = [];
  const client = new FakeClient([
    listResponse(2, target),
    detailResponse(3, target),
    progressResponse(4, target),
    failedActionResponse(5, target, "ACTIVITY_QUALIFICATION_NOT_MET")
  ], events);

  await assert.rejects(
    runActivityScenario(options, {
      fetchTicket: async () => ({ ticket: "test-ticket" }),
      createClient: () => client,
      authenticateClient: async () => {}
    }),
    /activity\.action\.first failed: ACTIVITY_QUALIFICATION_NOT_MET/
  );
  assert.equal(client.sent.length, 4);
  assert.equal(events.at(-1), "close");
});
