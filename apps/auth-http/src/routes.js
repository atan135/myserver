import { Router } from "express";

import { badRequest, unauthorized, rateLimited, forbidden } from "./http-errors.js";
import { assertValidGuestId, assertValidLoginName, normalizeGuestId, verifyPassword, createPasswordSalt, hashPassword } from "./password-utils.js";

function getBearerToken(req) {
  const authorization = req.headers.authorization;
  if (!authorization?.startsWith("Bearer ")) {
    return null;
  }

  return authorization.slice("Bearer ".length).trim();
}

function getClientIp(req) {
  const forwardedFor = req.headers["x-forwarded-for"];
  if (typeof forwardedFor === "string" && forwardedFor.length > 0) {
    return forwardedFor.split(",")[0].trim();
  }

  return req.socket.remoteAddress || null;
}

function handleGameServerError(res, error) {
  return res.status(502).json({
    ok: false,
    error: error.code || "GAME_SERVER_UNAVAILABLE",
    message: error.message
  });
}

function verifyInternalToken(req, res, config) {
  const token = config.internalApiToken;
  if (!token) {
    return true;
  }

  const provided = req.headers["x-service-token"];
  if (provided === token) {
    return true;
  }

  res.status(401).json({
    ok: false,
    error: "INVALID_SERVICE_TOKEN",
    message: "Missing or invalid X-Service-Token header"
  });
  return false;
}

async function buildServicePayload(config, serviceDiscovery) {
  if (!serviceDiscovery) {
    return {
      game: {
        host: config.gameProxyHost,
        port: config.gameProxyPort,
        protocol: "kcp"
      },
      chat: null,
      mail: null,
      announce: null
    };
  }

  return serviceDiscovery.discoverClientServices();
}

async function sendLoginSuccess(res, session, config, serviceDiscovery) {
  const services = await buildServicePayload(config, serviceDiscovery);

  return res.status(201).json({
    ok: true,
    playerId: session.playerId,
    guestId: session.guestId || null,
    loginName: session.loginName || null,
    accessToken: session.accessToken,
    ticket: session.gameTicket.value,
    ticketExpiresAt: session.gameTicket.expiresAt,
    gameProxyHost: config.gameProxyHost,
    gameProxyPort: config.gameProxyPort,
    services
  });
}

