import crypto from "node:crypto";

export const MYFORGE_PROTOCOL_VERSION = 1;
export const MYFORGE_SUBPROTOCOL = "myserver.myforge.v1";
export const MYFORGE_SIGNING_PREFIX = Buffer.from("MYFORGE-WS-V1\n", "ascii");
export const RESULT_FIXED_RESERVE_BYTES = 262144;

const UUID_V4_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const BASE64URL_PATTERN = /^[A-Za-z0-9_-]+$/;

export class MyforgeProtocolError extends Error {
  constructor(code, message, options = {}) {
    super(message);
    this.name = "MyforgeProtocolError";
    this.code = code;
    this.requestId = options.requestId ?? null;
    this.safeToRespond = options.safeToRespond !== false;
    this.fatal = options.fatal !== false;
  }
}

function protocolError(code, message, options) {
  return new MyforgeProtocolError(code, message, options);
}

function assertScalarString(value) {
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code >= 0xd800 && code <= 0xdbff) {
      const next = value.charCodeAt(index + 1);
      if (!(next >= 0xdc00 && next <= 0xdfff)) {
        throw protocolError("MYFORGE_MESSAGE_IJSON_INVALID", "JSON contains a lone surrogate");
      }
      index += 1;
    } else if (code >= 0xdc00 && code <= 0xdfff) {
      throw protocolError("MYFORGE_MESSAGE_IJSON_INVALID", "JSON contains a lone surrogate");
    }
  }
}

class StrictJsonParser {
  constructor(source) {
    this.source = source;
    this.index = 0;
  }

  fail(message) {
    throw protocolError("MYFORGE_MESSAGE_IJSON_INVALID", message, { safeToRespond: false });
  }

  skipWhitespace() {
    while (this.index < this.source.length && /[\u0009\u000a\u000d\u0020]/.test(this.source[this.index])) {
      this.index += 1;
    }
  }

  parse() {
    this.skipWhitespace();
    const value = this.parseValue();
    this.skipWhitespace();
    if (this.index !== this.source.length) this.fail("JSON contains trailing data");
    return value;
  }

  parseValue(depth = 0) {
    if (depth > 64) this.fail("JSON nesting exceeds the protocol limit");
    const char = this.source[this.index];
    if (char === "{") return this.parseObject(depth);
    if (char === "[") return this.parseArray(depth);
    if (char === "\"") return this.parseString();
    if (char === "t" && this.source.slice(this.index, this.index + 4) === "true") {
      this.index += 4;
      return true;
    }
    if (char === "f" && this.source.slice(this.index, this.index + 5) === "false") {
      this.index += 5;
      return false;
    }
    if (char === "n" && this.source.slice(this.index, this.index + 4) === "null") {
      this.index += 4;
      return null;
    }
    if (char === "-" || (char >= "0" && char <= "9")) return this.parseNumber();
    this.fail("JSON contains an invalid value");
  }

  parseObject(depth) {
    this.index += 1;
    this.skipWhitespace();
    const result = Object.create(null);
    const keys = new Set();
    if (this.source[this.index] === "}") {
      this.index += 1;
      return result;
    }
    while (this.index < this.source.length) {
      if (this.source[this.index] !== "\"") this.fail("JSON object key must be a string");
      const key = this.parseString();
      if (keys.has(key)) this.fail("JSON contains a duplicate object key");
      keys.add(key);
      this.skipWhitespace();
      if (this.source[this.index] !== ":") this.fail("JSON object is missing ':'");
      this.index += 1;
      this.skipWhitespace();
      result[key] = this.parseValue(depth + 1);
      this.skipWhitespace();
      if (this.source[this.index] === "}") {
        this.index += 1;
        return result;
      }
      if (this.source[this.index] !== ",") this.fail("JSON object is missing ','");
      this.index += 1;
      this.skipWhitespace();
    }
    this.fail("JSON object is not terminated");
  }

