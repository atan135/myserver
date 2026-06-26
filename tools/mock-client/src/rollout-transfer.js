import net from "node:net";

import { HEADER_LEN, MAGIC, MESSAGE_TYPE, VERSION } from "./constants.js";
import { encodePacket } from "./packet.js";
import {
  decodeFieldsWithRepeated,
  encodeBoolField,
  encodeStringField,
  encodeVarint,
  readBool,
  readInt64,
  readString,
  readUInt32
} from "./protocol.js";

export const ROOM_TRANSFER_STAGE = {
  OLD_FREEZE: "old_freeze",
  OLD_EXPORT: "old_export",
  NEW_IMPORT: "new_import",
  NEW_CONFIRM_OWNERSHIP: "new_confirm_ownership",
  PROXY_ROUTE_UPSERT: "proxy_route_upsert",
  OLD_RETIRE: "old_retire"
};

export const ROOM_TRANSFER_FAILURE_INJECTION = {
  IMPORT_CORRUPT_PAYLOAD: "import_corrupt_payload",
  PROXY_BAD_EXPECTED_ROOM_VERSION: "proxy_bad_expected_room_version",
  PROXY_BAD_EXPECTED_LAST_TRANSFER_CHECKSUM: "proxy_bad_expected_last_transfer_checksum",
  PROXY_MISSING_ROUTE_METADATA: "proxy_missing_route_metadata"
};

const DEFAULT_TIMEOUT_MS = 5000;
const DEFAULT_MAX_BODY_LEN = 1024 * 1024;

function encodeMessageField(fieldNumber, body) {
  const fieldKey = (fieldNumber << 3) | 2;
  return Buffer.concat([encodeVarint(fieldKey), encodeVarint(body.length), body]);
}

function encodeUInt64Field(fieldNumber, value) {
  const fieldKey = fieldNumber << 3;
  return Buffer.concat([encodeVarint(fieldKey), encodeVarint(BigInt(value))]);
}

function encodeFreezeRoomForTransferReq(rolloutEpoch, roomId) {
  return Buffer.concat([
    encodeStringField(1, rolloutEpoch),
    encodeStringField(2, roomId)
  ]);
}

function encodeExportRoomTransferReq(rolloutEpoch, roomId) {
  return Buffer.concat([
    encodeStringField(1, rolloutEpoch),
    encodeStringField(2, roomId)
  ]);
}

function encodeImportRoomTransferReq(payloadRaw) {
  return encodeMessageField(1, payloadRaw);
}

function encodeConfirmRoomOwnershipReq({ rolloutEpoch, roomId, checksum, roomVersion }) {
  return Buffer.concat([
    encodeStringField(1, rolloutEpoch),
    encodeStringField(2, roomId),
    encodeStringField(3, checksum),
    encodeUInt64Field(4, roomVersion)
  ]);
}

function encodeRetireTransferredRoomReq(rolloutEpoch, roomId, checksum) {
  return Buffer.concat([
    encodeStringField(1, rolloutEpoch),
    encodeStringField(2, roomId),
    encodeStringField(3, checksum)
  ]);
}

function encodeTriggerServerRedirectReq({
  roomId,
  rolloutEpoch,
  reason,
  targetHost,
  targetPort,
  targetServerId,
  transport,
  retryAfterMs
}) {
  return Buffer.concat([
    encodeStringField(1, roomId),
    encodeStringField(2, rolloutEpoch),
    encodeStringField(3, reason || "rollout_redirect"),
    encodeStringField(4, targetHost),
    encodeUInt64Field(5, targetPort),
    encodeStringField(6, targetServerId || ""),
    encodeStringField(7, transport || "kcp"),
    encodeUInt64Field(8, retryAfterMs || 0)
  ]);
}

function decodeFreezeRoomForTransferRes(body) {
  const fields = decodeFieldsWithRepeated(body);
  return {
    ok: readBool(fields, 1),
    roomId: readString(fields, 2),
    errorCode: readString(fields, 3),
    migrationState: readUInt32(fields, 4),
    roomVersion: readInt64(fields, 5)
  };
}

