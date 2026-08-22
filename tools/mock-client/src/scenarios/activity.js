import { fetchTicket } from "../auth.js";
import { TcpProtocolClient } from "../client.js";
import { MESSAGE_TYPE } from "../constants.js";
import {
  decodeByMessageType,
  encodeActivityActionReq,
  encodeActivityClaimReq,
  encodeActivityDetailReq,
  encodeActivityListReq,
  encodeActivityProgressReq
} from "../messages.js";
import { authenticateClient } from "./room.js";

const ACTIVITY_ACTIONS = new Set(["claim", "draw"]);
const MAX_UINT32 = 0xffffffff;
const MAX_ACTIVITY_PACING_MS = 5000;

function requireText(value, option, { allowEmpty = false, maxBytes = null } = {}) {
  const text = String(value || "").trim();
  if (!allowEmpty && !text) {
    throw new Error(`activity scenario requires ${option}`);
  }
  if (maxBytes !== null && Buffer.byteLength(text, "utf8") > maxBytes) {
    throw new Error(`${option} must not exceed ${maxBytes} UTF-8 bytes`);
  }
  return text;
}

export function validateActivityOptions(options) {
  const activityId = requireText(options.activityId, "--activity-id");
  const version = Number(options.activityVersion);
  if (!Number.isInteger(version) || version < 1 || version > MAX_UINT32) {
    throw new Error("activity scenario requires --activity-version as an integer from 1 to 4294967295");
  }

  const action = requireText(options.activityAction, "--activity-action").toLowerCase();
  if (!ACTIVITY_ACTIONS.has(action)) {
    throw new Error("--activity-action must be claim or draw");
  }
  const stageId = requireText(options.activityStageId, "--activity-stage-id", {
    allowEmpty: action === "draw"
  });
  const clientRequestId = requireText(
    options.activityRequestId,
    "--activity-request-id",
    { maxBytes: 128 }
  );
  const pacingMs = Number(options.activityPacingMs);
  if (!Number.isInteger(pacingMs) || pacingMs < 0 || pacingMs > MAX_ACTIVITY_PACING_MS) {
    throw new Error(`--activity-pacing-ms must be an integer from 0 to ${MAX_ACTIVITY_PACING_MS}`);
  }

  return { activityId, version, stageId, action, clientRequestId, pacingMs };
}

export function describeActivityTcpEndpoint(options, login) {
  const descriptor = login?.services?.game;
  const descriptorProtocol = String(descriptor?.protocol || "").toLowerCase();
  const host = options.gameHost || options.host;
  const port = Number(options.port);
  const usesTcpDescriptor = options.useServiceDiscovery !== false &&
    descriptor?.host === host && Number(descriptor?.port) === port &&
    (!descriptorProtocol || descriptorProtocol === "tcp");
  return {
    address: `${host}:${port}`,
    transport: "tcp",
    source: usesTcpDescriptor ? "services.game tcp descriptor" : "configured TCP endpoint",
    descriptorProtocol: descriptorProtocol || null
  };
}

/**
 * Build the player activity request sequence without opening a connection. The repeated action
 * deliberately reuses the same opaque retry id.
 */
