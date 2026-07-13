import crypto from "node:crypto";
import net from "node:net";

import { discoveryLogContext, recordDiscoveryMetric } from "../../../packages/service-registry/node/registry-schema.js";
import { discoverGameServerAdminEndpoints } from "./registry-client.js";
import { log } from "./logger.js";

const MAGIC = 0xcafe;
const VERSION = 1;
const HEADER_LEN = 14;

const MESSAGE_TYPE = {
  ADMIN_AUTH_REQ: 2099,
  GM_SEND_ITEM_REQ: 3003,
  GM_SEND_ITEM_RES: 3004,
  ERROR_RES: 9000
};
const ACTOR_PATTERN = /^[A-Za-z0-9._@-]{1,128}$/;
let nextSeqValue = 1;

function encodePacket(messageType, seq, body) {
  const header = Buffer.alloc(HEADER_LEN);
  header.writeUInt16BE(MAGIC, 0);
  header.writeUInt8(VERSION, 2);
  header.writeUInt8(0, 3);
  header.writeUInt16BE(messageType, 4);
  header.writeUInt32BE(seq, 6);
  header.writeUInt32BE(body.length, 10);
  return Buffer.concat([header, body]);
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
  if (flags !== 0) throw createAdminError("INVALID_FLAGS", "unsupported packet flags");
  if (buffer.length !== HEADER_LEN + bodyLen) throw new Error("INVALID_PACKET_LENGTH");

  return { messageType, seq, body: buffer.subarray(HEADER_LEN) };
}

function readVarint(bytes, start) {
  let value = 0n;
  let shift = 0n;
  let offset = start;
  while (offset < bytes.length && shift <= 63n) {
    const byte = bytes[offset++];
    value |= BigInt(byte & 0x7f) << shift;
    if ((byte & 0x80) === 0) {
      return { value, offset };
    }
    shift += 7n;
  }
  throw createAdminError("INVALID_PROTOBUF_RESPONSE", "invalid protobuf varint");
}

function decodeProtobufFields(body) {
  const bytes = body instanceof Uint8Array ? body : new Uint8Array(body);
  const fields = new Map();
  let offset = 0;

  while (offset < bytes.length) {
    const key = readVarint(bytes, offset);
    offset = key.offset;
    const fieldNumber = Number(key.value >> 3n);
    const wireType = Number(key.value & 0x07n);
    if (fieldNumber <= 0) {
      throw createAdminError("INVALID_PROTOBUF_RESPONSE", "invalid protobuf field number");
    }

    let value;
    if (wireType === 0) {
      const decoded = readVarint(bytes, offset);
      value = decoded.value;
      offset = decoded.offset;
    } else if (wireType === 2) {
      const decodedLength = readVarint(bytes, offset);
      const length = Number(decodedLength.value);
      offset = decodedLength.offset;
      if (!Number.isSafeInteger(length) || length < 0 || offset + length > bytes.length) {
        throw createAdminError("INVALID_PROTOBUF_RESPONSE", "invalid protobuf field length");
      }
      value = bytes.subarray(offset, offset + length);
      offset += length;
    } else if (wireType === 1) {
      if (offset + 8 > bytes.length) throw createAdminError("INVALID_PROTOBUF_RESPONSE");
      value = bytes.subarray(offset, offset + 8);
      offset += 8;
    } else if (wireType === 5) {
      if (offset + 4 > bytes.length) throw createAdminError("INVALID_PROTOBUF_RESPONSE");
      value = bytes.subarray(offset, offset + 4);
      offset += 4;
    } else {
      throw createAdminError("INVALID_PROTOBUF_RESPONSE", `unsupported protobuf wire type ${wireType}`);
    }

    const values = fields.get(fieldNumber) || [];
    values.push(value);
    fields.set(fieldNumber, values);
  }

  return fields;
}

function protobufString(fields, fieldNumber) {
  const value = fields.get(fieldNumber)?.at(-1);
  return value instanceof Uint8Array ? Buffer.from(value).toString("utf8") : "";
}

function protobufBool(fields, fieldNumber) {
  return fields.get(fieldNumber)?.at(-1) === 1n;
}

function decodeError(body) {
  const fields = decodeProtobufFields(body);
  return {
    errorCode: protobufString(fields, 1),
    message: protobufString(fields, 2)
  };
}

