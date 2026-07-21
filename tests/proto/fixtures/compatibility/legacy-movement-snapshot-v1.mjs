// Historical v1 projection deliberately knows MovementSnapshotPush fields 1..5 only.
// It is independent from the current mock-client decoder so future fields cannot pass by
// accidentally exercising the same decoder twice.

function readVarint(buffer, offset) {
  let value = 0n;
  let shift = 0n;
  let position = offset;
  while (position < buffer.length) {
    const byte = BigInt(buffer[position]);
    value |= (byte & 0x7fn) << shift;
    position += 1;
    if ((byte & 0x80n) === 0n) {
      return { value, nextOffset: position };
    }
    shift += 7n;
    if (shift > 70n) {
      throw new Error("legacy v1 fixture contains an oversized varint");
    }
  }
  throw new Error("legacy v1 fixture ends inside a varint");
}

function readLengthDelimited(buffer, offset) {
  const length = readVarint(buffer, offset);
  const end = length.nextOffset + Number(length.value);
  if (end > buffer.length) {
    throw new Error("legacy v1 fixture contains a truncated length-delimited field");
  }
  return { value: buffer.subarray(length.nextOffset, end), nextOffset: end };
}

function skipField(buffer, wireType, offset) {
  if (wireType === 0) {
    return readVarint(buffer, offset).nextOffset;
  }
  if (wireType === 1) {
    if (offset + 8 > buffer.length) {
      throw new Error("legacy v1 fixture contains a truncated fixed64 field");
    }
    return offset + 8;
  }
  if (wireType === 2) {
    return readLengthDelimited(buffer, offset).nextOffset;
  }
  if (wireType === 5) {
    if (offset + 4 > buffer.length) {
      throw new Error("legacy v1 fixture contains a truncated fixed32 field");
    }
    return offset + 4;
  }
  throw new Error(`legacy v1 projection cannot skip wire type ${wireType}`);
}

function decodeEntityTransformV1(buffer) {
  const entity = {
    entityId: 0,
    characterId: "",
    sceneId: 0,
    x: 0,
    y: 0,
    dirX: 0,
    dirY: 0,
    moving: false,
    lastInputFrame: 0
  };
  let offset = 0;
  while (offset < buffer.length) {
    const tag = readVarint(buffer, offset);
    const fieldNumber = Number(tag.value >> 3n);
    const wireType = Number(tag.value & 0x07n);
    offset = tag.nextOffset;
    if (wireType === 0 && fieldNumber === 1) {
      const value = readVarint(buffer, offset);
      entity.entityId = Number(value.value);
      offset = value.nextOffset;
    } else if (wireType === 2 && fieldNumber === 2) {
      const value = readLengthDelimited(buffer, offset);
      entity.characterId = value.value.toString("utf8");
      offset = value.nextOffset;
    } else if (wireType === 0 && fieldNumber === 3) {
      const value = readVarint(buffer, offset);
      entity.sceneId = Number(value.value);
      offset = value.nextOffset;
    } else if (wireType === 5 && fieldNumber >= 4 && fieldNumber <= 7) {
      if (fieldNumber === 4) entity.x = buffer.readFloatLE(offset);
      if (fieldNumber === 5) entity.y = buffer.readFloatLE(offset);
      if (fieldNumber === 6) entity.dirX = buffer.readFloatLE(offset);
      if (fieldNumber === 7) entity.dirY = buffer.readFloatLE(offset);
      offset += 4;
    } else if (wireType === 0 && fieldNumber === 8) {
      const value = readVarint(buffer, offset);
      entity.moving = value.value !== 0n;
      offset = value.nextOffset;
    } else if (wireType === 0 && fieldNumber === 9) {
      const value = readVarint(buffer, offset);
      entity.lastInputFrame = Number(value.value);
      offset = value.nextOffset;
    } else {
      offset = skipField(buffer, wireType, offset);
    }
  }
  return entity;
}

export function decodeMovementSnapshotV1(buffer) {
  const snapshot = {
    roomId: "",
    frameId: 0,
    entities: [],
    fullSync: false,
    reason: ""
  };
  let offset = 0;
  while (offset < buffer.length) {
    const tag = readVarint(buffer, offset);
    const fieldNumber = Number(tag.value >> 3n);
    const wireType = Number(tag.value & 0x07n);
    offset = tag.nextOffset;
    if (wireType === 2 && fieldNumber === 1) {
      const value = readLengthDelimited(buffer, offset);
      snapshot.roomId = value.value.toString("utf8");
      offset = value.nextOffset;
    } else if (wireType === 0 && fieldNumber === 2) {
      const value = readVarint(buffer, offset);
      snapshot.frameId = Number(value.value);
      offset = value.nextOffset;
    } else if (wireType === 2 && fieldNumber === 3) {
      const value = readLengthDelimited(buffer, offset);
      snapshot.entities.push(decodeEntityTransformV1(value.value));
      offset = value.nextOffset;
    } else if (wireType === 0 && fieldNumber === 4) {
      const value = readVarint(buffer, offset);
      snapshot.fullSync = value.value !== 0n;
      offset = value.nextOffset;
    } else if (wireType === 2 && fieldNumber === 5) {
      const value = readLengthDelimited(buffer, offset);
      snapshot.reason = value.value.toString("utf8");
      offset = value.nextOffset;
    } else {
      offset = skipField(buffer, wireType, offset);
    }
  }
  return snapshot;
}
