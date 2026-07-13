import { Inject, Injectable } from "@nestjs/common";

import { badRequest, conflict, notFound, serviceUnavailable } from "../common/http-exception.js";
import { MAIL_CONFIG, MAIL_GAME_ADMIN_CLIENT, MAIL_STORE } from "../tokens.js";

const CLAIM_STATUSES = new Set([
  "processing", "retryable_failure", "permanent_failure",
  "reconciliation_pending", "manual_review", "claimed"
]);
const IDENTIFIER_PATTERN = /^[A-Za-z0-9:_-]+$/;
const GAME_INSTANCE_ID_PATTERN = /^(?:game(?:-server)?-[a-z0-9][a-z0-9._-]{0,115}|local-fallback)$/;

function boundedIdentifier(value: any, name: string, maxBytes = 128) {
  const normalized = typeof value === "string" ? value.trim() : "";
  if (!normalized || Buffer.byteLength(normalized, "utf8") > maxBytes || !IDENTIFIER_PATTERN.test(normalized)) {
    throw badRequest(`INVALID_${name.toUpperCase()}`, `${name} is invalid`);
  }
  return normalized;
}

function boundedText(value: any, name: string, maxBytes: number) {
  const normalized = typeof value === "string" ? value.trim() : "";
  if (!normalized || Buffer.byteLength(normalized, "utf8") > maxBytes) {
    throw badRequest(`INVALID_${name.toUpperCase()}`, `${name} is required and must be at most ${maxBytes} bytes`);
  }
  return normalized;
}

function safeError(workflow: any) {
  if (!workflow?.last_error_code) return null;
  return {
    code: workflow.last_error_code,
    category: workflow.last_error_category,
    result_state: workflow.last_result_state,
    retryable: workflow.last_error_retryable === true
  };
}

function safeGameResultSummary(summary: any) {
  if (!summary || typeof summary !== "object" || Array.isArray(summary)) return null;
  const items = Array.isArray(summary.items) ? summary.items : [];
  const declaredCount = Number(summary.itemCount || summary.item_count);
  const itemCount = items.length > 0
    ? Math.min(items.length, 1000)
    : (Number.isSafeInteger(declaredCount) && declaredCount >= 0 && declaredCount <= 1000 ? declaredCount : 0);
  return {
    character_id: summary.characterId || summary.character_id || null,
    source: summary.source || null,
    item_count: itemCount
  };
}

function safeInstanceIds(values: any) {
  if (!Array.isArray(values)) return [];
  return values
    .filter((value) =>
      typeof value === "string" &&
      Buffer.byteLength(value, "utf8") > 0 &&
      Buffer.byteLength(value, "utf8") <= 128 &&
      GAME_INSTANCE_ID_PATTERN.test(value)
    )
    .slice(0, 32);
}

function safeQueryEvidence(workflow: any) {
  return {
    status: workflow.last_query_status || null,
    fingerprint: workflow.last_query_fingerprint || null,
    error_code: workflow.last_query_error_code || null,
    result_state: workflow.last_query_result_state || null,
    instance_ids: safeInstanceIds(workflow.last_query_instance_ids)
  };
}

@Injectable()
export class MailOperationsService {
  constructor(
    @Inject(MAIL_STORE) private readonly store: any,
    @Inject(MAIL_GAME_ADMIN_CLIENT) private readonly gameAdminClient: any,
    @Inject(MAIL_CONFIG) private readonly config: any = {}
  ) {}

