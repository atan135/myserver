import crypto from "node:crypto";

import { encodeSubjectToken } from "./nats-client.js";
import { log } from "./logger.js";
import { normalizeLoginName, verifyPassword } from "./password-utils.js";
import { BLOCKLIST_UNAVAILABLE_ERROR, PLAYER_BLOCKED_ERROR, RedisBlocklistChecker } from "./blocklist.js";

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

function sessionActivityKey(accessToken) {
  return `session-activity:${accessToken}`;
}

function ticketKey(ticket) {
  return `ticket:${hashTicket(ticket)}`;
}

function playerSessionKey(playerId) {
  return `player-session:${playerId}`;
}

function playerTicketVersionKey(playerId) {
  return `player-ticket-version:${playerId}`;
}

function createAuthError(code, message = code) {
  const error = new Error(message);
  error.code = code;
  return error;
}

const noopNatsClient = {
  async publishJson() {}
};

function logSecurity(level, message, extra) {
  try {
    log(level, message, extra);
  } catch {
    // Some focused tests instantiate AuthStore without bootstrapping log4js.
  }
}

export class AuthStore {
  constructor(config, redis, dbStore = null, nats = noopNatsClient, blocklist = RedisBlocklistChecker.disabled()) {
    this.config = config;
    this.redis = redis;
    this.dbStore = dbStore;
    this.nats = nats;
    this.blocklist = blocklist;
  }

  prefixedKey(key) {
    return `${this.config.redisKeyPrefix || ""}${key}`;
  }

  async markSessionActive(accessToken) {
    try {
      await this.redis.set(
        this.prefixedKey(sessionActivityKey(accessToken)),
        Date.now(),
        "EX",
        300
      );
    } catch (error) {
      // Session activity should improve observability, not break auth.
      console.error("[auth-store] markSessionActive error:", error);
    }
  }

  async createGuestSession(guestId, clientIp = null) {
    const normalizedGuestId = guestId || `guest-${crypto.randomUUID()}`;
    const account = this.dbStore
      ? await this.dbStore.findOrCreateGuestPlayer(normalizedGuestId)
      : {
          playerId: `player-${crypto.randomUUID()}`,
          guestId: normalizedGuestId
        };

    await this.assertAccountLoginAllowed(account, clientIp, {
      eventType: "guest_login_failed",
      details: { guestId: normalizedGuestId }
    });
    await this.assertPlayerNotBlocked(account.playerId, clientIp, "guest_login");
    await this.dbStore?.touchPlayerLastLogin?.(account.playerId);

    return this.createSessionForAccount(account, {
      clientIp,
      eventType: "guest_login"
    });
  }

