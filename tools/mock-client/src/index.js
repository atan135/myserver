import net from "node:net";
import process from "node:process";

const MAGIC = 0xcafe;
const VERSION = 1;
const HEADER_LEN = 14;

const MESSAGE_TYPE = {
  AUTH_REQ: 1001,
  AUTH_RES: 1002,
  PING_REQ: 1003,
  PING_RES: 1004,
  ROOM_JOIN_REQ: 1101,
  ROOM_JOIN_RES: 1102,
  ERROR_RES: 9000
};

const SCENARIO = {
  HAPPY: "happy",
  INVALID_TICKET: "invalid-ticket",
  UNAUTH_ROOM_JOIN: "unauth-room-join",
  UNKNOWN_MESSAGE: "unknown-message",
  OVERSIZED_ROOM_JOIN: "oversized-room-join"
};

function parseArgs(argv) {
  const result = {
    host: "127.0.0.1",
    port: 7000,
    httpBaseUrl: "http://127.0.0.1:3000",
    roomId: "room-default",
    guestId: "",
    ticket: "",
    timeoutMs: 5000,
    scenario: SCENARIO.HAPPY,
    maxBodyLen: 4096
  };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    const next = argv[index + 1];

    if (arg === "--host" && next) {
      result.host = next;
      index += 1;
    } else if (arg === "--port" && next) {
      result.port = Number.parseInt(next, 10);
      index += 1;
    } else if (arg === "--http-base-url" && next) {
      result.httpBaseUrl = next;
      index += 1;
    } else if (arg === "--room-id" && next) {
      result.roomId = next;
      index += 1;
    } else if (arg === "--guest-id" && next) {
      result.guestId = next;
      index += 1;
    } else if (arg === "--ticket" && next) {
      result.ticket = next;
      index += 1;
    } else if (arg === "--timeout-ms" && next) {
      result.timeoutMs = Number.parseInt(next, 10);
      index += 1;
    } else if (arg === "--scenario" && next) {
      result.scenario = next;
      index += 1;
    } else if (arg === "--max-body-len" && next) {
      result.maxBodyLen = Number.parseInt(next, 10);
      index += 1;
    }
  }

  return result;
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

function decodeVarint(buffer, offset) {
  let result = 0n;
  let shift = 0n;
  let position = offset;

  while (position < buffer.length) {
    const byte = BigInt(buffer[position]);
    result |= (byte & 0x7fn) << shift;
    position += 1;

    if ((byte & 0x80n) === 0n) {
      return { value: result, nextOffset: position };
    }

    shift += 7n;
  }

  throw new Error("Unexpected end of varint");
}

function encodeStringField(fieldNumber, value) {
  const fieldKey = (fieldNumber << 3) | 2;
  const data = Buffer.from(value, "utf8");

  return Buffer.concat([
    encodeVarint(fieldKey),
    encodeVarint(data.length),
    data
  ]);
}

function encodeInt64Field(fieldNumber, value) {
  const fieldKey = fieldNumber << 3;
  return Buffer.concat([encodeVarint(fieldKey), encodeVarint(BigInt(value))]);
}

function encodeAuthReq(ticket) {
  return encodeStringField(1, ticket);
}

function encodePingReq(clientTime) {
  return encodeInt64Field(1, clientTime);
}

function encodeRoomJoinReq(roomId) {
  return encodeStringField(1, roomId);
}

function decodeFields(buffer) {
  const fields = new Map();
  let offset = 0;

  while (offset < buffer.length) {
    const tag = decodeVarint(buffer, offset);
    const fieldNumber = Number(tag.value >> 3n);
    const wireType = Number(tag.value & 0x07n);
    offset = tag.nextOffset;

    if (wireType === 0) {
      const value = decodeVarint(buffer, offset);
      fields.set(fieldNumber, value.value);
      offset = value.nextOffset;
      continue;
    }

    if (wireType === 2) {
      const length = decodeVarint(buffer, offset);
      offset = length.nextOffset;
      const end = offset + Number(length.value);
      fields.set(fieldNumber, buffer.subarray(offset, end));
      offset = end;
      continue;
    }

    throw new Error(`Unsupported wire type: ${wireType}`);
  }

  return fields;
}