function nextSeq() {
  const seq = nextSeqValue >>> 0;
  nextSeqValue = (nextSeqValue + 1) >>> 0;
  if (nextSeqValue === 0) {
    nextSeqValue = 1;
  }
  return seq;
}

function normalizeGameAdminActor(actor) {
  if (actor === undefined || actor === null) {
    return null;
  }

  const normalized = String(actor).trim();
  return ACTOR_PATTERN.test(normalized) ? normalized : null;
}

function normalizeServiceActorCandidate(actor) {
  if (actor === undefined || actor === null) {
    return null;
  }

  const normalized = String(actor)
    .trim()
    .replace(/[^A-Za-z0-9._@-]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 128);
  return ACTOR_PATTERN.test(normalized) ? normalized : null;
}

function getDefaultGameAdminActor(config = {}) {
  return (
    normalizeGameAdminActor(config.gameAdminActor) ||
    normalizeServiceActorCandidate(config.serviceInstanceId) ||
    normalizeServiceActorCandidate(config.serviceName) ||
    "mail-service"
  );
}

function buildAdminAuthBody(config, actor) {
  const token = config.gameAdminToken || "";
  const normalizedActor = normalizeGameAdminActor(actor);
  if (!normalizedActor) {
    return Buffer.from(token, "utf8");
  }

  return Buffer.from(JSON.stringify({ token, actor: normalizedActor }), "utf8");
}

function normalizeGrantItems(attachments) {
  const merged = new Map();
  for (const item of attachments || []) {
    const itemId = Number(item?.itemId);
    const count = Number(item?.count);
    const binded = item?.binded === true;
    if (!Number.isInteger(itemId) || itemId <= 0 || !Number.isInteger(count) || count <= 0) {
      throw createAdminError("INVALID_GRANT_ITEMS", "invalid grant attachment item");
    }
    const key = `${itemId}:${binded ? 1 : 0}`;
    const nextCount = (merged.get(key)?.count || 0) + count;
    if (!Number.isSafeInteger(nextCount) || nextCount > 0xffffffff) {
      throw createAdminError("INVALID_GRANT_ITEMS", "grant attachment count overflow");
    }
    merged.set(key, { itemId, count: nextCount, binded });
  }
  if (merged.size === 0) {
    throw createAdminError("INVALID_GRANT_ITEMS", "grant attachments are empty");
  }
  return [...merged.values()].sort((left, right) =>
    left.itemId - right.itemId || Number(left.binded) - Number(right.binded)
  );
}

function mailIdFromRequestId(requestId) {
  const prefix = "mail_claim:";
  return typeof requestId === "string" && requestId.startsWith(prefix)
    ? requestId.slice(prefix.length)
    : "";
}

function computeGrantRequestFingerprint(mailId, characterId, attachments) {
  const items = normalizeGrantItems(attachments);
  const canonical = JSON.stringify({
    mail_id: mailId,
    character_id: characterId,
    source: "mail-claim",
    items: items.map((item) => ({
      item_id: item.itemId,
      count: item.count,
      binded: item.binded
    }))
  });
  return `sha256:${crypto.createHash("sha256").update(canonical).digest("hex")}`;
}

function buildGrantMailAttachmentsPayload(characterId, requestId, attachments, reason = "", options = {}) {
  const mailId = options.mailId || mailIdFromRequestId(requestId);
  const normalizedItems = normalizeGrantItems(attachments);
  const requestFingerprint = options.requestFingerprint ||
    computeGrantRequestFingerprint(mailId, characterId, normalizedItems);
  const traceId = options.traceId || crypto.randomBytes(16).toString("hex");
  const routeGeneration = options.routeGeneration || "";
  const routeToken = options.routeToken || "";
  return Buffer.from(JSON.stringify({
    requestId,
    mailId,
    characterId,
    items: normalizedItems,
    requestFingerprint,
    source: "mail-claim",
    reason,
    traceId,
    routeGeneration,
    routeToken
  }));
}

function createAdminError(code, message = code) {
  const error = new Error(message);
  error.code = code;
  return error;
}

