import { Router } from "express";
import { v4 as uuidv4 } from "uuid";

import { badRequest, notFound } from "./http-errors.js";
import { log } from "./logger.js";

function isSystemIdentity(value) {
  return typeof value === "string" && value.trim().toLowerCase() === "system";
}

function normalizeSender(body = {}) {
  const legacySenderId = body.from_player_id;
  const requestedSenderId = body.sender_id || legacySenderId || "system";
  const senderType = body.sender_type || (isSystemIdentity(requestedSenderId) ? "system" : (legacySenderId ? "player" : "system"));
  const senderId = senderType === "system" ? "system" : requestedSenderId;
  const senderName = body.sender_name || (senderType === "system" ? "系统" : senderId);
  const createdByType = body.created_by_type || senderType;
  const createdById = body.created_by_id || senderId;
  const createdByName = body.created_by_name || senderName;

  return {
    senderType,
    senderId,
    senderName,
    createdByType,
    createdById,
    createdByName
  };
}

function hasAttachments(attachments) {
  if (attachments === null || attachments === undefined) {
    return false;
  }

  if (Array.isArray(attachments)) {
    return attachments.length > 0;
  }

  if (typeof attachments === "object") {
    return Object.keys(attachments).length > 0;
  }

  return true;
}

function isExpired(expiresAt) {
  if (!expiresAt) {
    return false;
  }

  const expiresAtMs = new Date(expiresAt).getTime();
  return Number.isFinite(expiresAtMs) && expiresAtMs <= Date.now();
}

function normalizeMailAttachmentItems(attachments) {
  const list = Array.isArray(attachments) ? attachments : [attachments];

  return list.map((attachment, index) => {
    if (!attachment || typeof attachment !== "object") {
      throw new Error(`invalid attachment at index ${index}`);
    }

    const type = attachment.type || "item";
    if (type !== "item") {
      throw new Error(`unsupported attachment type at index ${index}: ${type}`);
    }

    const rawItemId = attachment.id ?? attachment.item_id;
    const rawCount = attachment.count;
    const itemId = Number.parseInt(String(rawItemId), 10);
    const count = Number.parseInt(String(rawCount), 10);

    if (!Number.isInteger(itemId) || itemId <= 0) {
      throw new Error(`invalid item id at index ${index}`);
    }

    if (!Number.isInteger(count) || count <= 0) {
      throw new Error(`invalid item count at index ${index}`);
    }

    return {
      itemId,
      count,
      binded: attachment.binded === true
    };
  });
}

export function createRoutes(config, mailStore, pubsubClient, gameAdminClient) {
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
      const sender = normalizeSender(req.body);

      if (!to_player_id) {
        return badRequest(res, "MISSING_TO_PLAYER_ID", "to_player_id is required");
      }

      if (!title) {
        return badRequest(res, "MISSING_TITLE", "title is required");
      }

      const mail = {
        mail_id: uuidv4(),
        sender_type: sender.senderType,
        sender_id: sender.senderId,
        sender_name: sender.senderName,
        from_player_id: sender.senderId,
        to_player_id,
        title,
        content: content || "",
        attachments: attachments || null,
        mail_type: mail_type || "system",
        created_by_type: sender.createdByType,
        created_by_id: sender.createdById,
        created_by_name: sender.createdByName,
        created_at: Date.now(),
        expires_at: expires_at || null
      };

      await mailStore.createMail(mail);

      // Publish notification via Redis Pub/Sub
      await pubsubClient.publishMailNotification(to_player_id, mail);

      log("info", "mail.sent", {
        mailId: mail.mail_id,
        toPlayerId: to_player_id,
        senderType: mail.sender_type,
        senderId: mail.sender_id,
        createdByType: mail.created_by_type,
        createdById: mail.created_by_id
      });

      return res.status(201).json({
        ok: true,
        mail_id: mail.mail_id,
        sender: {
          type: mail.sender_type,
          id: mail.sender_id,
          name: mail.sender_name
        },
        created_by: {
          type: mail.created_by_type,
          id: mail.created_by_id,
          name: mail.created_by_name
        }
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

  // Claim mail attachment
  router.post("/api/v1/mails/:mailId/claim", async (req, res) => {
    try {
      const { mailId } = req.params;
      const { player_id } = req.body || {};

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
          message: "You can only claim attachments from your own mail"
        });
      }

      if (isExpired(mail.expires_at)) {
        return res.status(410).json({
          ok: false,
          error: "MAIL_EXPIRED",
          message: "Mail has expired"
        });
      }

      if (!hasAttachments(mail.attachments)) {
        return res.status(409).json({
          ok: false,
          error: "MAIL_HAS_NO_ATTACHMENTS",
          message: "Mail does not contain claimable attachments"
        });
      }

      let normalizedAttachments;
      try {
        normalizedAttachments = normalizeMailAttachmentItems(mail.attachments);
      } catch (error) {
        return badRequest(res, "UNSUPPORTED_ATTACHMENT_FORMAT", error.message);
      }

      try {
        await gameAdminClient.grantMailAttachments(
          player_id,
          mail.mail_id,
          normalizedAttachments,
          `claim mail ${mail.mail_id}`
        );
      } catch (error) {
        log("error", "mail.claim_grant_failed", {
          mailId,
          playerId: player_id,
          error: error.message,
          code: error.code || null
        });
        return res.status(502).json({
          ok: false,
          error: "GAME_SERVER_GRANT_FAILED",
          message: error.message
        });
      }

      const result = await mailStore.claimAttachments(mailId);
      const currentMail = result.mail || mail;

      log("info", result.claimed ? "mail.claimed" : "mail.claimed_idempotent", {
        mailId,
        playerId: player_id,
        attachmentCount: Array.isArray(currentMail.attachments) ? currentMail.attachments.length : 1
      });

      return res.json({
        ok: true,
        mail_id: currentMail.mail_id,
        claimed: result.claimed,
        already_claimed: !result.claimed,
        status: currentMail.status,
        attachments: currentMail.attachments,
        read_at: currentMail.read_at,
        claimed_at: currentMail.claimed_at
      });
    } catch (error) {
      log("error", "route.claim_mail_failed", { error: error.message });
      return res.status(500).json({
        ok: false,
        error: "INTERNAL_ERROR"
      });
    }
  });

  return router;
}