export function createRoutes(
  config,
  authStore,
  gameAdminClient,
  rateLimiter,
  accountLockout,
  mysqlStore,
  serviceDiscovery
) {
  const router = Router();

  // Middleware: IP rate limiting for all routes
  router.use(async (req, res, next) => {
    const clientIp = getClientIp(req);

    if (config.ratelimitEnabled && rateLimiter) {
      const { limited, retryAfterSeconds } = await rateLimiter.isIpRateLimited(clientIp);
      if (limited) {
        // Log security event
        mysqlStore?.appendSecurityAudit?.({
          eventType: "ip_rate_limited",
          targetType: "ip",
          targetValue: clientIp,
          clientIp,
          severity: "warning",
          details: { path: req.path, retryAfterSeconds }
        });

        res.set("Retry-After", String(retryAfterSeconds));
        return rateLimited(res, "IP_RATE_LIMITED", "Too many requests from this IP");
      }
    }

    next();
  });

  router.get("/healthz", async (_req, res) => {
    const checks = { redis: "ok", mysql: "skipped" };
    let healthy = true;

    // Redis PING
    try {
      await authStore.redis.ping();
    } catch {
      checks.redis = "error";
      healthy = false;
    }

    // MySQL SELECT 1 (only if enabled)
    if (config.mysqlEnabled && mysqlStore?.enabled) {
      try {
        await mysqlStore.pool.execute("SELECT 1");
        checks.mysql = "ok";
      } catch {
        checks.mysql = "error";
        healthy = false;
      }
    }

    const status = healthy ? 200 : 503;
    return res.status(status).json({
      ok: healthy,
      service: config.appName,
      env: config.env,
      storage: config.mysqlEnabled ? "redis+mysql" : "redis",
      checks
    });
  });

  router.get("/api/v1/meta", (_req, res) => {
    res.json({
      project: "MyServer",
      service: config.appName,
      stage: "minimum-flow",
      protocol: "json",
      internalProtocol: "protobuf+tcp",
      storage: config.mysqlEnabled ? "redis+mysql" : "redis",
      nextSteps: [
        "room-game-loop",
        "rate-limit",
        "admin-control-plane"
      ]
    });
  });

  router.post("/api/v1/auth/login", async (req, res, next) => {
    const loginName = req.body?.loginName;
    const password = req.body?.password;
    const clientIp = getClientIp(req);

    if (typeof loginName !== "string" || loginName.trim().length === 0) {
      return badRequest(
        res,
        "INVALID_LOGIN_NAME",
        "loginName must be a non-empty string"
      );
    }

    try {
      assertValidLoginName(loginName);
    } catch (err) {
      return badRequest(res, "INVALID_LOGIN_NAME", err.message);
    }

    if (typeof password !== "string" || password.length === 0) {
      return badRequest(
        res,
        "INVALID_PASSWORD",
        "password must be a non-empty string"
      );
    }

    if (password.length < 6 || password.length > 128) {
      return badRequest(
        res,
        "INVALID_PASSWORD",
        "password must be between 6 and 128 characters"
      );
    }

    if (!config.mysqlEnabled) {
      return badRequest(
        res,
        "PASSWORD_LOGIN_UNAVAILABLE",
        "mysql auth store is disabled"
      );
    }

    // Check account lockout
    if (config.accountLockEnabled && accountLockout) {
      const lockStatus = await accountLockout.getLockStatus(loginName);
      if (lockStatus.locked) {
        mysqlStore?.appendSecurityAudit?.({
          eventType: "account_locked_login_attempt",
          targetType: "account",
          targetValue: loginName,
          clientIp,
          severity: "critical",
          details: { remainingSeconds: lockStatus.remainingSeconds }
        });

        res.set("Retry-After", String(lockStatus.remainingSeconds));
        return forbidden(
          res,
          "ACCOUNT_LOCKED",
          `Account is locked. Try again in ${lockStatus.remainingSeconds} seconds`
        );
      }
    }

    let session;
    try {
      session = await authStore.createPasswordSession(loginName, password, clientIp);

      // Clear failed attempts on successful login
      if (config.accountLockEnabled && accountLockout) {
        await accountLockout.clearFailedAttempts(loginName);
      }
    } catch (error) {
      // Record failed attempt
      if (config.accountLockEnabled && accountLockout) {
        const { locked, attempts } = await accountLockout.recordFailedAttempt(loginName);

        if (locked) {
          mysqlStore?.appendSecurityAudit?.({
            eventType: "account_locked",
            targetType: "account",
            targetValue: loginName,
            clientIp,
            severity: "critical",
            details: { attempts }
          });
        }
      }

      if (
        error.code === "INVALID_LOGIN_CREDENTIALS" ||
        error.code === "ACCOUNT_DISABLED"
      ) {
        mysqlStore?.appendSecurityAudit?.({
          eventType: "login_failed",
          targetType: "account",
          targetValue: loginName,
          clientIp,
          severity: "warning",
          details: { reason: error.code }
        });

        return unauthorized(res, error.code);
      }

      return next(error);
    }

    return sendLoginSuccess(res, session, config, serviceDiscovery);
  });

  router.post("/api/v1/auth/guest-login", async (req, res) => {
    const guestId = req.body?.guestId;

    let normalizedGuestId = null;
    if (guestId !== undefined) {
      if (typeof guestId !== "string") {
        return badRequest(res, "INVALID_GUEST_ID", "guestId must be a string");
      }
      try {
        normalizedGuestId = assertValidGuestId(guestId);
      } catch (err) {
        return badRequest(res, "INVALID_GUEST_ID", err.message);
      }
    }

    const session = await authStore.createGuestSession(normalizedGuestId, getClientIp(req));

    return sendLoginSuccess(res, session, config, serviceDiscovery);
  });

  router.get("/api/v1/auth/me", async (req, res) => {
    const accessToken = getBearerToken(req);
    if (!accessToken) {
      return unauthorized(res, "MISSING_BEARER_TOKEN");
    }

    const session = await authStore.getSessionByAccessToken(accessToken);
    if (!session) {
      return unauthorized(res, "INVALID_ACCESS_TOKEN");
    }

    return res.json({
      ok: true,
      playerId: session.playerId,
      guestId: session.guestId || null,
      loginName: session.loginName || null,
      createdAt: session.createdAt
    });
  });

  router.post("/api/v1/auth/logout", async (req, res) => {
    const accessToken = getBearerToken(req);
    if (!accessToken) {
      return unauthorized(res, "MISSING_BEARER_TOKEN");
    }

    const result = await authStore.destroySession(accessToken, getClientIp(req));
    if (!result.destroyed) {
      return unauthorized(res, "INVALID_ACCESS_TOKEN");
    }

    // Revoke ticket if provided
    const { ticket } = req.body || {};
    if (ticket && typeof ticket === "string") {
      await authStore.revokeTicket(ticket, getClientIp(req));
    }

    return res.json({
      ok: true,
      message: "Logged out"
    });
  });

  router.post("/api/v1/auth/change-password", async (req, res, next) => {
    const accessToken = getBearerToken(req);
    if (!accessToken) {
      return unauthorized(res, "MISSING_BEARER_TOKEN");
    }

    const session = await authStore.getSessionByAccessToken(accessToken);
    if (!session) {
      return unauthorized(res, "INVALID_ACCESS_TOKEN");
    }

    if (!config.mysqlEnabled || !mysqlStore?.enabled) {
      return badRequest(res, "PASSWORD_CHANGE_UNAVAILABLE", "mysql auth store is disabled");
    }

    const { oldPassword, newPassword } = req.body || {};

    if (typeof oldPassword !== "string" || oldPassword.length === 0) {
      return badRequest(res, "INVALID_OLD_PASSWORD", "oldPassword must be a non-empty string");
    }

    if (typeof newPassword !== "string" || newPassword.length === 0) {
      return badRequest(res, "INVALID_NEW_PASSWORD", "newPassword must be a non-empty string");
    }

    if (newPassword.length < 6 || newPassword.length > 128) {
      return badRequest(res, "INVALID_NEW_PASSWORD", "newPassword must be between 6 and 128 characters");
    }

    const clientIp = getClientIp(req);

    // Find the account by playerId (must be a password account)
    const account = await mysqlStore.findPasswordAccountByPlayerId(session.playerId);
    if (!account) {
      return badRequest(res, "NOT_PASSWORD_ACCOUNT", "This account does not support password change");
    }

    // Verify old password
    const passwordMatches =
      account.passwordAlgo === "scrypt" &&
      verifyPassword(oldPassword, account.passwordSalt, account.passwordHash);

    if (!passwordMatches) {
      mysqlStore.appendSecurityAudit({
        eventType: "change_password_failed",
        targetType: "account",
        targetValue: account.loginName,
        clientIp,
        severity: "warning",
        details: { reason: "invalid_old_password", playerId: session.playerId }
      });
      return forbidden(res, "OLD_PASSWORD_MISMATCH", "Old password is incorrect");
    }

    // Generate new salt + hash
    const newSalt = createPasswordSalt();
    const newHash = hashPassword(newPassword, newSalt);

    await mysqlStore.updatePassword(session.playerId, {
      passwordSalt: newSalt,
      passwordHash: newHash
    });

    // Audit log
    await mysqlStore.appendAuthAudit({
      playerId: session.playerId,
      eventType: "password_changed",
      accessToken,
      clientIp,
      details: { loginName: account.loginName }
    });

    // Kick existing sessions (force re-login with new password)
    // Destroy current session's player-session mapping and publish kick
    const psKey = authStore.prefixedKey(`player-session:${session.playerId}`);
    const currentMappedToken = await authStore.redis.get(psKey);
    if (currentMappedToken && currentMappedToken !== accessToken) {
      // There's another session for this player; kick it
      await authStore.redis.del(authStore.prefixedKey(`session:${currentMappedToken}`));
      await authStore.redis.del(authStore.prefixedKey(`session-activity:${currentMappedToken}`));
    }
    await authStore.redis.publish(
      authStore.prefixedKey(`session:kick:${session.playerId}`),
      JSON.stringify({ reason: "password_changed" })
    );

    // Destroy current session as well (user must re-login)
    await authStore.redis.del(authStore.prefixedKey(`session:${accessToken}`));
    await authStore.redis.del(authStore.prefixedKey(`session-activity:${accessToken}`));
    await authStore.redis.del(psKey);

    return res.json({
      ok: true,
      message: "Password changed successfully. Please login again."
    });
  });

  router.post("/api/v1/game-ticket/issue", async (req, res) => {
    const accessToken = getBearerToken(req);
    if (!accessToken) {
      return unauthorized(res, "MISSING_BEARER_TOKEN");
    }

    const session = await authStore.getSessionByAccessToken(accessToken);
    if (!session) {
      return unauthorized(res, "INVALID_ACCESS_TOKEN");
    }

    const ticket = await authStore.issueGameTicket(session.playerId, getClientIp(req));
    const services = await buildServicePayload(config, serviceDiscovery);

    return res.status(201).json({
      ok: true,
      playerId: session.playerId,
      ticket: ticket.value,
      ticketExpiresAt: ticket.expiresAt,
      gameProxyHost: config.gameProxyHost,
      gameProxyPort: config.gameProxyPort,
      services
    });
  });

  router.post("/api/v1/game-ticket/revoke", async (req, res) => {
    const accessToken = getBearerToken(req);
    if (!accessToken) {
      return unauthorized(res, "MISSING_BEARER_TOKEN");
    }

    const session = await authStore.getSessionByAccessToken(accessToken);
    if (!session) {
      return unauthorized(res, "INVALID_ACCESS_TOKEN");
    }

    const { ticket } = req.body || {};
    if (!ticket || typeof ticket !== "string") {
      return badRequest(res, "INVALID_TICKET", "ticket must be a non-empty string");
    }

    await authStore.revokeTicket(ticket, getClientIp(req));

    return res.json({
      ok: true,
      message: "Ticket revoked"
    });
  });

  router.get("/api/v1/internal/game-server/status", async (req, res) => {
    if (!verifyInternalToken(req, res, config)) return;

    try {
      const status = await gameAdminClient.getServerStatus();
      return res.json({
        ok: true,
        ...status
      });
    } catch (error) {
      return handleGameServerError(res, error);
    }
  });

  router.post("/api/v1/internal/game-server/config", async (req, res) => {
    if (!verifyInternalToken(req, res, config)) return;

    const key = req.body?.key;
    const value = req.body?.value;

    if (typeof key !== "string" || key.length === 0) {
      return badRequest(res, "INVALID_CONFIG_KEY", "key must be a non-empty string");
    }

    if (typeof value !== "string" || value.length === 0) {
      return badRequest(res, "INVALID_CONFIG_VALUE", "value must be a non-empty string");
    }

    try {
      const result = await gameAdminClient.updateConfig(key, value);
      return res.json({
        ok: result.ok,
        errorCode: result.errorCode
      });
    } catch (error) {
      return handleGameServerError(res, error);
    }
  });

  return router;
}