export function buildActivityRequestSequence(options, startSeq = 2) {
  const target = validateActivityOptions(options);
  const actionRequest = target.action === "claim"
    ? {
        messageType: MESSAGE_TYPE.ACTIVITY_CLAIM_REQ,
        responseType: MESSAGE_TYPE.ACTIVITY_CLAIM_RES,
        body: encodeActivityClaimReq(
          target.activityId,
          target.version,
          target.stageId,
          target.clientRequestId
        )
      }
    : {
        messageType: MESSAGE_TYPE.ACTIVITY_ACTION_REQ,
        responseType: MESSAGE_TYPE.ACTIVITY_ACTION_RES,
        body: encodeActivityActionReq(
          target.activityId,
          target.version,
          target.stageId,
          target.action,
          target.clientRequestId
        )
      };

  const request = (label, messageType, responseType, body, offset) => ({
    label,
    messageType,
    responseType,
    seq: startSeq + offset,
    body
  });

  return {
    target,
    requests: [
      request("list", MESSAGE_TYPE.ACTIVITY_LIST_REQ, MESSAGE_TYPE.ACTIVITY_LIST_RES, encodeActivityListReq(), 0),
      request(
        "detail.before",
        MESSAGE_TYPE.ACTIVITY_DETAIL_REQ,
        MESSAGE_TYPE.ACTIVITY_DETAIL_RES,
        encodeActivityDetailReq(target.activityId, target.version),
        1
      ),
      request(
        "progress.before",
        MESSAGE_TYPE.ACTIVITY_PROGRESS_REQ,
        MESSAGE_TYPE.ACTIVITY_PROGRESS_RES,
        encodeActivityProgressReq(target.activityId, target.version),
        2
      ),
      request("action.first", actionRequest.messageType, actionRequest.responseType, actionRequest.body, 3),
      request("action.replay", actionRequest.messageType, actionRequest.responseType, actionRequest.body, 4),
      request(
        "detail.after",
        MESSAGE_TYPE.ACTIVITY_DETAIL_REQ,
        MESSAGE_TYPE.ACTIVITY_DETAIL_RES,
        encodeActivityDetailReq(target.activityId, target.version),
        5
      ),
      request(
        "progress.after",
        MESSAGE_TYPE.ACTIVITY_PROGRESS_REQ,
        MESSAGE_TYPE.ACTIVITY_PROGRESS_RES,
        encodeActivityProgressReq(target.activityId, target.version),
        6
      )
    ]
  };
}

async function sendAndRead(client, options, request) {
  await client.send(request.messageType, request.seq, request.body);
  const deadline = Date.now() + options.timeoutMs;
  while (true) {
    const remainingMs = Math.max(1, deadline - Date.now());
    const packet = await client.readNextPacket(remainingMs);
    const decoded = decodeByMessageType(packet.messageType, packet.body);
    if (packet.messageType === request.responseType && packet.seq === request.seq) {
      console.log(`activity.${request.label}:`, JSON.stringify({
        messageType: packet.messageType,
        seq: packet.seq,
        decoded
      }, null, 2));
      return decoded;
    }
    if (packet.seq === request.seq || packet.messageType === request.responseType) {
      throw new Error(
        `activity.${request.label} expected messageType=${request.responseType} seq=${request.seq}, ` +
        `got messageType=${packet.messageType} seq=${packet.seq}`
      );
    }
    if (Date.now() >= deadline) {
      throw new Error(
        `activity.${request.label} expected messageType=${request.responseType} seq=${request.seq}, ` +
        `last packet was messageType=${packet.messageType} seq=${packet.seq}`
      );
    }
    console.log(`activity.${request.label}.ignoredPush:`, JSON.stringify({
      messageType: packet.messageType,
      seq: packet.seq,
      decoded
    }, null, 2));
  }
}

function requireSuccess(response, label) {
  if (!response.ok) {
    throw new Error(`activity.${label} failed: ${response.errorCode || "UNKNOWN_ERROR"}`);
  }
}

function requireTarget(response, target, label, { includeAction = false } = {}) {
  if (response.activityId !== target.activityId || response.version !== target.version) {
    throw new Error(`activity.${label} returned a different activity or version`);
  }
  if (includeAction) {
    if (response.stageId !== target.stageId || response.clientRequestId !== target.clientRequestId) {
      throw new Error(`activity.${label} returned a different stage or request id`);
    }
    if (target.action === "draw" && response.actionType !== target.action) {
      throw new Error(`activity.${label} returned action ${response.actionType || "<empty>"}, expected draw`);
    }
  }
}

function parseProgressJson(value, label) {
  if (!value) {
    throw new Error(`activity.${label} returned empty progress_json`);
  }
  try {
    return JSON.parse(value);
  } catch (error) {
    throw new Error(`activity.${label} returned invalid progress_json: ${error.message}`);
  }
}

