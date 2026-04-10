import crypto from "node:crypto";

import { normalizeLoginName, verifyPassword } from "./password-utils.js";

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

function createAuthError(code, message = code) {
  const error = new Error(message);
  error.code = code;
  return error;
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

    return this.createSessionForAccount(account, {
      clientIp,
      eventType: "guest_login"
    });
  }

  async createPasswordSession(loginName, password, clientIp = null) {
    if (!this.mysqlStore?.enabled) {
      throw createAuthError("PASSWORD_LOGIN_UNAVAILABLE");
    }

    const normalizedLoginName = normalizeLoginName(loginName);
    const account = await this.mysqlStore.findPasswordAccountByLoginName(
      normalizedLoginName
    );

    if (!account) {
      await this.mysqlStore.appendAuthAudit({
        eventType: "password_login_failed",
        clientIp,
        details: {
          loginName: normalizedLoginName,
          reason: "not_found"
        }
      });
      throw createAuthError("INVALID_LOGIN_CREDENTIALS");
    }

    if (account.status !== "active") {
      await this.mysqlStore.appendAuthAudit({
        playerId: account.playerId,
        eventType: "password_login_failed",
        clientIp,
        details: {
          loginName: account.loginName,
          reason: `status:${account.status}`
        }
      });
      throw createAuthError("ACCOUNT_DISABLED");
    }

    const passwordMatches =
      account.passwordAlgo === "scrypt" &&
      verifyPassword(password, account.passwordSalt, account.passwordHash);

    if (!passwordMatches) {
      await this.mysqlStore.appendAuthAudit({
        playerId: account.playerId,
        eventType: "password_login_failed",
        clientIp,
        details: {
          loginName: account.loginName,
          reason: "invalid_password"
        }
      });
      throw createAuthError("INVALID_LOGIN_CREDENTIALS");
    }

    await this.mysqlStore.touchPlayerLastLogin(account.playerId);

    return this.createSessionForAccount(account, {
      clientIp,
      eventType: "password_login"
    });
  }

  async createSessionForAccount(account, { clientIp = null, eventType }) {
    const accessToken = crypto.randomBytes(24).toString("hex");
    const session = {
      accessToken,
      playerId: account.playerId,
      guestId: account.guestId || null,
      loginName: account.loginName || null,
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
      guestId: account.guestId || null,
      eventType,
      accessToken,
      ticket: gameTicket.value,
      clientIp,
      details: {
        sessionCreatedAt: session.createdAt,
        loginName: account.loginName || null
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

  async revokeTicket(ticket, clientIp = null) {
    const key = this.prefixedKey(ticketKey(ticket));
    const playerId = await this.redis.get(key);

    await this.redis.del(key);

    if (playerId && this.mysqlStore) {
      await this.mysqlStore.appendAuthAudit({
        playerId,
        eventType: "revoke_ticket",
        ticket,
        clientIp,
        details: {
          action: "logout"
        }
      });

      this.mysqlStore.appendSecurityAudit?.({
        eventType: "ticket_revoked",
        targetType: "ticket",
        targetValue: hashTicket(ticket).slice(0, 16) + "...",
        clientIp,
        severity: "info",
        details: { playerId }
      });
    }

    return { revoked: true };
  }
}