function decodeRoomTransferPayloadRaw(payloadRaw) {
  const fields = decodeFieldsWithRepeated(payloadRaw);
  return {
    rolloutEpoch: readString(fields, 1),
    roomId: readString(fields, 2),
    roomVersion: readInt64(fields, 3),
    checksum: readString(fields, 17),
    raw: payloadRaw
  };
}

function decodeExportRoomTransferRes(body) {
  const fields = decodeFieldsWithRepeated(body);
  const payloadRaw = fields.get(4) ? Buffer.from(fields.get(4)) : null;
  const payload = payloadRaw ? decodeRoomTransferPayloadRaw(payloadRaw) : null;
  return {
    ok: readBool(fields, 1),
    roomId: readString(fields, 2),
    errorCode: readString(fields, 3),
    payload,
    checksum: readString(fields, 5)
  };
}

function decodeImportRoomTransferRes(body) {
  const fields = decodeFieldsWithRepeated(body);
  return {
    ok: readBool(fields, 1),
    roomId: readString(fields, 2),
    errorCode: readString(fields, 3),
    checksum: readString(fields, 4),
    roomVersion: readInt64(fields, 5)
  };
}

function decodeConfirmRoomOwnershipRes(body) {
  const fields = decodeFieldsWithRepeated(body);
  return {
    ok: readBool(fields, 1),
    roomId: readString(fields, 2),
    errorCode: readString(fields, 3),
    checksum: readString(fields, 4),
    roomVersion: readInt64(fields, 5)
  };
}

function decodeRetireTransferredRoomRes(body) {
  const fields = decodeFieldsWithRepeated(body);
  return {
    ok: readBool(fields, 1),
    roomId: readString(fields, 2),
    errorCode: readString(fields, 3)
  };
}

function decodeTriggerServerRedirectRes(body) {
  const fields = decodeFieldsWithRepeated(body);
  return {
    ok: readBool(fields, 1),
    roomId: readString(fields, 2),
    errorCode: readString(fields, 3),
    deliveredCount: readInt64(fields, 4),
    failedCount: readInt64(fields, 5),
    onlineMemberCount: readInt64(fields, 6)
  };
}

function decodeErrorRes(body) {
  const fields = decodeFieldsWithRepeated(body);
  return {
    errorCode: readString(fields, 1),
    message: readString(fields, 2)
  };
}

function decodePacket(buffer) {
  if (buffer.length < HEADER_LEN) {
    throw new Error("packet too short");
  }

  const magic = buffer.readUInt16BE(0);
  const version = buffer.readUInt8(2);
  const flags = buffer.readUInt8(3);
  const messageType = buffer.readUInt16BE(4);
  const seq = buffer.readUInt32BE(6);
  const bodyLen = buffer.readUInt32BE(10);

  if (magic !== MAGIC) throw new Error("INVALID_MAGIC");
  if (version !== VERSION) throw new Error("INVALID_VERSION");
  if (flags !== 0) throw new Error("UNSUPPORTED_FLAGS");
  if (buffer.length !== HEADER_LEN + bodyLen) throw new Error("INVALID_PACKET_LENGTH");

  return { messageType, seq, body: buffer.subarray(HEADER_LEN) };
}

function nextSeqFactory() {
  let nextSeq = 1;
  return () => {
    const seq = nextSeq >>> 0;
    nextSeq = (nextSeq + 1) >>> 0;
    if (nextSeq === 0) {
      nextSeq = 1;
    }
    return seq;
  };
}

function normalizeError(error) {
  if (!error) {
    return { message: "unknown error", code: "UNKNOWN_ERROR" };
  }
  if (typeof error === "string") {
    return { message: error, code: error };
  }
  return {
    message: error.message || String(error),
    code: error.code || error.errorCode || "ERROR"
  };
}

function assertOkResponse(response, fallbackCode) {
  if (!response?.ok) {
    const error = new Error(response?.errorCode || fallbackCode);
    error.code = response?.errorCode || fallbackCode;
    throw error;
  }
}