async function sendRequest(config, messageType, payload, expectedType, options = {}) {
  const endpoint = options.endpoint || {
    host: config.gameServerAdminHost,
    port: config.gameServerAdminPort
  };
  const socket = net.createConnection({
    host: endpoint.host,
    port: endpoint.port
  });

  let requestPhase = "connect";
  let requestWritten = false;
  try {
    await onceConnected(socket, config.gameAdminConnectTimeoutMs);
    requestPhase = "auth_write";
    await onceWritten(
      socket,
      encodePacket(MESSAGE_TYPE.ADMIN_AUTH_REQ, 0, buildAdminAuthBody(config, options.actor)),
      config.gameAdminWriteTimeoutMs
    );
    requestPhase = "request_write";
    const requestSeq = nextSeq();
    await onceWritten(socket, encodePacket(messageType, requestSeq, payload), config.gameAdminWriteTimeoutMs);
    requestWritten = true;
    requestPhase = "response_read";

    const responseBuffer = await readSinglePacket(
      socket,
      config.gameAdminReadTimeoutMs,
      config.gameAdminMaxResponseBytes
    );
    const response = decodePacket(responseBuffer);

    if (response.seq !== requestSeq) {
      throw createAdminError("UNEXPECTED_RESPONSE_SEQUENCE", `unexpected response sequence ${response.seq}`);
    }

    if (response.messageType === MESSAGE_TYPE.ERROR_RES) {
      const error = decodeError(response.body);
      const err = new Error(error.message || error.errorCode || "game-server admin error");
      err.code = error.errorCode || "GAME_SERVER_ADMIN_ERROR";
      throw err;
    }

    if (response.messageType !== expectedType) {
      const err = new Error(`unexpected response type ${response.messageType}`);
      err.code = "UNEXPECTED_RESPONSE";
      throw err;
    }

    return response.body;
  } catch (error) {
    const requestError = normalizeRequestError(error);
    requestError.requestPhase ||= requestPhase;
    requestError.requestWritten ??= requestWritten;
    if (!requestError.requestWritten && requestError.requestPhase === "connect") {
      throw markRouteUnavailable(requestError, {
        code: "GAME_ADMIN_CONNECT_FAILED",
        phase: "connect"
      });
    }
    if (
      requestError.requestWritten &&
      requestError.requestPhase === "response_read" &&
      isResponseTransportFailure(requestError)
    ) {
      requestError.errorCategory ||= "RESULT_UNKNOWN";
      requestError.resultState ||= "unknown";
      requestError.retryable ??= true;
    }
    throw requestError;
  } finally {
    socket.end();
    socket.destroy();
  }
}

function onceConnected(socket, timeoutMs = 3000) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      cleanup();
      socket.destroy();
      reject(createAdminError("GAME_ADMIN_CONNECT_TIMEOUT", "game-server admin connect timeout"));
    }, timeoutMs);

    const cleanup = () => {
      clearTimeout(timer);
      socket.off("connect", onConnect);
      socket.off("error", onError);
    };

    const onConnect = () => {
      cleanup();
      resolve();
    };

    const onError = (error) => {
      cleanup();
      reject(error);
    };

    socket.once("connect", onConnect);
    socket.once("error", onError);
  });
}

function onceWritten(socket, data, timeoutMs = 3000) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      cleanup();
      socket.destroy();
      reject(createAdminError("GAME_ADMIN_WRITE_TIMEOUT", "game-server admin write timeout"));
    }, timeoutMs);

    const cleanup = () => {
      clearTimeout(timer);
      socket.off("error", onError);
      socket.off("close", onClose);
    };

    const onError = (error) => {
      cleanup();
      reject(error);
    };

    const onClose = () => {
      cleanup();
      reject(createAdminError("GAME_ADMIN_CONNECTION_CLOSED", "game-server admin connection closed"));
    };

    socket.once("error", onError);
    socket.once("close", onClose);

    socket.write(data, (error) => {
      cleanup();
      if (error) {
        reject(error);
        return;
      }
      resolve();
    });
  });
}