export async function runActivityFlow(client, options, dependencies = {}) {
  const { target, requests } = buildActivityRequestSequence(options);
  const responses = {};
  const sleep = dependencies.sleep || ((delayMs) => new Promise((resolve) => setTimeout(resolve, delayMs)));
  let previousRequest = null;

  for (const request of requests) {
    // Keep the idempotent replay adjacent. Other requests are paced because detail and progress
    // share game-server's read:detail limiter key and its current window is 100ms.
    if (previousRequest && request.label !== "action.replay" && target.pacingMs > 0) {
      await sleep(target.pacingMs);
    }
    const response = await sendAndRead(client, options, request);
    requireSuccess(response, request.label);
    responses[request.label] = response;

    if (request.label === "list") {
      const listed = response.activities.find((item) => item.activityId === target.activityId);
      if (!listed || listed.version !== target.version) {
        throw new Error("activity.list did not contain the requested activity and version");
      }
    } else if (request.label.startsWith("detail")) {
      if (!response.activity) {
        throw new Error(`activity.${request.label} returned no activity summary`);
      }
      requireTarget(response.activity, target, request.label);
      parseProgressJson(response.progressJson, request.label);
    } else if (request.label.startsWith("progress")) {
      requireTarget(response, target, request.label);
      parseProgressJson(response.progressJson, request.label);
    } else {
      requireTarget(response, target, request.label, { includeAction: true });
      if (request.label === "action.first" && response.duplicate) {
        throw new Error("activity.action.first was already a duplicate; use a new --activity-request-id");
      }
      if (request.label === "action.replay") {
        const first = responses["action.first"];
        if (!response.duplicate) {
          throw new Error("activity.action.replay did not report duplicate=true");
        }
        if (response.processing !== first.processing) {
          throw new Error("activity.action.replay changed the processing state for the same request id");
        }
      }
    }
    previousRequest = request;
  }

  const first = responses["action.first"];
  const replay = responses["action.replay"];

  return {
    target,
    firstAction: first,
    replayAction: replay,
    resultQuery: {
      interface: "ActivityDetailRes.progress_json + ActivityProgressRes.progress_json",
      detail: parseProgressJson(responses["detail.after"].progressJson, "detail.after"),
      progress: parseProgressJson(responses["progress.after"].progressJson, "progress.after")
    }
  };
}

/**
 * Run the live player flow through auth-http and a configured TCP game endpoint. A tcp
 * services.game descriptor may override it; the normal kcp descriptor is intentionally ignored.
 * Dependencies are injectable so the full orchestration can be tested without opening sockets.
 */
export async function runActivityScenario(options, dependencies = {}) {
  const sequence = buildActivityRequestSequence(options);
  if (options.activityDryRun) {
    return { dryRun: true, ...sequence };
  }

  const fetchTicketFn = dependencies.fetchTicket || fetchTicket;
  const createClient = dependencies.createClient || ((clientOptions) => new TcpProtocolClient(clientOptions, "activity"));
  const authenticate = dependencies.authenticateClient || authenticateClient;
  const login = await fetchTicketFn(options);
  const client = createClient(options);
  await client.connect();
  try {
    await authenticate(client, options, login, 1);
    const endpoint = describeActivityTcpEndpoint(options, login);
    console.log("activity.target:", JSON.stringify({
      accountPlayerId: login.accountPlayerId || login.playerId || null,
      characterId: login.characterId || null,
      gameEndpoint: endpoint.address,
      gameTransport: endpoint.transport,
      gameEndpointSource: endpoint.source,
      loginGameDescriptorProtocol: endpoint.descriptorProtocol,
      activityId: sequence.target.activityId,
      version: sequence.target.version,
      action: sequence.target.action,
      stageId: sequence.target.stageId || null,
      pacingMs: sequence.target.pacingMs
    }, null, 2));
    return await runActivityFlow(client, options, { sleep: dependencies.sleep });
  } finally {
    client.close();
  }
}
