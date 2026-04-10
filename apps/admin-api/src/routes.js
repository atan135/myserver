import { Router } from "express";
import jwt from "jsonwebtoken";
import crypto from "node:crypto";

import { badRequest, unauthorized, forbidden, notFound } from "./http-errors.js";

function getClientIp(req) {
  const forwardedFor = req.headers["x-forwarded-for"];
  if (typeof forwardedFor === "string" && forwardedFor.length > 0) {
    return forwardedFor.split(",")[0].trim();
  }
  return req.socket.remoteAddress || null;
}

function getTokenFromHeader(req) {
  const auth = req.headers.authorization;
  if (!auth?.startsWith("Bearer ")) return null;
  return auth.slice("Bearer ".length).trim();
}

export function createRoutes(config, adminStore, gameAdminClient) {
  const router = Router();

  // ============================================================
  // Public Routes
  // ============================================================

  router.post("/api/v1/auth/login", async (req, res) => {
    const { username, password } = req.body || {};

    if (!username || typeof username !== "string" || username.trim().length === 0) {
      return badRequest(res, "INVALID_USERNAME", "username is required");
    }

    if (!password || typeof password !== "string" || password.length === 0) {
      return badRequest(res, "INVALID_PASSWORD", "password is required");
    }

    const admin = await adminStore.findAdminByUsername(username.trim());
    if (!admin) {
      return unauthorized(res, "INVALID_CREDENTIALS", "Invalid username or password");
    }

    if (admin.status !== "active") {
      return forbidden(res, "ACCOUNT_DISABLED", "Account is disabled");
    }

    const passwordValid = await adminStore.verifyPassword(password, admin.passwordHash);
    if (!passwordValid) {
      return unauthorized(res, "INVALID_CREDENTIALS", "Invalid username or password");
    }

    // Generate JWT
    const tokenPayload = {
      sub: admin.id,
      username: admin.username,
      role: admin.role
    };
    const accessToken = jwt.sign(tokenPayload, config.jwtSecret, {
      expiresIn: config.jwtExpiresIn
    });

    await adminStore.updateLastLogin(admin.id);
    await adminStore.appendAuditLog({
      adminId: admin.id,
      adminUsername: admin.username,
      action: "admin_login",
      ip: getClientIp(req)
    });

    return res.status(200).json({
      ok: true,
      accessToken,
      expiresIn: config.jwtExpiresIn,
      admin: {
        id: admin.id,
        username: admin.username,
        displayName: admin.displayName,
        role: admin.role
      }
    });
  });

  // ============================================================
  // Auth Middleware
  // ============================================================

  function requireAuth(roles = []) {
    return async (req, res, next) => {
      const token = getTokenFromHeader(req);
      if (!token) {
        return unauthorized(res, "MISSING_TOKEN", "Authorization token required");
      }

      try {
        const decoded = jwt.verify(token, config.jwtSecret);
        req.admin = decoded;
        next();
      } catch (err) {
        if (err.name === "TokenExpiredError") {
          return unauthorized(res, "TOKEN_EXPIRED", "Token has expired");
        }
        return unauthorized(res, "INVALID_TOKEN", "Invalid token");
      }
    };
  }

  function requireRole(...roles) {
    return (req, res, next) => {
      if (!roles.includes(req.admin.role)) {
        return forbidden(res, "INSUFFICIENT_PERMISSION", "Insufficient permissions");
      }
      next();
    };
  }

  // ============================================================
  // Protected Routes
  // ============================================================

  router.get("/api/v1/auth/me", requireAuth(), async (req, res) => {
    const admin = await adminStore.findAdminByUsername(req.admin.username);
    if (!admin) {
      return notFound(res, "ADMIN_NOT_FOUND");
    }

    return res.json({
      ok: true,
      admin: {
        id: admin.id,
        username: admin.username,
        displayName: admin.displayName,
        role: admin.role
      }
    });
  });

  router.post("/api/v1/auth/logout", requireAuth(), async (req, res) => {
    await adminStore.appendAuditLog({
      adminId: req.admin.sub,
      adminUsername: req.admin.username,
      action: "admin_logout",
      ip: getClientIp(req)
    });

    return res.json({ ok: true, message: "Logged out" });
  });

  // ============================================================
  // Audit Logs
  // ============================================================

  router.get("/api/v1/audit-logs", requireAuth(), async (req, res) => {
    const { limit = 50, offset = 0, action, target_type } = req.query;

    const logs = await adminStore.getAuditLogs({
      limit: Math.min(Number(limit) || 50, 100),
      offset: Number(offset) || 0,
      action,
      targetType: target_type
    });

    const total = await adminStore.countAuditLogs({ action, targetType: target_type });

    return res.json({
      ok: true,
      logs,
      total,
      limit: Math.min(Number(limit) || 50, 100),
      offset: Number(offset) || 0
    });
  });

  // ============================================================
  // Security Logs
  // ============================================================

  router.get("/api/v1/security-logs", requireAuth(), async (req, res) => {
    const { limit = 50, offset = 0, event_type, target_type, severity, client_ip } = req.query;

    const logs = await adminStore.getSecurityLogs({
      limit: Math.min(Number(limit) || 50, 100),
      offset: Number(offset) || 0,
      eventType: event_type,
      targetType: target_type,
      severity,
      clientIp: client_ip
    });

    const total = await adminStore.countSecurityLogs({
      eventType: event_type,
      targetType: target_type,
      severity,
      clientIp: client_ip
    });

    return res.json({
      ok: true,
      logs,
      total,
      limit: Math.min(Number(limit) || 50, 100),
      offset: Number(offset) || 0
    });
  });

  // ============================================================
  // Player Management
  // ============================================================

  router.get("/api/v1/players", requireAuth("admin", "operator", "viewer"), async (req, res) => {
    const { login_name, guest_id, status, limit = 50, offset = 0 } = req.query;

    const players = await adminStore.findPlayers({
      loginName: login_name,
      guestId: guest_id,
      status,
      limit: Math.min(Number(limit) || 50, 100),
      offset: Number(offset) || 0
    });

    const total = await adminStore.countPlayers({
      loginName: login_name,
      guestId: guest_id,
      status
    });

    return res.json({
      ok: true,
      players,
      total,
      limit: Math.min(Number(limit) || 50, 100),
      offset: Number(offset) || 0
    });
  });

  router.get("/api/v1/players/:playerId", requireAuth("admin", "operator", "viewer"), async (req, res) => {
    const { playerId } = req.params;

    const player = await adminStore.findPlayerById(playerId);
    if (!player) {
      return notFound(res, "PLAYER_NOT_FOUND", "Player not found");
    }

    return res.json({ ok: true, player });
  });

  router.put("/api/v1/players/:playerId/status", requireAuth("admin", "operator"), async (req, res) => {
    const { playerId } = req.params;
    const { status } = req.body || {};

    if (!status || !["active", "disabled", "banned"].includes(status)) {
      return badRequest(res, "INVALID_STATUS", "status must be active, disabled, or banned");
    }

    const player = await adminStore.findPlayerById(playerId);
    if (!player) {
      return notFound(res, "PLAYER_NOT_FOUND", "Player not found");
    }

    await adminStore.updatePlayerStatus(playerId, status);

    await adminStore.appendAuditLog({
      adminId: req.admin.sub,
      adminUsername: req.admin.username,
      action: "player_status_change",
      targetType: "player",
      targetValue: playerId,
      details: { from: player.status, to: status },
      ip: getClientIp(req)
    });

    return res.json({ ok: true, message: "Player status updated" });
  });

  // ============================================================
  // Maintenance Mode
  // ============================================================

  router.get("/api/v1/maintenance", requireAuth(), async (req, res) => {
    const status = await adminStore.getMaintenanceStatus();
    return res.json({ ok: true, ...status });
  });

  router.post("/api/v1/maintenance", requireAuth("admin"), async (req, res) => {
    const { enabled, reason } = req.body || {};

    await adminStore.setMaintenanceMode(enabled, reason || "");

    await adminStore.appendAuditLog({
      adminId: req.admin.sub,
      adminUsername: req.admin.username,
      action: enabled ? "maintenance_enabled" : "maintenance_disabled",
      targetType: "system",
      targetValue: "maintenance",
      details: { reason },
      ip: getClientIp(req)
    });

    return res.json({ ok: true, message: enabled ? "Maintenance mode enabled" : "Maintenance mode disabled" });
  });

  // ============================================================
  // GM Commands
  // ============================================================

  router.post("/api/v1/gm/broadcast", requireAuth("admin", "operator"), async (req, res) => {
    const { title, content, sender } = req.body || {};

    if (!title || typeof title !== "string" || title.trim().length === 0) {
      return badRequest(res, "INVALID_TITLE", "title is required");
    }

    if (!content || typeof content !== "string" || content.trim().length === 0) {
      return badRequest(res, "INVALID_CONTENT", "content is required");
    }

    try {
      await gameAdminClient.broadcast(title.trim(), content.trim(), sender || "System");

      await adminStore.appendAuditLog({
        adminId: req.admin.sub,
        adminUsername: req.admin.username,
        action: "gm_broadcast",
        targetType: "system",
        targetValue: "all",
        details: { title, content, sender },
        ip: getClientIp(req)
      });

      return res.json({ ok: true, message: "Broadcast sent" });
    } catch (error) {
      return res.status(502).json({
        ok: false,
        error: "GAME_SERVER_ERROR",
        message: error.message
      });
    }
  });

  router.post("/api/v1/gm/send-item", requireAuth("admin", "operator"), async (req, res) => {
    const { playerId, itemId, itemCount, reason } = req.body || {};

    if (!playerId || typeof playerId !== "string") {
      return badRequest(res, "INVALID_PLAYER_ID", "playerId is required");
    }

    if (!itemId || typeof itemId !== "string") {
      return badRequest(res, "INVALID_ITEM_ID", "itemId is required");
    }

    if (!itemCount || typeof itemCount !== "number" || itemCount <= 0) {
      return badRequest(res, "INVALID_ITEM_COUNT", "itemCount must be a positive number");
    }

    try {
      await gameAdminClient.sendItem(playerId, itemId, itemCount, reason || "");

      await adminStore.appendAuditLog({
        adminId: req.admin.sub,
        adminUsername: req.admin.username,
        action: "gm_send_item",
        targetType: "player",
        targetValue: playerId,
        details: { itemId, itemCount, reason },
        ip: getClientIp(req)
      });

      return res.json({ ok: true, message: "Item sent" });
    } catch (error) {
      return res.status(502).json({
        ok: false,
        error: "GAME_SERVER_ERROR",
        message: error.message
      });
    }
  });

  router.post("/api/v1/gm/kick-player", requireAuth("admin", "operator"), async (req, res) => {
    const { playerId, reason } = req.body || {};

    if (!playerId || typeof playerId !== "string") {
      return badRequest(res, "INVALID_PLAYER_ID", "playerId is required");
    }

    try {
      await gameAdminClient.kickPlayer(playerId, reason || "");

      await adminStore.appendAuditLog({
        adminId: req.admin.sub,
        adminUsername: req.admin.username,
        action: "gm_kick_player",
        targetType: "player",
        targetValue: playerId,
        details: { reason },
        ip: getClientIp(req)
      });

      return res.json({ ok: true, message: "Player kicked" });
    } catch (error) {
      return res.status(502).json({
        ok: false,
        error: "GAME_SERVER_ERROR",
        message: error.message
      });
    }
  });

  router.post("/api/v1/gm/ban-player", requireAuth("admin"), async (req, res) => {
    const { playerId, durationSeconds, reason } = req.body || {};

    if (!playerId || typeof playerId !== "string") {
      return badRequest(res, "INVALID_PLAYER_ID", "playerId is required");
    }

    if (!durationSeconds || typeof durationSeconds !== "number" || durationSeconds <= 0) {
      return badRequest(res, "INVALID_DURATION", "durationSeconds must be a positive number");
    }

    try {
      await gameAdminClient.banPlayer(playerId, durationSeconds, reason || "");

      await adminStore.appendAuditLog({
        adminId: req.admin.sub,
        adminUsername: req.admin.username,
        action: "gm_ban_player",
        targetType: "player",
        targetValue: playerId,
        details: { durationSeconds, reason },
        ip: getClientIp(req)
      });

      return res.json({ ok: true, message: "Player banned" });
    } catch (error) {
      return res.status(502).json({
        ok: false,
        error: "GAME_SERVER_ERROR",
        message: error.message
      });
    }
  });

  // ============================================================
  // Health Check
  // ============================================================

  router.get("/healthz", (_req, res) => {
    res.json({ ok: true, service: "admin-api" });
  });

  return router;
}
