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
  ROOM_LEAVE_REQ: 1103,
  ROOM_LEAVE_RES: 1104,
  ROOM_READY_REQ: 1105,
  ROOM_READY_RES: 1106,
  ROOM_START_REQ: 1107,
  ROOM_START_RES: 1108,
  PLAYER_INPUT_REQ: 1111,
  PLAYER_INPUT_RES: 1112,
  ROOM_END_REQ: 1113,
  ROOM_END_RES: 1114,
  ROOM_STATE_PUSH: 1201,
  GET_ROOM_DATA_REQ: 1301,
  GET_ROOM_DATA_RES: 1302,
  GAME_MESSAGE_PUSH: 1202,
  ERROR_RES: 9000
};

const SCENARIO = {
  HAPPY: "happy",
  INVALID_TICKET: "invalid-ticket",
  UNAUTH_ROOM_JOIN: "unauth-room-join",
  UNKNOWN_MESSAGE: "unknown-message",
  OVERSIZED_ROOM_JOIN: "oversized-room-join",
  TWO_CLIENT_ROOM: "two-client-room",
  START_GAME_SINGLE_CLIENT: "start-game-single-client",
  START_GAME_READY_ROOM: "start-game-ready-room",
  GAMEPLAY_ROUNDTRIP: "gameplay-roundtrip",
  GET_ROOM_DATA: "get-room-data"
};

