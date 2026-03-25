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
  constructor(config, redis, mysqlStore = null) {
    this.config = config;
    this.redis = redis;
    this.mysqlStore = mysqlStore;
  }

  prefixedKey(key) {
    return `${this.config.redisKeyPrefix || ""}${key}`;
  }

  async createGuestSession(guestId, clientIp = null) {
    const normalizedGuestId = guestId || `guest-${crypto.randomUUID()}`;
    const account = this.mysqlStore
      ? await this.mysqlStore.findOrCreateGuestPlayer(normalizedGuestId)
      : {
          playerId: `player-${crypto.randomUUID()}`,
          guestId: normalizedGuestId
        };
    const accessToken = crypto.randomBytes(24).toString("hex");
    const session = {
      accessToken,
      guestId: account.guestId,
      playerId: account.playerId,
      createdAt: new Date().toISOString()
    };
    const gameTicket = await this.issueGameTicket(account.playerId, clientIp);

    await this.redis.set(
      this.prefixedKey(sessionKey(accessToken)),
      JSON.stringify(session),
      "EX",
      this.config.sessionTtlSeconds
    );

    await this.mysqlStore?.appendAuthAudit({
      playerId: account.playerId,
      guestId: account.guestId,
      eventType: "guest_login",
      accessToken,
      ticket: gameTicket.value,
      clientIp,
      details: {
        sessionCreatedAt: session.createdAt
      }
    });

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

  async issueGameTicket(playerId, clientIp = null) {
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

    await this.mysqlStore?.appendAuthAudit({
      playerId,
      eventType: "issue_ticket",
      ticket,
      clientIp,
      details: {
        expiresAt
      }
    });

    return {
      value: ticket,
      expiresAt
    };
  }
}