function readSinglePacket(socket, timeoutMs = 3000, maxResponseBytes = 1024 * 1024) {
  return new Promise((resolve, reject) => {
    let buffer = Buffer.alloc(0);
    const timer = setTimeout(() => {
      cleanup();
      socket.destroy();
      reject(createAdminError("GAME_ADMIN_READ_TIMEOUT", "game-server admin read timeout"));
    }, timeoutMs);

    const onData = (chunk) => {
      buffer = Buffer.concat([buffer, chunk]);
      if (buffer.length > maxResponseBytes) {
        cleanup();
        socket.destroy();
        reject(createAdminError("GAME_ADMIN_RESPONSE_TOO_LARGE", "game-server admin response too large"));
        return;
      }

      if (buffer.length < HEADER_LEN) {
        return;
      }

      const bodyLen = buffer.readUInt32BE(10);
      const packetLen = HEADER_LEN + bodyLen;
      if (packetLen > maxResponseBytes) {
        cleanup();
        socket.destroy();
        reject(createAdminError("GAME_ADMIN_RESPONSE_TOO_LARGE", "game-server admin response too large"));
        return;
      }

      if (buffer.length < packetLen) {
        return;
      }

      cleanup();
      resolve(buffer.subarray(0, packetLen));
    };

    const onError = (error) => {
      cleanup();
      reject(error);
    };

    const onClose = () => {
      cleanup();
      reject(createAdminError(
        "GAME_ADMIN_CONNECTION_CLOSED",
        "admin connection closed before response"
      ));
    };

    const cleanup = () => {
      clearTimeout(timer);
      socket.off("data", onData);
      socket.off("error", onError);
      socket.off("close", onClose);
    };

    socket.on("data", onData);
    socket.once("error", onError);
    socket.once("close", onClose);
  });
}

export class GameAdminClient {
  constructor(config, redis = null) {
    this.config = config;
    this.redis = redis;
  }

  async listAdminEndpoints() {
    if (!this.config.registryDiscoveryEnabled) {
      if (this.config.registryDiscoveryRequired || !this.config.localDiscoveryFallbackEnabled) {
        logDiscovery("warn", "registry.discovery_fallback_forbidden", {
          source: "registry",
          reason: this.config.registryDiscoveryRequired ? "registry_disabled" : "fallback_forbidden"
        });
        throw createAdminError(
          "SERVICE_DISCOVERY_REQUIRED",
          "Required registry discovery failed: REGISTRY_ENABLED=false"
        );
      }

      logDiscovery("warn", "registry.discovery_fallback", {
        source: "fallback",
        reason: "fallback_used",
        instanceId: "local-fallback"
      });
      return [
        {
          service: "game-server",
          instanceId: "local-fallback",
          instance_id: "local-fallback",
          endpointName: "admin",
          endpoint_name: "admin",
          protocol: "tcp",
          host: this.config.gameServerAdminHost,
          port: this.config.gameServerAdminPort,
          healthy: true,
          fallback: true,
          source: "fallback",
          reason: "fallback_used"
        }
      ];
    }

    if (!this.redis) {
      throw createAdminError(
        "SERVICE_DISCOVERY_UNAVAILABLE",
        "Redis client is required for game-server admin discovery"
      );
    }

    return discoverGameServerAdminEndpoints(this.redis, this.config.registryKeyPrefix || "");
  }

  async resolveAdminEndpoint(options = {}) {
    if (options.endpoint) {
      return options.endpoint;
    }

    const endpoints = await this.listAdminEndpoints();
    if (endpoints.length === 0) {
      throw createAdminError(
        "GAME_SERVER_ADMIN_ENDPOINT_NOT_FOUND",
        "game-server admin endpoint not found in service registry"
      );
    }

    const targetInstanceId = normalizeTargetInstanceId(options.targetInstanceId || options.target_instance_id);
    if (targetInstanceId) {
      const selected = endpoints.find((endpoint) => endpoint.instanceId === targetInstanceId);
      if (!selected) {
        throw createAdminError(
          "GAME_SERVER_ADMIN_TARGET_NOT_FOUND",
          `game-server admin target instance not found: ${targetInstanceId}`
        );
      }
      return selected;
    }

    if (endpoints.length > 1 && options.requireExplicitTarget) {
      throw createAdminError(
        "GAME_SERVER_ADMIN_TARGET_REQUIRED",
        "multiple game-server admin endpoints are available; targetInstanceId is required"
      );
    }

    return endpoints[0];
  }

