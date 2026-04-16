import net from "node:net";

const MAGIC = 0xcafe;
const VERSION = 1;
const HEADER_LEN = 14;

const MESSAGE_TYPE = {
  GM_SEND_ITEM_REQ: 3003,
  GM_SEND_ITEM_RES: 3004,
  ERROR_RES: 9000
};
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

async function sendRequest(config, messageType, payload, expectedType) {
  return new Promise((resolve, reject) => {
    const socket = net.createConnection({
      host: config.gameServerAdminHost,
      port: config.gameServerAdminPort
    });

    const cleanup = () => {
      socket.removeAllListeners();
      socket.end();
      socket.destroy();
    };

    socket.on("connect", () => {
      const packet = encodePacket(messageType, nextSeq(), payload);
      socket.write(packet, (err) => {
        if (err) {
          cleanup();
          reject(err);
        }
      });
    });

    socket.on("error", (err) => {
      cleanup();
      reject(err);
    });

    socket.on("close", () => {
      cleanup();
    });

    let buffer = Buffer.alloc(0);

    socket.on("data", (chunk) => {
      buffer = Buffer.concat([buffer, chunk]);

      if (buffer.length < HEADER_LEN) return;

      const bodyLen = buffer.readUInt32BE(10);
      const packetLen = HEADER_LEN + bodyLen;

      if (buffer.length < packetLen) return;

      try {
        const response = decodePacket(buffer.subarray(0, packetLen));

        if (response.messageType === MESSAGE_TYPE.ERROR_RES) {
          const error = decodeError(response.body);
          const err = new Error(error.message || error.errorCode || "game-server admin error");
          err.code = error.errorCode || "GAME_SERVER_ADMIN_ERROR";
          reject(err);
          return;
        }

        if (response.messageType !== expectedType) {
          const err = new Error(`unexpected response type ${response.messageType}`);
          err.code = "UNEXPECTED_RESPONSE";
          reject(err);
          return;
        }

        resolve(response.body);
      } catch (err) {
        reject(err);
      }

      cleanup();
    });
  });
}

export class GameAdminClient {
  constructor(config) {
    this.config = config;
  }

  async grantMailAttachments(playerId, mailId, attachments, reason = "") {
    const payload = Buffer.from(JSON.stringify({
      requestId: mailId,
      playerId,
      items: attachments,
      source: "mail-claim",
      reason
    }));

    await sendRequest(
      this.config,
      MESSAGE_TYPE.GM_SEND_ITEM_REQ,
      payload,
      MESSAGE_TYPE.GM_SEND_ITEM_RES
    );

    return { ok: true };
  }
}
