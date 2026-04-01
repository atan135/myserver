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
  ERROR_RES: 9000
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

async function sendAdminRequest(config, messageType, payload, expectedType, decodeMessage) {
  const socket = net.createConnection({
    host: config.gameServerAdminHost,
    port: config.gameServerAdminPort
  });

  try {
    await onceConnected(socket);
    await onceWritten(socket, encodePacket(messageType, 1, Buffer.from(payload.serializeBinary())));
    const responseBuffer = await readSinglePacket(socket);
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

function onceConnected(socket) {
  return new Promise((resolve, reject) => {
    socket.once("connect", resolve);
    socket.once("error", reject);
  });
}

function onceWritten(socket, data) {
  return new Promise((resolve, reject) => {
    socket.write(data, (error) => {
      if (error) {
        reject(error);
        return;
      }
      resolve();
    });
  });
}

function readSinglePacket(socket) {
  return new Promise((resolve, reject) => {
    let buffer = Buffer.alloc(0);

    const onData = (chunk) => {
      buffer = Buffer.concat([buffer, chunk]);
      if (buffer.length < HEADER_LEN) {
        return;
      }

      const bodyLen = buffer.readUInt32BE(10);
      const packetLen = HEADER_LEN + bodyLen;
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
}