  async createPasswordSession(loginName, password, clientIp = null) {
    if (!this.dbStore?.enabled) {
      throw createAuthError("PASSWORD_LOGIN_UNAVAILABLE");
    }

    const normalizedLoginName = normalizeLoginName(loginName);
    const account = await this.dbStore.findPasswordAccountByLoginName(
      normalizedLoginName
    );

    if (!account) {
      await this.dbStore.appendAuthAudit({
        eventType: "password_login_failed",
        clientIp,
        details: {
          loginName: normalizedLoginName,
          reason: "not_found"
        }
      });
      throw createAuthError("INVALID_LOGIN_CREDENTIALS");
    }

    await this.assertAccountLoginAllowed(account, clientIp, {
      eventType: "password_login_failed",
      details: { loginName: account.loginName }
    });

    if (account.status !== "active") {
      await this.dbStore.appendAuthAudit({
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
      await verifyPassword(password, account.passwordSalt, account.passwordHash);

    if (!passwordMatches) {
      await this.dbStore.appendAuthAudit({
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

    await this.assertPlayerNotBlocked(account.playerId, clientIp, "password_login");

    await this.dbStore.touchPlayerLastLogin(account.playerId);

    return this.createSessionForAccount(account, {
      clientIp,
      eventType: "password_login"
    });
  }

  async assertAccountLoginAllowed(account, clientIp = null, audit = {}) {
    if (!account || !account.status || account.status === "active") {
      return;
    }

    if (account.status === "banned" && account.banExpiresAt) {
      const expiresAt = new Date(account.banExpiresAt);
      if (!Number.isNaN(expiresAt.getTime()) && expiresAt.getTime() <= Date.now()) {
        const restored = await this.dbStore?.restoreExpiredBan?.(account.playerId);
        if (restored) {
          account.status = "active";
          account.banExpiresAt = null;
          await this.dbStore?.appendAuthAudit?.({
            playerId: account.playerId,
            eventType: "account_ban_expired",
            clientIp,
            details: { banExpiresAt: expiresAt.toISOString() }
          });
          return;
        }
      }
    }

    await this.dbStore?.appendAuthAudit?.({
      playerId: account.playerId,
      eventType: audit.eventType || "login_failed",
      clientIp,
      details: {
        ...(audit.details || {}),
        reason: `status:${account.status}`,
        banExpiresAt: account.banExpiresAt || null
      }
    });
    throw createAuthError("ACCOUNT_DISABLED");
  }

  async assertPlayerCanIssueTicket(playerId, clientIp = null) {
    const account = await this.dbStore?.findPlayerAuthStateByPlayerId?.(playerId);
    if (!account) {
      return;
    }
    await this.assertAccountLoginAllowed(account, clientIp, {
      eventType: "ticket_issue_failed",
      details: { reason: "account_status" }
    });
  }

  async assertPlayerNotBlocked(playerId, clientIp = null, source = null) {
    const decision = await this.blocklist.checkPlayer(playerId);
    if (!decision.blocked) {
      return;
    }

    if (decision.unavailable) {
      logSecurity("warn", "security.blocklist_unavailable", {
        targetType: "player",
        playerId,
        clientIp,
        source
      });
      await this.dbStore?.appendSecurityAudit?.({
        eventType: "blocklist_unavailable",
        targetType: "player",
        targetValue: playerId,
        clientIp,
        severity: "critical",
        details: { source }
      });
      throw createAuthError(BLOCKLIST_UNAVAILABLE_ERROR, "redis blocklist is unavailable");
    }

    if (decision.error === PLAYER_BLOCKED_ERROR) {
      logSecurity("warn", "security.player_blocked", {
        playerId,
        clientIp,
        source
      });
      await this.dbStore?.appendSecurityAudit?.({
        eventType: "player_blocked",
        targetType: "player",
        targetValue: playerId,
        clientIp,
        severity: "critical",
        details: { source }
      });
    }
    throw createAuthError(PLAYER_BLOCKED_ERROR, "player is blocked");
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

    const psKey = this.prefixedKey(playerSessionKey(account.playerId));
    const sessionKeyName = this.prefixedKey(sessionKey(accessToken));
    const oldAccessToken = await this.replacePlayerSession({
      playerSessionKeyName: psKey,
      accessToken,
      sessionKeyName,
      sessionData: JSON.stringify(session)
    });

    if (oldAccessToken) {
      await this.publishSessionKick(account.playerId, "new_login");
      await this.dbStore?.appendAuthAudit({
        playerId: account.playerId,
        eventType: "session_kicked",
        accessToken: oldAccessToken,
        clientIp,
        details: { reason: "new_login" }
      });
    }

    const gameTicket = await this.issueGameTicket(account.playerId, clientIp);
    await this.markSessionActive(accessToken);

    await this.dbStore?.appendAuthAudit({
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

  async replacePlayerSession({
    playerSessionKeyName,
    accessToken,
    sessionKeyName,
    sessionData
  }) {
    const script = `
      local player_session_key = KEYS[1]
      local new_session_key = KEYS[2]
      local old_token = redis.call("GET", player_session_key)
      if old_token then
        redis.call("DEL", ARGV[1] .. old_token)
        redis.call("DEL", ARGV[2] .. old_token)
      end
      redis.call("SET", new_session_key, ARGV[3], "EX", tonumber(ARGV[5]))
      redis.call("SET", player_session_key, ARGV[4], "EX", tonumber(ARGV[5]))
      return old_token
    `;

    if (typeof this.redis.eval === "function") {
      return this.redis.eval(
        script,
        2,
        playerSessionKeyName,
        sessionKeyName,
        this.prefixedKey("session:"),
        this.prefixedKey("session-activity:"),
        sessionData,
        accessToken,
        this.config.sessionTtlSeconds
      );
    }

    const oldAccessToken = await this.redis.get(playerSessionKeyName);
    if (oldAccessToken) {
      await this.redis.del(this.prefixedKey(sessionKey(oldAccessToken)));
      await this.redis.del(this.prefixedKey(sessionActivityKey(oldAccessToken)));
    }
    await this.redis.set(sessionKeyName, sessionData, "EX", this.config.sessionTtlSeconds);
    await this.redis.set(playerSessionKeyName, accessToken, "EX", this.config.sessionTtlSeconds);
    return oldAccessToken;
  }

  async publishSessionKick(playerId, reason) {
    const payload = { player_id: playerId, reason };

    await this.nats.publishJson(
      `myserver.session.kick.${encodeSubjectToken(playerId)}`,
      payload
    );
  }

  async getSessionByAccessToken(accessToken) {
    const raw = await this.redis.get(this.prefixedKey(sessionKey(accessToken)));
    if (!raw) {
      return null;
    }

    // Sliding window: renew session TTL on every access
    await this.redis.expire(
      this.prefixedKey(sessionKey(accessToken)),
      this.config.sessionTtlSeconds
    );
    const session = JSON.parse(raw);
    // Also renew player-session mapping
    if (session.playerId) {
      await this.redis.expire(
        this.prefixedKey(playerSessionKey(session.playerId)),
        this.config.sessionTtlSeconds
      );
    }

    await this.markSessionActive(accessToken);
    return session;
  }

  async issueGameTicket(playerId, clientIp = null) {
    const versionKey = this.prefixedKey(playerTicketVersionKey(playerId));
    let ticketVersion = await this.redis.get(versionKey);
    if (!ticketVersion) {
      ticketVersion = "1";
      await this.redis.set(versionKey, ticketVersion);
    }

    const expiresAt = new Date(
      Date.now() + this.config.ticketTtlSeconds * 1000
    ).toISOString();
    const payload = {
      playerId,
      nonce: crypto.randomBytes(12).toString("hex"),
      ver: Number.parseInt(ticketVersion, 10) || 1,
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

    await this.dbStore?.appendAuthAudit({
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

  async destroySession(accessToken, clientIp = null) {
    const sessionData = await this.getSessionByAccessToken(accessToken);
    if (!sessionData) {
      return { destroyed: false };
    }

    await this.redis.del(this.prefixedKey(sessionKey(accessToken)));
    await this.redis.del(this.prefixedKey(sessionActivityKey(accessToken)));

    // Clean up player-session mapping if it still points to this token
    const psKey = this.prefixedKey(playerSessionKey(sessionData.playerId));
    const currentToken = await this.redis.get(psKey);
    if (currentToken === accessToken) {
      await this.redis.del(psKey);
    }

    await this.dbStore?.appendAuthAudit({
      playerId: sessionData.playerId,
      guestId: sessionData.guestId || null,
      eventType: "logout",
      accessToken,
      clientIp,
      details: {
        loginName: sessionData.loginName || null
      }
    });

    return { destroyed: true, playerId: sessionData.playerId };
  }

  async getTicketOwner(ticket) {
    const key = this.prefixedKey(ticketKey(ticket));
    return this.redis.get(key);
  }

  async invalidatePlayerTickets(playerId) {
    return this.redis.incr(this.prefixedKey(playerTicketVersionKey(playerId)));
  }

  async revokeTicket(ticket, clientIp = null, options = {}) {
    const key = this.prefixedKey(ticketKey(ticket));
    const playerId = await this.redis.get(key);

    if (
      options.expectedPlayerId &&
      playerId &&
      playerId !== options.expectedPlayerId
    ) {
      const error = createAuthError("TICKET_OWNER_MISMATCH");
      error.ticketOwner = playerId;
      throw error;
    }

    await this.redis.del(key);

    if (playerId && this.dbStore) {
      await this.dbStore.appendAuthAudit({
        playerId,
        eventType: "revoke_ticket",
        ticket,
        clientIp,
        details: {
          action: "logout"
        }
      });

      this.dbStore.appendSecurityAudit?.({
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
