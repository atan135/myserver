import {
  encodeStringField,
  encodeBoolField,
  encodeInt64Field,
  encodeUInt32Field,
  encodeInt32Field,
  encodeFloatField,
  encodeVarint,
  decodeFieldsWithRepeated,
  readString,
  readStringList,
  readBool,
  readInt64,
  readUInt32,
  readFloat
} from "./protocol.js";
import { MESSAGE_TYPE } from "./constants.js";

// ============ Message Encoders ============

// Auth
export function encodeAuthReq(ticket) {
  return encodeStringField(1, ticket);
}

export function encodeChatAuthReq(ticket) {
  return encodeStringField(2, ticket);
}

export function encodePingReq(clientTime) {
  return encodeInt64Field(1, clientTime);
}

// Room
export function encodeRoomJoinReq(roomId, policyId = "") {
  const fields = [encodeStringField(1, roomId)];
  if (policyId) {
    fields.push(encodeStringField(2, policyId));
  }
  return Buffer.concat(fields);
}

export function encodeRoomReconnectReq(playerId) {
  return encodeStringField(1, playerId);
}

export function encodeRoomJoinAsObserverReq(roomId) {
  return encodeStringField(1, roomId);
}

export function encodeRoomLeaveReq() {
  return Buffer.alloc(0);
}

export function encodeRoomReadyReq(ready) {
  return encodeBoolField(1, ready);
}

export function encodeRoomStartReq() {
  return Buffer.alloc(0);
}

export function encodePlayerInputReq(frameId, action, payloadJson) {
  return Buffer.concat([
    encodeUInt32Field(1, frameId),
    encodeStringField(2, action),
    encodeStringField(3, payloadJson)
  ]);
}

export function encodeMoveInputReq(frameId, inputType, dirX = 0, dirY = 0, clientState = null) {
  const fields = [
    encodeUInt32Field(1, frameId),
    encodeInt32Field(2, inputType),
    encodeFloatField(3, dirX),
    encodeFloatField(4, dirY)
  ];

  if (clientState) {
    const hasClientState = clientState.hasClientState ?? true;
    fields.push(encodeBoolField(5, hasClientState));
    if (hasClientState) {
      fields.push(encodeFloatField(6, clientState.x ?? 0));
      fields.push(encodeFloatField(7, clientState.y ?? 0));
      fields.push(encodeUInt32Field(8, clientState.frameId ?? frameId));
    }
  }

  return Buffer.concat(fields);
}

export function encodeRoomEndReq(reason) {
  return encodeStringField(1, reason);
}

export function encodeGetRoomDataReq(idStart, idEnd) {
  return Buffer.concat([
    encodeInt32Field(1, idStart),
    encodeInt32Field(2, idEnd)
  ]);
}

// Match
export function encodeCreateMatchedRoomReq(matchId, roomId, playerIds, mode) {
  const playerIdsBuffers = playerIds.map((id) => encodeStringField(3, id));
  return Buffer.concat([
    encodeStringField(1, matchId),
    encodeStringField(2, roomId),
    ...playerIdsBuffers,
    encodeStringField(4, mode)
  ]);
}

// Chat
export function encodeChatPrivateReq(targetId, content) {
  return Buffer.concat([
    encodeStringField(1, targetId),
    encodeStringField(2, content)
  ]);
}

export function encodeChatGroupReq(groupId, content) {
  return Buffer.concat([
    encodeStringField(1, groupId),
    encodeStringField(2, content)
  ]);
}

export function encodeGroupCreateReq(name) {
  return encodeStringField(1, name);
}

export function encodeGroupJoinReq(groupId) {
  return encodeStringField(1, groupId);
}

export function encodeGroupLeaveReq(groupId) {
  return encodeStringField(1, groupId);
}

export function encodeGroupDismissReq(groupId) {
  return encodeStringField(1, groupId);
}

export function encodeGroupListReq() {
  return Buffer.alloc(0);
}

export function encodeChatHistoryReq(chatType, targetId, beforeTime, limit) {
  return Buffer.concat([
    encodeInt32Field(1, chatType),
    encodeStringField(2, targetId),
    encodeInt64Field(3, beforeTime),
    encodeInt32Field(4, limit)
  ]);
}