function failure(stage, error, context, completedStages, options = {}) {
  const normalized = normalizeError(error);
  const hasExpectedFailure = typeof error?.expectedFailure === "boolean" ||
    Object.hasOwn(options, "expectedFailure");
  const expectedFailure = typeof error?.expectedFailure === "boolean"
    ? error.expectedFailure
    : options.expectedFailure === true;
  const result = {
    ok: false,
    stage,
    errorCode: normalized.code,
    error: normalized.message,
    completedStages: [...completedStages],
    ...context
  };
  if (hasExpectedFailure) {
    result.expectedFailure = expectedFailure;
  }
  return result;
}

function success(context, completedStages) {
  return {
    ok: true,
    stage: "complete",
    completedStages: [...completedStages],
    ...context
  };
}

function routeField(route, camelName, snakeName, fallback) {
  if (!route) {
    return fallback;
  }
  if (route[camelName] !== undefined) {
    return route[camelName];
  }
  if (route[snakeName] !== undefined) {
    return route[snakeName];
  }
  return fallback;
}

function buildProxyRouteUpsert(request, importResult, existingRoute) {
  const hasExistingRoute = Boolean(existingRoute);
  const expectedRoomVersion = request.proxyExpectedRoomVersion ?? routeField(
    existingRoute,
    "roomVersion",
    "room_version",
    0
  );
  const expectedLastTransferChecksum = request.proxyExpectedLastTransferChecksum ?? routeField(
    existingRoute,
    "lastTransferChecksum",
    "last_transfer_checksum",
    ""
  );
  const roomVersion = request.proxyRoomVersion ??
    (hasExistingRoute ? Number(expectedRoomVersion) + 1 : 1);

  return {
    roomId: request.roomId,
    ownerServerId: request.newServerId,
    migrationState: "OwnedByNew",
    memberCount: request.memberCount ?? routeField(existingRoute, "memberCount", "member_count", 0),
    onlineMemberCount: request.onlineMemberCount ?? 0,
    emptySinceMs: request.emptySinceMs,
    roomVersion,
    rolloutEpoch: request.rolloutEpoch,
    lastTransferChecksum: importResult.checksum,
    expectedRoomVersion,
    expectedLastTransferChecksum,
    importedRoomVersion: importResult.roomVersion
  };
}

function summarizeExistingRouteMetadata(route) {
  if (!route) {
    return null;
  }
  return {
    roomId: routeField(route, "roomId", "room_id", ""),
    ownerServerId: routeField(route, "ownerServerId", "owner_server_id", ""),
    migrationState: routeField(route, "migrationState", "migration_state", ""),
    roomVersion: routeField(route, "roomVersion", "room_version", 0),
    lastTransferChecksum: routeField(
      route,
      "lastTransferChecksum",
      "last_transfer_checksum",
      ""
    )
  };
}

function buildRouteMetadataReport(existingRoute, options = {}) {
  const found = Boolean(existingRoute);
  return {
    requiredExistingRoute: options.requiredExistingRoute === true,
    found,
    checkedVia: options.checkedVia || "proxy.getRoomRoute",
    actionOnMissing: options.actionOnMissing || "allow_first_route_create",
    ...(found ? { existingRoute: summarizeExistingRouteMetadata(existingRoute) } : {})
  };
}

function metadataMissingError(expectedFailure = false) {
  const error = Object.assign(new Error("ROOM_ROUTE_METADATA_MISSING"), {
    code: "ROOM_ROUTE_METADATA_MISSING"
  });
  if (expectedFailure) {
    error.expectedFailure = true;
  }
  return error;
}

function stageForFailureInjectionMode(mode) {
  switch (mode) {
    case ROOM_TRANSFER_FAILURE_INJECTION.IMPORT_CORRUPT_PAYLOAD:
      return ROOM_TRANSFER_STAGE.NEW_IMPORT;
    case ROOM_TRANSFER_FAILURE_INJECTION.PROXY_BAD_EXPECTED_ROOM_VERSION:
    case ROOM_TRANSFER_FAILURE_INJECTION.PROXY_BAD_EXPECTED_LAST_TRANSFER_CHECKSUM:
    case ROOM_TRANSFER_FAILURE_INJECTION.PROXY_MISSING_ROUTE_METADATA:
      return ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT;
    default:
      return undefined;
  }
}