function parseArgs(argv) {
  const result = {
    host: "127.0.0.1",
    port: 7000,
    httpBaseUrl: "http://127.0.0.1:3000",
    roomId: "room-default",
    guestId: "",
    loginName: "",
    password: "",
    loginNameA: "",
    passwordA: "",
    loginNameB: "",
    passwordB: "",
    ticket: "",
    timeoutMs: 5000,
    scenario: SCENARIO.HAPPY,
    maxBodyLen: 4096,
    idStart: 1000,
    idEnd: 1000
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
    } else if (arg === "--login-name" && next) {
      result.loginName = next;
      index += 1;
    } else if (arg === "--password" && next) {
      result.password = next;
      index += 1;
    } else if (arg === "--login-name-a" && next) {
      result.loginNameA = next;
      index += 1;
    } else if (arg === "--password-a" && next) {
      result.passwordA = next;
      index += 1;
    } else if (arg === "--login-name-b" && next) {
      result.loginNameB = next;
      index += 1;
    } else if (arg === "--password-b" && next) {
      result.passwordB = next;
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
    } else if (arg === "--id-start" && next) {
      result.idStart = Number.parseInt(next, 10);
      index += 1;
    } else if (arg === "--id-end" && next) {
      result.idEnd = Number.parseInt(next, 10);
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
  return Buffer.concat([encodeVarint(fieldKey), encodeVarint(data.length), data]);
}

function encodeBoolField(fieldNumber, value) {
  const fieldKey = fieldNumber << 3;
  return Buffer.concat([encodeVarint(fieldKey), encodeVarint(value ? 1 : 0)]);
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

function encodeRoomLeaveReq() {
  return Buffer.alloc(0);
}

function encodeRoomReadyReq(ready) {
  return encodeBoolField(1, ready);
}

function encodeRoomStartReq() {
  return Buffer.alloc(0);
}

function encodePlayerInputReq(action, payloadJson) {
  return Buffer.concat([
    encodeStringField(1, action),
    encodeStringField(2, payloadJson)
  ]);
}

function encodeRoomEndReq(reason) {
  return encodeStringField(1, reason);
}

function encodeInt32Field(fieldNumber, value) {
  const fieldKey = fieldNumber << 3;
  return Buffer.concat([encodeVarint(fieldKey), encodeVarint(value)]);
}

function encodeGetRoomDataReq(idStart, idEnd) {
  return Buffer.concat([
    encodeInt32Field(1, idStart),
    encodeInt32Field(2, idEnd)
  ]);
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

function decodeFieldsWithRepeated(buffer) {
  const fields = new Map();
  let offset = 0;

  while (offset < buffer.length) {
    const tag = decodeVarint(buffer, offset);
    const fieldNumber = Number(tag.value >> 3n);
    const wireType = Number(tag.value & 0x07n);
    offset = tag.nextOffset;

    if (wireType === 0) {
      const value = decodeVarint(buffer, offset);
      appendField(fields, fieldNumber, value.value);
      offset = value.nextOffset;
      continue;
    }

    if (wireType === 2) {
      const length = decodeVarint(buffer, offset);
      offset = length.nextOffset;
      const end = offset + Number(length.value);
      appendField(fields, fieldNumber, buffer.subarray(offset, end));
      offset = end;
      continue;
    }

    throw new Error(`Unsupported wire type: ${wireType}`);
  }

  return fields;
}

function readString(fields, fieldNumber) {
  const value = fields.get(fieldNumber);
  return value ? Buffer.from(value).toString("utf8") : "";
}

function readStringList(fields, fieldNumber) {
  const value = fields.get(fieldNumber);
  if (!value) {
    return [];
  }
  if (Array.isArray(value)) {
    return value.map((entry) => Buffer.from(entry).toString("utf8"));
  }
  return [Buffer.from(value).toString("utf8")];
}
function readBool(fields, fieldNumber) {
  return Number(fields.get(fieldNumber) || 0n) !== 0;
}

function readInt64(fields, fieldNumber) {
  return Number(fields.get(fieldNumber) || 0n);
}

function decodeRoomMember(buffer) {
  const fields = decodeFieldsWithRepeated(buffer);
  return {
    playerId: readString(fields, 1),
    ready: readBool(fields, 2),
    isOwner: readBool(fields, 3)
  };
}

function decodeRoomSnapshot(buffer) {
  const fields = decodeFieldsWithRepeated(buffer);
  const membersRaw = fields.get(4);
  let members = [];

  if (membersRaw) {
    if (Array.isArray(membersRaw)) {
      members = membersRaw.map(decodeRoomMember);
    } else {
      members = [decodeRoomMember(membersRaw)];
    }
  }

  return {
    roomId: readString(fields, 1),
    ownerPlayerId: readString(fields, 2),
    state: readString(fields, 3),
    members
  };
}

function decodeByMessageType(messageType, body) {
  const fields = decodeFieldsWithRepeated(body);

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
    case MESSAGE_TYPE.ROOM_LEAVE_RES:
      return {
        ok: readBool(fields, 1),
        roomId: readString(fields, 2),
        errorCode: readString(fields, 3)
      };
    case MESSAGE_TYPE.ROOM_READY_RES:
      return {
        ok: readBool(fields, 1),
        roomId: readString(fields, 2),
        ready: readBool(fields, 3),
        errorCode: readString(fields, 4)
      };
    case MESSAGE_TYPE.ROOM_START_RES:
      return {
        ok: readBool(fields, 1),
        roomId: readString(fields, 2),
        errorCode: readString(fields, 3)
      };
    case MESSAGE_TYPE.PLAYER_INPUT_RES:
      return {
        ok: readBool(fields, 1),
        roomId: readString(fields, 2),
        errorCode: readString(fields, 3)
      };
    case MESSAGE_TYPE.ROOM_END_RES:
      return {
        ok: readBool(fields, 1),
        roomId: readString(fields, 2),
        errorCode: readString(fields, 3)
      };
    case MESSAGE_TYPE.GET_ROOM_DATA_RES:
      return {
        ok: readBool(fields, 1),
        field0List: readStringList(fields, 2),
        errorCode: readString(fields, 3)
      };
    case MESSAGE_TYPE.ROOM_STATE_PUSH:
      return {
        event: readString(fields, 1),
        snapshot: fields.get(2) ? decodeRoomSnapshot(fields.get(2)) : null
      };
    case MESSAGE_TYPE.GAME_MESSAGE_PUSH:
      return {
        event: readString(fields, 1),
        roomId: readString(fields, 2),
        playerId: readString(fields, 3),
        action: readString(fields, 4),
        payloadJson: readString(fields, 5)
      };
    case MESSAGE_TYPE.ERROR_RES:
      return {
        errorCode: readString(fields, 1),
        message: readString(fields, 2)
      };
    default:
      return { rawHex: body.toString("hex") };
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
      this.waiters.shift().resolve(this.packetQueue.shift());
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

  close() {
    this.socket.end();
    this.socket.destroy();
  }
}

function resolveAccountCredentials(options, overrides = {}) {
  const loginName = overrides.loginName ?? options.loginName;
  const password = overrides.password ?? options.password;

  if (!loginName && !password) {
    return null;
  }

  if (!loginName || !password) {
    throw new Error("account login requires both loginName and password");
  }

  return {
    loginName,
    password
  };
}

function resolveMultiClientLoginOverrides(options, clientSuffix, guestId) {
  const loginNameKey = `loginName${clientSuffix}`;
  const passwordKey = `password${clientSuffix}`;
  const loginName = options[loginNameKey];
  const password = options[passwordKey];

  if (loginName || password) {
    if (!loginName || !password) {
      throw new Error(
        `client${clientSuffix} account login requires both --login-name-${clientSuffix.toLowerCase()} and --password-${clientSuffix.toLowerCase()}`
      );
    }

    return {
      loginName,
      password
    };
  }

  if (options.loginName || options.password) {
    throw new Error(
      "multi-client account login requires --login-name-a/--password-a and --login-name-b/--password-b"
    );
  }

  return { guestId };
}

function formatLoginSummary(login) {
  return {
    playerId: login.playerId,
    loginName: login.loginName || null,
    guestId: login.guestId || null,
    hasAccessToken: Boolean(login.accessToken),
    ticketPreview: login.ticket ? `${login.ticket.slice(0, 16)}...` : null
  };
}

async function fetchTicket(options, overrides = {}) {
  if (options.ticket && Object.keys(overrides).length === 0) {
    return { playerId: "manual-ticket", accessToken: "", ticket: options.ticket };
  }

  const accountCredentials = resolveAccountCredentials(options, overrides);
  if (accountCredentials) {
    const response = await fetch(`${options.httpBaseUrl}/api/v1/auth/login`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(accountCredentials)
    });

    if (!response.ok) {
      throw new Error(`account login failed with status ${response.status}`);
    }

    const payload = await response.json();
    if (!payload.ok) {
      throw new Error(`account login failed: ${JSON.stringify(payload)}`);
    }

    return payload;
  }

  const guestId = overrides.guestId || options.guestId;
  const response = await fetch(`${options.httpBaseUrl}/api/v1/auth/guest-login`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(guestId ? { guestId } : {})
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
  console.log(`${label}:`, JSON.stringify({ messageType: packet.messageType, seq: packet.seq, decoded }, null, 2));
  return decoded;
}

function tamperTicket(ticket) {
  const last = ticket.at(-1) === "a" ? "b" : "a";
  return `${ticket.slice(0, -1)}${last}`;
}

async function expectErrorPacket(client, timeoutMs, expectedErrorCode, label = "error") {
  const packet = await client.readNextPacket(timeoutMs);
  const decoded = printResponse(`${client.label}.${label}`, packet);
  if (packet.messageType !== MESSAGE_TYPE.ERROR_RES) {
    throw new Error(`expected ERROR_RES, got ${packet.messageType}`);
  }
  if (decoded.errorCode !== expectedErrorCode) {
    throw new Error(`expected ${expectedErrorCode}, got ${decoded.errorCode}`);
  }
}

async function authenticateClient(client, options, login, seq = 1) {
  await client.send(MESSAGE_TYPE.AUTH_REQ, seq, encodeAuthReq(login.ticket));
  const auth = printResponse(`${client.label}.auth`, await client.readNextPacket(options.timeoutMs));
  if (!auth.ok) {
    throw new Error(`${client.label} auth failed: ${auth.errorCode}`);
  }
}

async function runHappyPath(client, options, login) {
  await authenticateClient(client, options, login, 1);

  await client.send(MESSAGE_TYPE.PING_REQ, 2, encodePingReq(Date.now()));
  printResponse(`${client.label}.ping`, await client.readNextPacket(options.timeoutMs));

  await client.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 3, encodeRoomJoinReq(options.roomId));
  const roomJoin = printResponse(`${client.label}.roomJoin`, await client.readNextPacket(options.timeoutMs));
  if (!roomJoin.ok) {
    throw new Error(`room join failed: ${roomJoin.errorCode}`);
  }

  printResponse(`${client.label}.roomStatePush(join)`, await client.readNextPacket(options.timeoutMs));

  await client.send(MESSAGE_TYPE.ROOM_READY_REQ, 4, encodeRoomReadyReq(true));
  const readyRes = printResponse(`${client.label}.roomReady`, await client.readNextPacket(options.timeoutMs));
  if (!readyRes.ok) {
    throw new Error(`room ready failed: ${readyRes.errorCode}`);
  }

  const readyPush = printResponse(`${client.label}.roomStatePush(ready)`, await client.readNextPacket(options.timeoutMs));
  if (readyPush.snapshot?.state !== "ready") {
    throw new Error("expected room state to become ready");
  }

  await client.send(MESSAGE_TYPE.ROOM_LEAVE_REQ, 5, encodeRoomLeaveReq());
  const leaveRes = printResponse(`${client.label}.roomLeave`, await client.readNextPacket(options.timeoutMs));
  if (!leaveRes.ok) {
    throw new Error(`room leave failed: ${leaveRes.errorCode}`);
  }
}

async function runStartGameSingleClient(client, options, login) {
  await authenticateClient(client, options, login, 1);

  await client.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(options.roomId));
  const joinRes = printResponse(`${client.label}.roomJoin`, await client.readNextPacket(options.timeoutMs));
  if (!joinRes.ok) {
    throw new Error(`room join failed: ${joinRes.errorCode}`);
  }
  printResponse(`${client.label}.roomStatePush(join)`, await client.readNextPacket(options.timeoutMs));

  await client.send(MESSAGE_TYPE.ROOM_READY_REQ, 3, encodeRoomReadyReq(true));
  const readyRes = printResponse(`${client.label}.roomReady`, await client.readNextPacket(options.timeoutMs));
  if (!readyRes.ok) {
    throw new Error(`room ready failed: ${readyRes.errorCode}`);
  }
  printResponse(`${client.label}.roomStatePush(ready)`, await client.readNextPacket(options.timeoutMs));

  await client.send(MESSAGE_TYPE.ROOM_START_REQ, 4, encodeRoomStartReq());
  const startRes = printResponse(`${client.label}.roomStart`, await client.readNextPacket(options.timeoutMs));
  if (startRes.ok) {
    throw new Error("expected single-client start game to fail");
  }
  if (startRes.errorCode !== "ROOM_NOT_ENOUGH_PLAYERS") {
    throw new Error(`expected ROOM_NOT_ENOUGH_PLAYERS, got ${startRes.errorCode}`);
  }
}

async function runGetRoomData(client, options, login) {
  await authenticateClient(client, options, login, 1);

  await client.send(
    MESSAGE_TYPE.GET_ROOM_DATA_REQ,
    2,
    encodeGetRoomDataReq(options.idStart, options.idEnd)
  );
  const response = printResponse(`${client.label}.getRoomData`, await client.readNextPacket(options.timeoutMs));
  if (!response.ok) {
    throw new Error(`get room data failed: ${response.errorCode}`);
  }
  if (response.field0List.length === 0) {
    throw new Error("expected field0List to contain at least one string");
  }

  console.log(`${client.label}.getRoomData.field0List:`, JSON.stringify(response.field0List, null, 2));
}
async function runGameplayRoundtrip(options) {
  const loginA = await fetchTicket(options, `${options.roomId}-owner`);
  const loginB = await fetchTicket(options, `${options.roomId}-member`);

  console.log("clientA.login:", JSON.stringify({ playerId: loginA.playerId }, null, 2));
  console.log("clientB.login:", JSON.stringify({ playerId: loginB.playerId }, null, 2));

  const clientA = new TcpProtocolClient(options, "clientA");
  const clientB = new TcpProtocolClient(options, "clientB");
  await clientA.connect();
  await clientB.connect();

  try {
    await authenticateClient(clientA, options, loginA, 1);
    await authenticateClient(clientB, options, loginB, 1);

    await clientA.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(options.roomId));
    printResponse("clientA.roomJoin", await clientA.readNextPacket(options.timeoutMs));
    printResponse("clientA.roomStatePush(join1)", await clientA.readNextPacket(options.timeoutMs));

    await clientB.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(options.roomId));
    printResponse("clientB.roomJoin", await clientB.readNextPacket(options.timeoutMs));
    printResponse("clientB.roomStatePush(join)", await clientB.readNextPacket(options.timeoutMs));
    printResponse("clientA.roomStatePush(join2)", await clientA.readNextPacket(options.timeoutMs));

    await clientA.send(MESSAGE_TYPE.ROOM_READY_REQ, 3, encodeRoomReadyReq(true));
    printResponse("clientA.roomReady", await clientA.readNextPacket(options.timeoutMs));
    printResponse("clientA.roomStatePush(ready1)", await clientA.readNextPacket(options.timeoutMs));
    printResponse("clientB.roomStatePush(ready1)", await clientB.readNextPacket(options.timeoutMs));

    await clientB.send(MESSAGE_TYPE.ROOM_READY_REQ, 3, encodeRoomReadyReq(true));
    printResponse("clientB.roomReady", await clientB.readNextPacket(options.timeoutMs));
    printResponse("clientB.roomStatePush(ready2)", await clientB.readNextPacket(options.timeoutMs));
    printResponse("clientA.roomStatePush(ready2)", await clientA.readNextPacket(options.timeoutMs));

    await clientA.send(MESSAGE_TYPE.ROOM_START_REQ, 4, encodeRoomStartReq());
    const startRes = printResponse("clientA.roomStart", await clientA.readNextPacket(options.timeoutMs));
    if (!startRes.ok) {
      throw new Error(`clientA room start failed: ${startRes.errorCode}`);
    }
    printResponse("clientA.roomStatePush(gameStarted)", await clientA.readNextPacket(options.timeoutMs));
    printResponse("clientB.roomStatePush(gameStarted)", await clientB.readNextPacket(options.timeoutMs));

    const payloadJson = JSON.stringify({ x: 4, y: 7, frame: 1 });
    await clientA.send(MESSAGE_TYPE.PLAYER_INPUT_REQ, 5, encodePlayerInputReq("move", payloadJson));
    const inputRes = printResponse("clientA.playerInput", await clientA.readNextPacket(options.timeoutMs));
    if (!inputRes.ok) {
      throw new Error(`clientA player input failed: ${inputRes.errorCode}`);
    }

    const gamePushA = printResponse("clientA.gameMessagePush", await clientA.readNextPacket(options.timeoutMs));
    const gamePushB = printResponse("clientB.gameMessagePush", await clientB.readNextPacket(options.timeoutMs));
    if (gamePushA.action !== "move" || gamePushB.action !== "move") {
      throw new Error("expected game message push action to be move");
    }
    if (gamePushA.payloadJson !== payloadJson || gamePushB.payloadJson !== payloadJson) {
      throw new Error("expected game message push payload to match input payload");
    }
    if (gamePushA.playerId !== loginA.playerId || gamePushB.playerId !== loginA.playerId) {
      throw new Error("expected game message push playerId to be the input sender");
    }

    await clientA.send(MESSAGE_TYPE.ROOM_END_REQ, 6, encodeRoomEndReq("round-complete"));
    const endRes = printResponse("clientA.roomEnd", await clientA.readNextPacket(options.timeoutMs));
    if (!endRes.ok) {
      throw new Error(`clientA room end failed: ${endRes.errorCode}`);
    }

    const endPushA = printResponse("clientA.roomStatePush(gameEnded)", await clientA.readNextPacket(options.timeoutMs));
    const endPushB = printResponse("clientB.roomStatePush(gameEnded)", await clientB.readNextPacket(options.timeoutMs));
    if (endPushA.event !== "game_ended" || endPushB.event !== "game_ended") {
      throw new Error("expected game_ended room state push");
    }
    if (endPushA.snapshot?.state !== "waiting" || endPushB.snapshot?.state !== "waiting") {
      throw new Error("expected room to return to waiting after game end");
    }
    if (endPushA.snapshot?.members?.some((member) => member.ready) || endPushB.snapshot?.members?.some((member) => member.ready)) {
      throw new Error("expected all members ready state to reset after game end");
    }

    await clientA.send(MESSAGE_TYPE.ROOM_LEAVE_REQ, 7, encodeRoomLeaveReq());
    const leaveA = printResponse("clientA.roomLeave", await clientA.readNextPacket(options.timeoutMs));
    if (!leaveA.ok) {
      throw new Error(`clientA room leave failed: ${leaveA.errorCode}`);
    }
    printResponse("clientB.roomStatePush(afterOwnerLeave)", await clientB.readNextPacket(options.timeoutMs));

    await clientB.send(MESSAGE_TYPE.ROOM_LEAVE_REQ, 7, encodeRoomLeaveReq());
    const leaveB = printResponse("clientB.roomLeave", await clientB.readNextPacket(options.timeoutMs));
    if (!leaveB.ok) {
      throw new Error(`clientB room leave failed: ${leaveB.errorCode}`);
    }
  } finally {
    clientA.close();
    clientB.close();
  }
}

