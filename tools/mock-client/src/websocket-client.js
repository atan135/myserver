import WebSocket from "ws";

import { HEADER_LEN } from "./constants.js";
import { decodeByMessageType } from "./messages.js";
import { decodePacketFrame, encodePacket } from "./packet.js";

const DEFAULT_MAX_BODY_LEN = 1024 * 1024;

/**
 * WebSocket transport adapter for the existing 14-byte packet protocol.
 * One complete logical binary WebSocket message maps to exactly one packet.
 */
export class WebSocketProtocolClient {
  constructor(options, label = "client") {
    this.options = options;
    this.label = label;
    this.socket = null;
    this.packetQueue = [];
    this.waiters = [];
    this.maxBodyLen = options.maxBodyLen || DEFAULT_MAX_BODY_LEN;
  }

  async connect() {
    const url = this.options.url;
    if (!url) {
      throw new Error(`${this.label} WebSocket URL is required`);
    }

    const socket = new WebSocket(url, {
      maxPayload: this.maxBodyLen + HEADER_LEN
    });
    this.socket = socket;

    socket.on("message", (data, isBinary) => {
      if (this.socket !== socket) {
        return;
      }
      if (!isBinary) {
        this.failProtocol(socket, new Error(`${this.label} received a text WebSocket message`));
        return;
      }

      try {
        this.packetQueue.push(decodePacketFrame(data, this.maxBodyLen));
        this.resolveWaiters();
      } catch (error) {
        this.failProtocol(socket, error);
      }
    });

    socket.on("error", (error) => {
      if (this.socket === socket) {
        this.rejectWaiters(error);
      }
    });

    socket.on("close", (code, reason) => {
      if (this.socket === socket) {
        const detail = reason?.toString() || "";
        this.rejectWaiters(new Error(`${this.label} WebSocket closed (${code})${detail ? `: ${detail}` : ""}`));
      }
    });

    await new Promise((resolve, reject) => {
      const onOpen = () => {
        cleanup();
        resolve();
      };
      const onError = (error) => {
        cleanup();
        reject(error);
      };
      const cleanup = () => {
        socket.off("open", onOpen);
        socket.off("error", onError);
      };
      socket.once("open", onOpen);
      socket.once("error", onError);
    });
  }

  async reconnect() {
    this.close();
    await this.connect();
  }

  async send(messageType, seq, body) {
    const socket = this.socket;
    if (!socket || socket.readyState !== WebSocket.OPEN) {
      throw new Error(`${this.label} WebSocket is not connected`);
    }

    const packet = encodePacket(messageType, seq, body);
    if (packet.length > this.maxBodyLen + HEADER_LEN) {
      throw new Error(`${this.label} packet exceeds WebSocket body limit`);
    }
    await new Promise((resolve, reject) => {
      socket.send(packet, { binary: true }, (error) => {
        if (error) {
          reject(error);
          return;
        }
        resolve();
      });
    });
  }

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

  async readUntil(timeoutMs, predicate, label = "packet") {
    while (true) {
      const packet = await this.readNextPacket(timeoutMs);
      const decoded = decodeByMessageType(packet.messageType, packet.body);
      decoded.messageType = packet.messageType;
      decoded.seq = packet.seq;
      console.log(`${this.label}.${label}:`, JSON.stringify({ messageType: packet.messageType, seq: packet.seq, decoded }, null, 2));
      if (predicate(packet, decoded)) {
        return decoded;
      }
    }
  }

  close() {
    const socket = this.socket;
    this.socket = null;
    this.rejectWaiters(new Error(`${this.label} WebSocket connection closed`));
    if (!socket) {
      return;
    }
    socket.terminate();
  }

  resolveWaiters() {
    while (this.packetQueue.length > 0 && this.waiters.length > 0) {
      this.waiters.shift().resolve(this.packetQueue.shift());
    }
  }

  rejectWaiters(error) {
    while (this.waiters.length > 0) {
      this.waiters.shift().reject(error);
    }
  }

  failProtocol(socket, error) {
    this.rejectWaiters(error);
    if (socket.readyState === WebSocket.OPEN || socket.readyState === WebSocket.CLOSING) {
      socket.close(1002, "invalid chat packet frame");
    }
  }
}
