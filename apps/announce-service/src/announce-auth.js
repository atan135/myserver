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

function firstHeaderValue(value) {
  return Array.isArray(value) ? value[0] || "" : value || "";
}

function getHeaderValue(headers = {}, name) {
  const direct = firstHeaderValue(headers[name]);
  if (direct) {
    return direct;
  }

  const lowerName = name.toLowerCase();
  for (const [key, value] of Object.entries(headers)) {
    if (key.toLowerCase() === lowerName) {
      return firstHeaderValue(value);
    }
  }

  return "";
}

function constantTimeEqual(actual, expected) {
  const actualBuffer = Buffer.from(String(actual || ""));
  const expectedBuffer = Buffer.from(String(expected || ""));
  return (
    actualBuffer.length === expectedBuffer.length &&
    crypto.timingSafeEqual(actualBuffer, expectedBuffer)
  );
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
  const authorization = getHeaderValue(headers, "authorization").trim();
  const match = authorization.match(/^Bearer\s+(.+)$/i);
  return match ? match[1].trim() : null;
}

export function extractGameTicket(headers = {}) {
  return getHeaderValue(headers, "x-game-ticket").trim() || extractBearerToken(headers) || null;
}

export function extractReadTokenCandidates(headers = {}) {
  return [
    extractBearerToken(headers),
    getHeaderValue(headers, "x-read-token").trim(),
    getHeaderValue(headers, "x-service-token").trim()
  ].filter((token) => token);
}

export function hasExplicitReadToken(headers = {}) {
  return Boolean(
    getHeaderValue(headers, "x-read-token").trim() ||
      getHeaderValue(headers, "x-service-token").trim()
  );
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

  if (!payload || typeof payload.playerId !== "string" || !payload.playerId) {
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
    ver: payload.ver === undefined ? undefined : Number(payload.ver),
    exp: payload.exp
  };
}

export class AnnounceReadAuthService {
  constructor(config, redis) {
    this.config = config;
    this.redis = redis;
  }

  async authenticateTicket(ticket) {
    if (!ticket) {
      throw createAuthError("ANNOUNCE_PLAYER_TICKET_REQUIRED", "game ticket is required");
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
    if (
      !Number.isInteger(ticketVersion) ||
      !Number.isInteger(activeVersion) ||
      ticketVersion !== activeVersion
    ) {
      throw createAuthError("TICKET_REVOKED", "ticket version is no longer active");
    }

    return {
      playerId: payload.playerId,
      ticketVersion
    };
  }
}

export function validateReadToken(headers, config) {
  const expectedToken = String(config?.announceReadToken || "").trim();
  if (!expectedToken) {
    return false;
  }

  return extractReadTokenCandidates(headers).some((candidate) =>
    constantTimeEqual(candidate, expectedToken)
  );
}

export async function authenticateAnnounceReadHeaders(headers, authService, config) {
  if (!config?.announceReadAuthRequired) {
    return { type: "disabled" };
  }

  if (validateReadToken(headers, config)) {
    return { type: "read_token" };
  }

  const ticket = extractGameTicket(headers);
  if (!ticket) {
    if (hasExplicitReadToken(headers)) {
      throw createAuthError(
        "ANNOUNCE_READ_TOKEN_INVALID",
        "announcement read token is invalid",
        403
      );
    }

    throw createAuthError(
      "ANNOUNCE_READ_AUTH_REQUIRED",
      "Announcement read APIs require a read token or game ticket"
    );
  }

  const auth = await authService.authenticateTicket(ticket);
  return {
    type: "game_ticket",
    ...auth
  };
}
