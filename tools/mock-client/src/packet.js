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