async function runStartGameReadyRoom(options) {
  const loginA = await fetchTicket(options, `${options.roomId}-owner`);
  const loginB = await fetchTicket(options, `${options.roomId}-member`);

  console.log("clientA.login:", JSON.stringify({ playerId: loginA.playerId }, null, 2));
  console.log("clientB.login:", JSON.stringify({ playerId: loginB.playerId }, null, 2));

  const clientA = new TcpProtocolClient(options, "clientA");
  const clientB = new TcpProtocolClient(options, "clientB");
  await clientA.connect();
  await clientB.connect();

  try {
    await authenticateClient(clientA, options, loginA, 1);
    await authenticateClient(clientB, options, loginB, 1);

    await clientA.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(options.roomId));
    printResponse("clientA.roomJoin", await clientA.readNextPacket(options.timeoutMs));
    printResponse("clientA.roomStatePush(join1)", await clientA.readNextPacket(options.timeoutMs));

    await clientB.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(options.roomId));
    printResponse("clientB.roomJoin", await clientB.readNextPacket(options.timeoutMs));
    printResponse("clientB.roomStatePush(join)", await clientB.readNextPacket(options.timeoutMs));
    printResponse("clientA.roomStatePush(join2)", await clientA.readNextPacket(options.timeoutMs));

    await clientA.send(MESSAGE_TYPE.ROOM_READY_REQ, 3, encodeRoomReadyReq(true));
    printResponse("clientA.roomReady", await clientA.readNextPacket(options.timeoutMs));
    printResponse("clientA.roomStatePush(ready1)", await clientA.readNextPacket(options.timeoutMs));
    printResponse("clientB.roomStatePush(ready1)", await clientB.readNextPacket(options.timeoutMs));

    await clientB.send(MESSAGE_TYPE.ROOM_READY_REQ, 3, encodeRoomReadyReq(true));
    printResponse("clientB.roomReady", await clientB.readNextPacket(options.timeoutMs));
    const readyPushB = printResponse("clientB.roomStatePush(ready2)", await clientB.readNextPacket(options.timeoutMs));
    const readyPushA = printResponse("clientA.roomStatePush(ready2)", await clientA.readNextPacket(options.timeoutMs));
    if (readyPushA.snapshot?.state !== "ready" || readyPushB.snapshot?.state !== "ready") {
      throw new Error("expected room state to become ready before start");
    }

    await clientA.send(MESSAGE_TYPE.ROOM_START_REQ, 4, encodeRoomStartReq());
    const startRes = printResponse("clientA.roomStart", await clientA.readNextPacket(options.timeoutMs));
    if (!startRes.ok) {
      throw new Error(`clientA room start failed: ${startRes.errorCode}`);
    }

    const startPushA = printResponse("clientA.roomStatePush(gameStarted)", await clientA.readNextPacket(options.timeoutMs));
    const startPushB = printResponse("clientB.roomStatePush(gameStarted)", await clientB.readNextPacket(options.timeoutMs));
    if (startPushA.event !== "game_started" || startPushB.event !== "game_started") {
      throw new Error("expected game_started room state push");
    }
    if (startPushA.snapshot?.state !== "in_game" || startPushB.snapshot?.state !== "in_game") {
      throw new Error("expected room state to become in_game");
    }

    await clientA.send(MESSAGE_TYPE.ROOM_LEAVE_REQ, 5, encodeRoomLeaveReq());
    const leaveA = printResponse("clientA.roomLeave", await clientA.readNextPacket(options.timeoutMs));
    if (!leaveA.ok) {
      throw new Error(`clientA room leave failed: ${leaveA.errorCode}`);
    }
    printResponse("clientB.roomStatePush(afterOwnerLeave)", await clientB.readNextPacket(options.timeoutMs));

    await clientB.send(MESSAGE_TYPE.ROOM_LEAVE_REQ, 5, encodeRoomLeaveReq());
    const leaveB = printResponse("clientB.roomLeave", await clientB.readNextPacket(options.timeoutMs));
    if (!leaveB.ok) {
      throw new Error(`clientB room leave failed: ${leaveB.errorCode}`);
    }
  } finally {
    clientA.close();
    clientB.close();
  }
}
async function runTwoClientRoom(options) {
  const loginA = await fetchTicket(options, `${options.roomId}-owner`);
  const loginB = await fetchTicket(options, `${options.roomId}-member`);

  console.log("clientA.login:", JSON.stringify({ playerId: loginA.playerId }, null, 2));
  console.log("clientB.login:", JSON.stringify({ playerId: loginB.playerId }, null, 2));

  const clientA = new TcpProtocolClient(options, "clientA");
  const clientB = new TcpProtocolClient(options, "clientB");
  await clientA.connect();
  await clientB.connect();

  try {
    await authenticateClient(clientA, options, loginA, 1);
    await authenticateClient(clientB, options, loginB, 1);

    await clientA.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(options.roomId));
    const joinA = printResponse("clientA.roomJoin", await clientA.readNextPacket(options.timeoutMs));
    if (!joinA.ok) {
      throw new Error(`clientA room join failed: ${joinA.errorCode}`);
    }
    const pushA1 = printResponse("clientA.roomStatePush(join1)", await clientA.readNextPacket(options.timeoutMs));
    if (pushA1.snapshot?.ownerPlayerId !== loginA.playerId) {
      throw new Error("clientA should be initial owner");
    }

    await clientB.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(options.roomId));
    const joinB = printResponse("clientB.roomJoin", await clientB.readNextPacket(options.timeoutMs));
    if (!joinB.ok) {
      throw new Error(`clientB room join failed: ${joinB.errorCode}`);
    }

    const pushB1 = printResponse("clientB.roomStatePush(join)", await clientB.readNextPacket(options.timeoutMs));
    const pushA2 = printResponse("clientA.roomStatePush(join2)", await clientA.readNextPacket(options.timeoutMs));
    if (pushA2.snapshot?.members?.length !== 2 || pushB1.snapshot?.members?.length !== 2) {
      throw new Error("expected both clients to observe two room members");
    }
    if (pushA2.snapshot?.ownerPlayerId !== loginA.playerId) {
      throw new Error("owner should remain clientA before leave");
    }

    await clientA.send(MESSAGE_TYPE.ROOM_LEAVE_REQ, 3, encodeRoomLeaveReq());
    const leaveA = printResponse("clientA.roomLeave", await clientA.readNextPacket(options.timeoutMs));
    if (!leaveA.ok) {
      throw new Error(`clientA room leave failed: ${leaveA.errorCode}`);
    }

    const pushB2 = printResponse("clientB.roomStatePush(ownerTransfer)", await clientB.readNextPacket(options.timeoutMs));
    if (pushB2.snapshot?.ownerPlayerId !== loginB.playerId) {
      throw new Error("expected owner to transfer to clientB");
    }
    if (pushB2.snapshot?.members?.length !== 1) {
      throw new Error("expected only one member after owner leave");
    }

    await clientB.send(MESSAGE_TYPE.ROOM_LEAVE_REQ, 3, encodeRoomLeaveReq());
    const leaveB = printResponse("clientB.roomLeave", await clientB.readNextPacket(options.timeoutMs));
    if (!leaveB.ok) {
      throw new Error(`clientB room leave failed: ${leaveB.errorCode}`);
    }
  } finally {
    clientA.close();
    clientB.close();
  }
}

