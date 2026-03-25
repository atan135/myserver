import { Router } from "express";

import { badRequest, unauthorized } from "./http-errors.js";

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

export function createRoutes(config, authStore) {
  const router = Router();

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
      storage: config.mysqlEnabled ? "redis+mysql" : "redis",
      nextSteps: [
        "room-game-loop",
        "rate-limit",
        "admin-control-plane"
      ]
    });
  });

  router.post("/api/v1/auth/guest-login", async (req, res) => {
    const guestId = req.body?.guestId;
    if (guestId !== undefined && typeof guestId !== "string") {
      return badRequest(res, "INVALID_GUEST_ID", "guestId must be a string");
    }

    const session = await authStore.createGuestSession(guestId, getClientIp(req));

    return res.status(201).json({
      ok: true,
      playerId: session.playerId,
      guestId: session.guestId,
      accessToken: session.accessToken,
      ticket: session.gameTicket.value,
      ticketExpiresAt: session.gameTicket.expiresAt
    });
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
      guestId: session.guestId,
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
      ticketExpiresAt: ticket.expiresAt
    });
  });

  return router;
}