// ============ Inventory Encoders ============

export function encodeItemEquipReq(itemUid, equipSlot) {
  return Buffer.concat([
    encodeInt64Field(1, itemUid),
    encodeStringField(2, equipSlot)
  ]);
}

export function encodeItemUseReq(itemUid) {
  return encodeInt64Field(1, itemUid);
}

export function encodeItemDiscardReq(itemUid, count) {
  return Buffer.concat([
    encodeInt64Field(1, itemUid),
    encodeUInt32Field(2, count)
  ]);
}

export function encodeWarehouseAccessReq(action, itemUid, count) {
  return Buffer.concat([
    encodeStringField(1, action),
    encodeInt64Field(2, itemUid),
    encodeUInt32Field(3, count)
  ]);
}

export function encodeItemAddReq(itemId, count, binded) {
  return Buffer.concat([
    encodeInt32Field(1, itemId),
    encodeUInt32Field(2, count),
    encodeBoolField(3, binded)
  ]);
}

export function encodeGetInventoryReq() {
  return Buffer.alloc(0);
}

// ============ Message Decoders ============

function decodeFrameInput(buffer) {
  const fields = decodeFieldsWithRepeated(buffer);
  return {
    playerId: readString(fields, 1),
    action: readString(fields, 2),
    payloadJson: readString(fields, 3),
    frameId: readUInt32(fields, 4) || 0
  };
}

function decodeRepeatedMessage(fields, fieldNumber, decoder) {
  const raw = fields.get(fieldNumber);
  if (!raw) {
    return [];
  }
  return Array.isArray(raw) ? raw.map(decoder) : [decoder(raw)];
}

function decodeEntityTransform(buffer) {
  const fields = decodeFieldsWithRepeated(buffer);
  return {
    entityId: readInt64(fields, 1),
    playerId: readString(fields, 2),
    sceneId: readUInt32(fields, 3),
    x: readFloat(fields, 4),
    y: readFloat(fields, 5),
    dirX: readFloat(fields, 6),
    dirY: readFloat(fields, 7),
    moving: readBool(fields, 8),
    lastInputFrame: readUInt32(fields, 9)
  };
}

function decodeRoomMember(buffer) {
  const fields = decodeFieldsWithRepeated(buffer);
  return {
    playerId: readString(fields, 1),
    ready: readBool(fields, 2),
    isOwner: readBool(fields, 3),
    offline: readBool(fields, 4),
    role: readUInt32(fields, 5) // 0=Player, 1=Observer
  };
}

function decodeRoomSnapshot(buffer) {
  const fields = decodeFieldsWithRepeated(buffer);
  return {
    roomId: readString(fields, 1),
    ownerPlayerId: readString(fields, 2),
    state: readString(fields, 3),
    members: decodeRepeatedMessage(fields, 4, decodeRoomMember),
    currentFrameId: readUInt32(fields, 5) || 0,
    gameState: readString(fields, 6) || ""
  };
}