  async resolveGrantEndpoint(characterId, options = {}) {
    let endpoints;
    try {
      endpoints = await this.listAdminEndpoints();
    } catch (error) {
      throw markRouteUnavailable(error, {
        code: "SERVICE_DISCOVERY_UNAVAILABLE",
        phase: "discovery"
      });
    }
    if (endpoints.length === 0) {
      throw createRouteError(
        "MAIL_CLAIM_ROUTE_UNAVAILABLE",
        "no healthy game-server admin endpoint is available"
      );
    }

    const requestedTarget = normalizeTargetInstanceId(
      options.targetInstanceId || options.target_instance_id
    );
    if (requestedTarget) {
      if (!this.config.localDiscoveryFallbackEnabled || this.config.registryDiscoveryRequired) {
        throw createAdminError(
          "CLIENT_TARGET_INSTANCE_FORBIDDEN",
          "targetInstanceId is only available for local development diagnostics"
        );
      }
    }

    const route = await this.readOnlineRoute(characterId);
    const fixedLocalEndpoint =
      !this.config.registryDiscoveryEnabled &&
      !this.config.registryDiscoveryRequired &&
      this.config.localDiscoveryFallbackEnabled &&
      endpoints.length === 1 &&
      endpoints[0].fallback === true
        ? endpoints[0]
        : null;

    if (requestedTarget) {
      if (!route || route.instanceId !== requestedTarget) {
        throw createRouteError(
          "MAIL_CLAIM_ROUTE_TARGET_NOT_FOUND",
          "debug target is not the current authoritative route owner"
        );
      }

      if (fixedLocalEndpoint) {
        return {
          endpoint: {
            ...fixedLocalEndpoint,
            instanceId: route.instanceId,
            instance_id: route.instanceId
          },
          route,
          source: "local-debug-fallback"
        };
      }

      const endpoint = endpoints.find((candidate) => candidate.instanceId === requestedTarget);
      if (!endpoint) {
        throw createRouteError(
          "MAIL_CLAIM_ROUTE_TARGET_NOT_FOUND",
          `debug target instance is not present in service discovery: ${requestedTarget}`
        );
      }
      return { endpoint, route, source: "local-debug" };
    }

    if (!route) {
      throw createRouteError(
        "MAIL_CLAIM_ROUTE_UNAVAILABLE",
        "authoritative online character route is unavailable"
      );
    }

    if (fixedLocalEndpoint) {
      return {
        endpoint: {
          ...fixedLocalEndpoint,
          instanceId: route.instanceId,
          instance_id: route.instanceId
        },
        route,
        source: "online-route-local-fallback"
      };
    }

    const endpoint = endpoints.find((candidate) => candidate.instanceId === route.instanceId);
    if (!endpoint) {
      throw createRouteError(
        "MAIL_CLAIM_ROUTE_STALE",
        "authoritative route instance is absent from service discovery"
      );
    }
    return { endpoint, route, source: "online-route" };
  }

  async readOnlineRoute(characterId) {
    if (!this.redis || typeof this.redis.get !== "function") {
      throw createRouteError(
        "MAIL_CLAIM_ROUTE_BACKEND_UNAVAILABLE",
        "Redis client is required for authoritative character routing"
      );
    }

    const key = gameOnlineRouteKey(this.config.redisKeyPrefix || "", characterId);
    let raw;
    try {
      raw = await this.redis.get(key);
    } catch (error) {
      const routeError = createRouteError(
        "MAIL_CLAIM_ROUTE_BACKEND_UNAVAILABLE",
        "authoritative character route lookup failed"
      );
      routeError.cause = error;
      throw routeError;
    }
    if (!raw) return null;

    try {
      const value = JSON.parse(raw);
      if (
        value?.version !== 2 ||
        value?.character_id !== characterId ||
        typeof value?.instance_id !== "string" ||
        !value.instance_id.trim() ||
        typeof value?.session_id !== "string" ||
        !/^\d+$/.test(value.session_id) ||
        typeof value?.authority_generation !== "string" ||
        !/^[1-9]\d*$/.test(value.authority_generation) ||
        typeof value?.authority_token !== "string" ||
        !/^[0-9a-f]{64}$/.test(value.authority_token)
      ) {
        throw new Error("invalid route fields");
      }
      return {
        characterId: value.character_id,
        instanceId: value.instance_id,
        sessionId: value.session_id,
        authorityGeneration: value.authority_generation,
        authorityToken: value.authority_token
      };
    } catch (error) {
      const routeError = createRouteError(
        "MAIL_CLAIM_ROUTE_INVALID",
        "authoritative character route payload is invalid"
      );
      routeError.cause = error;
      throw routeError;
    }
  }