function normalizeFailureInjection(failureInjection) {
  if (!failureInjection) {
    return null;
  }
  if (typeof failureInjection === "string") {
    return {
      mode: failureInjection,
      stage: stageForFailureInjectionMode(failureInjection)
    };
  }
  return {
    ...failureInjection,
    stage: failureInjection.stage || stageForFailureInjectionMode(failureInjection.mode)
  };
}

function hasFailureInjection(failureInjection, stage, modes = []) {
  const normalized = normalizeFailureInjection(failureInjection);
  if (!normalized) {
    return false;
  }
  if (normalized.stage && normalized.stage !== stage) {
    return false;
  }
  return modes.length === 0 || modes.includes(normalized.mode);
}

function markFailureInjection(context, stage, mode, detail) {
  context.failureInjection = {
    stage,
    mode,
    expectedFailure: true,
    ...(detail ? { detail } : {})
  };
}

function corruptPayloadRawForFailureDrill(payloadRaw) {
  const corrupted = Buffer.from(payloadRaw);
  if (corrupted.length === 0) {
    return Buffer.from([0xff]);
  }
  corrupted[corrupted.length - 1] = corrupted[corrupted.length - 1] ^ 0xff;
  return corrupted;
}

function withProxyRouteFailureInjection(route, failureInjection, context) {
  if (hasFailureInjection(
    failureInjection,
    ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT,
    [ROOM_TRANSFER_FAILURE_INJECTION.PROXY_BAD_EXPECTED_ROOM_VERSION]
  )) {
    const expectedRoomVersion = Number(route.expectedRoomVersion ?? 0);
    const badExpectedRoomVersion = Number.isSafeInteger(expectedRoomVersion)
      ? expectedRoomVersion + 1000003
      : 1000003;
    markFailureInjection(
      context,
      ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT,
      ROOM_TRANSFER_FAILURE_INJECTION.PROXY_BAD_EXPECTED_ROOM_VERSION,
      { expectedRoomVersion: badExpectedRoomVersion }
    );
    return {
      ...route,
      expectedRoomVersion: badExpectedRoomVersion
    };
  }

  if (hasFailureInjection(
    failureInjection,
    ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT,
    [ROOM_TRANSFER_FAILURE_INJECTION.PROXY_BAD_EXPECTED_LAST_TRANSFER_CHECKSUM]
  )) {
    const badChecksum = "fault-drill-expected-checksum-mismatch";
    markFailureInjection(
      context,
      ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT,
      ROOM_TRANSFER_FAILURE_INJECTION.PROXY_BAD_EXPECTED_LAST_TRANSFER_CHECKSUM,
      { expectedLastTransferChecksum: badChecksum }
    );
    return {
      ...route,
      expectedLastTransferChecksum: badChecksum
    };
  }

  return route;
}

