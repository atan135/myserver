import { Router } from "express";

import { badRequest, unauthorized, rateLimited, forbidden } from "./http-errors.js";
import { assertValidGuestId, normalizeGuestId } from "./password-utils.js";

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

function sendLoginSuccess(res, session, config) {
  return res.status(201).json({
    ok: true,
    playerId: session.playerId,
    guestId: session.guestId || null,
    loginName: session.loginName || null,
    accessToken: session.accessToken,
    ticket: session.gameTicket.value,
    ticketExpiresAt: session.gameTicket.expiresAt,
    gameProxyHost: config.gameProxyHost,
    gameProxyPort: config.gameProxyPort
  });
}

export function createRoutes(config, authStore, gameAdminClient, rateLimiter, accountLockout, mysqlStore) {
  const router = Router();

  // Middleware: IP rate limiting for all routes
  router.use(async (req, res, next) => {
    const clientIp = getClientIp(req);

    if (config.ratelimitEnabled && rateLimiter) {
      const isLimited = await rateLimiter.isIpRateLimited(clientIp);
      if (isLimited) {
        // Log security event
        mysqlStore?.appendSecurityAudit?.({
          eventType: "ip_rate_limited",
          targetType: "ip",
          targetValue: clientIp,
          clientIp,
          severity: "warning",
          details: { path: req.path }
        });

        return rateLimited(res, "IP_RATE_LIMITED", "Too many requests from this IP");
      }
    }

    next();
  });

  router.get("/healthz", async (_req, res) => {
    res.json({
      ok: true,
      service: config.appName,
      env: config.env,
      storage: config.mysqlEnabled ? "redis+mysql" : "redis"
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

    if (typeof password !== "string" || password.length === 0) {
      return badRequest(
        res,
        "INVALID_PASSWORD",
        "password must be a non-empty string"
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

        return forbidden(
          res,
          "ACCOUNT_LOCKED",
          `Account is locked. Try again in ${lockStatus.remainingSeconds} seconds`
        );
      }
    }

    try {
      const session = await authStore.createPasswordSession(
        loginName,
        password,
        clientIp
      );

      // Clear failed attempts on successful login
      if (config.accountLockEnabled && accountLockout) {
        await accountLockout.clearFailedAttempts(loginName);
      }

      return sendLoginSuccess(res, session, config);
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

    return sendLoginSuccess(res, session, config);
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

    return res.status(201).json({
      ok: true,
      playerId: session.playerId,
      ticket: ticket.value,
      ticketExpiresAt: ticket.expiresAt,
      gameProxyHost: config.gameProxyHost,
      gameProxyPort: config.gameProxyPort
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

  router.get("/api/v1/internal/game-server/status", async (_req, res) => {
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