function readString(fields, fieldNumber) {
  const value = fields.get(fieldNumber);
  if (!value) {
    return "";
  }

  return Buffer.from(value).toString("utf8");
}

function readBool(fields, fieldNumber) {
  return Number(fields.get(fieldNumber) || 0n) !== 0;
}

function readInt64(fields, fieldNumber) {
  const value = fields.get(fieldNumber) || 0n;
  return Number(value);
}

function decodeByMessageType(messageType, body) {
  const fields = decodeFields(body);

  switch (messageType) {
    case MESSAGE_TYPE.AUTH_RES:
      return {
        ok: readBool(fields, 1),
        playerId: readString(fields, 2),
        errorCode: readString(fields, 3)
      };
    case MESSAGE_TYPE.PING_RES:
      return {
        serverTime: readInt64(fields, 1)
      };
    case MESSAGE_TYPE.ROOM_JOIN_RES:
      return {
        ok: readBool(fields, 1),
        roomId: readString(fields, 2),
        errorCode: readString(fields, 3)
      };
    case MESSAGE_TYPE.ERROR_RES:
      return {
        errorCode: readString(fields, 1),
        message: readString(fields, 2)
      };
    default:
      return {
        rawHex: body.toString("hex")
      };
  }
}

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

class TcpProtocolClient {
  constructor(options) {
    this.options = options;
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
        const waiter = this.waiters.shift();
        waiter.reject(error);
      }
    });

    this.socket.on("close", () => {
      while (this.waiters.length > 0) {
        const waiter = this.waiters.shift();
        waiter.reject(new Error("TCP connection closed"));
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
        throw new Error(`Invalid magic: ${magic}`);
      }

      const messageType = this.buffer.readUInt16BE(4);
      const seq = this.buffer.readUInt32BE(6);
      const bodyLen = this.buffer.readUInt32BE(10);
      const packetLen = HEADER_LEN + bodyLen;

      if (this.buffer.length < packetLen) {
        return;
      }

      const body = this.buffer.subarray(HEADER_LEN, packetLen);
      this.buffer = this.buffer.subarray(packetLen);
      this.packetQueue.push({ messageType, seq, body });
    }

    while (this.packetQueue.length > 0 && this.waiters.length > 0) {
      const waiter = this.waiters.shift();
      waiter.resolve(this.packetQueue.shift());
    }
  }

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
        reject(new Error(`Timed out waiting for TCP packet after ${timeoutMs}ms`));
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

  close() {
    this.socket.end();
    this.socket.destroy();
  }
}

async function fetchTicket(options) {
  if (options.ticket) {
    return {
      playerId: "manual-ticket",
      accessToken: "",
      ticket: options.ticket
    };
  }

  const response = await fetch(`${options.httpBaseUrl}/api/v1/auth/guest-login`, {
    method: "POST",
    headers: {
      "content-type": "application/json"
    },
    body: JSON.stringify(
      options.guestId ? { guestId: options.guestId } : {}
    )
  });

  if (!response.ok) {
    throw new Error(`guest-login failed with status ${response.status}`);
  }

  const payload = await response.json();
  if (!payload.ok) {
    throw new Error(`guest-login failed: ${JSON.stringify(payload)}`);
  }

  return payload;
}

function printResponse(label, packet) {
  const decoded = decodeByMessageType(packet.messageType, packet.body);
  console.log(`${label}:`, JSON.stringify({
    messageType: packet.messageType,
    seq: packet.seq,
    decoded
  }, null, 2));

  return decoded;
}

function tamperTicket(ticket) {
  const last = ticket.at(-1) === "a" ? "b" : "a";
  return `${ticket.slice(0, -1)}${last}`;
}

