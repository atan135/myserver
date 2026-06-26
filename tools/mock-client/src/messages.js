import {
  encodeStringField,
  encodeMessageField,
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
  readInt32,
  readInt32List,
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

export function encodeRoomReconnectReq() {
  return Buffer.alloc(0);
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

export function encodePlayerInputReq(frameId, action, payloadJson, clientTimestampMs = Date.now()) {
  return Buffer.concat([
    encodeUInt32Field(1, frameId),
    encodeStringField(2, action),
    encodeStringField(3, payloadJson),
    encodeInt64Field(4, clientTimestampMs)
  ]);
}

export function encodeMoveInputReq(
  frameId,
  inputType,
  dirX = 0,
  dirY = 0,
  clientState = null,
  clientTimestampMs = Date.now()
) {
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

  fields.push(encodeInt64Field(9, clientState?.clientTimestampMs ?? clientTimestampMs));

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
export function encodeCreateMatchedRoomReq(matchId, roomId, characterIds, mode) {
  const characterIdsBuffers = characterIds.map((id) => encodeStringField(3, id));
  return Buffer.concat([
    encodeStringField(1, matchId),
    encodeStringField(2, roomId),
    ...characterIdsBuffers,
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

// ============ Character Element Encoders ============

function encodeElementValues(value = {}) {
  return Buffer.concat([
    encodeInt32Field(1, value.earth || 0),
    encodeInt32Field(2, value.fire || 0),
    encodeInt32Field(3, value.water || 0),
    encodeInt32Field(4, value.wind || 0)
  ]);
}

export function encodeGetCharacterElementsReq() {
  return Buffer.alloc(0);
}

export function encodeDebugApplyCharacterElementChangeReq(
  affinityDelta = {},
  masteryDelta = {},
  reason = "",
  debugToken = ""
) {
  return Buffer.concat([
    encodeMessageField(1, encodeElementValues(affinityDelta)),
    encodeMessageField(2, encodeElementValues(masteryDelta)),
    encodeStringField(3, reason),
    encodeStringField(4, debugToken)
  ]);
}

// ============ Character Title / Discipline Encoders ============

export function encodeGetCharacterTitlesReq() {
  return Buffer.alloc(0);
}

export function encodeEquipCharacterTitleReq(titleId) {
  return encodeStringField(1, titleId);
}

export function encodeGetCharacterDisciplinesReq() {
  return Buffer.alloc(0);
}

export function encodeDebugCharacterTitleReq({
  action = "",
  titleId = "",
  disciplineId = "",
  disciplineTier = "",
  disciplinePoints = 0,
  disciplineActive = true,
  triggerUnlockCheck = false,
  reason = "",
  debugToken = "",
  expiresAt = ""
} = {}) {
  return Buffer.concat([
    encodeStringField(1, action),
    encodeStringField(2, titleId),
    encodeStringField(3, disciplineId),
    encodeStringField(4, disciplineTier),
    encodeInt64Field(5, disciplinePoints),
    encodeBoolField(6, disciplineActive),
    encodeBoolField(7, triggerUnlockCheck),
    encodeStringField(8, reason),
    encodeStringField(9, debugToken),
    encodeStringField(10, expiresAt)
  ]);
}

// ============ Message Decoders ============

function decodeFrameInput(buffer) {
  const fields = decodeFieldsWithRepeated(buffer);
  return {
    characterId: readString(fields, 1),
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
    characterId: readString(fields, 2),
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
    characterId: readString(fields, 1),
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
    ownerCharacterId: readString(fields, 2),
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

function decodeElementValues(buffer) {
  const fields = decodeFieldsWithRepeated(buffer);
  return {
    earth: readInt32(fields, 1),
    fire: readInt32(fields, 2),
    water: readInt32(fields, 3),
    wind: readInt32(fields, 4)
  };
}

function decodeCharacterElements(buffer) {
  const fields = decodeFieldsWithRepeated(buffer);
  return {
    affinity: fields.get(1) ? decodeElementValues(fields.get(1)) : null,
    mastery: fields.get(2) ? decodeElementValues(fields.get(2)) : null
  };
}

function decodeCharacterTitleDefinitionSummary(buffer) {
  const fields = decodeFieldsWithRepeated(buffer);
  return {
    id: readString(fields, 1),
    name: readString(fields, 2),
    type: readString(fields, 3),
    rarity: readString(fields, 4),
    icon: readString(fields, 5),
    color: readString(fields, 6),
    tags: readStringList(fields, 7),
    hidden: readBool(fields, 8),
    limited: readBool(fields, 9),
    sortOrder: readInt32(fields, 10)
  };
}

function decodeCharacterTitleSummary(buffer) {
  const fields = decodeFieldsWithRepeated(buffer);
  return {
    definition: fields.get(1) ? decodeCharacterTitleDefinitionSummary(fields.get(1)) : null,
    owned: readBool(fields, 2),
    equipped: readBool(fields, 3),
    sourceType: readString(fields, 4),
    sourceId: readString(fields, 5),
    unlockedAt: readString(fields, 6),
    expiresAt: readString(fields, 7),
    expired: readBool(fields, 8)
  };
}

function decodeCharacterDisciplineSummary(buffer) {
  const fields = decodeFieldsWithRepeated(buffer);
  return {
    disciplineId: readString(fields, 1),
    points: readInt64(fields, 2),
    tier: readString(fields, 3),
    active: readBool(fields, 4),
    learnedAt: readString(fields, 5),
    updatedAt: readString(fields, 6)
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
        characterId: readString(fields, 3),
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
        targetCharacterIds: readStringList(fields, 8),
        referenceFrameId: readUInt32(fields, 9) || 0
      };
    }
    case MESSAGE_TYPE.MOVEMENT_REJECT_PUSH:
      return {
        roomId: readString(fields, 1),
        frameId: readUInt32(fields, 2),
        characterId: readString(fields, 3),
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
        newBuffIds: readInt32List(fields, 4)
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
    case MESSAGE_TYPE.GET_CHARACTER_ELEMENTS_RES:
      return {
        ok: readBool(fields, 1),
        errorCode: readString(fields, 2),
        characterId: readString(fields, 3),
        elements: fields.get(4) ? decodeCharacterElements(fields.get(4)) : null
      };
    case MESSAGE_TYPE.DEBUG_APPLY_CHARACTER_ELEMENT_CHANGE_RES:
      return {
        ok: readBool(fields, 1),
        errorCode: readString(fields, 2),
        characterId: readString(fields, 3),
        before: fields.get(4) ? decodeCharacterElements(fields.get(4)) : null,
        after: fields.get(5) ? decodeCharacterElements(fields.get(5)) : null
      };
    case MESSAGE_TYPE.GET_CHARACTER_TITLES_RES:
      return {
        ok: readBool(fields, 1),
        errorCode: readString(fields, 2),
        characterId: readString(fields, 3),
        titles: decodeRepeatedMessage(fields, 4, decodeCharacterTitleSummary),
        equippedTitle: fields.get(5) ? decodeCharacterTitleSummary(fields.get(5)) : null
      };
    case MESSAGE_TYPE.EQUIP_CHARACTER_TITLE_RES:
      return {
        ok: readBool(fields, 1),
        errorCode: readString(fields, 2),
        characterId: readString(fields, 3),
        equippedTitle: fields.get(4) ? decodeCharacterTitleSummary(fields.get(4)) : null
      };
    case MESSAGE_TYPE.GET_CHARACTER_DISCIPLINES_RES:
      return {
        ok: readBool(fields, 1),
        errorCode: readString(fields, 2),
        characterId: readString(fields, 3),
        disciplines: decodeRepeatedMessage(fields, 4, decodeCharacterDisciplineSummary)
      };
    case MESSAGE_TYPE.DEBUG_CHARACTER_TITLE_RES:
      return {
        ok: readBool(fields, 1),
        errorCode: readString(fields, 2),
        characterId: readString(fields, 3),
        action: readString(fields, 4),
        title: fields.get(5) ? decodeCharacterTitleSummary(fields.get(5)) : null,
        discipline: fields.get(6) ? decodeCharacterDisciplineSummary(fields.get(6)) : null,
        unlockedTitles: decodeRepeatedMessage(fields, 7, decodeCharacterTitleSummary)
      };
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
      return {
        appearance: readUInt32(fields, 1),
        activeBuffIds: readInt32List(fields, 2)
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
    case MESSAGE_TYPE.SERVER_REDIRECT_PUSH:
      return {
        reason: readString(fields, 1),
        roomId: readString(fields, 2),
        rolloutEpoch: readString(fields, 3),
        reconnectRequired: readBool(fields, 4),
        retryAfterMs: readUInt32(fields, 5) || 0,
        targetHost: readString(fields, 6),
        targetPort: readUInt32(fields, 7) || 0,
        targetServerId: readString(fields, 8),
        transport: readString(fields, 9)
      };
    case MESSAGE_TYPE.REQUEST_SERVER_SHUTDOWN_RES:
      return {
        ok: readBool(fields, 1),
        errorCode: readString(fields, 2),
        connectionCount: readInt64(fields, 3),
        ownedRoomCount: readInt64(fields, 4),
        migratingRoomCount: readInt64(fields, 5),
        drainModeEnabled: readBool(fields, 6),
        retiredRoomCount: readInt64(fields, 7)
      };
    case MESSAGE_TYPE.SESSION_KICK_PUSH:
      return {
        reason: readString(fields, 1),
        timestamp: readInt64(fields, 2)
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
