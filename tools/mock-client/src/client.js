import net from "node:net";
import { MAGIC, VERSION, HEADER_LEN } from "./constants.js";
import { encodePacket } from "./packet.js";
import { decodeByMessageType } from "./messages.js";

/**
 * TCP Protocol Client for game-server communication
 */
export class TcpProtocolClient {
  /**
   * @param {Object} options - Connection options (host, port)
   * @param {string} label - Client label for logging (e.g., "clientA")
   */
  constructor(options, label = "client") {
    this.options = options;
    this.label = label;
    this.socket = new net.Socket();
    this.buffer = Buffer.alloc(0);
    this.packetQueue = [];
    this.waiters = [];
  }

  async connect() {
    this.socket.on("data", (chunk) => {
      this.buffer = Buffer.concat([this.buffer, chunk]);
      this.drainPackets();
    });

    this.socket.on("error", (error) => {
      while (this.waiters.length > 0) {
        this.waiters.shift().reject(error);
      }
    });

    this.socket.on("close", () => {
      while (this.waiters.length > 0) {
        this.waiters.shift().reject(new Error(`${this.label} TCP connection closed`));
      }
    });

    await new Promise((resolve, reject) => {
      this.socket.connect(this.options.port, this.options.host, resolve);
      this.socket.once("error", reject);
    });
  }

  drainPackets() {
    while (this.buffer.length >= HEADER_LEN) {
      const magic = this.buffer.readUInt16BE(0);
      if (magic !== MAGIC) {
        // Scan forward to find next potential magic marker
        let foundIdx = -1;
        for (let i = 1; i <= this.buffer.length - HEADER_LEN; i++) {
          if (this.buffer.readUInt16BE(i) === MAGIC) {
            foundIdx = i;
            break;
          }
        }

        if (foundIdx > 0) {
          const skipped = foundIdx;
          if (skipped <= 16) {
            console.warn(`Invalid magic ${magic} at offset 0, skipping ${skipped} bytes to find magic`);
          }
          this.buffer = this.buffer.subarray(foundIdx);
          continue;
        }

        if (this.buffer.length > 64) {
          const hexDump = this.buffer.subarray(0, 64).toString("hex");
          console.warn(`No magic found in ${this.buffer.length} bytes, discarding. First 64 bytes: ${hexDump}`);
        }
        this.buffer = Buffer.alloc(0);
        return;
      }

      const messageType = this.buffer.readUInt16BE(4);
      const seq = this.buffer.readUInt32BE(6);
      const bodyLen = this.buffer.readUInt32BE(10);

      const MAX_BODY_LEN = 1024 * 1024;
      if (bodyLen > MAX_BODY_LEN) {
        console.warn(`Suspicious body_len ${bodyLen} at magic position, skipping byte`);
        this.buffer = this.buffer.subarray(1);
        continue;
      }

      const packetLen = HEADER_LEN + bodyLen;
      if (this.buffer.length < packetLen) {
        return;
      }

      const body = this.buffer.subarray(HEADER_LEN, packetLen);
      this.buffer = this.buffer.subarray(packetLen);
      this.packetQueue.push({ messageType, seq, body });
    }

    while (this.packetQueue.length > 0 && this.waiters.length > 0) {
      this.waiters.shift().resolve(this.packetQueue.shift());
    }
  }

  /**
   * Send a packet to the server
   * @param {number} messageType
   * @param {number} seq
   * @param {Buffer} body
   */
  async send(messageType, seq, body) {
    const packet = encodePacket(messageType, seq, body);
    await new Promise((resolve, reject) => {
      this.socket.write(packet, (error) => {
        if (error) {
          reject(error);
          return;
        }
        resolve();
      });
    });
  }

  /**
   * Read the next packet from the queue or wait for one
   * @param {number} timeoutMs
   * @returns {Promise<{messageType: number, seq: number, body: Buffer}>}
   */
  async readNextPacket(timeoutMs) {
    if (this.packetQueue.length > 0) {
      return this.packetQueue.shift();
    }

    return await new Promise((resolve, reject) => {
      let waiter;
      const timer = setTimeout(() => {
        const index = this.waiters.indexOf(waiter);
        if (index >= 0) {
          this.waiters.splice(index, 1);
        }
        reject(new Error(`Timed out waiting for ${this.label} packet after ${timeoutMs}ms`));
      }, timeoutMs);

      waiter = {
        resolve: (packet) => {
          clearTimeout(timer);
          resolve(packet);
        },
        reject: (error) => {
          clearTimeout(timer);
          reject(error);
        }
      };

      this.waiters.push(waiter);
    });
  }

  /**
   * Read packets until predicate returns true
   * @param {number} timeoutMs
   * @param {(packet: Object, decoded: Object) => boolean} predicate
   * @param {string} label - Log label
   * @returns {Promise<Object>}
   */
  async readUntil(timeoutMs, predicate, label = "packet") {
    while (true) {
      const packet = await this.readNextPacket(timeoutMs);
      const decoded = decodeByMessageType(packet.messageType, packet.body);
      console.log(`${this.label}.${label}:`, JSON.stringify({ messageType: packet.messageType, seq: packet.seq, decoded }, null, 2));
      if (predicate(packet, decoded)) {
        return decoded;
      }
    }
  }

  close() {
    this.socket.end();
    this.socket.destroy();
  }
}
