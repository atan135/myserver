import { Inject, Injectable, OnModuleDestroy, OnModuleInit } from "@nestjs/common";

import { badGateway, badRequest, conflict, forbidden, gone, notFound } from "../common/http-exception.js";
import { generateMailId } from "../global-id.js";
import { log } from "../logger.js";
import { MAIL_GAME_ADMIN_CLIENT, MAIL_PUBSUB_CLIENT, MAIL_STORE } from "../tokens.js";

function isSystemIdentity(value: any) {
  return typeof value === "string" && value.trim().toLowerCase() === "system";
}

function normalizeSender(body: any = {}) {
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

function hasAttachments(attachments: any) {
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

function isExpired(expiresAt: any) {
  if (!expiresAt) {
    return false;
  }

  const expiresAtMs = new Date(expiresAt).getTime();
  return Number.isFinite(expiresAtMs) && expiresAtMs <= Date.now();
}

function assertAuthenticatedPlayer(playerId: any) {
  if (!playerId) {
    throw badRequest("MISSING_PLAYER_ID", "player_id is required");
  }
}

function assertPlayerIdMatches(authenticatedPlayerId: string, requestedPlayerId: any) {
  if (
    requestedPlayerId !== undefined &&
    requestedPlayerId !== null &&
    requestedPlayerId !== "" &&
    requestedPlayerId !== authenticatedPlayerId
  ) {
    throw forbidden("PLAYER_ID_MISMATCH", "player_id does not match authenticated player");
  }
}

function normalizeTargetInstanceId(value: any) {
  if (value === undefined || value === null || value === "") {
    return "";
  }

  return String(value).trim();
}

function normalizeMailAttachmentItems(attachments: any) {
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

@Injectable()
export class MailsService implements OnModuleInit, OnModuleDestroy {
  private outboxTimer: NodeJS.Timeout | null = null;
  private outboxProcessing = false;

  constructor(
    @Inject(MAIL_STORE) private readonly mailStore: any,
    @Inject(MAIL_PUBSUB_CLIENT) private readonly pubsubClient: any,
    @Inject(MAIL_GAME_ADMIN_CLIENT) private readonly gameAdminClient: any
  ) {}

  onModuleInit() {
    this.outboxTimer = setInterval(() => {
      this.processPendingNotificationOutbox().catch((error: any) => {
        log("error", "mail.outbox_worker_failed", { error: error.message });
      });
    }, 5000);
    this.outboxTimer.unref?.();
  }

  onModuleDestroy() {
    if (this.outboxTimer) {
      clearInterval(this.outboxTimer);
      this.outboxTimer = null;
    }
  }

  async processPendingNotificationOutbox(limit = 20) {
    if (this.outboxProcessing) {
      return {
        processed: 0,
        sent: 0,
        failed: 0,
        skipped: true
      };
    }

    this.outboxProcessing = true;
    let processed = 0;
    let sent = 0;
    let failed = 0;

    try {
      const entries = await this.mailStore.reservePendingMailNotificationOutbox(limit);
      for (const entry of entries) {
        processed += 1;
        try {
          const payload = entry.payload || {};
          await this.pubsubClient.publishMailNotification(payload.to_player_id || entry.to_player_id, payload.mail);
          await this.mailStore.markMailNotificationOutboxSent(entry.id);
          sent += 1;
        } catch (error: any) {
          await this.mailStore.markMailNotificationOutboxFailed(entry.id, error.message);
          failed += 1;
          log("warn", "mail.outbox_publish_failed", {
            outboxId: entry.id,
            mailId: entry.mail_id,
            attempts: entry.attempts,
            error: error.message
          });
        }
      }

      return {
        processed,
        sent,
        failed,
        skipped: false
      };
    } finally {
      this.outboxProcessing = false;
    }
  }

  async list(authenticatedPlayerId: string, query: any = {}) {
    try {
      const { player_id, status, limit, offset } = query;
      assertAuthenticatedPlayer(authenticatedPlayerId);
      assertPlayerIdMatches(authenticatedPlayerId, player_id);

      const mails = await this.mailStore.getMailsByPlayerId(authenticatedPlayerId, {
        status,
        limit: limit ? parseInt(limit, 10) : 50,
        offset: offset ? parseInt(offset, 10) : 0
      });

      const unreadCount = await this.mailStore.countUnread(authenticatedPlayerId);

      return {
        ok: true,
        mails,
        unread_count: unreadCount
      };
    } catch (error: any) {
      if (error?.getStatus?.()) {
        throw error;
      }
      log("error", "route.get_mails_failed", { error: error.message });
      throw error;
    }
  }

  async get(mailId: string, authenticatedPlayerId?: string, query: any = {}) {
    try {
      assertAuthenticatedPlayer(authenticatedPlayerId);
      assertPlayerIdMatches(authenticatedPlayerId as string, query?.player_id);

      const mail = await this.mailStore.getMailById(mailId);

      if (!mail) {
        throw notFound("MAIL_NOT_FOUND", "Mail not found");
      }

      if (mail.to_player_id !== authenticatedPlayerId) {
        throw forbidden("FORBIDDEN", "You can only read your own mail");
      }

      return {
        ok: true,
        mail
      };
    } catch (error: any) {
      if (error?.getStatus?.()) {
        throw error;
      }
      log("error", "route.get_mail_failed", { error: error.message });
      throw error;
    }
  }

  async create(body: any) {
    try {
      const { to_player_id, title, content, attachments, mail_type, expires_at } = body || {};
      const sender = normalizeSender(body || {});

      if (!to_player_id) {
        throw badRequest("MISSING_TO_PLAYER_ID", "to_player_id is required");
      }

      if (!title) {
        throw badRequest("MISSING_TITLE", "title is required");
      }

      const mail = {
        mail_id: generateMailId(),
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

      await this.mailStore.createMailWithNotificationOutbox(mail);
      const outboxResult = await this.processPendingNotificationOutbox(1);

      log("info", outboxResult.sent > 0 ? "mail.sent" : "mail.outbox_pending", {
        mailId: mail.mail_id,
        toPlayerId: to_player_id,
        senderType: mail.sender_type,
        senderId: mail.sender_id,
        createdByType: mail.created_by_type,
        createdById: mail.created_by_id,
        outboxSent: outboxResult.sent,
        outboxFailed: outboxResult.failed
      });

      return {
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
      };
    } catch (error: any) {
      if (error?.getStatus?.()) {
        throw error;
      }
      log("error", "route.send_mail_failed", { error: error.message });
      throw error;
    }
  }

  async markRead(mailId: string, authenticatedPlayerId: string, body: any = {}) {
    try {
      const { player_id } = body || {};
      assertAuthenticatedPlayer(authenticatedPlayerId);
      assertPlayerIdMatches(authenticatedPlayerId, player_id);

      const mail = await this.mailStore.getMailById(mailId);
      if (!mail) {
        throw notFound("MAIL_NOT_FOUND", "Mail not found");
      }

      if (mail.to_player_id !== authenticatedPlayerId) {
        throw forbidden("FORBIDDEN", "You can only read your own mail");
      }

      const updated = await this.mailStore.markAsRead(mailId);

      return {
        ok: true,
        updated
      };
    } catch (error: any) {
      if (error?.getStatus?.()) {
        throw error;
      }
      log("error", "route.mark_read_failed", { error: error.message });
      throw error;
    }
  }

  async claim(mailId: string, authenticatedPlayerId: string, body: any = {}) {
    try {
      const { player_id } = body || {};
      const targetInstanceId = normalizeTargetInstanceId(body?.targetInstanceId ?? body?.target_instance_id);
      assertAuthenticatedPlayer(authenticatedPlayerId);
      assertPlayerIdMatches(authenticatedPlayerId, player_id);

      const mail = await this.mailStore.getMailById(mailId);
      if (!mail) {
        throw notFound("MAIL_NOT_FOUND", "Mail not found");
      }

      if (mail.to_player_id !== authenticatedPlayerId) {
        throw forbidden("FORBIDDEN", "You can only claim attachments from your own mail");
      }

      if (isExpired(mail.expires_at)) {
        throw gone("MAIL_EXPIRED", "Mail has expired");
      }

      if (!hasAttachments(mail.attachments)) {
        throw conflict("MAIL_HAS_NO_ATTACHMENTS", "Mail does not contain claimable attachments");
      }

      let normalizedAttachments;
      try {
        normalizedAttachments = normalizeMailAttachmentItems(mail.attachments);
      } catch (error: any) {
        throw badRequest("UNSUPPORTED_ATTACHMENT_FORMAT", error.message);
      }

      const claimBegin = await this.mailStore.beginClaimAttachments(mailId);
      if (!claimBegin.mail) {
        throw notFound("MAIL_NOT_FOUND", "Mail not found");
      }

      if (claimBegin.alreadyClaimed) {
        const currentMail = claimBegin.mail;
        return {
          ok: true,
          mail_id: currentMail.mail_id,
          claimed: false,
          already_claimed: true,
          status: currentMail.status,
          attachments: currentMail.attachments,
          read_at: currentMail.read_at,
          claimed_at: currentMail.claimed_at
        };
      }

      if (claimBegin.inProgress || !claimBegin.reserved) {
        throw conflict("MAIL_CLAIM_IN_PROGRESS", "Mail attachments are being claimed");
      }

      let result;
      try {
        await this.gameAdminClient.grantMailAttachments(
          authenticatedPlayerId,
          `mail_claim:${mail.mail_id}`,
          normalizedAttachments,
          `claim mail ${mail.mail_id}`,
          { targetInstanceId }
        );
      } catch (error: any) {
        await this.mailStore.releaseClaimAttachments(mailId);
        log("error", "mail.claim_grant_failed", {
          mailId,
          playerId: authenticatedPlayerId,
          error: error.message,
          code: error.code || null
        });
        throw badGateway("GAME_SERVER_GRANT_FAILED", error.message);
      }

      result = await this.mailStore.completeClaimAttachments(mailId);

      const currentMail = result.mail || mail;

      log("info", result.claimed ? "mail.claimed" : "mail.claimed_idempotent", {
        mailId,
        playerId: authenticatedPlayerId,
        attachmentCount: Array.isArray(currentMail.attachments) ? currentMail.attachments.length : 1
      });

      return {
        ok: true,
        mail_id: currentMail.mail_id,
        claimed: result.claimed,
        already_claimed: !result.claimed,
        status: currentMail.status,
        attachments: currentMail.attachments,
        read_at: currentMail.read_at,
        claimed_at: currentMail.claimed_at
      };
    } catch (error: any) {
      if (error?.getStatus?.()) {
        throw error;
      }
      log("error", "route.claim_mail_failed", { error: error.message });
      throw error;
    }
  }
}
