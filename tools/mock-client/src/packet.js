import { MAGIC, VERSION, HEADER_LEN } from "./constants.js";

/**
 * Encode a packet with header
 * @param {number} messageType
 * @param {number} seq
 * @param {Buffer} body
 * @returns {Buffer}
 */
export function encodePacket(messageType, seq, body) {
  const header = Buffer.alloc(HEADER_LEN);
  header.writeUInt16BE(MAGIC, 0);
  header.writeUInt8(VERSION, 2);
  header.writeUInt8(0, 3);
  header.writeUInt16BE(messageType, 4);
  header.writeUInt32BE(seq, 6);
  header.writeUInt32BE(body.length, 10);
  return Buffer.concat([header, body]);
}

/**
 * Decode exactly one complete protocol packet from a WebSocket binary message.
 * TCP callers keep their streaming parser; WebSocket callers must not accept
 * partial or concatenated packets in one logical binary message.
 *
 * @param {Buffer|Uint8Array|ArrayBuffer} frame
 * @param {number} maxBodyLen
 * @returns {{messageType: number, seq: number, body: Buffer}}
 */
export function decodePacketFrame(frame, maxBodyLen = 1024 * 1024) {
  const packet = Buffer.from(frame);
  if (packet.length < HEADER_LEN) {
    throw new Error(`packet frame is too short: ${packet.length} < ${HEADER_LEN}`);
  }

  const magic = packet.readUInt16BE(0);
  if (magic !== MAGIC) {
    throw new Error(`invalid packet magic: 0x${magic.toString(16)}`);
  }
  if (packet.readUInt8(2) !== VERSION) {
    throw new Error(`unsupported packet version: ${packet.readUInt8(2)}`);
  }
  if (packet.readUInt8(3) !== 0) {
    throw new Error(`unsupported packet flags: ${packet.readUInt8(3)}`);
  }

  const bodyLen = packet.readUInt32BE(10);
  if (!Number.isInteger(maxBodyLen) || maxBodyLen <= 0 || bodyLen > maxBodyLen) {
    throw new Error(`packet body exceeds limit: ${bodyLen} > ${maxBodyLen}`);
  }
  const expectedLength = HEADER_LEN + bodyLen;
  if (packet.length !== expectedLength) {
    throw new Error(`packet frame length mismatch: ${packet.length} !== ${expectedLength}`);
  }

  return {
    messageType: packet.readUInt16BE(4),
    seq: packet.readUInt32BE(6),
    body: packet.subarray(HEADER_LEN)
  };
}