function decodeMovementRecoveryState(buffer) {
  const fields = decodeFieldsWithRepeated(buffer);
  return {
    frameId: readUInt32(fields, 1) || 0,
    entities: decodeRepeatedMessage(fields, 2, decodeEntityTransform),
    correctionKind: readUInt32(fields, 3) || 0,
    reasonCode: readUInt32(fields, 4) || 0,
    referenceFrameId: readUInt32(fields, 5) || 0,
    aoiEnabled: readBool(fields, 6),
    aoiRadius: readFloat(fields, 7)
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

function decodeItem(buffer) {
  const fields = decodeFieldsWithRepeated(buffer);
  return {
    uid: readInt64(fields, 1),
    itemId: readUInt32(fields, 2),
    count: readUInt32(fields, 3),
    binded: readBool(fields, 4)
  };
}

function decodeAttrPanel(buffer) {
  const fields = decodeFieldsWithRepeated(buffer);
  return {
    hp: readInt64(fields, 1),
    maxHp: readInt64(fields, 2),
    attack: readInt64(fields, 3),
    defense: readInt64(fields, 4),
    speed: readUInt32(fields, 5),
    critRate: readFloat(fields, 6),
    critDmg: readFloat(fields, 7)
  };
}

function decodeAttrRecord(buffer) {
  const fields = decodeFieldsWithRepeated(buffer);
  return {
    source: readString(fields, 1),
    attrType: readString(fields, 2),
    value: readUInt32(fields, 3)
  };
}

/**
 * Decode a message body by message type
 * @param {number} messageType
 * @param {Buffer} body
 * @returns {Object}
 */
export function decodeByMessageType(messageType, body) {
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
    case MESSAGE_TYPE.CHAT_AUTH_RES:
      return {
        ok: readBool(fields, 1),
        errorCode: readString(fields, 2)
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
    case MESSAGE_TYPE.MOVE_INPUT_RES:
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
    case MESSAGE_TYPE.ROOM_RECONNECT_RES:
    case MESSAGE_TYPE.ROOM_JOIN_AS_OBSERVER_RES: {
      return {
        ok: readBool(fields, 1),
        roomId: readString(fields, 2),
        errorCode: readString(fields, 3),
        snapshot: fields.get(4) ? decodeRoomSnapshot(fields.get(4)) : null,
        currentFrameId: readUInt32(fields, 5) || 0,
        recentInputs: decodeRepeatedMessage(fields, 6, decodeFrameInput),
        waitingFrameId: readUInt32(fields, 7) || 0,
        waitingInputs: decodeRepeatedMessage(fields, 8, decodeFrameInput),
        inputDelayFrames: readUInt32(fields, 9) || 0,
        movementRecovery: fields.get(10) ? decodeMovementRecoveryState(fields.get(10)) : null
      };
    }
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
      return {
        roomId: readString(fields, 1),
        frameId: readUInt32(fields, 2),
        fps: readUInt32(fields, 3),
        inputs: decodeRepeatedMessage(fields, 4, decodeFrameInput),
        isSilentFrame: readBool(fields, 5),
        snapshot: fields.get(6) ? decodeRoomSnapshot(fields.get(6)) : null
      };
    }
    case MESSAGE_TYPE.ROOM_FRAME_RATE_PUSH:
      return {
        roomId: readString(fields, 1),
        fps: readUInt32(fields, 2),
        reason: readString(fields, 3)
      };
    case MESSAGE_TYPE.MOVEMENT_SNAPSHOT_PUSH: {
      return {
        roomId: readString(fields, 1),
        frameId: readUInt32(fields, 2),
        entities: decodeRepeatedMessage(fields, 3, decodeEntityTransform),
        fullSync: readBool(fields, 4),
        reason: readString(fields, 5),
        correctionKind: readUInt32(fields, 6) || 0,
        reasonCode: readUInt32(fields, 7) || 0,
        targetPlayerIds: readStringList(fields, 8),
        referenceFrameId: readUInt32(fields, 9) || 0
      };
    }
    case MESSAGE_TYPE.MOVEMENT_REJECT_PUSH:
      return {
        roomId: readString(fields, 1),
        frameId: readUInt32(fields, 2),
        playerId: readString(fields, 3),
        errorCode: readString(fields, 4),
        corrected: fields.get(5) ? decodeEntityTransform(fields.get(5)) : null,
        correctionKind: readUInt32(fields, 6) || 0,
        reasonCode: readUInt32(fields, 7) || 0,
        referenceFrameId: readUInt32(fields, 8) || 0,
        hasClientState: readBool(fields, 9),
        clientX: readFloat(fields, 10),
        clientY: readFloat(fields, 11),
        serverX: readFloat(fields, 12),
        serverY: readFloat(fields, 13)
      };
    case MESSAGE_TYPE.CREATE_MATCHED_ROOM_RES:
      return {
        ok: readBool(fields, 1),
        roomId: readString(fields, 2),
        errorCode: readString(fields, 3),
        snapshot: fields.get(4) ? decodeRoomSnapshot(fields.get(4)) : null
      };
    case MESSAGE_TYPE.ITEM_EQUIP_RES:
      return {
        ok: readBool(fields, 1),
        errorCode: readString(fields, 2),
        unequippedItem: fields.get(3) ? decodeItem(fields.get(3)) : null
      };
    case MESSAGE_TYPE.ITEM_USE_RES:
      return {
        ok: readBool(fields, 1),
        errorCode: readString(fields, 2),
        hpChange: readInt64(fields, 3),
        newBuffIds: fields.get(4) ? (Array.isArray(fields.get(4)) ? fields.get(4).map(f => readUInt32(decodeFieldsWithRepeated(f), 1)) : [readUInt32(decodeFieldsWithRepeated(fields.get(4)), 1)]) : []
      };
    case MESSAGE_TYPE.ITEM_DISCARD_RES:
      return {
        ok: readBool(fields, 1),
        errorCode: readString(fields, 2)
      };
    case MESSAGE_TYPE.ITEM_ADD_RES:
      return {
        ok: readBool(fields, 1),
        errorCode: readString(fields, 2),
        item: fields.get(3) ? decodeItem(fields.get(3)) : null
      };
    case MESSAGE_TYPE.WAREHOUSE_ACCESS_RES:
      return {
        ok: readBool(fields, 1),
        errorCode: readString(fields, 2)
      };
    case MESSAGE_TYPE.GET_INVENTORY_RES: {
      // Field layout: 1=ok(bool), 2=error_code(string), 3=inventory_items(repeated Item), 4=warehouse_items(repeated Item)
      const invRaw = fields.get(3);
      const whRaw = fields.get(4);
      return {
        ok: readBool(fields, 1),
        errorCode: readString(fields, 2),
        inventoryItems: invRaw ? (Array.isArray(invRaw) ? invRaw.map(decodeItem) : [decodeItem(invRaw)]) : [],
        warehouseItems: whRaw ? (Array.isArray(whRaw) ? whRaw.map(decodeItem) : [decodeItem(whRaw)]) : []
      };
    }
    // Inventory push messages
    case MESSAGE_TYPE.INVENTORY_UPDATE_PUSH: {
      const invRaw = fields.get(1);
      const whRaw = fields.get(2);
      return {
        inventoryItems: invRaw ? (Array.isArray(invRaw) ? invRaw.map(decodeItem) : [decodeItem(invRaw)]) : [],
        warehouseItems: whRaw ? (Array.isArray(whRaw) ? whRaw.map(decodeItem) : [decodeItem(whRaw)]) : []
      };
    }
    case MESSAGE_TYPE.ATTR_CHANGE_PUSH:
      return {
        base: fields.get(1) ? decodeAttrPanel(fields.get(1)) : null,
        bonus: fields.get(2) ? (Array.isArray(fields.get(2)) ? fields.get(2).map(decodeAttrRecord) : [decodeAttrRecord(fields.get(2))]) : [],
        final: fields.get(3) ? decodeAttrPanel(fields.get(3)) : null
      };
    case MESSAGE_TYPE.VISUAL_CHANGE_PUSH: {
      const buffsRaw = fields.get(2);
      return {
        appearance: readUInt32(fields, 1),
        activeBuffIds: buffsRaw ? (Array.isArray(buffsRaw) ? buffsRaw.map(f => readUInt32(decodeFieldsWithRepeated(f), 1)) : [readUInt32(decodeFieldsWithRepeated(buffsRaw), 1)]) : []
      };
    }
    case MESSAGE_TYPE.GROUP_CREATE_RES:
      return {
        ok: readBool(fields, 1),
        groupId: readString(fields, 2),
        errorCode: readString(fields, 3)
      };
    case MESSAGE_TYPE.GROUP_JOIN_RES:
    case MESSAGE_TYPE.GROUP_LEAVE_RES:
    case MESSAGE_TYPE.GROUP_DISMISS_RES:
      return {
        ok: readBool(fields, 1),
        errorCode: readString(fields, 2)
      };
    case MESSAGE_TYPE.GROUP_LIST_RES: {
      const groupsRaw = fields.get(1);
      return {
        groups: groupsRaw
          ? (Array.isArray(groupsRaw) ? groupsRaw.map(decodeGroupInfo) : [decodeGroupInfo(groupsRaw)])
          : []
      };
    }
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
    case MESSAGE_TYPE.MAIL_NOTIFY_PUSH:
      return {
        mailId: readString(fields, 1),
        title: readString(fields, 2),
        fromPlayerId: readString(fields, 3),
        mailType: readString(fields, 4),
        createdAt: readInt64(fields, 5)
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
