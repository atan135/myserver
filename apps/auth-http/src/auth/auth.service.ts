import { Inject, Injectable } from "@nestjs/common";

import { assertValidGuestId, assertValidLoginName, createPasswordSalt, hashPassword, verifyPassword } from "../password-utils.js";
import { AUTH_ACCOUNT_LOCKOUT, AUTH_CONFIG, AUTH_MYSQL_STORE, AUTH_SERVICE_DISCOVERY, AUTH_STORE } from "../tokens.js";
import { getClientIp } from "../common/client-ip.js";
import { badRequest, forbidden, unauthorized } from "../common/http-exception.js";
import type { GuestLoginDto } from "./dto/guest-login.dto.js";
import type { LoginDto } from "./dto/login.dto.js";

function getBearerToken(req: any): string | null {
  const authorization = req.headers.authorization;
  if (!authorization?.startsWith("Bearer ")) {
    return null;
  }

  return authorization.slice("Bearer ".length).trim();
}

@Injectable()
export class AuthService {
  constructor(
    @Inject(AUTH_CONFIG) private readonly config: any,
    @Inject(AUTH_STORE) private readonly authStore: any,
    @Inject(AUTH_ACCOUNT_LOCKOUT) private readonly accountLockout: any,
    @Inject(AUTH_MYSQL_STORE) private readonly mysqlStore: any,
    @Inject(AUTH_SERVICE_DISCOVERY) private readonly serviceDiscovery: any
  ) {}

  get gameProxyHost() {
    return this.config.gameProxyHost;
  }

  get gameProxyPort() {
    return this.config.gameProxyPort;
  }

  async buildServicePayload() {
    if (!this.serviceDiscovery) {
      return {
        game: {
          host: this.config.gameProxyHost,
          port: this.config.gameProxyPort,
          protocol: "kcp"
        },
        chat: null,
        mail: null,
        announce: null
      };
    }

    return this.serviceDiscovery.discoverClientServices();
  }

  async buildLoginSuccess(session: any) {
    const services = await this.buildServicePayload();

    return {
      ok: true,
      playerId: session.playerId,
      guestId: session.guestId || null,
      loginName: session.loginName || null,
      accessToken: session.accessToken,
      ticket: session.gameTicket.value,
      ticketExpiresAt: session.gameTicket.expiresAt,
      gameProxyHost: this.config.gameProxyHost,
      gameProxyPort: this.config.gameProxyPort,
      services
    };
  }

  async login(dto: LoginDto, req: any, res: any) {
    const loginName = dto?.loginName;
    const password = dto?.password;
    const clientIp = getClientIp(req, this.config);

    if (typeof loginName !== "string" || loginName.trim().length === 0) {
      throw badRequest("INVALID_LOGIN_NAME", "loginName must be a non-empty string");
    }

    try {
      assertValidLoginName(loginName);
    } catch (err: any) {
      throw badRequest("INVALID_LOGIN_NAME", err.message);
    }

    if (typeof password !== "string" || password.length === 0) {
      throw badRequest("INVALID_PASSWORD", "password must be a non-empty string");
    }

    if (password.length < 6 || password.length > 128) {
      throw badRequest("INVALID_PASSWORD", "password must be between 6 and 128 characters");
    }

    if (!this.config.mysqlEnabled) {
      throw badRequest("PASSWORD_LOGIN_UNAVAILABLE", "mysql auth store is disabled");
    }

    if (this.config.accountLockEnabled && this.accountLockout) {
      const lockStatus = await this.accountLockout.getLockStatus(loginName);
      if (lockStatus.locked) {
        this.mysqlStore?.appendSecurityAudit?.({
          eventType: "account_locked_login_attempt",
          targetType: "account",
          targetValue: loginName,
          clientIp,
          severity: "critical",
          details: { remainingSeconds: lockStatus.remainingSeconds }
        });

        if (typeof res?.setHeader === "function") {
          res.setHeader("Retry-After", String(lockStatus.remainingSeconds));
        } else {
          res?.header?.("Retry-After", String(lockStatus.remainingSeconds));
        }
        throw forbidden("ACCOUNT_LOCKED", `Account is locked. Try again in ${lockStatus.remainingSeconds} seconds`);
      }
    }

    let session;
    try {
      session = await this.authStore.createPasswordSession(loginName, password, clientIp);

      if (this.config.accountLockEnabled && this.accountLockout) {
        await this.accountLockout.clearFailedAttempts(loginName);
      }
    } catch (error: any) {
      if (this.config.accountLockEnabled && this.accountLockout) {
        const { locked, attempts } = await this.accountLockout.recordFailedAttempt(loginName);

        if (locked) {
          this.mysqlStore?.appendSecurityAudit?.({
            eventType: "account_locked",
            targetType: "account",
            targetValue: loginName,
            clientIp,
            severity: "critical",
            details: { attempts }
          });
        }
      }

      if (error.code === "INVALID_LOGIN_CREDENTIALS" || error.code === "ACCOUNT_DISABLED") {
        this.mysqlStore?.appendSecurityAudit?.({
          eventType: "login_failed",
          targetType: "account",
          targetValue: loginName,
          clientIp,
          severity: "warning",
          details: { reason: error.code }
        });

        throw unauthorized(error.code);
      }

      throw error;
    }

    return this.buildLoginSuccess(session);
  }

