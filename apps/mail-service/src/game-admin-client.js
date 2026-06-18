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
  const messageType = buffer.readUInt16BE(4);
  const seq = buffer.readUInt32BE(6);
  const bodyLen = buffer.readUInt32BE(10);

  if (magic !== MAGIC) throw new Error("INVALID_MAGIC");
  if (version !== VERSION) throw new Error("INVALID_VERSION");
  if (buffer.length !== HEADER_LEN + bodyLen) throw new Error("INVALID_PACKET_LENGTH");

  return { messageType, seq, body: buffer.subarray(HEADER_LEN) };
}

function decodeError(body) {
  const bytes = body instanceof Uint8Array ? body : new Uint8Array(body);
  let errorCode = "";
  let message = "";
  let offset = 0;

  while (offset < bytes.length) {
    const tag = bytes[offset++];
    const fieldNumber = tag >> 3;
    const wireType = tag & 0x07;
    if (wireType !== 2) break;
    const length = bytes[offset++];
    const value = Buffer.from(bytes.subarray(offset, offset + length)).toString("utf8");
    offset += length;
    if (fieldNumber === 1) errorCode = value;
    else if (fieldNumber === 2) message = value;
  }

  return { errorCode, message };
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

function buildGrantMailAttachmentsPayload(playerId, mailId, attachments, reason = "") {
  return Buffer.from(JSON.stringify({
    requestId: mailId,
    playerId,
    items: attachments,
    source: "mail-claim",
    reason
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

  try {
    await onceConnected(socket, config.gameAdminConnectTimeoutMs);
    await onceWritten(
      socket,
      encodePacket(MESSAGE_TYPE.ADMIN_AUTH_REQ, 0, buildAdminAuthBody(config, options.actor)),
      config.gameAdminWriteTimeoutMs
    );
    await onceWritten(socket, encodePacket(messageType, nextSeq(), payload), config.gameAdminWriteTimeoutMs);

    const responseBuffer = await readSinglePacket(
      socket,
      config.gameAdminReadTimeoutMs,
      config.gameAdminMaxResponseBytes
    );
    const response = decodePacket(responseBuffer);

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
      reject(new Error("admin connection closed before response"));
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

  async grantMailAttachments(playerId, mailId, attachments, reason = "", options = {}) {
    const payload = buildGrantMailAttachmentsPayload(playerId, mailId, attachments, reason);
    const endpoint = await this.resolveAdminEndpoint({ ...options, requireExplicitTarget: true });

    await sendRequest(
      this.config,
      MESSAGE_TYPE.GM_SEND_ITEM_REQ,
      payload,
      MESSAGE_TYPE.GM_SEND_ITEM_RES,
      {
        ...options,
        endpoint,
        actor: normalizeGameAdminActor(options.actor) || getDefaultGameAdminActor(this.config)
      }
    );

    return { ok: true, instanceId: endpoint.instanceId };
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
  createAdminError,
  getDefaultGameAdminActor,
  normalizeGameAdminActor,
  normalizeServiceActorCandidate,
  sendRequest
};