async function runInvalidTicket(client, options, login) {
  await client.send(MESSAGE_TYPE.AUTH_REQ, 1, encodeAuthReq(tamperTicket(login.ticket)));
  const auth = printResponse(`${client.label}.auth`, await client.readNextPacket(options.timeoutMs));
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
  await authenticateClient(client, options, login, 1);
  const oversizedRoomId = "r".repeat(options.maxBodyLen + 64);
  await client.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(oversizedRoomId));
  await expectErrorPacket(client, options.timeoutMs, "BODY_TOO_LARGE");
}

async function main() {
  const options = parseArgs(process.argv.slice(2));

  if (options.scenario === SCENARIO.TWO_CLIENT_ROOM) {
    await runTwoClientRoom(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.START_GAME_READY_ROOM) {
    await runStartGameReadyRoom(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.GAMEPLAY_ROUNDTRIP) {
    await runGameplayRoundtrip(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  const needsLogin = [
    SCENARIO.HAPPY,
    SCENARIO.INVALID_TICKET,
    SCENARIO.OVERSIZED_ROOM_JOIN,
    SCENARIO.START_GAME_SINGLE_CLIENT,
    SCENARIO.GET_ROOM_DATA
  ].includes(options.scenario) || Boolean(options.ticket);
  const login = needsLogin ? await fetchTicket(options) : null;

  if (login) {
    console.log("login:", JSON.stringify({
      playerId: login.playerId,
      hasAccessToken: Boolean(login.accessToken),
      ticketPreview: `${login.ticket.slice(0, 16)}...`
    }, null, 2));
  }

  const client = new TcpProtocolClient(options, "client");
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
      case SCENARIO.START_GAME_SINGLE_CLIENT:
        await runStartGameSingleClient(client, options, login);
        break;
      case SCENARIO.GET_ROOM_DATA:
        await runGetRoomData(client, options, login);
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













