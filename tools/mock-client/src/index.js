import net from "node:net";
import process from "node:process";
import readline from "node:readline/promises";
import { Readable, Writable } from "node:stream";

const MAGIC = 0x4D53; // 'MS' for chat-server
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
  FRAME_BUNDLE_PUSH: 1203,
  ROOM_FRAME_RATE_PUSH: 1204,
  // Chat (1401-1422)
  CHAT_PRIVATE_REQ: 1401,
  CHAT_PRIVATE_RES: 1402,
  CHAT_GROUP_REQ: 1403,
  CHAT_GROUP_RES: 1404,
  CHAT_PUSH: 1405,
  GROUP_CREATE_REQ: 1411,
  GROUP_CREATE_RES: 1412,
  GROUP_JOIN_REQ: 1413,
  GROUP_JOIN_RES: 1414,
  GROUP_LEAVE_REQ: 1415,
  GROUP_LEAVE_RES: 1416,
  GROUP_DISMISS_REQ: 1417,
  GROUP_DISMISS_RES: 1418,
  GROUP_LIST_REQ: 1419,
  GROUP_LIST_RES: 1420,
  CHAT_HISTORY_REQ: 1421,
  CHAT_HISTORY_RES: 1422,
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
  GET_ROOM_DATA: "get-room-data",
  GET_ROOM_DATA_IN_ROOM: "get-room-data-in-room",
  // Chat scenarios
  CHAT_PRIVATE: "chat-private",
  CHAT_GROUP: "chat-group",
  GROUP_CREATE: "group-create",
  GROUP_JOIN: "group-join",
  GROUP_LEAVE: "group-leave",
  GROUP_DISMISS: "group-dismiss",
  GROUP_LIST: "group-list",
  CHAT_HISTORY: "chat-history",
  CHAT_TWO_CLIENT: "chat-two-client",
  CHAT_PRIVATE_TWO_CLIENT: "chat-private-two-client",
  CHAT_INTERACTIVE: "chat-interactive"
};