  async grantMailAttachments(characterId, mailId, attachments, reason = "", options = {}) {
    const requestId = mailId;
    const actualMailId = mailIdFromRequestId(requestId);
    const normalizedItems = normalizeGrantItems(attachments);
    const requestFingerprint = computeGrantRequestFingerprint(
      actualMailId,
      characterId,
      normalizedItems
    );
    if (options.requestFingerprint && options.requestFingerprint !== requestFingerprint) {
      throw createAdminError(
        "GRANT_REQUEST_FINGERPRINT_MISMATCH",
        "persisted attachment fingerprint does not match the canonical grant request"
      );
    }
    const traceId = options.traceId || crypto.randomBytes(16).toString("hex");
    let lastError;
    for (let attempt = 0; attempt < 2; attempt += 1) {
      let resolved;
      try {
        resolved = await this.resolveGrantEndpoint(characterId, options);
        const payload = buildGrantMailAttachmentsPayload(
          characterId,
          requestId,
          normalizedItems,
          reason,
          {
            mailId: actualMailId,
            requestFingerprint,
            traceId,
            routeGeneration: resolved.route.authorityGeneration,
            routeToken: resolved.route.authorityToken
          }
        );
        const responseBody = await sendRequest(
          this.config,
          MESSAGE_TYPE.GM_SEND_ITEM_REQ,
          payload,
          MESSAGE_TYPE.GM_SEND_ITEM_RES,
          {
            endpoint: resolved.endpoint,
            actor: normalizeGameAdminActor(options.actor) || getDefaultGameAdminActor(this.config)
          }
        );
        let result;
        try {
          result = decodeGrantItemsResponse(responseBody);
          validateGrantItemsResponse(result, {
            requestId,
            requestFingerprint,
            traceId,
            characterId,
            items: normalizedItems
          });
        } catch (error) {
          throw markGrantResponseValidationFailure(error);
        }

        logSafely("info", "mail.claim_game_admin_succeeded", {
          instanceId: resolved.endpoint.instanceId,
          requestId,
          traceId,
          applied: result.applied
        });

        return {
          ...result,
          instanceId: resolved.endpoint.instanceId,
          requestId,
          requestFingerprint,
          traceId
        };
      } catch (error) {
        lastError = error;
        const instanceId = resolved?.endpoint?.instanceId || "";
        if (error && typeof error === "object") {
          error.requestId ||= requestId;
          error.traceId ||= traceId;
          error.instanceId ||= instanceId;
        }
        logSafely("warn", "mail.claim_game_admin_failed", {
          instanceId,
          requestId,
          traceId,
          errorCode: error?.code || "GAME_SERVER_ADMIN_ERROR",
          requestPhase: error?.requestPhase || "route"
        });
        if (attempt === 0 && shouldRediscoverGrantRoute(error)) {
          continue;
        }
        throw error;
      }
    }

    throw lastError;
  }
}

function gameOnlineRouteKey(prefix, characterId) {
  const digest = crypto.createHash("sha256").update(String(characterId)).digest("hex");
  return `${prefix || ""}game:online-route:${digest}`;
}

function createRouteError(code, message) {
  return markRouteUnavailable(createAdminError(code, message), {
    code,
    phase: "route"
  });
}

function normalizeRequestError(error, fallbackCode = "GAME_SERVER_ADMIN_ERROR") {
  if (error && typeof error === "object") {
    error.code ||= fallbackCode;
    return error;
  }
  const normalized = createAdminError(fallbackCode, String(error || fallbackCode));
  normalized.cause = error;
  return normalized;
}

function markRouteUnavailable(error, { code, phase } = {}) {
  const routeError = normalizeRequestError(error, code || "MAIL_CLAIM_ROUTE_UNAVAILABLE");
  routeError.errorCategory = "ROUTE_UNAVAILABLE";
  routeError.resultState = "not_applied";
  routeError.retryable = true;
  routeError.requestWritten ??= false;
  routeError.requestPhase ||= phase || "route";
  return routeError;
}

function markGrantResponseValidationFailure(error) {
  const validationError = normalizeRequestError(error, "INVALID_GRANT_RESPONSE");
  validationError.requestWritten = true;
  validationError.requestPhase ||= "response_validation";
  const explicitNotApplied =
    validationError.structuredGrantFailure === true &&
    validationError.resultState === "not_applied" &&
    validationError.errorCategory &&
    validationError.errorCategory !== "RESULT_UNKNOWN";
  if (!explicitNotApplied) {
    validationError.errorCategory = "RESULT_UNKNOWN";
    validationError.resultState = "unknown";
    validationError.retryable = true;
  }
  return validationError;
}

