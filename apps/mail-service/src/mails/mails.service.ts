import { Inject, Injectable, OnModuleDestroy, OnModuleInit } from "@nestjs/common";
import { randomBytes } from "node:crypto";

import { badRequest, conflict, forbidden, gone, notFound } from "../common/http-exception.js";
import { computeGrantRequestFingerprint, normalizeGrantItems } from "../game-admin-client.js";
import { generateMailId } from "../global-id.js";
import { log } from "../logger.js";
import {
  calculateOutboxBackoffMs,
  normalizeMailNotificationEvent,
  PermanentOutboxPayloadError
} from "../notification-outbox.js";
import { MAIL_CONFIG, MAIL_GAME_ADMIN_CLIENT, MAIL_METRICS, MAIL_PUBSUB_CLIENT, MAIL_STORE } from "../tokens.js";

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

function assertAuthenticatedCharacter(characterId: any) {
  if (!characterId || typeof characterId !== "string" || characterId.trim().length === 0) {
    throw badRequest("MISSING_CHARACTER_ID", "character_id is required");
  }
  return characterId.trim();
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

function classifyClaimFailure(error: any) {
  const resultState = error?.resultState || (error?.requestWritten === true ? "unknown" : "not_applied");
  const errorCategory = error?.errorCategory || (resultState === "unknown" ? "RESULT_UNKNOWN" : "RETRYABLE_FAILURE");
  const permanentContractError = new Set([
    "GRANT_REQUEST_FINGERPRINT_MISMATCH",
    "INVALID_GRANT_ITEMS"
  ]).has(error?.code);
  if (resultState === "unknown" || errorCategory === "RESULT_UNKNOWN") {
    return { status: "reconciliation_pending", httpStatus: 202, retryable: false, resultState: "unknown", errorCategory };
  }
  if (
    errorCategory === "PERMANENT_FAILURE" ||
    error?.retryable === false ||
    permanentContractError
  ) {
    return {
      status: "permanent_failure",
      httpStatus: 422,
      retryable: false,
      resultState: "not_applied",
      errorCategory: permanentContractError ? "PERMANENT_FAILURE" : errorCategory
    };
  }
  return { status: "retryable_failure", httpStatus: 503, retryable: true, resultState: "not_applied", errorCategory };
}

function claimStatusHttpStatus(claimStatus: string) {
  if (claimStatus === "processing" || claimStatus === "reconciliation_pending") return 202;
  if (claimStatus === "manual_review") return 409;
  if (claimStatus === "retryable_failure") return 503;
  if (claimStatus === "permanent_failure") return 422;
  return 200;
}

function claimStatusIsOk(claimStatus: string) {
  return claimStatus === "claimed" || claimStatus === "processing" || claimStatus === "reconciliation_pending";
}

const PUBLIC_PERMANENT_CLAIM_ERRORS = new Map([
  ["ITEM_NOT_FOUND", "A mail attachment is currently unavailable"],
  ["INVENTORY_FULL", "The character inventory does not have enough space"],
  ["BACKPACK_FULL", "The character inventory does not have enough space"],
  ["MAIL_CLAIM_CHARACTER_MISMATCH", "Continue this claim with the character that started it"]
]);

function publicClaimError(claimStatus: string, internalErrorCode: any, errorCategory: any) {
  if (claimStatus === "retryable_failure") {
    return errorCategory === "ROUTE_UNAVAILABLE"
      ? "MAIL_CLAIM_ROUTE_UNAVAILABLE"
      : "MAIL_CLAIM_RETRYABLE_FAILURE";
  }
  if (claimStatus === "permanent_failure") {
    return PUBLIC_PERMANENT_CLAIM_ERRORS.has(internalErrorCode)
      ? internalErrorCode
      : "MAIL_CLAIM_PERMANENT_FAILURE";
  }
  if (claimStatus === "reconciliation_pending") {
    return "MAIL_CLAIM_RECONCILIATION_PENDING";
  }
  if (claimStatus === "manual_review") {
    return "MAIL_CLAIM_MANUAL_REVIEW_REQUIRED";
  }
  return undefined;
}

function publicClaimMessage(claimStatus: string, publicErrorCode: any) {
  if (claimStatus === "processing") return "Mail attachment claim is processing";
  if (claimStatus === "retryable_failure") return "Mail attachment claim could not be completed yet; retry later";
  if (claimStatus === "reconciliation_pending") return "Mail attachment claim result is being verified";
  if (claimStatus === "manual_review") return "Mail attachment claim requires support review";
  if (claimStatus === "permanent_failure") {
    return PUBLIC_PERMANENT_CLAIM_ERRORS.get(publicErrorCode) || "Mail attachments cannot be claimed in the current state";
  }
  return undefined;
}

function claimResponse(workflow: any, mail: any, options: any = {}) {
  const claimStatus = options.claimStatus || workflow?.status || (mail?.status === "claimed" ? "claimed" : "processing");
  const isClaimed = claimStatus === "claimed";
  const attachments = workflow?.attachments_snapshot ?? mail?.attachments ?? null;
  const errorCategory = options.errorCategory || workflow?.last_error_category || undefined;
  const internalErrorCode = options.errorCode || workflow?.last_error_code || undefined;
  const publicErrorCode = publicClaimError(claimStatus, internalErrorCode, errorCategory);
  return {
    _http_status: options.httpStatus ?? claimStatusHttpStatus(claimStatus),
    ok: options.ok ?? claimStatusIsOk(claimStatus),
    mail_id: workflow?.mail_id || mail?.mail_id,
    claim_status: claimStatus,
    claimed: options.claimed ?? false,
    already_claimed: options.alreadyClaimed ?? isClaimed,
    processing: claimStatus === "processing",
    retryable: options.retryable ?? workflow?.last_error_retryable ?? false,
    request_id: workflow?.claim_request_id,
    character_id: workflow?.character_id,
    attachments_fingerprint: workflow?.attachments_fingerprint,
    attempts: workflow?.attempts || 0,
    error: publicErrorCode,
    error_category: errorCategory,
    result_state: options.resultState || workflow?.last_result_state || (isClaimed ? "applied" : undefined),
    message: publicClaimMessage(claimStatus, publicErrorCode),
    status: isClaimed ? "claimed" : (mail?.status || "claiming"),
    attachments,
    read_at: mail?.read_at || null,
    claimed_at: mail?.claimed_at || workflow?.completed_at || null,
    completed_at: workflow?.completed_at || null
  };
}

@Injectable()
export class MailsService implements OnModuleInit, OnModuleDestroy {
  private outboxTimer: NodeJS.Timeout | null = null;
  private outboxCleanupTimer: NodeJS.Timeout | null = null;
  private outboxProcessing = false;
  private outboxCleanupProcessing = false;

  constructor(
    @Inject(MAIL_STORE) private readonly mailStore: any,
    @Inject(MAIL_PUBSUB_CLIENT) private readonly pubsubClient: any,
    @Inject(MAIL_GAME_ADMIN_CLIENT) private readonly gameAdminClient: any,
    @Inject(MAIL_CONFIG) private readonly config: any = {},
    @Inject(MAIL_METRICS) private readonly metrics: any = null
  ) {}

  onModuleInit() {
    this.outboxTimer = setInterval(() => {
      this.processPendingNotificationOutbox().catch((error: any) => {
        log("error", "mail.outbox_worker_failed", { error: error.message });
      });
    }, this.config.outboxPollIntervalMs || 5000);
    this.outboxTimer.unref?.();
    this.outboxCleanupTimer = setInterval(() => {
      this.cleanupNotificationOutbox().catch((error: any) => {
        log("error", "mail.outbox_cleanup_failed", { error: error.message });
      });
    }, this.config.outboxCleanupIntervalMs || 3_600_000);
    this.outboxCleanupTimer.unref?.();
  }

  onModuleDestroy() {
    if (this.outboxTimer) {
      clearInterval(this.outboxTimer);
      this.outboxTimer = null;
    }
    if (this.outboxCleanupTimer) {
      clearInterval(this.outboxCleanupTimer);
      this.outboxCleanupTimer = null;
    }
  }

  async processPendingNotificationOutbox(limit = this.config.outboxBatchSize || 20) {
    if (this.outboxProcessing) {
      return {
        processed: 0,
        sent: 0,
        failed: 0,
        terminal: 0,
        skipped: true
      };
    }

    this.outboxProcessing = true;
    let processed = 0;
    let sent = 0;
    let failed = 0;
    let terminal = 0;

    try {
      const entries = await this.mailStore.reservePendingMailNotificationOutbox(limit, {
        leaseMs: this.config.outboxLeaseMs || 30_000,
        leaseOwner: this.config.serviceInstanceId || "mail-service"
      });
      for (const entry of entries) {
        processed += 1;
        if (entry.lease_taken_over) {
          this.metrics?.recordOutboxLeaseTakeover?.();
        }
        if (entry.attempts_exhausted) {
          const marked = await this.mailStore.markMailNotificationOutboxTerminal(
            entry.id,
            "OUTBOX_MAX_ATTEMPTS_EXHAUSTED: previous publish result remained unknown after the maximum attempts",
            { leaseToken: entry.lease_token }
          );
          terminal += marked ? 1 : 0;
          if (marked) {
            this.metrics?.recordOutboxTerminal?.();
          }
          log("warn", "mail.outbox_attempts_exhausted", {
            outboxId: entry.id,
            mailId: entry.mail_id,
            attempts: entry.attempts,
            maxAttempts: entry.max_attempts,
            terminated: marked
          });
          continue;
        }
        try {
          const event = normalizeMailNotificationEvent(entry);
          await this.pubsubClient.publishMailNotification(event.player_id, event);
          const marked = await this.mailStore.markMailNotificationOutboxSent(entry.id, entry.lease_token);
          if (marked) {
            sent += 1;
            this.metrics?.recordOutboxPublished?.(Math.max(0, Date.now() - event.occurred_at));
          } else {
            log("warn", "mail.outbox_lease_lost_after_publish", {
              outboxId: entry.id,
              mailId: entry.mail_id,
              eventId: event.event_id,
              traceId: event.trace_id
            });
          }
        } catch (error: any) {
          const errorSummary = `${error.code ? `${error.code}: ` : ""}${error.message || "unknown outbox error"}`;
          const permanent = error instanceof PermanentOutboxPayloadError || entry.attempts >= entry.max_attempts;
          if (permanent) {
            const marked = await this.mailStore.markMailNotificationOutboxTerminal(entry.id, errorSummary, {
              leaseToken: entry.lease_token
            });
            terminal += marked ? 1 : 0;
            if (marked) {
              this.metrics?.recordOutboxTerminal?.();
            }
          } else {
            const delayMs = calculateOutboxBackoffMs(entry.attempts, {
              baseMs: this.config.outboxBackoffBaseMs || 1000,
              maxMs: this.config.outboxBackoffMaxMs || 60_000,
              jitterRatio: this.config.outboxBackoffJitterRatio ?? 0.2
            }, this.config.outboxRandom || Math.random);
            const marked = await this.mailStore.markMailNotificationOutboxFailed(entry.id, errorSummary, {
              delayMs,
              leaseToken: entry.lease_token
            });
            failed += marked ? 1 : 0;
            if (marked) {
              this.metrics?.recordOutboxRetry?.();
            }
          }
          log("warn", "mail.outbox_publish_failed", {
            outboxId: entry.id,
            mailId: entry.mail_id,
            attempts: entry.attempts,
            terminal: permanent,
            error: String(error.message || "unknown outbox error").slice(0, 512)
          });
        }
      }

      const stats = await this.mailStore.getMailNotificationOutboxStats();
      this.metrics?.setOutboxSnapshot?.(stats);

      return {
        processed,
        sent,
        failed,
        terminal,
        skipped: false
      };
    } finally {
      this.outboxProcessing = false;
    }
  }

  async cleanupNotificationOutbox() {
    if (this.outboxCleanupProcessing) {
      return { deleted: 0, skipped: true };
    }
    this.outboxCleanupProcessing = true;
    try {
      const dayMs = 24 * 60 * 60 * 1000;
      const deleted = await this.mailStore.cleanupMailNotificationOutbox({
        sentRetentionMs: (this.config.outboxSentRetentionDays || 7) * dayMs,
        terminalRetentionMs: (this.config.outboxTerminalRetentionDays || 30) * dayMs,
        limit: this.config.outboxCleanupBatchSize || 500
      });
      return { deleted, skipped: false };
    } finally {
      this.outboxCleanupProcessing = false;
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
      let outboxResult = { sent: 0, failed: 0, terminal: 0 };
      try {
        outboxResult = await this.processPendingNotificationOutbox(1);
      } catch (error: any) {
        log("warn", "mail.outbox_immediate_process_failed", {
          mailId: mail.mail_id,
          error: String(error.message || "unknown outbox error").slice(0, 512)
        });
      }

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

  async claim(mailId: string, authenticatedPlayerId: string, authenticatedCharacterId: string, body: any = {}) {
    try {
      const { player_id } = body || {};
      const targetInstanceId = normalizeTargetInstanceId(body?.targetInstanceId ?? body?.target_instance_id);
      assertAuthenticatedPlayer(authenticatedPlayerId);
      const characterId = assertAuthenticatedCharacter(authenticatedCharacterId);
      assertPlayerIdMatches(authenticatedPlayerId, player_id);
      if (
        targetInstanceId &&
        (!this.config.localDiscoveryFallbackEnabled || this.config.registryDiscoveryRequired)
      ) {
        throw forbidden(
          "CLIENT_TARGET_INSTANCE_FORBIDDEN",
          "targetInstanceId is only available for local development diagnostics"
        );
      }

      let mail = null;
      let existingWorkflow = await this.mailStore.getMailClaimWorkflow(mailId);
      let normalizedAttachments;
      let attachmentsFingerprint;
      const requestId = existingWorkflow?.claim_request_id || `mail_claim:${mailId}`;

      if (existingWorkflow) {
        normalizedAttachments = existingWorkflow.attachments_snapshot;
        attachmentsFingerprint = existingWorkflow.attachments_fingerprint;
      } else {
        mail = await this.mailStore.getMailById(mailId);
        if (!mail) {
          throw notFound("MAIL_NOT_FOUND", "Mail not found");
        }
        if (mail.to_player_id !== authenticatedPlayerId) {
          throw forbidden("FORBIDDEN", "You can only claim attachments from your own mail");
        }
        if (mail.status === "claimed" || mail.claimed_at) {
          return claimResponse(null, mail, { claimStatus: "claimed", alreadyClaimed: true });
        }
        if (isExpired(mail.expires_at)) {
          throw gone("MAIL_EXPIRED", "Mail has expired");
        }
        if (!hasAttachments(mail.attachments)) {
          throw conflict("MAIL_HAS_NO_ATTACHMENTS", "Mail does not contain claimable attachments");
        }
        try {
          normalizedAttachments = normalizeGrantItems(normalizeMailAttachmentItems(mail.attachments));
        } catch (error: any) {
          throw badRequest("UNSUPPORTED_ATTACHMENT_FORMAT", error.message);
        }
        attachmentsFingerprint = computeGrantRequestFingerprint(mailId, characterId, normalizedAttachments);
      }

      const traceId = randomBytes(16).toString("hex");
      const claimBegin = await this.mailStore.reserveMailClaimWorkflow({
        mailId,
        playerId: authenticatedPlayerId,
        characterId,
        requestId,
        attachmentsSnapshot: normalizedAttachments,
        attachmentsFingerprint,
        expectedAttachments: mail?.attachments,
        traceId
      }, {
        leaseMs: this.config.claimLeaseMs || 30_000,
        leaseOwner: this.config.serviceInstanceId || "mail-service"
      });
      existingWorkflow = claimBegin.workflow;

      if (claimBegin.notFound) {
        throw notFound("MAIL_NOT_FOUND", "Mail not found");
      }
      if (claimBegin.ownerMismatch) {
        throw forbidden("FORBIDDEN", "You can only claim attachments from your own mail");
      }
      if (claimBegin.expired) {
        throw gone("MAIL_EXPIRED", "Mail has expired");
      }
      if (claimBegin.attachmentChanged) {
        throw conflict("MAIL_CHANGED_DURING_CLAIM", "Mail attachments changed while the claim was starting; retry the request");
      }
      if (claimBegin.characterMismatch) {
        return claimResponse(existingWorkflow, claimBegin.mail, {
          claimStatus: "permanent_failure",
          httpStatus: 409,
          ok: false,
          retryable: false,
          errorCode: "MAIL_CLAIM_CHARACTER_MISMATCH",
          errorCategory: "PERMANENT_FAILURE",
          resultState: "not_applied"
        });
      }
      if (claimBegin.alreadyClaimed) {
        return claimResponse(existingWorkflow, claimBegin.mail || mail, {
          claimStatus: "claimed",
          alreadyClaimed: true
        });
      }
      if (claimBegin.reconciliationPending) {
        return claimResponse(existingWorkflow, claimBegin.mail, {
          claimStatus: "reconciliation_pending",
          httpStatus: 202,
          retryable: false
        });
      }
      if (claimBegin.manualReview) {
        return claimResponse(existingWorkflow, claimBegin.mail, {
          claimStatus: "manual_review",
          httpStatus: 409,
          ok: false,
          retryable: false
        });
      }
      if (claimBegin.inProgress || !claimBegin.acquired) {
        return claimResponse(existingWorkflow, claimBegin.mail, {
          claimStatus: "processing",
          httpStatus: 202,
          retryable: false
        });
      }

      let grantResult;
      try {
        grantResult = await this.gameAdminClient.grantMailAttachments(
          existingWorkflow.character_id,
          existingWorkflow.claim_request_id,
          existingWorkflow.attachments_snapshot,
          `claim mail ${mailId}`,
          {
            targetInstanceId,
            traceId,
            requestFingerprint: existingWorkflow.attachments_fingerprint
          }
        );
      } catch (error: any) {
        const classification = classifyClaimFailure(error);
        const failureResult = await this.mailStore.recordMailClaimWorkflowFailure(
          mailId,
          existingWorkflow.lease_token,
          {
            status: classification.status,
            traceId: error?.traceId || traceId,
            errorCode: error?.code || "GAME_SERVER_GRANT_FAILED",
            errorCategory: classification.errorCategory,
            resultState: classification.resultState,
            retryable: classification.retryable,
            message: error?.message || "game-server attachment grant failed",
            instanceId: error?.instanceId || ""
          }
        );
        const currentWorkflow = failureResult.workflow || await this.mailStore.getMailClaimWorkflow(mailId);
        const isRouteFailure = classification.errorCategory === "ROUTE_UNAVAILABLE";
        if (isRouteFailure) {
          this.metrics?.recordMailClaimRouteUnavailable?.();
        } else {
          this.metrics?.recordMailClaimGrantFailure?.();
        }
        if (classification.status === "reconciliation_pending") {
          this.metrics?.recordMailClaimResultUnknown?.();
        } else if (classification.status === "retryable_failure") {
          this.metrics?.recordMailClaimRetryableFailure?.();
        } else {
          this.metrics?.recordMailClaimPermanentFailure?.();
        }
        log("error", isRouteFailure ? "mail.claim_route_unavailable" : "mail.claim_grant_failed", {
          mailId,
          instanceId: error?.instanceId || "",
          requestId: error?.requestId || existingWorkflow.claim_request_id,
          traceId: error?.traceId || traceId,
          errorCode: error?.code || "GAME_SERVER_GRANT_FAILED",
          claimStatus: currentWorkflow?.status,
          persisted: failureResult.updated
        });
        if (!failureResult.updated) {
          return claimResponse(currentWorkflow, claimBegin.mail, {
            claimStatus: currentWorkflow?.status || "processing",
            alreadyClaimed: currentWorkflow?.status === "claimed"
          });
        }
        return claimResponse(currentWorkflow, claimBegin.mail, {
          claimStatus: currentWorkflow?.status || classification.status,
          httpStatus: classification.httpStatus,
          ok: classification.status === "reconciliation_pending",
          retryable: classification.retryable,
          errorCode: error?.code || "GAME_SERVER_GRANT_FAILED",
          errorCategory: classification.errorCategory,
          resultState: classification.resultState
        });
      }

      const result = await this.mailStore.completeMailClaimWorkflow(
        mailId,
        existingWorkflow.lease_token,
        {
          traceId: grantResult?.traceId || traceId,
          resultSummary: grantResult?.resultSummary || null,
          instanceId: grantResult?.instanceId || ""
        }
      );
      const currentMail = result.mail || mail;
      if (!result.claimed) {
        const currentWorkflow = result.workflow || await this.mailStore.getMailClaimWorkflow(mailId);
        return claimResponse(currentWorkflow, currentMail, {
          claimStatus: currentWorkflow?.status || "processing",
          alreadyClaimed: currentWorkflow?.status === "claimed"
        });
      }

      log("info", "mail.claimed", {
        mailId,
        playerId: authenticatedPlayerId,
        characterId: existingWorkflow.character_id,
        requestId: existingWorkflow.claim_request_id,
        attachmentCount: existingWorkflow.attachments_snapshot.length
      });

      return claimResponse(result.workflow, currentMail, {
        claimStatus: "claimed",
        claimed: true,
        alreadyClaimed: false
      });
    } catch (error: any) {
      if (error?.getStatus?.()) {
        throw error;
      }
      log("error", "route.claim_mail_failed", { error: error.message });
      throw error;
    }
  }
}