function parseArgs(argv) {
  const result = {
    host: "127.0.0.1",
    port: 7000,
    chatPort: 9001,
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
    idEnd: 1000,
    // Chat args
    targetId: "",
    groupId: "",
    content: "Hello from mock-client!",
    groupName: "",
    limit: 20,
    beforeTime: 0
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
    } else if (arg === "--chat-port" && next) {
      result.chatPort = Number.parseInt(next, 10);
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
    } else if (arg === "--target-id" && next) {
      result.targetId = next;
      index += 1;
    } else if (arg === "--group-id" && next) {
      result.groupId = next;
      index += 1;
    } else if (arg === "--content" && next) {
      result.content = next;
      index += 1;
    } else if (arg === "--group-name" && next) {
      result.groupName = next;
      index += 1;
    } else if (arg === "--limit" && next) {
      result.limit = Number.parseInt(next, 10);
      index += 1;
    } else if (arg === "--before-time" && next) {
      result.beforeTime = Number.parseInt(next, 10);
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

// Chat server uses ChatAuthReq with ticket in field 2 (token)
function encodeChatAuthReq(ticket) {
  return encodeStringField(2, ticket);
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

function encodeUInt32Field(fieldNumber, value) {
  const fieldKey = fieldNumber << 3;
  return Buffer.concat([encodeVarint(fieldKey), encodeVarint(value)]);
}

function encodePlayerInputReq(frameId, action, payloadJson) {
  return Buffer.concat([
    encodeUInt32Field(1, frameId),
    encodeStringField(2, action),
    encodeStringField(3, payloadJson)
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

// Chat encode functions
function encodeChatPrivateReq(targetId, content) {
  return Buffer.concat([
    encodeStringField(1, targetId),
    encodeStringField(2, content)
  ]);
}

function encodeChatGroupReq(groupId, content) {
  return Buffer.concat([
    encodeStringField(1, groupId),
    encodeStringField(2, content)
  ]);
}

function encodeGroupCreateReq(name) {
  return encodeStringField(1, name);
}

function encodeGroupJoinReq(groupId) {
  return encodeStringField(1, groupId);
}

function encodeGroupLeaveReq(groupId) {
  return encodeStringField(1, groupId);
}

function encodeGroupDismissReq(groupId) {
  return encodeStringField(1, groupId);
}

function encodeGroupListReq() {
  return Buffer.alloc(0);
}

function encodeChatHistoryReq(chatType, targetId, beforeTime, limit) {
  return Buffer.concat([
    encodeInt32Field(1, chatType),
    encodeStringField(2, targetId),
    encodeInt64Field(3, beforeTime),
    encodeInt32Field(4, limit)
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

function readUInt32(fields, fieldNumber) {
  return Number(fields.get(fieldNumber) || 0n);
}

function decodeFrameInput(buffer) {
  const fields = decodeFieldsWithRepeated(buffer);
  return {
    playerId: readString(fields, 1),
    action: readString(fields, 2),
    payloadJson: readString(fields, 3)
  };
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

function decodeGroupInfo(buffer) {
  const fields = decodeFieldsWithRepeated(buffer);
  return {
    groupId: readString(fields, 1),
    name: readString(fields, 2),
    memberCount: readUInt32(fields, 3)
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
    case MESSAGE_TYPE.FRAME_BUNDLE_PUSH: {
      const inputsRaw = fields.get(4);
      let inputs = [];
      if (inputsRaw) {
        if (Array.isArray(inputsRaw)) {
          inputs = inputsRaw.map(decodeFrameInput);
        } else {
          inputs = [decodeFrameInput(inputsRaw)];
        }
      }
      return {
        roomId: readString(fields, 1),
        frameId: readUInt32(fields, 2),
        fps: readUInt32(fields, 3),
        inputs,
        isSilentFrame: readBool(fields, 5)
      };
    }
    case MESSAGE_TYPE.ROOM_FRAME_RATE_PUSH:
      return {
        roomId: readString(fields, 1),
        fps: readUInt32(fields, 2),
        reason: readString(fields, 3)
      };
    // Chat responses
    case MESSAGE_TYPE.CHAT_PRIVATE_RES:
      return {
        ok: readBool(fields, 1),
        errorCode: readString(fields, 2),
        msgId: readString(fields, 3)
      };
    case MESSAGE_TYPE.CHAT_GROUP_RES:
      return {
        ok: readBool(fields, 1),
        errorCode: readString(fields, 2),
        msgId: readString(fields, 3)
      };
    case MESSAGE_TYPE.CHAT_PUSH:
      return {
        msgId: readString(fields, 1),
        chatType: readUInt32(fields, 2),
        senderId: readString(fields, 3),
        senderName: readString(fields, 4),
        content: readString(fields, 5),
        timestamp: readInt64(fields, 6),
        targetId: readString(fields, 7),
        groupId: readString(fields, 8)
      };
    case MESSAGE_TYPE.GROUP_CREATE_RES:
      return {
        ok: readBool(fields, 1),
        groupId: readString(fields, 2),
        errorCode: readString(fields, 3)
      };
    case MESSAGE_TYPE.GROUP_JOIN_RES:
      return {
        ok: readBool(fields, 1),
        errorCode: readString(fields, 3)
      };
    case MESSAGE_TYPE.GROUP_LEAVE_RES:
      return {
        ok: readBool(fields, 1),
        errorCode: readString(fields, 3)
      };
    case MESSAGE_TYPE.GROUP_DISMISS_RES:
      return {
        ok: readBool(fields, 1),
        errorCode: readString(fields, 3)
      };
    case MESSAGE_TYPE.GROUP_LIST_RES: {
      const groupsRaw = fields.get(1);
      let groups = [];
      if (groupsRaw) {
        if (Array.isArray(groupsRaw)) {
          groups = groupsRaw.map(decodeGroupInfo);
        } else {
          groups = [decodeGroupInfo(groupsRaw)];
        }
      }
      return { groups };
    }
    case MESSAGE_TYPE.CHAT_HISTORY_RES: {
      const messagesRaw = fields.get(1);
      let messages = [];
      if (messagesRaw) {
        if (Array.isArray(messagesRaw)) {
          messages = messagesRaw.map((buf) => {
            const f = decodeFieldsWithRepeated(buf);
            return {
              msgId: readString(f, 1),
              chatType: readUInt32(f, 2),
              senderId: readString(f, 3),
              senderName: readString(f, 4),
              content: readString(f, 5),
              timestamp: readInt64(f, 6),
              targetId: readString(f, 7),
              groupId: readString(f, 8)
            };
          });
        } else {
          const f = decodeFieldsWithRepeated(messagesRaw);
          messages = [{
            msgId: readString(f, 1),
            chatType: readUInt32(f, 2),
            senderId: readString(f, 3),
            senderName: readString(f, 4),
            content: readString(f, 5),
            timestamp: readInt64(f, 6),
            targetId: readString(f, 7),
            groupId: readString(f, 8)
          }];
        }
      }
      return { messages };
    }
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
        // Scan forward to find next potential magic marker
        let foundIdx = -1;
        for (let i = 1; i <= this.buffer.length - HEADER_LEN; i++) {
          if (this.buffer.readUInt16BE(i) === MAGIC) {
            foundIdx = i;
            break;
          }
        }

        if (foundIdx > 0) {
          // Found potential magic at foundIdx, skip bytes before it
          const skipped = foundIdx;
          if (skipped <= 16) {
            // Only log if we skipped a small amount (likely just misalignment)
            console.warn(`Invalid magic ${magic} at offset 0, skipping ${skipped} bytes to find magic`);
          }
          this.buffer = this.buffer.subarray(foundIdx);
          continue;
        }

        // No magic found in entire buffer - discard everything and wait for more data
        // This can happen if we received garbage or partial data
        if (this.buffer.length > 64) {
          // Show hex dump of first 64 bytes to help diagnose
          const hexDump = this.buffer.subarray(0, 64).toString("hex");
          console.warn(`No magic found in ${this.buffer.length} bytes, discarding. First 64 bytes: ${hexDump}`);
        }
        this.buffer = Buffer.alloc(0);
        return;
      }

      const messageType = this.buffer.readUInt16BE(4);
      const seq = this.buffer.readUInt32BE(6);
      const bodyLen = this.buffer.readUInt32BE(10);

      // Sanity check: if bodyLen is unreasonably large, we likely have garbage
      // Maximum reasonable body size is 1MB
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

async function authenticateClient(client, options, login, seq = 1, encodeAuthFn = encodeAuthReq) {
  await client.send(MESSAGE_TYPE.AUTH_REQ, seq, encodeAuthFn(login.ticket));
  const auth = printResponse(`${client.label}.auth`, await client.readNextPacket(options.timeoutMs));
  if (!auth.ok) {
    throw new Error(`${client.label} auth failed: ${auth.errorCode}`);
  }
}

async function waitForFrameBundle(client, timeoutMs, expectedAction = null) {
  return client.readUntil(
    timeoutMs,
    (packet, decoded) => {
      if (packet.messageType !== MESSAGE_TYPE.FRAME_BUNDLE_PUSH) {
        return false;
      }
      if (decoded.isSilentFrame) {
        return false;
      }
      if (!expectedAction) {
        return true;
      }
      return decoded.inputs.some((input) => input.action === expectedAction);
    },
    "frameBundle"
  );
}

async function delayBeforeFinalLeave(client, timeoutMs, delayMs = 10000) {
  console.log(`${client.label}.delayBeforeFinalLeave: waiting ${delayMs}ms before final leave`);
  const startedAt = Date.now();
  let pingSeq = 900000;

  while (Date.now() - startedAt < delayMs) {
    const remainingMs = delayMs - (Date.now() - startedAt);
    const sleepMs = Math.min(3000, Math.max(0, remainingMs));
    if (sleepMs > 0) {
      await new Promise((resolve) => setTimeout(resolve, sleepMs));
    }
    if (Date.now() - startedAt >= delayMs) {
      break;
    }

    await client.send(MESSAGE_TYPE.PING_REQ, pingSeq, encodePingReq(Date.now()));
    await client.readUntil(
      timeoutMs,
      (packet) => packet.messageType === MESSAGE_TYPE.PING_RES && packet.seq === pingSeq,
      "delayPing"
    );
    pingSeq += 1;
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

  await delayBeforeFinalLeave(client, options.timeoutMs);
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

async function runGetRoomDataInRoom(client, options, login) {
  await authenticateClient(client, options, login, 1);

  await client.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(options.roomId));
  const joinRes = printResponse(`${client.label}.roomJoin`, await client.readNextPacket(options.timeoutMs));
  if (!joinRes.ok) {
    throw new Error(`room join failed: ${joinRes.errorCode}`);
  }

  printResponse(`${client.label}.roomStatePush(join)`, await client.readNextPacket(options.timeoutMs));

  await client.send(
    MESSAGE_TYPE.GET_ROOM_DATA_REQ,
    3,
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

  await delayBeforeFinalLeave(client, options.timeoutMs);
  await client.send(MESSAGE_TYPE.ROOM_LEAVE_REQ, 4, encodeRoomLeaveReq());
  const leaveRes = printResponse(`${client.label}.roomLeave`, await client.readNextPacket(options.timeoutMs));
  if (!leaveRes.ok) {
    throw new Error(`room leave failed: ${leaveRes.errorCode}`);
  }
}

async function runGameplayRoundtrip(options) {
  const loginA = await fetchTicket(options, { guestId: `${options.roomId}-owner` });
  const loginB = await fetchTicket(options, { guestId: `${options.roomId}-member` });

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
    await clientA.send(MESSAGE_TYPE.PLAYER_INPUT_REQ, 5, encodePlayerInputReq(1, "move", payloadJson));
    const inputRes = printResponse("clientA.playerInput", await clientA.readNextPacket(options.timeoutMs));
    if (!inputRes.ok) {
      throw new Error(`clientA player input failed: ${inputRes.errorCode}`);
    }

    const framePushA = await waitForFrameBundle(clientA, options.timeoutMs, "move");
    const framePushB = await waitForFrameBundle(clientB, options.timeoutMs, "move");
    if (framePushA.inputs.length !== 1 || framePushB.inputs.length !== 1) {
      throw new Error("expected one frame input in the first non-silent frame");
    }
    if (framePushA.inputs[0].action !== "move" || framePushB.inputs[0].action !== "move") {
      throw new Error("expected frame bundle action to be move");
    }
    if (framePushA.inputs[0].payloadJson !== payloadJson || framePushB.inputs[0].payloadJson !== payloadJson) {
      throw new Error("expected frame bundle payload to match input payload");
    }
    if (framePushA.inputs[0].playerId !== loginA.playerId || framePushB.inputs[0].playerId !== loginA.playerId) {
      throw new Error("expected frame bundle playerId to be the input sender");
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

    await delayBeforeFinalLeave(clientB, options.timeoutMs);
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
  const loginA = await fetchTicket(options, { guestId: `${options.roomId}-owner` });
  const loginB = await fetchTicket(options, { guestId: `${options.roomId}-member` });

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

    await delayBeforeFinalLeave(clientB, options.timeoutMs);
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
  const loginA = await fetchTicket(options, { guestId: `${options.roomId}-owner` });
  const loginB = await fetchTicket(options, { guestId: `${options.roomId}-member` });

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

    await delayBeforeFinalLeave(clientB, options.timeoutMs);
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

// Chat scenario helper
async function connectToChatServer(options) {
  const chatOptions = { ...options, port: options.chatPort };
  const client = new TcpProtocolClient(chatOptions, "chat");
  await client.connect();
  return client;
}

async function runChatPrivate(options) {
  const login = await fetchTicket(options);
  console.log("login:", JSON.stringify({ playerId: login.playerId }, null, 2));

  const client = await connectToChatServer(options);
  await authenticateClient(client, options, login, 1, encodeChatAuthReq);

  const targetId = options.targetId || "target-player-id";
  await client.send(MESSAGE_TYPE.CHAT_PRIVATE_REQ, 2, encodeChatPrivateReq(targetId, options.content));
  const res = printResponse("chat.privateRes", await client.readNextPacket(options.timeoutMs));
  if (!res.ok) {
    throw new Error(`private chat failed: ${res.errorCode}`);
  }
  console.log("private chat sent successfully, msgId:", res.msgId);

  client.close();
}

async function runChatGroup(options) {
  const login = await fetchTicket(options);
  console.log("login:", JSON.stringify({ playerId: login.playerId }, null, 2));

  const client = await connectToChatServer(options);
  await authenticateClient(client, options, login, 1);

  const groupId = options.groupId || "grp_test";
  await client.send(MESSAGE_TYPE.CHAT_GROUP_REQ, 2, encodeChatGroupReq(groupId, options.content));
  const res = printResponse("chat.groupRes", await client.readNextPacket(options.timeoutMs));
  if (!res.ok) {
    throw new Error(`group chat failed: ${res.errorCode}`);
  }
  console.log("group chat sent successfully, msgId:", res.msgId);

  client.close();
}

async function runGroupCreate(options) {
  const login = await fetchTicket(options);
  console.log("login:", JSON.stringify({ playerId: login.playerId }, null, 2));

  const client = await connectToChatServer(options);
  await authenticateClient(client, options, login, 1, encodeChatAuthReq);

  const groupName = options.groupName || "Test Group";
  await client.send(MESSAGE_TYPE.GROUP_CREATE_REQ, 2, encodeGroupCreateReq(groupName));
  const res = printResponse("chat.groupCreateRes", await client.readNextPacket(options.timeoutMs));
  if (!res.ok) {
    throw new Error(`group create failed: ${res.errorCode}`);
  }
  console.log("group created, groupId:", res.groupId);

  client.close();
  return res.groupId;
}

async function runGroupJoin(options, groupId) {
  const login = await fetchTicket(options, { guestId: "joiner-" + options.roomId });
  console.log("login:", JSON.stringify({ playerId: login.playerId }, null, 2));

  const client = await connectToChatServer(options);
  await authenticateClient(client, options, login, 1, encodeChatAuthReq);

  await client.send(MESSAGE_TYPE.GROUP_JOIN_REQ, 2, encodeGroupJoinReq(groupId));
  const res = printResponse("chat.groupJoinRes", await client.readNextPacket(options.timeoutMs));
  if (!res.ok) {
    throw new Error(`group join failed: ${res.errorCode}`);
  }
  console.log("joined group successfully");

  client.close();
}

async function runGroupLeave(options, groupId) {
  const login = await fetchTicket(options);
  console.log("login:", JSON.stringify({ playerId: login.playerId }, null, 2));

  const client = await connectToChatServer(options);
  await authenticateClient(client, options, login, 1, encodeChatAuthReq);

  await client.send(MESSAGE_TYPE.GROUP_LEAVE_REQ, 2, encodeGroupLeaveReq(groupId));
  const res = printResponse("chat.groupLeaveRes", await client.readNextPacket(options.timeoutMs));
  if (!res.ok) {
    throw new Error(`group leave failed: ${res.errorCode}`);
  }
  console.log("left group successfully");

  client.close();
}

async function runGroupDismiss(options, groupId) {
  const login = await fetchTicket(options);
  console.log("login:", JSON.stringify({ playerId: login.playerId }, null, 2));

  const client = await connectToChatServer(options);
  await authenticateClient(client, options, login, 1, encodeChatAuthReq);

  await client.send(MESSAGE_TYPE.GROUP_DISMISS_REQ, 2, encodeGroupDismissReq(groupId));
  const res = printResponse("chat.groupDismissRes", await client.readNextPacket(options.timeoutMs));
  if (!res.ok) {
    throw new Error(`group dismiss failed: ${res.errorCode}`);
  }
  console.log("group dismissed successfully");

  client.close();
}

async function runGroupList(options) {
  const login = await fetchTicket(options);
  console.log("login:", JSON.stringify({ playerId: login.playerId }, null, 2));

  const client = await connectToChatServer(options);
  await authenticateClient(client, options, login, 1, encodeChatAuthReq);

  await client.send(MESSAGE_TYPE.GROUP_LIST_REQ, 2, encodeGroupListReq());
  const res = printResponse("chat.groupListRes", await client.readNextPacket(options.timeoutMs));
  console.log("group list:", JSON.stringify(res.groups, null, 2));

  client.close();
}

async function runChatHistory(options) {
  const login = await fetchTicket(options);
  console.log("login:", JSON.stringify({ playerId: login.playerId }, null, 2));

  const client = await connectToChatServer(options);
  await authenticateClient(client, options, login, 1, encodeChatAuthReq);

  const chatType = 1; // private
  const targetId = options.targetId || "";
  const beforeTime = options.beforeTime || 0;
  const limit = options.limit || 20;

  await client.send(MESSAGE_TYPE.CHAT_HISTORY_REQ, 2, encodeChatHistoryReq(chatType, targetId, beforeTime, limit));
  const res = printResponse("chat.historyRes", await client.readNextPacket(options.timeoutMs));
  console.log("chat history:", JSON.stringify(res.messages, null, 2));

  client.close();
}

async function runChatTwoClient(options) {
  const loginA = await fetchTicket(options, { guestId: `${options.roomId}-owner` });
  const loginB = await fetchTicket(options, { guestId: `${options.roomId}-member` });

  console.log("clientA.login:", JSON.stringify({ playerId: loginA.playerId }, null, 2));
  console.log("clientB.login:", JSON.stringify({ playerId: loginB.playerId }, null, 2));

  // Create a group first
  const clientA = await connectToChatServer(options);
  await authenticateClient(clientA, options, loginA, 1, encodeChatAuthReq);

  const groupName = options.groupName || "Test Group";
  await clientA.send(MESSAGE_TYPE.GROUP_CREATE_REQ, 2, encodeGroupCreateReq(groupName));
  const createRes = printResponse("clientA.groupCreate", await clientA.readNextPacket(options.timeoutMs));
  if (!createRes.ok) {
    throw new Error(`group create failed: ${createRes.errorCode}`);
  }
  const groupId = createRes.groupId;
  console.log("group created:", groupId);

  // Client B joins
  const clientB = await connectToChatServer(options);
  await authenticateClient(clientB, options, loginB, 1, encodeChatAuthReq);

  await clientB.send(MESSAGE_TYPE.GROUP_JOIN_REQ, 3, encodeGroupJoinReq(groupId));
  const joinRes = printResponse("clientB.groupJoin", await clientB.readNextPacket(options.timeoutMs));
  if (!joinRes.ok) {
    throw new Error(`group join failed: ${joinRes.errorCode}`);
  }
  console.log("clientB joined group");

  // Client A sends a group message
  await clientA.send(MESSAGE_TYPE.CHAT_GROUP_REQ, 3, encodeChatGroupReq(groupId, options.content));
  const chatRes = printResponse("clientA.groupChat", await clientA.readNextPacket(options.timeoutMs));
  if (!chatRes.ok) {
    throw new Error(`group chat failed: ${chatRes.errorCode}`);
  }
  console.log("clientA sent group message");

  // Client B receives push
  const push = await clientB.readUntil(
    options.timeoutMs,
    (packet) => packet.messageType === MESSAGE_TYPE.CHAT_PUSH,
    "chatPush"
  );
  console.log("clientB received push:", JSON.stringify(push, null, 2));

  clientA.close();
  clientB.close();
}

async function runChatPrivateTwoClient(options) {
  const loginA = await fetchTicket(options, { guestId: `${options.roomId}-owner` });
  const loginB = await fetchTicket(options, { guestId: `${options.roomId}-member` });

  console.log("clientA.login:", JSON.stringify({ playerId: loginA.playerId }, null, 2));
  console.log("clientB.login:", JSON.stringify({ playerId: loginB.playerId }, null, 2));

  // Client A connects and waits for messages
  const clientA = await connectToChatServer(options);
  await authenticateClient(clientA, options, loginA, 1, encodeChatAuthReq);
  console.log("clientA connected, waiting for private message...");

  // Client B connects and sends private message to A
  const clientB = await connectToChatServer(options);
  await authenticateClient(clientB, options, loginB, 1, encodeChatAuthReq);

  await clientB.send(MESSAGE_TYPE.CHAT_PRIVATE_REQ, 2, encodeChatPrivateReq(loginA.playerId, options.content));
  const chatRes = printResponse("clientB.privateChat", await clientB.readNextPacket(options.timeoutMs));
  if (!chatRes.ok) {
    throw new Error(`private chat failed: ${chatRes.errorCode}`);
  }
  console.log("clientB sent private message to clientA");

  // Client A receives push
  const push = await clientA.readUntil(
    options.timeoutMs,
    (packet) => packet.messageType === MESSAGE_TYPE.CHAT_PUSH,
    "chatPush"
  );
  console.log("clientA received push:", JSON.stringify(push, null, 2));

  // Now A replies to B
  await clientA.send(MESSAGE_TYPE.CHAT_PRIVATE_REQ, 2, encodeChatPrivateReq(loginB.playerId, "Reply: " + options.content));
  const replyRes = printResponse("clientA.privateChat", await clientA.readNextPacket(options.timeoutMs));
  if (!replyRes.ok) {
    throw new Error(`reply chat failed: ${replyRes.errorCode}`);
  }
  console.log("clientA replied to clientB");

  // Client B receives A's reply
  const replyPush = await clientB.readUntil(
    options.timeoutMs,
    (packet) => packet.messageType === MESSAGE_TYPE.CHAT_PUSH,
    "chatPush"
  );
  console.log("clientB received reply push:", JSON.stringify(replyPush, null, 2));

  clientA.close();
  clientB.close();
}

async function runChatInteractive(options) {
  const loginA = await fetchTicket(options, { guestId: `${options.roomId}-owner` });
  const loginB = await fetchTicket(options, { guestId: `${options.roomId}-member` });

  console.log("clientA.playerId:", loginA.playerId);
  console.log("clientB.playerId:", loginB.playerId);
  console.log("");
  console.log("=== Interactive Chat ===");
  console.log("clientA (you) <---> clientB");
  console.log("Type messages and press Enter to send from clientA to clientB");
  console.log("clientB will auto-reply with your message prefixed with 'B: '");
  console.log("Press Ctrl+C to exit");
  console.log("");

  // Client A connects - this is "us"
  const clientA = await connectToChatServer(options);
  await authenticateClient(clientA, options, loginA, 1, encodeChatAuthReq);
  console.log("[connected as clientA, waiting for messages...]");

  // Client B connects - this is "other player"
  const clientB = await connectToChatServer(options);
  await authenticateClient(clientB, options, loginB, 1, encodeChatAuthReq);
  console.log("[clientB connected]");

  // Create readline interface for interactive input
  const rl = readline.createInterface({
    input: process.stdin,
    output: process.stdout
  });

  let seq = 2;
  const clientBPlayerId = loginB.playerId;
  let replyingToSeq = 1;

  // Helper to send message from clientA to clientB
  async function sendMessage(content) {
    await clientA.send(MESSAGE_TYPE.CHAT_PRIVATE_REQ, seq, encodeChatPrivateReq(clientBPlayerId, content));
    seq++;
  }

  // Task to handle clientB receiving messages and auto-reply
  const clientBTask = async () => {
    while (true) {
      try {
        const packet = await clientB.readNextPacket(60000);
        const decoded = decodeByMessageType(packet.messageType, packet.body);

        if (packet.messageType === MESSAGE_TYPE.CHAT_PUSH) {
          console.log(`\n[clientB received from ${decoded.senderId}]: ${decoded.content}`);

          // Auto reply
          const replyContent = `B: ${decoded.content}`;
          await clientB.send(MESSAGE_TYPE.CHAT_PRIVATE_REQ, replyingToSeq, encodeChatPrivateReq(decoded.senderId, replyContent));
          replyingToSeq++;
        }
      } catch (e) {
        // Timeout is normal, just continue
        if (!e.message.includes("Timed out")) {
          console.error("clientB read error:", e.message);
        }
      }
    }
  };

  // Task to handle clientA receiving messages
  const clientATask = async () => {
    while (true) {
      try {
        const packet = await clientA.readNextPacket(60000);
        const decoded = decodeByMessageType(packet.messageType, packet.body);

        if (packet.messageType === MESSAGE_TYPE.CHAT_PUSH) {
          console.log(`\n[received from ${decoded.senderId}]: ${decoded.content}`);
        }
      } catch (e) {
        // Timeout is normal, just continue
        if (!e.message.includes("Timed out")) {
          console.error("clientA read error:", e.message);
        }
      }
    }
  };

  // Start both tasks in background
  clientBTask();
  clientATask();

  // Main input loop
  const askQuestion = async () => {
    try {
      const answer = await rl.question("> ");
      if (answer.trim()) {
        await sendMessage(answer.trim());
      }
      askQuestion();
    } catch {
      // Input closed, exit
    }
  };

  askQuestion();

  // Keep running until interrupted
  await new Promise(() => {});
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
    SCENARIO.GET_ROOM_DATA,
    SCENARIO.GET_ROOM_DATA_IN_ROOM
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
      case SCENARIO.GET_ROOM_DATA_IN_ROOM:
        await runGetRoomDataInRoom(client, options, login);
        break;
      // Chat scenarios
      case SCENARIO.CHAT_PRIVATE:
        await runChatPrivate(options);
        break;
      case SCENARIO.CHAT_GROUP:
        await runChatGroup(options);
        break;
      case SCENARIO.GROUP_CREATE:
        await runGroupCreate(options);
        break;
      case SCENARIO.GROUP_LIST:
        await runGroupList(options);
        break;
      case SCENARIO.CHAT_HISTORY:
        await runChatHistory(options);
        break;
      case SCENARIO.CHAT_TWO_CLIENT:
        await runChatTwoClient(options);
        break;
      case SCENARIO.CHAT_PRIVATE_TWO_CLIENT:
        await runChatPrivateTwoClient(options);
        break;
      case SCENARIO.CHAT_INTERACTIVE:
        await runChatInteractive(options);
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