async function expectErrorPacket(client, timeoutMs, expectedErrorCode) {
  const packet = await client.readNextPacket(timeoutMs);
  const decoded = printResponse("error", packet);
  if (packet.messageType !== MESSAGE_TYPE.ERROR_RES) {
    throw new Error(`expected ERROR_RES, got ${packet.messageType}`);
  }
  if (decoded.errorCode !== expectedErrorCode) {
    throw new Error(`expected ${expectedErrorCode}, got ${decoded.errorCode}`);
  }
}

async function runHappyPath(client, options, login) {
  await client.send(MESSAGE_TYPE.AUTH_REQ, 1, encodeAuthReq(login.ticket));
  const authPacket = await client.readNextPacket(options.timeoutMs);
  const auth = printResponse("auth", authPacket);
  if (!auth.ok) {
    throw new Error(`auth failed: ${auth.errorCode}`);
  }

  await client.send(MESSAGE_TYPE.PING_REQ, 2, encodePingReq(Date.now()));
  const pingPacket = await client.readNextPacket(options.timeoutMs);
  printResponse("ping", pingPacket);

  await client.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 3, encodeRoomJoinReq(options.roomId));
  const roomPacket = await client.readNextPacket(options.timeoutMs);
  const room = printResponse("roomJoin", roomPacket);
  if (!room.ok) {
    throw new Error(`room join failed: ${room.errorCode}`);
  }
}

async function runInvalidTicket(client, options, login) {
  await client.send(MESSAGE_TYPE.AUTH_REQ, 1, encodeAuthReq(tamperTicket(login.ticket)));
  const authPacket = await client.readNextPacket(options.timeoutMs);
  const auth = printResponse("auth", authPacket);
  if (auth.ok) {
    throw new Error("expected invalid ticket auth failure");
  }
}

async function runUnauthRoomJoin(client, options) {
  await client.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 1, encodeRoomJoinReq(options.roomId));
  await expectErrorPacket(client, options.timeoutMs, "NOT_AUTHENTICATED");
}

async function runUnknownMessage(client, options) {
  await client.send(7777, 1, Buffer.alloc(0));
  await expectErrorPacket(client, options.timeoutMs, "UNKNOWN_MESSAGE_TYPE");
}

async function runOversizedRoomJoin(client, options, login) {
  await client.send(MESSAGE_TYPE.AUTH_REQ, 1, encodeAuthReq(login.ticket));
  const authPacket = await client.readNextPacket(options.timeoutMs);
  const auth = printResponse("auth", authPacket);
  if (!auth.ok) {
    throw new Error(`auth failed: ${auth.errorCode}`);
  }

  const oversizedRoomId = "r".repeat(options.maxBodyLen + 64);
  await client.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(oversizedRoomId));
  await expectErrorPacket(client, options.timeoutMs, "BODY_TOO_LARGE");
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const needsLogin = [SCENARIO.HAPPY, SCENARIO.INVALID_TICKET, SCENARIO.OVERSIZED_ROOM_JOIN].includes(options.scenario) || Boolean(options.ticket);
  const login = needsLogin ? await fetchTicket(options) : null;

  if (login) {
    console.log("login:", JSON.stringify({
      playerId: login.playerId,
      hasAccessToken: Boolean(login.accessToken),
      ticketPreview: `${login.ticket.slice(0, 16)}...`
    }, null, 2));
  }

  const client = new TcpProtocolClient(options);
  await client.connect();

  try {
    switch (options.scenario) {
      case SCENARIO.HAPPY:
        await runHappyPath(client, options, login);
        break;
      case SCENARIO.INVALID_TICKET:
        await runInvalidTicket(client, options, login);
        break;
      case SCENARIO.UNAUTH_ROOM_JOIN:
        await runUnauthRoomJoin(client, options);
        break;
      case SCENARIO.UNKNOWN_MESSAGE:
        await runUnknownMessage(client, options);
        break;
      case SCENARIO.OVERSIZED_ROOM_JOIN:
        await runOversizedRoomJoin(client, options, login);
        break;
      default:
        throw new Error(`unknown scenario: ${options.scenario}`);
    }

    console.log(`scenario completed: ${options.scenario}`);
  } finally {
    client.close();
  }
}

main().catch((error) => {
  console.error(error.message);
  process.exitCode = 1;
});