  parseArray(depth) {
    this.index += 1;
    this.skipWhitespace();
    const result = [];
    if (this.source[this.index] === "]") {
      this.index += 1;
      return result;
    }
    while (this.index < this.source.length) {
      result.push(this.parseValue(depth + 1));
      this.skipWhitespace();
      if (this.source[this.index] === "]") {
        this.index += 1;
        return result;
      }
      if (this.source[this.index] !== ",") this.fail("JSON array is missing ','");
      this.index += 1;
      this.skipWhitespace();
    }
    this.fail("JSON array is not terminated");
  }

  parseString() {
    this.index += 1;
    let result = "";
    while (this.index < this.source.length) {
      const char = this.source[this.index++];
      const code = char.charCodeAt(0);
      if (char === "\"") return result;
      if (code <= 0x1f) this.fail("JSON string contains an unescaped control character");
      if (char === "\\") {
        const escape = this.source[this.index++];
        const escaped = { "\"": "\"", "\\": "\\", "/": "/", b: "\b", f: "\f", n: "\n", r: "\r", t: "\t" }[escape];
        if (escaped !== undefined) {
          result += escaped;
          continue;
        }
        if (escape !== "u") this.fail("JSON string contains an invalid escape");
        result += this.parseUnicodeEscape();
        continue;
      }
      if (code >= 0xd800 && code <= 0xdbff) {
        const nextCode = this.source.charCodeAt(this.index);
        if (!(nextCode >= 0xdc00 && nextCode <= 0xdfff)) this.fail("JSON contains a lone surrogate");
        result += char + this.source[this.index++];
      } else if (code >= 0xdc00 && code <= 0xdfff) {
        this.fail("JSON contains a lone surrogate");
      } else {
        result += char;
      }
    }
    this.fail("JSON string is not terminated");
  }

  parseUnicodeEscape() {
    const firstHex = this.source.slice(this.index, this.index + 4);
    if (!/^[0-9a-fA-F]{4}$/.test(firstHex)) this.fail("JSON string contains an invalid Unicode escape");
    this.index += 4;
    const first = Number.parseInt(firstHex, 16);
    if (first >= 0xdc00 && first <= 0xdfff) this.fail("JSON contains a lone surrogate");
    if (first >= 0xd800 && first <= 0xdbff) {
      if (this.source.slice(this.index, this.index + 2) !== "\\u") this.fail("JSON contains a lone surrogate");
      this.index += 2;
      const secondHex = this.source.slice(this.index, this.index + 4);
      if (!/^[0-9a-fA-F]{4}$/.test(secondHex)) this.fail("JSON string contains an invalid Unicode escape");
      this.index += 4;
      const second = Number.parseInt(secondHex, 16);
      if (!(second >= 0xdc00 && second <= 0xdfff)) this.fail("JSON contains a lone surrogate");
      return String.fromCharCode(first, second);
    }
    return String.fromCharCode(first);
  }

  parseNumber() {
    const start = this.index;
    while (this.index < this.source.length && !/[\u0009\u000a\u000d\u0020,\]}]/.test(this.source[this.index])) {
      this.index += 1;
    }
    const token = this.source.slice(start, this.index);
    if (!/^(?:0|-[1-9][0-9]*|[1-9][0-9]*)$/.test(token)) {
      this.fail("JSON numbers must be canonical safe integers");
    }
    const value = Number(token);
    if (!Number.isSafeInteger(value) || Object.is(value, -0)) {
      this.fail("JSON integer is outside the interoperable range");
    }
    return value;
  }
}

function frameBytes(frame) {
  if (typeof frame === "string") {
    assertScalarString(frame);
    return Buffer.from(frame, "utf8");
  }
  if (Buffer.isBuffer(frame)) return frame;
  if (frame instanceof ArrayBuffer) return Buffer.from(frame);
  if (ArrayBuffer.isView(frame)) return Buffer.from(frame.buffer, frame.byteOffset, frame.byteLength);
  throw protocolError("MYFORGE_MESSAGE_IJSON_INVALID", "WebSocket frame is not text data", { safeToRespond: false });
}