  async queryClaims(query: any = {}) {
    const filters: any = {};
    for (const [source, target, max] of [
      ["mail_id", "mailId", 64], ["request_id", "requestId", 128],
      ["player_id", "playerId", 64], ["character_id", "characterId", 64]
    ] as const) {
      if (query[source] !== undefined && query[source] !== "") {
        filters[target] = boundedIdentifier(query[source], source, max);
      }
    }
    if (query.status !== undefined && query.status !== "") {
      if (!CLAIM_STATUSES.has(query.status)) throw badRequest("INVALID_STATUS", "status is invalid");
      filters.status = query.status;
    }
    if (Object.keys(filters).length === 0) {
      throw badRequest("CLAIM_QUERY_FILTER_REQUIRED", "at least one exact claim filter is required");
    }
    const rawLimit = String(query.limit || "20");
    const limit = /^\d+$/.test(rawLimit) ? Number.parseInt(rawLimit, 10) : Number.NaN;
    if (!Number.isSafeInteger(limit) || limit < 1 || limit > 50) {
      throw badRequest("INVALID_LIMIT", "limit must be between 1 and 50");
    }
    const rawBeforeId = query.before_id === undefined || query.before_id === ""
      ? null
      : String(query.before_id);
    const beforeId = rawBeforeId === null
      ? null
      : (/^\d+$/.test(rawBeforeId) ? Number.parseInt(rawBeforeId, 10) : Number.NaN);
    if (beforeId !== null && (!Number.isSafeInteger(beforeId) || beforeId < 1)) {
      throw badRequest("INVALID_BEFORE_ID", "before_id must be a positive integer");
    }

    const page = await this.store.queryMailClaimWorkflows(filters, { limit, beforeId });
    const items = [];
    for (const row of page.items) {
      let gameResult: any;
      try {
        gameResult = await this.gameAdminClient.queryMailAttachmentGrant(
          row.claim_request_id,
          row.attachments_fingerprint,
          { characterId: row.character_id, items: row.attachments_snapshot }
        );
      } catch (error: any) {
        gameResult = {
          queryStatus: "result_unavailable",
          errorCode: error?.code || "GRANT_RESULT_QUERY_UNAVAILABLE",
          resultState: "unknown",
          instanceIds: []
        };
      }
      const operations = await this.store.getMailAdminOperationAudits?.("mail_claim", row.mail_id, 5) || [];
      items.push({
        mail_id: row.mail_id,
        request_id: row.claim_request_id,
        player_id: row.player_id,
        character_id: row.character_id,
        mail_status: row.mail_status,
        workflow_status: row.status,
        attachments_fingerprint: row.attachments_fingerprint,
        attempts: row.attempts,
        recovery_attempts: row.recovery_attempts,
        last_error: safeError(row),
        last_query: safeQueryEvidence(row),
        game_result: {
          status: gameResult?.queryStatus || "result_unavailable",
          request_id: gameResult?.requestId || row.claim_request_id,
          fingerprint: gameResult?.requestFingerprint || null,
          result_state: gameResult?.resultState || "unknown",
          error_code: gameResult?.errorCode || null,
          instance_ids: safeInstanceIds(gameResult?.instanceIds),
          result_summary: gameResult?.queryStatus === "succeeded"
            ? safeGameResultSummary(gameResult.resultSummary)
            : null
        },
        notification: row.outbox_event_id ? {
          event_id: row.outbox_event_id,
          status: row.outbox_status,
          attempts: row.outbox_attempts,
          terminal_at: row.outbox_terminal_at
        } : null,
        operations,
        created_at: row.created_at,
        updated_at: row.updated_at,
        completed_at: row.completed_at
      });
    }
    return {
      ok: true,
      items,
      next_before_id: page.nextBeforeId,
      retention: this.retentionPolicy()
    };
  }

  async scheduleClaim(mailIdValue: string, action: string, body: any, highRisk = false) {
    if (!new Set(["reconcile", "retry_original", "manual_recover"]).has(action)) {
      throw badRequest("INVALID_MAIL_OPERATION", "mail operation is invalid");
    }
    const mailId = boundedIdentifier(mailIdValue, "mail_id", 64);
    const operation = {
      operationRequestId: boundedIdentifier(body?.operation_request_id, "operation_request_id", 128),
      actor: boundedText(body?.actor, "actor", 128),
      reason: boundedText(body?.reason, "reason", 512),
      action,
      mailId,
      highRisk
    };
    try {
      const result = await this.store.scheduleMailClaimAdminRecovery(operation);
      return { ok: true, ...result };
    } catch (error: any) {
      if (error?.code === "MAIL_CLAIM_NOT_FOUND") throw notFound(error.code, "mail claim workflow not found");
      if (error?.code === "ADMIN_OPERATION_CONFLICT" || error?.code === "MAIL_CLAIM_OPERATION_NOT_ALLOWED") {
        throw conflict(error.code, error.message);
      }
      throw error;
    }
  }

  async replayOutbox(eventIdValue: string, body: any) {
    const operation = {
      operationRequestId: boundedIdentifier(body?.operation_request_id, "operation_request_id", 128),
      actor: boundedText(body?.actor, "actor", 128),
      reason: boundedText(body?.reason, "reason", 512),
      action: "outbox_terminal_replay",
      eventId: boundedIdentifier(eventIdValue, "event_id", 128)
    };
    try {
      return { ok: true, ...(await this.store.replayTerminalMailNotification(operation)) };
    } catch (error: any) {
      if (error?.code === "OUTBOX_EVENT_NOT_FOUND") throw notFound(error.code, "notification event not found");
      if (error?.code === "ADMIN_OPERATION_CONFLICT" || error?.code === "OUTBOX_REPLAY_NOT_ALLOWED") {
        throw conflict(error.code, error.message);
      }
      throw serviceUnavailable(error?.code || "OUTBOX_REPLAY_FAILED", "notification replay could not be scheduled");
    }
  }

  retentionPolicy() {
    return {
      mails_days: this.config.mailRetentionDays || 400,
      notification_outbox_sent_days: this.config.outboxSentRetentionDays || 7,
      notification_outbox_terminal_days: this.config.outboxTerminalRetentionDays || 30,
      claim_workflows_days: this.config.claimWorkflowRetentionDays || 400,
      game_grant_idempotency_days: this.config.gameGrantRetentionDays || 400,
      operation_audit: "append_only_no_automatic_delete"
    };
  }
}
