import { Router } from "express";
import { v4 as uuidv4 } from "uuid";

import { badRequest, notFound } from "./http-errors.js";
import { log } from "./logger.js";

export function createRoutes(config, mailStore, pubsubClient) {
  const router = Router();

  // Health check
  router.get("/healthz", (_req, res) => {
    res.json({
      ok: true,
      service: config.appName,
      env: config.env,
      storage: config.mysqlEnabled ? "mysql" : "memory"
    });
  });

  // Get mail list
  router.get("/api/v1/mails", async (req, res) => {
    try {
      const { player_id, status, limit, offset } = req.query;

      if (!player_id) {
        return badRequest(res, "MISSING_PLAYER_ID", "player_id is required");
      }

      const mails = await mailStore.getMailsByPlayerId(player_id, {
        status,
        limit: limit ? parseInt(limit, 10) : 50,
        offset: offset ? parseInt(offset, 10) : 0
      });

      const unreadCount = await mailStore.countUnread(player_id);

      return res.json({
        ok: true,
        mails,
        unread_count: unreadCount
      });
    } catch (error) {
      log("error", "route.get_mails_failed", { error: error.message });
      return res.status(500).json({
        ok: false,
        error: "INTERNAL_ERROR"
      });
    }
  });

  // Get mail detail
  router.get("/api/v1/mails/:mailId", async (req, res) => {
    try {
      const { mailId } = req.params;
      const mail = await mailStore.getMailById(mailId);

      if (!mail) {
        return notFound(res, "MAIL_NOT_FOUND", "Mail not found");
      }

      return res.json({
        ok: true,
        mail
      });
    } catch (error) {
      log("error", "route.get_mail_failed", { error: error.message });
      return res.status(500).json({
        ok: false,
        error: "INTERNAL_ERROR"
      });
    }
  });

  // Send mail (internal API for admin-api)
  router.post("/api/v1/mails", async (req, res) => {
    try {
      const { to_player_id, title, content, attachments, mail_type, expires_at } = req.body;

      if (!to_player_id) {
        return badRequest(res, "MISSING_TO_PLAYER_ID", "to_player_id is required");
      }

      if (!title) {
        return badRequest(res, "MISSING_TITLE", "title is required");
      }

      const mail = {
        mail_id: uuidv4(),
        from_player_id: req.body.from_player_id || "system",
        to_player_id,
        title,
        content: content || "",
        attachments: attachments || null,
        mail_type: mail_type || "system",
        created_at: Date.now(),
        expires_at: expires_at || null
      };

      await mailStore.createMail(mail);

      // Publish notification via Redis Pub/Sub
      await pubsubClient.publishMailNotification(to_player_id, mail);

      log("info", "mail.sent", {
        mailId: mail.mail_id,
        toPlayerId: to_player_id,
        fromPlayerId: mail.from_player_id
      });

      return res.status(201).json({
        ok: true,
        mail_id: mail.mail_id
      });
    } catch (error) {
      log("error", "route.send_mail_failed", { error: error.message });
      return res.status(500).json({
        ok: false,
        error: "INTERNAL_ERROR"
      });
    }
  });

  // Mark mail as read
  router.put("/api/v1/mails/:mailId/read", async (req, res) => {
    try {
      const { mailId } = req.params;
      const { player_id } = req.body;

      if (!player_id) {
        return badRequest(res, "MISSING_PLAYER_ID", "player_id is required");
      }

      const mail = await mailStore.getMailById(mailId);
      if (!mail) {
        return notFound(res, "MAIL_NOT_FOUND", "Mail not found");
      }

      if (mail.to_player_id !== player_id) {
        return res.status(403).json({
          ok: false,
          error: "FORBIDDEN",
          message: "You can only read your own mail"
        });
      }

      const updated = await mailStore.markAsRead(mailId);

      return res.json({
        ok: true,
        updated
      });
    } catch (error) {
      log("error", "route.mark_read_failed", { error: error.message });
      return res.status(500).json({
        ok: false,
        error: "INTERNAL_ERROR"
      });
    }
  });

  // Claim mail attachment (todo)
  router.post("/api/v1/mails/:mailId/claim", async (req, res) => {
    return res.status(501).json({
      ok: false,
      error: "NOT_IMPLEMENTED",
      message: "Attachment claim not implemented yet"
    });
  });

  return router;
}