export async function orchestrateRoomTransfer(request, clients) {
  const completedStages = [];
  const context = {
    rolloutEpoch: request.rolloutEpoch,
    roomId: request.roomId,
    oldServerId: request.oldServerId,
    newServerId: request.newServerId
  };
  const failureInjection = normalizeFailureInjection(request.failureInjection);

  let exportResult;
  let importResult;
  let confirmResult;

  try {
    const freezeResult = await clients.oldServer.freezeRoomForTransfer({
      rolloutEpoch: request.rolloutEpoch,
      roomId: request.roomId
    });
    assertOkResponse(freezeResult, "OLD_FREEZE_FAILED");
    completedStages.push(ROOM_TRANSFER_STAGE.OLD_FREEZE);
    context.freeze = freezeResult;
  } catch (error) {
    return failure(ROOM_TRANSFER_STAGE.OLD_FREEZE, error, context, completedStages);
  }

  try {
    exportResult = await clients.oldServer.exportRoomTransfer({
      rolloutEpoch: request.rolloutEpoch,
      roomId: request.roomId
    });
    assertOkResponse(exportResult, "OLD_EXPORT_FAILED");
    if (!exportResult.payload?.raw) {
      throw Object.assign(new Error("ROOM_TRANSFER_MISSING_PAYLOAD"), {
        code: "ROOM_TRANSFER_MISSING_PAYLOAD"
      });
    }
    const payloadChecksum = exportResult.payload.checksum;
    if (!exportResult.checksum || (payloadChecksum && exportResult.checksum !== payloadChecksum)) {
      throw Object.assign(new Error("ROOM_TRANSFER_CHECKSUM_MISMATCH"), {
        code: "ROOM_TRANSFER_CHECKSUM_MISMATCH"
      });
    }
    completedStages.push(ROOM_TRANSFER_STAGE.OLD_EXPORT);
    context.exported = {
      checksum: exportResult.checksum,
      roomVersion: exportResult.payload.roomVersion
    };
  } catch (error) {
    return failure(ROOM_TRANSFER_STAGE.OLD_EXPORT, error, context, completedStages);
  }

  try {
    let payloadRaw = exportResult.payload.raw;
    if (hasFailureInjection(
      failureInjection,
      ROOM_TRANSFER_STAGE.NEW_IMPORT,
      [ROOM_TRANSFER_FAILURE_INJECTION.IMPORT_CORRUPT_PAYLOAD]
    )) {
      payloadRaw = corruptPayloadRawForFailureDrill(payloadRaw);
      markFailureInjection(
        context,
        ROOM_TRANSFER_STAGE.NEW_IMPORT,
        ROOM_TRANSFER_FAILURE_INJECTION.IMPORT_CORRUPT_PAYLOAD,
        { payloadMutation: "last-byte-xor" }
      );
    }

    importResult = await clients.newServer.importRoomTransfer({
      payloadRaw
    });
    assertOkResponse(importResult, "NEW_IMPORT_FAILED");
    if (importResult.checksum !== exportResult.checksum) {
      throw Object.assign(new Error("ROOM_TRANSFER_IMPORT_CHECKSUM_MISMATCH"), {
        code: "ROOM_TRANSFER_IMPORT_CHECKSUM_MISMATCH"
      });
    }
    if (hasFailureInjection(failureInjection, ROOM_TRANSFER_STAGE.NEW_IMPORT)) {
      throw Object.assign(new Error("FAULT_DRILL_IMPORT_UNEXPECTED_SUCCESS"), {
        code: "FAULT_DRILL_IMPORT_UNEXPECTED_SUCCESS",
        expectedFailure: false
      });
    }
    completedStages.push(ROOM_TRANSFER_STAGE.NEW_IMPORT);
    context.imported = {
      checksum: importResult.checksum,
      roomVersion: importResult.roomVersion
    };
  } catch (error) {
    return failure(ROOM_TRANSFER_STAGE.NEW_IMPORT, error, context, completedStages, {
      expectedFailure: hasFailureInjection(failureInjection, ROOM_TRANSFER_STAGE.NEW_IMPORT)
    });
  }

  try {
    confirmResult = await clients.newServer.confirmRoomOwnership({
      rolloutEpoch: request.rolloutEpoch,
      roomId: request.roomId,
      checksum: importResult.checksum,
      roomVersion: importResult.roomVersion
    });
    assertOkResponse(confirmResult, "NEW_CONFIRM_OWNERSHIP_FAILED");
    if (confirmResult.checksum !== importResult.checksum) {
      throw Object.assign(new Error("ROOM_TRANSFER_CONFIRM_CHECKSUM_MISMATCH"), {
        code: "ROOM_TRANSFER_CONFIRM_CHECKSUM_MISMATCH"
      });
    }
    if (confirmResult.roomVersion !== importResult.roomVersion) {
      throw Object.assign(new Error("ROOM_TRANSFER_CONFIRM_VERSION_MISMATCH"), {
        code: "ROOM_TRANSFER_CONFIRM_VERSION_MISMATCH"
      });
    }
    completedStages.push(ROOM_TRANSFER_STAGE.NEW_CONFIRM_OWNERSHIP);
    context.confirmed = {
      checksum: confirmResult.checksum,
      roomVersion: confirmResult.roomVersion
    };
  } catch (error) {
    return failure(ROOM_TRANSFER_STAGE.NEW_CONFIRM_OWNERSHIP, error, context, completedStages);
  }

  try {
    const existingRoute = clients.proxy.getRoomRoute
      ? await clients.proxy.getRoomRoute(request.roomId)
      : null;
    const missingRouteMetadataInjection = hasFailureInjection(
      failureInjection,
      ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT,
      [ROOM_TRANSFER_FAILURE_INJECTION.PROXY_MISSING_ROUTE_METADATA]
    );
    const requireExistingRouteMetadata =
      request.requireExistingRouteMetadata === true || missingRouteMetadataInjection;
    context.routeMetadata = buildRouteMetadataReport(existingRoute, {
      requiredExistingRoute: requireExistingRouteMetadata,
      checkedVia: clients.proxy.getRoomRoute
        ? "proxy.getRoomRoute"
        : "proxy.getRoomRoute unavailable",
      actionOnMissing: requireExistingRouteMetadata
        ? "fail_before_proxy_route_upsert"
        : "allow_first_route_create"
    });

    if (missingRouteMetadataInjection) {
      markFailureInjection(
        context,
        ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT,
        ROOM_TRANSFER_FAILURE_INJECTION.PROXY_MISSING_ROUTE_METADATA,
        {
          missingRouteMetadata: true,
          actualRouteFound: Boolean(existingRoute)
        }
      );
      if (existingRoute) {
        throw Object.assign(new Error("FAULT_DRILL_ROUTE_METADATA_PRESENT"), {
          code: "FAULT_DRILL_ROUTE_METADATA_PRESENT",
          expectedFailure: false
        });
      }
    }

    if (requireExistingRouteMetadata && !existingRoute) {
      throw metadataMissingError(
        missingRouteMetadataInjection || request.expectMissingRouteMetadataFailure === true
      );
    }

    const route = withProxyRouteFailureInjection(
      buildProxyRouteUpsert(request, importResult, existingRoute),
      failureInjection,
      context
    );
    await clients.proxy.upsertRoomRoute(route);
    if (hasFailureInjection(failureInjection, ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT)) {
      throw Object.assign(new Error("FAULT_DRILL_ROUTE_UPSERT_UNEXPECTED_SUCCESS"), {
        code: "FAULT_DRILL_ROUTE_UPSERT_UNEXPECTED_SUCCESS",
        expectedFailure: false
      });
    }
    completedStages.push(ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT);
    context.proxyRoute = route;
  } catch (error) {
    return failure(ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT, error, context, completedStages, {
      expectedFailure: hasFailureInjection(failureInjection, ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT)
    });
  }

  try {
    const retireResult = await clients.oldServer.retireTransferredRoom({
      rolloutEpoch: request.rolloutEpoch,
      roomId: request.roomId,
      checksum: importResult.checksum
    });
    assertOkResponse(retireResult, "OLD_RETIRE_FAILED");
    completedStages.push(ROOM_TRANSFER_STAGE.OLD_RETIRE);
    context.retired = retireResult;
  } catch (error) {
    return failure(ROOM_TRANSFER_STAGE.OLD_RETIRE, error, context, completedStages);
  }

  return success(context, completedStages);
}

