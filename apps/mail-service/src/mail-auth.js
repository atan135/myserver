import crypto from "node:crypto";

function base64UrlDecode(value) {
  return Buffer.from(value, "base64url");
}

function createAuthError(code, message = code, statusCode = 401) {
  const error = new Error(message);
  error.code = code;
  error.statusCode = statusCode;
  return error;
}

export function hashTicket(ticket) {
  return crypto.createHash("sha256").update(ticket).digest("hex");
}

export function ticketKey(prefix, ticket) {
  return `${prefix || ""}ticket:${hashTicket(ticket)}`;
}

export function ticketVersionKey(prefix, playerId) {
  return `${prefix || ""}player-ticket-version:${playerId}`;
}

export function extractBearerToken(headers = {}) {
  const authorization = headers.authorization || headers.Authorization;
  if (typeof authorization !== "string") {
    return null;
  }

  const match = authorization.match(/^Bearer\s+(.+)$/i);
  return match ? match[1].trim() : null;
}

export function extractGameTicket(headers = {}) {
  return extractBearerToken(headers) ||
    headers["x-game-ticket"] ||
    headers["X-Game-Ticket"] ||
    null;
}

export function extractServiceToken(headers = {}) {
  return extractBearerToken(headers) ||
    headers["x-service-token"] ||
    headers["X-Service-Token"] ||
    headers["x-admin-token"] ||
    headers["X-Admin-Token"] ||
    null;
}

export function verifyTicketSignature(secret, ticket, nowMs = Date.now()) {
  if (!secret) {
    throw createAuthError("INVALID_TICKET_SECRET", "ticket secret is not configured");
  }

  const parts = String(ticket || "").split(".");
  if (parts.length !== 2 || !parts[0] || !parts[1]) {
    throw createAuthError("INVALID_TICKET_FORMAT", "ticket format is invalid");
  }

  const [payloadB64, signatureB64] = parts;
  const expectedSignature = crypto
    .createHmac("sha256", secret)
    .update(payloadB64)
    .digest();

  let providedSignature;
  try {
    providedSignature = base64UrlDecode(signatureB64);
  } catch {
    throw createAuthError("INVALID_TICKET_SIGNATURE", "ticket signature is invalid");
  }

  if (
    expectedSignature.length !== providedSignature.length ||
    !crypto.timingSafeEqual(expectedSignature, providedSignature)
  ) {
    throw createAuthError("INVALID_TICKET_SIGNATURE", "ticket signature is invalid");
  }

  let payload;
  try {
    payload = JSON.parse(base64UrlDecode(payloadB64).toString("utf8"));
  } catch {
    throw createAuthError("INVALID_TICKET_PAYLOAD", "ticket payload is invalid");
  }

  if (
    !payload ||
    typeof payload.playerId !== "string" ||
    !payload.playerId ||
    typeof payload.characterId !== "string" ||
    !payload.characterId
  ) {
    throw createAuthError("INVALID_TICKET_PAYLOAD", "ticket payload is invalid");
  }

  const expiresAtMs = Date.parse(payload.exp);
  if (!Number.isFinite(expiresAtMs)) {
    throw createAuthError("INVALID_TICKET_EXP", "ticket expiration is invalid");
  }

  if (expiresAtMs <= nowMs) {
    throw createAuthError("TICKET_EXPIRED", "ticket is expired");
  }

  return {
    playerId: payload.playerId,
    characterId: payload.characterId,
    ver: payload.ver === undefined ? undefined : Number(payload.ver),
    exp: payload.exp
  };
}

export class MailPlayerAuthService {
  constructor(config, redis) {
    this.config = config;
    this.redis = redis;
  }

  async authenticateTicket(ticket) {
    if (!ticket) {
      throw createAuthError("MAIL_PLAYER_TICKET_REQUIRED", "game ticket is required");
    }

    const payload = verifyTicketSignature(this.config.ticketSecret, ticket);
    const redis = this.redis;
    if (!redis || typeof redis.get !== "function") {
      throw createAuthError("AUTH_BACKEND_UNAVAILABLE", "ticket backend is unavailable");
    }

    let owner;
    let currentVersion;
    try {
      owner = await redis.get(ticketKey(this.config.redisKeyPrefix, ticket));
      currentVersion = await redis.get(ticketVersionKey(this.config.redisKeyPrefix, payload.playerId));
    } catch {
      throw createAuthError("AUTH_BACKEND_UNAVAILABLE", "ticket backend is unavailable");
    }

    if (owner !== payload.playerId) {
      throw createAuthError("TICKET_REVOKED", "ticket is revoked or owned by another player");
    }

    if (currentVersion === null || currentVersion === undefined) {
      throw createAuthError("TICKET_REVOKED", "ticket version is missing");
    }

    const ticketVersion = payload.ver ?? 1;
    const activeVersion = Number.parseInt(String(currentVersion), 10);
    if (!Number.isInteger(ticketVersion) || !Number.isInteger(activeVersion) || ticketVersion !== activeVersion) {
      throw createAuthError("TICKET_REVOKED", "ticket version is no longer active");
    }

    return {
      playerId: payload.playerId,
      characterId: payload.characterId,
      ticketVersion
    };
  }
}

export async function authenticatePlayerHeaders(headers, authService) {
  const ticket = extractGameTicket(headers);
  return authService.authenticateTicket(ticket);
}

export function validateServiceToken(headers, config) {
  const expectedToken = String(config?.mailServiceToken || "").trim();
  const providedToken = extractServiceToken(headers);

  if (!expectedToken) {
    throw createAuthError("MAIL_SERVICE_TOKEN_NOT_CONFIGURED", "mail service token is not configured");
  }

  if (!providedToken) {
    throw createAuthError("MAIL_SERVICE_TOKEN_REQUIRED", "mail service token is required");
  }

  const expectedBuffer = Buffer.from(expectedToken);
  const providedBuffer = Buffer.from(String(providedToken));
  if (
    expectedBuffer.length !== providedBuffer.length ||
    !crypto.timingSafeEqual(expectedBuffer, providedBuffer)
  ) {
    throw createAuthError("MAIL_SERVICE_TOKEN_INVALID", "mail service token is invalid", 403);
  }

  return true;
}
