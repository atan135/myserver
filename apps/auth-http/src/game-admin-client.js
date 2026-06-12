import net from "node:net";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const {
  ServerStatusReq,
  ServerStatusRes,
  UpdateConfigReq,
  UpdateConfigRes
} = require("./generated/admin_pb.cjs");

const MAGIC = 0xcafe;
const VERSION = 1;
const HEADER_LEN = 14;
const MESSAGE_TYPE = {
  ADMIN_SERVER_STATUS_REQ: 2001,
  ADMIN_SERVER_STATUS_RES: 2002,
  ADMIN_UPDATE_CONFIG_REQ: 2003,
  ADMIN_UPDATE_CONFIG_RES: 2004,
  ADMIN_AUTH_REQ: 2099,
  GET_ROLLOUT_DRAIN_STATUS_REQ: 1609,
  GET_ROLLOUT_DRAIN_STATUS_RES: 1610,
  REQUEST_SERVER_SHUTDOWN_REQ: 1617,
  REQUEST_SERVER_SHUTDOWN_RES: 1618,
  ERROR_RES: 9000
};

const ROOM_MIGRATION_STATE = {
  0: "OwnedByOld",
  1: "DrainingOnOld",
  2: "FrozenForTransfer",
  3: "ImportingToNew",
  4: "OwnedByNew",
  5: "TransferFailed",
  6: "RetiredOnOld"
};

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

function encodeAdminAuthBody(config) {
  const token = config.gameAdminToken || "";
  const actor = String(config.gameAdminActor || "").trim();

  if (!actor) {
    return Buffer.from(token, "utf8");
  }

  return Buffer.from(JSON.stringify({ token, actor }), "utf8");
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

  if (magic !== MAGIC) {
    throw new Error("INVALID_MAGIC");
  }
  if (version !== VERSION) {
    throw new Error("INVALID_VERSION");
  }
  if (flags !== 0) {
    throw new Error("UNSUPPORTED_FLAGS");
  }
  if (buffer.length !== HEADER_LEN + bodyLen) {
    throw new Error("INVALID_PACKET_LENGTH");
  }

  return {
    messageType,
    seq,
    body: buffer.subarray(HEADER_LEN)
  };
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

    if (wireType !== 2) {
      throw new Error("UNSUPPORTED_ERROR_WIRE_TYPE");
    }

    const length = bytes[offset++];
    const value = Buffer.from(bytes.subarray(offset, offset + length)).toString("utf8");
    offset += length;

    if (fieldNumber === 1) {
      errorCode = value;
    } else if (fieldNumber === 2) {
      message = value;
    }
  }

  return { errorCode, message };
}

function createAdminError(code, message = code) {
  const error = new Error(message);
  error.code = code;
  return error;
}

function decodeVarint(bytes, offset) {
  let result = 0n;
  let shift = 0n;
  let position = offset;

  while (position < bytes.length) {
    const byte = BigInt(bytes[position]);
    result |= (byte & 0x7fn) << shift;
    position += 1;
    if ((byte & 0x80n) === 0n) {
      return { value: result, nextOffset: position };
    }
    shift += 7n;
  }

  throw new Error("UNEXPECTED_END_OF_VARINT");
}

function appendField(fields, fieldNumber, value) {
  const current = fields.get(fieldNumber);
  if (current === undefined) {
    fields.set(fieldNumber, value);
    return;
  }
  if (Array.isArray(current)) {
    current.push(value);
    return;
  }
  fields.set(fieldNumber, [current, value]);
}

function decodeFieldsWithRepeated(body) {
  const bytes = body instanceof Uint8Array ? body : new Uint8Array(body);
  const fields = new Map();
  let offset = 0;

  while (offset < bytes.length) {
    const tag = decodeVarint(bytes, offset);
    const fieldNumber = Number(tag.value >> 3n);
    const wireType = Number(tag.value & 0x07n);
    offset = tag.nextOffset;

    if (wireType === 0) {
      const value = decodeVarint(bytes, offset);
      appendField(fields, fieldNumber, value.value);
      offset = value.nextOffset;
      continue;
    }

    if (wireType === 2) {
      const length = decodeVarint(bytes, offset);
      offset = length.nextOffset;
      const end = offset + Number(length.value);
      if (end > bytes.length) {
        throw new Error("UNEXPECTED_END_OF_LENGTH_DELIMITED_FIELD");
      }
      appendField(fields, fieldNumber, Buffer.from(bytes.subarray(offset, end)));
      offset = end;
      continue;
    }

    throw new Error(`UNSUPPORTED_WIRE_TYPE_${wireType}`);
  }

  return fields;
}

function readString(fields, fieldNumber) {
  const value = fields.get(fieldNumber);
  if (!value) {
    return "";
  }
  return Buffer.from(Array.isArray(value) ? value[0] : value).toString("utf8");
}

function readBool(fields, fieldNumber) {
  return Number(fields.get(fieldNumber) || 0n) !== 0;
}

function readUInt64(fields, fieldNumber) {
  return Number(fields.get(fieldNumber) || 0n);
}

function readRepeatedMessages(fields, fieldNumber, decoder) {
  const value = fields.get(fieldNumber);
  if (!value) {
    return [];
  }
  return (Array.isArray(value) ? value : [value]).map(decoder);
}

function encodeVarint(value) {
  let current = BigInt(value);
  const bytes = [];
  while (current >= 0x80n) {
    bytes.push(Number((current & 0x7fn) | 0x80n));
    current >>= 7n;
  }
  bytes.push(Number(current));
  return Buffer.from(bytes);
}