export class GameServerTransferClient {
  constructor(options) {
    if (!options.host || !options.port) {
      throw new Error("game-server admin client requires a resolved game-server.admin host and port");
    }
    this.host = options.host;
    this.port = options.port;
    this.token = options.token || "";
    this.timeoutMs = options.timeoutMs || DEFAULT_TIMEOUT_MS;
    this.maxBodyLen = options.maxBodyLen || DEFAULT_MAX_BODY_LEN;
    this.authMessageType = options.authMessageType || MESSAGE_TYPE.ADMIN_AUTH_REQ;
    this.nextSeq = nextSeqFactory();
  }

  async freezeRoomForTransfer({ rolloutEpoch, roomId }) {
    const body = encodeFreezeRoomForTransferReq(rolloutEpoch, roomId);
    const response = await this.sendRequest(
      MESSAGE_TYPE.FREEZE_ROOM_FOR_TRANSFER_REQ,
      body,
      MESSAGE_TYPE.FREEZE_ROOM_FOR_TRANSFER_RES
    );
    return decodeFreezeRoomForTransferRes(response.body);
  }

  async exportRoomTransfer({ rolloutEpoch, roomId }) {
    const body = encodeExportRoomTransferReq(rolloutEpoch, roomId);
    const response = await this.sendRequest(
      MESSAGE_TYPE.EXPORT_ROOM_TRANSFER_REQ,
      body,
      MESSAGE_TYPE.EXPORT_ROOM_TRANSFER_RES
    );
    return decodeExportRoomTransferRes(response.body);
  }

