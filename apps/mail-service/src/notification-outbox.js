import { createHash, randomBytes } from "node:crypto";

export const MAIL_NOTIFICATION_EVENT_TYPE = "mail.created";
export const MAIL_NOTIFICATION_EVENT_VERSION = 1;

export class PermanentOutboxPayloadError extends Error {
  constructor(code, message) {
    super(message);
    this.name = "PermanentOutboxPayloadError";
    this.code = code;
  }
}

function byteLength(value) {
  return Buffer.byteLength(String(value ?? ""), "utf8");
}

function epochMillis(value, fallback = Date.now()) {
  if (Number.isInteger(value)) {
    return value;
  }
  if (typeof value === "string" && /^\d+$/.test(value)) {
    const parsedInteger = Number(value);
    if (Number.isSafeInteger(parsedInteger)) {
      return parsedInteger;
    }
  }
  const parsed = new Date(value ?? fallback).getTime();
  return Number.isFinite(parsed) ? parsed : fallback;
}

function legacyTraceId(mailId) {
  return createHash("sha256").update(`mail-notify:${mailId}`).digest("hex").slice(0, 32);
}

export function generateTraceId() {
  return randomBytes(16).toString("hex");
}

export function buildMailNotificationEvent(mail, options = {}) {
  const occurredAt = epochMillis(options.occurredAt ?? mail.created_at);
  const senderId = typeof mail.sender_id === "string" && mail.sender_id.toLowerCase() === "system"
    ? "system"
    : (mail.sender_id || mail.from_player_id);

  return {
    event_id: options.eventId || `mail.notify:${mail.mail_id}`,
    event_type: MAIL_NOTIFICATION_EVENT_TYPE,
    version: MAIL_NOTIFICATION_EVENT_VERSION,
    occurred_at: occurredAt,
    player_id: mail.to_player_id,
    mail: {
      mail_id: mail.mail_id,
      title: mail.title || "",
      from_player_id: senderId,
      from_name: mail.sender_name || (senderId === "system" ? "系统" : senderId),
      mail_type: mail.mail_type || "system",
      created_at: epochMillis(mail.created_at, occurredAt)
    },
    trace_id: options.traceId || generateTraceId()
  };
}

export function normalizeMailNotificationEvent(entry) {
  const payload = entry?.payload && typeof entry.payload === "object" ? entry.payload : {};
  const sourceMail = payload.mail && typeof payload.mail === "object" ? payload.mail : payload;
  const mailId = sourceMail.mail_id || payload.mail_id || entry?.mail_id;
  const playerId = payload.player_id || payload.to_player_id || entry?.to_player_id;
  const occurredAt = epochMillis(
    payload.occurred_at ?? sourceMail.created_at ?? entry?.occurred_at ?? entry?.created_at
  );
  const senderId = sourceMail.from_player_id || sourceMail.sender_id || payload.from || "system";

  const event = {
    event_id: payload.event_id || entry?.event_id || (mailId ? `mail.notify:${mailId}` : ""),
    event_type: payload.event_type || MAIL_NOTIFICATION_EVENT_TYPE,
    version: payload.version ?? entry?.event_version ?? MAIL_NOTIFICATION_EVENT_VERSION,
    occurred_at: occurredAt,
    player_id: playerId,
    mail: {
      mail_id: mailId,
      title: sourceMail.title || "",
      from_player_id: senderId,
      from_name: sourceMail.from_name || sourceMail.sender_name || payload.from_name || (senderId === "system" ? "系统" : senderId),
      mail_type: sourceMail.mail_type || sourceMail.type || payload.type || "system",
      created_at: epochMillis(sourceMail.created_at, occurredAt)
    },
    trace_id: payload.trace_id || entry?.trace_id || (mailId ? legacyTraceId(mailId) : "")
  };

  validateMailNotificationEvent(event, entry);
  return event;
}

export function validateMailNotificationEvent(event, entry = null) {
  const invalid = (code, message) => {
    throw new PermanentOutboxPayloadError(code, message);
  };
  const nonEmptyWithin = (value, maxBytes, field) => {
    if (typeof value !== "string" || byteLength(value) === 0 || byteLength(value) > maxBytes) {
      invalid("INVALID_MAIL_NOTIFICATION_PAYLOAD", `${field} must be a non-empty string no longer than ${maxBytes} bytes`);
    }
  };
  const stringWithin = (value, maxBytes, field) => {
    if (typeof value !== "string" || byteLength(value) > maxBytes) {
      invalid("INVALID_MAIL_NOTIFICATION_PAYLOAD", `${field} must be a string no longer than ${maxBytes} bytes`);
    }
  };

  nonEmptyWithin(event.event_id, 128, "event_id");
  if (event.event_type !== MAIL_NOTIFICATION_EVENT_TYPE) {
    invalid("UNSUPPORTED_MAIL_NOTIFICATION_TYPE", `event_type must be ${MAIL_NOTIFICATION_EVENT_TYPE}`);
  }
  if (event.version !== MAIL_NOTIFICATION_EVENT_VERSION) {
    invalid("UNSUPPORTED_MAIL_NOTIFICATION_VERSION", `version must be ${MAIL_NOTIFICATION_EVENT_VERSION}`);
  }
  if (!Number.isInteger(event.occurred_at) || event.occurred_at <= 0) {
    invalid("INVALID_MAIL_NOTIFICATION_PAYLOAD", "occurred_at must be a positive Unix epoch millisecond integer");
  }
  nonEmptyWithin(event.player_id, 128, "player_id");
  if (entry?.to_player_id && event.player_id !== entry.to_player_id) {
    invalid("MAIL_NOTIFICATION_TARGET_MISMATCH", "player_id does not match outbox to_player_id");
  }
  if (!event.mail || typeof event.mail !== "object") {
    invalid("INVALID_MAIL_NOTIFICATION_PAYLOAD", "mail must be an object");
  }
  nonEmptyWithin(event.mail.mail_id, 64, "mail.mail_id");
  if (entry?.mail_id && event.mail.mail_id !== entry.mail_id) {
    invalid("MAIL_NOTIFICATION_ID_MISMATCH", "mail.mail_id does not match outbox mail_id");
  }
  stringWithin(event.mail.title, 256, "mail.title");
  nonEmptyWithin(event.mail.from_player_id, 128, "mail.from_player_id");
  stringWithin(event.mail.from_name, 128, "mail.from_name");
  nonEmptyWithin(event.mail.mail_type, 32, "mail.mail_type");
  if (!Number.isInteger(event.mail.created_at) || event.mail.created_at <= 0) {
    invalid("INVALID_MAIL_NOTIFICATION_PAYLOAD", "mail.created_at must be a positive Unix epoch millisecond integer");
  }
  if (typeof event.trace_id !== "string" || !/^[0-9a-f]{32}$/.test(event.trace_id)) {
    invalid("INVALID_MAIL_NOTIFICATION_PAYLOAD", "trace_id must be 32 lowercase hexadecimal characters");
  }
}

export function calculateOutboxBackoffMs(attempt, options = {}, random = Math.random) {
  const baseMs = options.baseMs ?? 1000;
  const maxMs = options.maxMs ?? 60_000;
  const jitterRatio = options.jitterRatio ?? 0.2;
  const exponent = Math.max(0, Math.trunc(attempt) - 1);
  const uncapped = baseMs * (2 ** Math.min(exponent, 30));
  const capped = Math.min(maxMs, uncapped);
  const sample = Math.min(1, Math.max(0, Number(random())));
  const factor = 1 + ((sample * 2 - 1) * jitterRatio);
  return Math.max(0, Math.min(maxMs, Math.round(capped * factor)));
}