function encodeStringField(fieldNumber, value) {
  if (!value) {
    return Buffer.alloc(0);
  }
  const data = Buffer.from(value, "utf8");
  return Buffer.concat([
    encodeVarint((fieldNumber << 3) | 2),
    encodeVarint(data.length),
    data
  ]);
}

function encodeRequestServerShutdownReq(reason = "") {
  return encodeStringField(1, reason);
}

function decodeRoomRouteStatus(body) {
  const fields = decodeFieldsWithRepeated(body);
  const migrationStateValue = readUInt64(fields, 3);

  return {
    roomId: readString(fields, 1),
    ownerServerId: readString(fields, 2),
    migrationState: ROOM_MIGRATION_STATE[migrationStateValue] || `Unknown(${migrationStateValue})`,
    memberCount: readUInt64(fields, 4),
    onlineMemberCount: readUInt64(fields, 5),
    emptySinceMs: readUInt64(fields, 6),
    roomVersion: readUInt64(fields, 7)
  };
}

export function decodeRolloutDrainStatusRes(body) {
  const fields = decodeFieldsWithRepeated(body);

  return {
    ok: readBool(fields, 1),
    errorCode: readString(fields, 2),
    rolloutEpoch: readString(fields, 3),
    ownerServerId: readString(fields, 4),
    ownedRoomCount: readUInt64(fields, 5),
    migratingRoomCount: readUInt64(fields, 6),
    connectionCount: readUInt64(fields, 7),
    routes: readRepeatedMessages(fields, 8, decodeRoomRouteStatus),
    drainModeEnabled: readBool(fields, 9),
    drainModeEnteredAtMs: readUInt64(fields, 10),
    transferableEmptyRoomCount: readUInt64(fields, 11),
    transferableEmptyRoomSamples: readRepeatedMessages(fields, 12, decodeRoomRouteStatus),
    drainModeReason: readString(fields, 13),
    drainModeSource: readString(fields, 14),
    retiredRoomCount: readUInt64(fields, 15)
  };
}

export function decodeRequestServerShutdownRes(body) {
  const fields = decodeFieldsWithRepeated(body);

  return {
    ok: readBool(fields, 1),
    errorCode: readString(fields, 2),
    connectionCount: readUInt64(fields, 3),
    ownedRoomCount: readUInt64(fields, 4),
    migratingRoomCount: readUInt64(fields, 5),
    drainModeEnabled: readBool(fields, 6),
    retiredRoomCount: readUInt64(fields, 7)
  };
}

async function sendAdminRequest(config, messageType, payload, expectedType, decodeMessage) {
  const socket = net.createConnection({
    host: config.gameServerAdminHost,
    port: config.gameServerAdminPort
  });

  try {
    await onceConnected(socket, config.gameAdminConnectTimeoutMs);
    await onceWritten(
      socket,
      encodePacket(MESSAGE_TYPE.ADMIN_AUTH_REQ, 0, encodeAdminAuthBody(config)),
      config.gameAdminWriteTimeoutMs
    );
    await onceWritten(
      socket,
      encodePacket(messageType, 1, Buffer.from(payload.serializeBinary())),
      config.gameAdminWriteTimeoutMs
    );
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
      const err = new Error(`unexpected admin response type ${response.messageType}`);
      err.code = "UNEXPECTED_ADMIN_RESPONSE";
      throw err;
    }

    return decodeMessage(response.body);
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
  constructor(config) {
    this.config = config;
  }

  async getServerStatus() {
    return sendAdminRequest(
      this.config,
      MESSAGE_TYPE.ADMIN_SERVER_STATUS_REQ,
      new ServerStatusReq(),
      MESSAGE_TYPE.ADMIN_SERVER_STATUS_RES,
      (body) => {
        const message = ServerStatusRes.deserializeBinary(body);
        return {
          connectionCount: message.getConnectionCount(),
          roomCount: message.getRoomCount(),
          status: message.getStatus(),
          maxBodyLen: message.getMaxBodyLen(),
          heartbeatTimeoutSecs: message.getHeartbeatTimeoutSecs()
        };
      }
    );
  }

  async updateConfig(key, value) {
    const request = new UpdateConfigReq();
    request.setKey(key);
    request.setValue(value);

    return sendAdminRequest(
      this.config,
      MESSAGE_TYPE.ADMIN_UPDATE_CONFIG_REQ,
      request,
      MESSAGE_TYPE.ADMIN_UPDATE_CONFIG_RES,
      (body) => {
        const message = UpdateConfigRes.deserializeBinary(body);
        return {
          ok: message.getOk(),
          errorCode: message.getErrorCode()
        };
      }
    );
  }

  async getRolloutDrainStatus() {
    return sendAdminRequest(
      this.config,
      MESSAGE_TYPE.GET_ROLLOUT_DRAIN_STATUS_REQ,
      { serializeBinary: () => Buffer.alloc(0) },
      MESSAGE_TYPE.GET_ROLLOUT_DRAIN_STATUS_RES,
      decodeRolloutDrainStatusRes
    );
  }

  async requestServerShutdown(reason = "") {
    return sendAdminRequest(
      this.config,
      MESSAGE_TYPE.REQUEST_SERVER_SHUTDOWN_REQ,
      { serializeBinary: () => encodeRequestServerShutdownReq(reason) },
      MESSAGE_TYPE.REQUEST_SERVER_SHUTDOWN_RES,
      decodeRequestServerShutdownRes
    );
  }
}