  async importRoomTransfer({ payloadRaw }) {
    const body = encodeImportRoomTransferReq(payloadRaw);
    const response = await this.sendRequest(
      MESSAGE_TYPE.IMPORT_ROOM_TRANSFER_REQ,
      body,
      MESSAGE_TYPE.IMPORT_ROOM_TRANSFER_RES
    );
    return decodeImportRoomTransferRes(response.body);
  }

  async confirmRoomOwnership(request) {
    const body = encodeConfirmRoomOwnershipReq(request);
    const response = await this.sendRequest(
      MESSAGE_TYPE.CONFIRM_ROOM_OWNERSHIP_REQ,
      body,
      MESSAGE_TYPE.CONFIRM_ROOM_OWNERSHIP_RES
    );
    return decodeConfirmRoomOwnershipRes(response.body);
  }

  async retireTransferredRoom({ rolloutEpoch, roomId, checksum }) {
    const body = encodeRetireTransferredRoomReq(rolloutEpoch, roomId, checksum);
    const response = await this.sendRequest(
      MESSAGE_TYPE.RETIRE_TRANSFERRED_ROOM_REQ,
      body,
      MESSAGE_TYPE.RETIRE_TRANSFERRED_ROOM_RES
    );
    return decodeRetireTransferredRoomRes(response.body);
  }

  async triggerServerRedirect(request) {
    const body = encodeTriggerServerRedirectReq(request);
    const response = await this.sendRequest(
      MESSAGE_TYPE.TRIGGER_SERVER_REDIRECT_REQ,
      body,
      MESSAGE_TYPE.TRIGGER_SERVER_REDIRECT_RES
    );
    return decodeTriggerServerRedirectRes(response.body);
  }

  async sendRequest(messageType, body, expectedMessageType) {
    return await new Promise((resolve, reject) => {
      const socket = net.createConnection({ host: this.host, port: this.port });
      let buffer = Buffer.alloc(0);
      let done = false;

      const timer = setTimeout(() => {
        finish(new Error(`timed out waiting for game-server admin response after ${this.timeoutMs}ms`));
      }, this.timeoutMs);

      const finish = (error, value) => {
        if (done) return;
        done = true;
        clearTimeout(timer);
        socket.removeAllListeners();
        socket.end();
        socket.destroy();
        if (error) reject(error);
        else resolve(value);
      };

      socket.on("connect", () => {
        const authPacket = encodePacket(this.authMessageType, 0, Buffer.from(this.token, "utf8"));
        const packet = encodePacket(messageType, this.nextSeq(), body);
        socket.write(Buffer.concat([authPacket, packet]), (error) => {
          if (error) {
            finish(error);
          }
        });
      });

      socket.on("error", finish);
      socket.on("data", (chunk) => {
        buffer = Buffer.concat([buffer, chunk]);
        if (buffer.length < HEADER_LEN) return;

        const bodyLen = buffer.readUInt32BE(10);
        if (bodyLen > this.maxBodyLen) {
          finish(Object.assign(new Error("ADMIN_RESPONSE_BODY_TOO_LARGE"), {
            code: "ADMIN_RESPONSE_BODY_TOO_LARGE"
          }));
          return;
        }

        const packetLen = HEADER_LEN + bodyLen;
        if (buffer.length < packetLen) return;

        try {
          const response = decodePacket(buffer.subarray(0, packetLen));
          if (response.messageType === MESSAGE_TYPE.ERROR_RES) {
            const errorResponse = decodeErrorRes(response.body);
            finish(Object.assign(new Error(errorResponse.message || errorResponse.errorCode), {
              code: errorResponse.errorCode || "GAME_SERVER_ERROR"
            }));
            return;
          }
          if (response.messageType !== expectedMessageType) {
            finish(Object.assign(new Error(`unexpected response type ${response.messageType}`), {
              code: "UNEXPECTED_RESPONSE"
            }));
            return;
          }
          finish(null, response);
        } catch (error) {
          finish(error);
        }
      });
    });
  }
}

