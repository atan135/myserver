import crypto from "node:crypto";

function base64UrlEncode(value) {
  return Buffer.from(value).toString("base64url");
}

function signTicketPayload(payloadB64, secret) {
  return crypto
    .createHmac("sha256", secret)
    .update(payloadB64)
    .digest("base64url");
}

function hashTicket(ticket) {
  return crypto.createHash("sha256").update(ticket).digest("hex");
}

function sessionKey(accessToken) {
  return `session:${accessToken}`;
}

function ticketKey(ticket) {
  return `ticket:${hashTicket(ticket)}`;
}

export class AuthStore {
  constructor(config, redis) {
    this.config = config;
    this.redis = redis;
  }

  prefixedKey(key) {
    return `${this.config.redisKeyPrefix || ""}${key}`;
  }

  async createGuestSession(guestId) {
    const normalizedGuestId = guestId || `guest-${crypto.randomUUID()}`;
    const playerId = `player-${crypto.randomUUID()}`;
    const accessToken = crypto.randomBytes(24).toString("hex");
    const session = {
      accessToken,
      guestId: normalizedGuestId,
      playerId,
      createdAt: new Date().toISOString()
    };
    const gameTicket = await this.issueGameTicket(playerId);

    await this.redis.set(
      this.prefixedKey(sessionKey(accessToken)),
      JSON.stringify(session),
      "EX",
      this.config.sessionTtlSeconds
    );

    return {
      ...session,
      gameTicket
    };
  }

  async getSessionByAccessToken(accessToken) {
    const raw = await this.redis.get(this.prefixedKey(sessionKey(accessToken)));
    if (!raw) {
      return null;
    }

    return JSON.parse(raw);
  }

  async issueGameTicket(playerId) {
    const expiresAt = new Date(
      Date.now() + this.config.ticketTtlSeconds * 1000
    ).toISOString();
    const payload = {
      playerId,
      nonce: crypto.randomBytes(12).toString("hex"),
      exp: expiresAt
    };
    const payloadB64 = base64UrlEncode(JSON.stringify(payload));
    const signature = signTicketPayload(payloadB64, this.config.ticketSecret);
    const ticket = `${payloadB64}.${signature}`;

    await this.redis.set(
      this.prefixedKey(ticketKey(ticket)),
      playerId,
      "EX",
      this.config.ticketTtlSeconds
    );

    return {
      value: ticket,
      expiresAt
    };
  }
}