function isResponseTransportFailure(error) {
  return new Set([
    "GAME_ADMIN_READ_TIMEOUT",
    "GAME_ADMIN_CONNECTION_CLOSED",
    "ECONNRESET",
    "ECONNABORTED",
    "ETIMEDOUT",
    "EPIPE"
  ]).has(error?.code);
}

function decodeGrantItemsResponse(body) {
  const fields = decodeProtobufFields(body);
  const summaryBytes = fields.get(9)?.at(-1);
  let resultSummary = null;
  if (summaryBytes instanceof Uint8Array) {
    const summaryFields = decodeProtobufFields(summaryBytes);
    resultSummary = {
      characterId: protobufString(summaryFields, 1),
      source: protobufString(summaryFields, 2),
      items: (summaryFields.get(3) || []).map((itemBytes) => {
        const itemFields = decodeProtobufFields(itemBytes);
        return {
          itemId: Number(itemFields.get(1)?.at(-1) || 0n),
          count: Number(itemFields.get(2)?.at(-1) || 0n),
          binded: protobufBool(itemFields, 3)
        };
      })
    };
  }

  return {
    ok: protobufBool(fields, 1),
    errorCode: protobufString(fields, 2),
    applied: protobufBool(fields, 3),
    requestId: protobufString(fields, 4),
    requestFingerprint: protobufString(fields, 5),
    errorCategory: protobufString(fields, 6),
    resultState: protobufString(fields, 7),
    retryable: protobufBool(fields, 8),
    resultSummary,
    traceId: protobufString(fields, 10)
  };
}

function validateGrantItemsResponse(result, expected) {
  if (
    result.requestId !== expected.requestId ||
    result.requestFingerprint !== expected.requestFingerprint ||
    result.traceId !== expected.traceId
  ) {
    throw createAdminError(
      "GRANT_RESPONSE_CONTRACT_MISMATCH",
      "game-server grant response does not match request identity"
    );
  }

  if (!result.ok) {
    const error = createAdminError(
      result.errorCode || "GAME_SERVER_GRANT_REJECTED",
      "game-server rejected attachment grant"
    );
    error.errorCategory = result.errorCategory || "RETRYABLE_FAILURE";
    error.resultState = result.resultState || "not_applied";
    error.retryable = result.retryable;
    error.requestWritten = true;
    error.structuredGrantFailure = Boolean(result.errorCategory && result.resultState === "not_applied");
    throw error;
  }

  if (
    result.resultState !== "applied" ||
    !result.resultSummary ||
    result.resultSummary.characterId !== expected.characterId ||
    result.resultSummary.source !== "mail-claim" ||
    JSON.stringify(normalizeGrantItems(result.resultSummary.items)) !==
      JSON.stringify(normalizeGrantItems(expected.items))
  ) {
    throw createAdminError(
      "GRANT_RESPONSE_CONTRACT_MISMATCH",
      "game-server grant response lacks matching applied result evidence"
    );
  }
}

function shouldRediscoverGrantRoute(error) {
  if (error?.errorCategory === "ROUTE_UNAVAILABLE" && error?.resultState === "not_applied") {
    return true;
  }
  return error?.requestWritten === false && error?.requestPhase === "connect";
}

function logSafely(level, event, context) {
  try {
    log(level, event, context);
  } catch {
    // Focused tests may instantiate the client before logger bootstrap.
  }
}

function normalizeTargetInstanceId(value) {
  if (value === undefined || value === null) {
    return "";
  }
  return String(value).trim();
}

function logDiscovery(level, event, context = {}) {
  if (!context.__discoveryMetricRecorded) {
    recordDiscoveryMetric({
      serviceName: "game-server",
      endpointName: "admin",
      ...context
    });
  }

  try {
    log(level, event, discoveryLogContext({
      serviceName: "game-server",
      endpointName: "admin",
      ...context
    }));
  } catch {
    // Focused tests may instantiate the client before logger bootstrap.
  }
}

export {
  MESSAGE_TYPE,
  buildAdminAuthBody,
  buildGrantMailAttachmentsPayload,
  computeGrantRequestFingerprint,
  createAdminError,
  decodeGrantItemsResponse,
  gameOnlineRouteKey,
  getDefaultGameAdminActor,
  normalizeGrantItems,
  normalizeGameAdminActor,
  normalizeServiceActorCandidate,
  sendRequest,
  validateGrantItemsResponse
};