export function parseStrictJson(frame, maxBytes = Number.MAX_SAFE_INTEGER) {
  const bytes = frameBytes(frame);
  if (bytes.length > maxBytes) {
    throw protocolError("MYFORGE_OUTPUT_TOO_LARGE", "WebSocket frame exceeds the configured limit", { safeToRespond: false });
  }
  let source;
  try {
    source = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    throw protocolError("MYFORGE_MESSAGE_IJSON_INVALID", "WebSocket frame is not valid UTF-8", { safeToRespond: false });
  }
  assertScalarString(source);
  return new StrictJsonParser(source).parse();
}

export function jcsCanonicalize(value) {
  if (value === null) return "null";
  if (value === true) return "true";
  if (value === false) return "false";
  if (typeof value === "number") {
    if (!Number.isSafeInteger(value) || Object.is(value, -0)) {
      throw protocolError("MYFORGE_MESSAGE_IJSON_INVALID", "JCS only accepts safe integers");
    }
    return String(value);
  }
  if (typeof value === "string") {
    assertScalarString(value);
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) return `[${value.map(jcsCanonicalize).join(",")}]`;
  if (typeof value === "object") {
    const keys = Object.keys(value).sort();
    return `{${keys.map((key) => `${jcsCanonicalize(key)}:${jcsCanonicalize(value[key])}`).join(",")}}`;
  }
  throw protocolError("MYFORGE_MESSAGE_IJSON_INVALID", "Value cannot be represented as I-JSON");
}

export function withoutTopLevelFields(message, fields) {
  const result = Object.create(null);
  for (const key of Object.keys(message)) {
    if (!fields.has(key)) result[key] = message[key];
  }
  return result;
}

export function signingBytes(message) {
  const unsigned = withoutTopLevelFields(message, new Set(["signature"]));
  return Buffer.concat([MYFORGE_SIGNING_PREFIX, Buffer.from(jcsCanonicalize(unsigned), "utf8")]);
}

export function strictBase64UrlDecode(value, expectedBytes, field = "value") {
  if (typeof value !== "string" || !BASE64URL_PATTERN.test(value) || value.includes("=")) {
    throw protocolError("MYFORGE_MESSAGE_SCHEMA_INVALID", `${field} must be unpadded base64url`);
  }
  const decoded = Buffer.from(value, "base64url");
  if (decoded.length !== expectedBytes || decoded.toString("base64url") !== value) {
    throw protocolError("MYFORGE_MESSAGE_SCHEMA_INVALID", `${field} has an invalid length or encoding`);
  }
  return decoded;
}

export function signMessage(unsignedMessage, privateKey) {
  if (Object.prototype.hasOwnProperty.call(unsignedMessage, "signature")) {
    throw protocolError("MYFORGE_MESSAGE_SCHEMA_INVALID", "Unsigned message must not include signature");
  }
  const signature = crypto.sign(null, signingBytes(unsignedMessage), privateKey).toString("base64url");
  return { ...unsignedMessage, signature };
}

export function verifyMessageSignature(message, publicKey) {
  let signature;
  try {
    signature = strictBase64UrlDecode(message?.signature, 64, "signature");
  } catch {
    throw protocolError("MYFORGE_AGENT_SIGNATURE_INVALID", "message signature is invalid");
  }
  if (!crypto.verify(null, signingBytes(message), publicKey, signature)) {
    throw protocolError("MYFORGE_AGENT_SIGNATURE_INVALID", "message signature is invalid");
  }
  return true;
}

export function serializeMessage(message) {
  return jcsCanonicalize(message);
}

export function assertCanonicalMessageFrame(frame, message) {
  const received = frameBytes(frame);
  const expected = Buffer.from(serializeMessage(message), "utf8");
  if (received.length !== expected.length || !received.equals(expected)) {
    throw protocolError("MYFORGE_MESSAGE_IJSON_INVALID", "WebSocket text frame must use canonical JCS encoding");
  }
}