export class ProxyAdminClient {
  constructor(options) {
    if (!options.baseUrl) {
      throw new Error("proxy admin client requires a resolved game-proxy.admin URL");
    }
    this.baseUrl = options.baseUrl.replace(/\/+$/, "");
    this.token = options.token || "";
    this.actor = options.actor || "";
    this.timeoutMs = options.timeoutMs || DEFAULT_TIMEOUT_MS;
  }

  async getRoomRoute(roomId) {
    const data = await this.requestJson("/room-routes");
    return (data.routes || []).find((route) => route.room_id === roomId || route.roomId === roomId) || null;
  }

  async upsertRoomRoute(route) {
    const params = new URLSearchParams();
    params.set("room_id", route.roomId);
    params.set("owner_server_id", route.ownerServerId);
    params.set("migration_state", route.migrationState);
    params.set("member_count", String(route.memberCount ?? 0));
    params.set("online_member_count", String(route.onlineMemberCount ?? 0));
    if (route.emptySinceMs !== undefined && route.emptySinceMs !== null) {
      params.set("empty_since_ms", String(route.emptySinceMs));
    }
    params.set("room_version", String(route.roomVersion));
    params.set("rollout_epoch", route.rolloutEpoch);
    params.set("last_transfer_checksum", route.lastTransferChecksum);
    params.set("expected_room_version", String(route.expectedRoomVersion));
    params.set("expected_last_transfer_checksum", route.expectedLastTransferChecksum || "");

    await this.requestText(`/room-route/upsert?${params.toString()}`, { method: "POST" });
    return { ok: true };
  }

  async upsertCharacterRoute(route) {
    const params = new URLSearchParams();
    params.set("character_id", route.characterId);
    if (route.currentRoomId) {
      params.set("current_room_id", route.currentRoomId);
    }
    if (route.preferredServerId) {
      params.set("preferred_server_id", route.preferredServerId);
    }
    params.set("rollout_epoch", route.rolloutEpoch || "");

    await this.requestText(`/character-route/upsert?${params.toString()}`, { method: "POST" });
    return { ok: true };
  }

  async requestJson(path, init = {}) {
    const text = await this.requestText(path, init);
    return JSON.parse(text);
  }

  async requestText(path, init = {}) {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), this.timeoutMs);
    try {
      const response = await fetch(`${this.baseUrl}${path}`, {
        ...init,
        signal: controller.signal,
        headers: {
          authorization: `Bearer ${this.token}`,
          ...(this.actor ? { "x-admin-actor": this.actor } : {}),
          ...(init.headers || {})
        }
      });
      const text = await response.text();
      if (!response.ok) {
        throw Object.assign(new Error(text || `proxy admin HTTP ${response.status}`), {
          code: "PROXY_ADMIN_ERROR",
          status: response.status
        });
      }
      return text;
    } finally {
      clearTimeout(timer);
    }
  }
}

export function encodeRoomTransferPayloadForTest(fields) {
  const buffers = [
    encodeStringField(1, fields.rolloutEpoch || "rollout-test"),
    encodeStringField(2, fields.roomId || "room-test"),
    encodeUInt64Field(3, fields.roomVersion ?? 1),
    encodeStringField(17, fields.checksum || "checksum-test")
  ];
  if (fields.snapshotRaw) {
    buffers.push(encodeMessageField(9, fields.snapshotRaw));
  }
  if (fields.ok !== undefined) {
    buffers.push(encodeBoolField(99, fields.ok));
  }
  return Buffer.concat(buffers);
}