  async guestLogin(dto: GuestLoginDto, req: any) {
    const guestId = dto?.guestId;

    let normalizedGuestId = null;
    if (guestId !== undefined) {
      if (typeof guestId !== "string") {
        throw badRequest("INVALID_GUEST_ID", "guestId must be a string");
      }
      try {
        normalizedGuestId = assertValidGuestId(guestId);
      } catch (err: any) {
        throw badRequest("INVALID_GUEST_ID", err.message);
      }
    }

    const session = await this.authStore.createGuestSession(normalizedGuestId, getClientIp(req, this.config));
    return this.buildLoginSuccess(session);
  }

  async me(req: any) {
    const accessToken = getBearerToken(req);
    if (!accessToken) {
      throw unauthorized("MISSING_BEARER_TOKEN");
    }

    const session = await this.authStore.getSessionByAccessToken(accessToken);
    if (!session) {
      throw unauthorized("INVALID_ACCESS_TOKEN");
    }

    return {
      ok: true,
      playerId: session.playerId,
      guestId: session.guestId || null,
      loginName: session.loginName || null,
      createdAt: session.createdAt
    };
  }

  async logout(req: any, body: any) {
    const accessToken = getBearerToken(req);
    if (!accessToken) {
      throw unauthorized("MISSING_BEARER_TOKEN");
    }

    const clientIp = getClientIp(req, this.config);
    const result = await this.authStore.destroySession(accessToken, clientIp);
    if (!result.destroyed) {
      throw unauthorized("INVALID_ACCESS_TOKEN");
    }

    const { ticket } = body || {};
    if (ticket && typeof ticket === "string") {
      await this.authStore.revokeTicket(ticket, clientIp, { expectedPlayerId: result.playerId });
    }

    return {
      ok: true,
      message: "Logged out"
    };
  }

  async changePassword(req: any, body: any) {
    const accessToken = getBearerToken(req);
    if (!accessToken) {
      throw unauthorized("MISSING_BEARER_TOKEN");
    }

    const session = await this.authStore.getSessionByAccessToken(accessToken);
    if (!session) {
      throw unauthorized("INVALID_ACCESS_TOKEN");
    }

    if (!this.config.mysqlEnabled || !this.mysqlStore?.enabled) {
      throw badRequest("PASSWORD_CHANGE_UNAVAILABLE", "mysql auth store is disabled");
    }

    const { oldPassword, newPassword } = body || {};

    if (typeof oldPassword !== "string" || oldPassword.length === 0) {
      throw badRequest("INVALID_OLD_PASSWORD", "oldPassword must be a non-empty string");
    }

    if (typeof newPassword !== "string" || newPassword.length === 0) {
      throw badRequest("INVALID_NEW_PASSWORD", "newPassword must be a non-empty string");
    }

    if (newPassword.length < 6 || newPassword.length > 128) {
      throw badRequest("INVALID_NEW_PASSWORD", "newPassword must be between 6 and 128 characters");
    }

    const clientIp = getClientIp(req, this.config);
    const account = await this.mysqlStore.findPasswordAccountByPlayerId(session.playerId);
    if (!account) {
      throw badRequest("NOT_PASSWORD_ACCOUNT", "This account does not support password change");
    }

    const passwordMatches =
      account.passwordAlgo === "scrypt" &&
      await verifyPassword(oldPassword, account.passwordSalt, account.passwordHash);

    if (!passwordMatches) {
      this.mysqlStore.appendSecurityAudit({
        eventType: "change_password_failed",
        targetType: "account",
        targetValue: account.loginName,
        clientIp,
        severity: "warning",
        details: { reason: "invalid_old_password", playerId: session.playerId }
      });
      throw forbidden("OLD_PASSWORD_MISMATCH", "Old password is incorrect");
    }

    const newSalt = createPasswordSalt();
    const newHash = await hashPassword(newPassword, newSalt);

    await this.mysqlStore.updatePassword(session.playerId, {
      passwordSalt: newSalt,
      passwordHash: newHash
    });

    await this.mysqlStore.appendAuthAudit({
      playerId: session.playerId,
      eventType: "password_changed",
      accessToken,
      clientIp,
      details: { loginName: account.loginName }
    });

    const psKey = this.authStore.prefixedKey(`player-session:${session.playerId}`);
    const currentMappedToken = await this.authStore.redis.get(psKey);
    if (currentMappedToken && currentMappedToken !== accessToken) {
      await this.authStore.redis.del(this.authStore.prefixedKey(`session:${currentMappedToken}`));
      await this.authStore.redis.del(this.authStore.prefixedKey(`session-activity:${currentMappedToken}`));
    }
    await this.authStore.publishSessionKick(session.playerId, "password_changed");
    await this.authStore.invalidatePlayerTickets(session.playerId);

    await this.authStore.redis.del(this.authStore.prefixedKey(`session:${accessToken}`));
    await this.authStore.redis.del(this.authStore.prefixedKey(`session-activity:${accessToken}`));
    await this.authStore.redis.del(psKey);

    return {
      ok: true,
      message: "Password changed successfully. Please login again."
    };
  }
}
