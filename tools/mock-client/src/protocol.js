// Protobuf-like encoding/decoding utilities

/**
 * Encode a varint
 * @param {number|BigInt} value
 * @returns {Buffer}
 */
export function encodeVarint(value) {
  let current = BigInt(value);
  const bytes = [];
  while (current >= 0x80n) {
    bytes.push(Number((current & 0x7fn) | 0x80n));
    current >>= 7n;
  }
  bytes.push(Number(current));
  return Buffer.from(bytes);
}

/**
 * Decode a varint from buffer at offset
 * @param {Buffer} buffer
 * @param {number} offset
 * @returns {{ value: BigInt, nextOffset: number }}
 */
export function decodeVarint(buffer, offset) {
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

// Field encoding functions
export function encodeStringField(fieldNumber, value) {
  const fieldKey = (fieldNumber << 3) | 2;
  const data = Buffer.from(value, "utf8");
  return Buffer.concat([encodeVarint(fieldKey), encodeVarint(data.length), data]);
}

export function encodeBoolField(fieldNumber, value) {
  const fieldKey = fieldNumber << 3;
  return Buffer.concat([encodeVarint(fieldKey), encodeVarint(value ? 1 : 0)]);
}

export function encodeInt64Field(fieldNumber, value) {
  const fieldKey = fieldNumber << 3;
  return Buffer.concat([encodeVarint(fieldKey), encodeVarint(BigInt(value))]);
}

export function encodeUInt32Field(fieldNumber, value) {
  const fieldKey = fieldNumber << 3;
  return Buffer.concat([encodeVarint(fieldKey), encodeVarint(value)]);
}

export function encodeInt32Field(fieldNumber, value) {
  const fieldKey = fieldNumber << 3;
  return Buffer.concat([encodeVarint(fieldKey), encodeVarint(value)]);
}

export function encodeFloatField(fieldNumber, value) {
  const fieldKey = (fieldNumber << 3) | 5;
  const data = Buffer.allocUnsafe(4);
  data.writeFloatLE(value, 0);
  return Buffer.concat([encodeVarint(fieldKey), data]);
}

// Field decoding helpers
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

/**
 * Decode all fields from a protobuf message body, supporting repeated fields
 * @param {Buffer} buffer
 * @returns {Map<number, any>}
 */
export function decodeFieldsWithRepeated(buffer) {
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

    if (wireType === 5) {
      const end = offset + 4;
      appendField(fields, fieldNumber, buffer.subarray(offset, end));
      offset = end;
      continue;
    }

    throw new Error(`Unsupported wire type: ${wireType}`);
  }

  return fields;
}

// Field reading helpers
export function readString(fields, fieldNumber) {
  const value = fields.get(fieldNumber);
  return value ? Buffer.from(value).toString("utf8") : "";
}

export function readStringList(fields, fieldNumber) {
  const value = fields.get(fieldNumber);
  if (!value) {
    return [];
  }
  if (Array.isArray(value)) {
    return value.map((entry) => Buffer.from(entry).toString("utf8"));
  }
  return [Buffer.from(value).toString("utf8")];
}

export function readBool(fields, fieldNumber) {
  return Number(fields.get(fieldNumber) || 0n) !== 0;
}

export function readInt64(fields, fieldNumber) {
  return Number(fields.get(fieldNumber) || 0n);
}

export function readUInt32(fields, fieldNumber) {
  return Number(fields.get(fieldNumber) || 0n);
}

export function readFloat(fields, fieldNumber) {
  const value = fields.get(fieldNumber);
  if (!value) {
    return 0;
  }
  const buffer = Buffer.from(value);
  return buffer.readFloatLE(0);
}