export function semanticDigest(message) {
  const semantic = withoutTopLevelFields(
    message,
    new Set(["signature", "timestampMs", "expiresAtMs", "nonce"])
  );
  return crypto.createHash("sha256").update(jcsCanonicalize(semantic), "utf8").digest("hex");
}

export function randomBase64Url(bytes) {
  return crypto.randomBytes(bytes).toString("base64url");
}

export function randomUuidV4() {
  return crypto.randomUUID();
}

export function isUuidV4(value) {
  return typeof value === "string" && UUID_V4_PATTERN.test(value);
}

export function validateMessageTime(message, { nowMs, ttlMs, exactLifetimeMs = null }) {
  const lifetime = message.expiresAtMs - message.timestampMs;
  if (lifetime <= 0) {
    throw protocolError("MYFORGE_MESSAGE_EXPIRED", "message has an invalid validity window");
  }
  if (lifetime > ttlMs || (exactLifetimeMs !== null && lifetime !== exactLifetimeMs)) {
    throw protocolError("MYFORGE_LIMIT_MISMATCH", "message lifetime does not match negotiated limits");
  }
  if (message.timestampMs > nowMs.now + nowMs.clockSkewMs ||
      message.expiresAtMs < nowMs.now - nowMs.clockSkewMs) {
    throw protocolError("MYFORGE_MESSAGE_EXPIRED", "message is outside the accepted time window");
  }
}

export class ReplayCache {
  constructor(maxEntries = 65536) {
    if (!Number.isSafeInteger(maxEntries) || maxEntries < 1) {
      throw new TypeError("ReplayCache maxEntries must be a positive safe integer");
    }
    this.maxEntries = maxEntries;
    this.entries = new Map();
    this.expirations = [];
  }

  pushExpiration(entry) {
    const heap = this.expirations;
    heap.push(entry);
    let index = heap.length - 1;
    while (index > 0) {
      const parent = Math.floor((index - 1) / 2);
      if (heap[parent].expiresAtMs <= entry.expiresAtMs) break;
      heap[index] = heap[parent];
      index = parent;
    }
    heap[index] = entry;
  }

  popExpiration() {
    const heap = this.expirations;
    const root = heap[0];
    const last = heap.pop();
    if (heap.length > 0) {
      let index = 0;
      while (true) {
        const left = index * 2 + 1;
        if (left >= heap.length) break;
        const right = left + 1;
        const child = right < heap.length && heap[right].expiresAtMs < heap[left].expiresAtMs
          ? right
          : left;
        if (heap[child].expiresAtMs >= last.expiresAtMs) break;
        heap[index] = heap[child];
        index = child;
      }
      heap[index] = last;
    }
    return root;
  }

  removeExpired(nowMs) {
    while (this.expirations.length > 0 && this.expirations[0].expiresAtMs < nowMs) {
      const expired = this.popExpiration();
      if (this.entries.get(expired.key) === expired.expiresAtMs) {
        this.entries.delete(expired.key);
      }
    }
  }

  checkAndInsert(key, expiresAtMs, nowMs) {
    this.removeExpired(nowMs);
    if (this.entries.has(key)) {
      throw protocolError("MYFORGE_REPLAY_DETECTED", "message nonce was already used");
    }
    if (this.entries.size >= this.maxEntries) {
      throw protocolError("MYFORGE_AGENT_BUSY", "replay cache capacity is exhausted");
    }
    this.entries.set(key, expiresAtMs);
    this.pushExpiration({ key, expiresAtMs });
  }

  get size() {
    return this.entries.size;
  }
}

export class AsyncMutex {
  constructor() {
    this.tail = Promise.resolve();
  }

  async runExclusive(callback) {
    let release;
    const previous = this.tail;
    this.tail = new Promise((resolve) => { release = resolve; });
    await previous;
    try {
      return await callback();
    } finally {
      release();
    }
  }
}
