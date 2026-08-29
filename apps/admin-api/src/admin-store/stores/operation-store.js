import { UNIQUE_VIOLATION } from "../constants.js";
import { createAdminStoreError } from "../errors.js";
import { toRequiredJsonb, operationIsTerminal } from "../formatters.js";
import { toAdminOperation, toAdminOperationAuditEvent } from "../mappers/admin.js";

// Preserve original (latent bug) behavior: callers in this file used a
// local operationStoreError symbol that was never defined. We bind it to
// createAdminStoreError so runtime semantics are unchanged.
const operationStoreError = createAdminStoreError;

export class OperationStore {
  constructor(pool) {
    this.pool = pool;
  }

  async insertAdminOperationAuditEvent(client, {
    operation = null,
    breakglassGrantId = null,
    eventType,
    actorAdminId = null,
    actorSubject,
    requestId = null,
    permissionKey = null,
    riskLevel = null,
    traceId = null,
    reason,
    targetSummary = null,
    resultSummary = null,
    details = {}
  }) {
    await client.query(
      `INSERT INTO admin_operation_audit_events (
         operation_id, breakglass_grant_id, event_type, actor_admin_id, actor_subject,
         request_id, permission_key, risk_level, trace_id, reason,
         target_summary_json, result_summary_json, details_json
       ) VALUES (
         $1, $2, $3, $4, $5,
         $6, $7, $8, $9, $10,
         $11::jsonb, $12::jsonb, $13::jsonb
       )`,
      [
        operation?.operationId || operation?.operation_id || null,
        breakglassGrantId,
        eventType,
        actorAdminId,
        actorSubject,
        requestId || operation?.requestId || operation?.request_id || null,
        permissionKey || operation?.permissionKey || operation?.permission_key || null,
        riskLevel || operation?.riskLevel || operation?.risk_level || null,
        traceId || operation?.traceId || operation?.trace_id || null,
        reason,
        targetSummary === null ? null : toRequiredJsonb(targetSummary),
        resultSummary === null ? null : toRequiredJsonb(resultSummary),
        toRequiredJsonb(details)
      ]
    );
  }

  async reserveAdminOperationPreflight({
    operationId,
    requestId,
    actorAdminId,
    actorSubject,
    permissionKey,
    riskLevel,
    authorizationScope,
    requestedScope,
    scopeSha256,
    targetSummary,
    targetSha256,
    payloadSha256,
    semanticSha256,
    reason,
    traceId,
    approvalStatus,
    preview
  }) {
    const client = await this.pool.connect();
    try {
      await client.query("BEGIN");
      const existing = await client.query(
        `SELECT r.*,
                p.preview_id, p.summary_sha256, p.expires_at AS preview_expires_at,
                p.consumed_at AS preview_consumed_at
         FROM admin_operation_requests r
         LEFT JOIN admin_operation_previews p ON p.operation_id = r.operation_id
         WHERE r.request_id = $1
         FOR UPDATE OF r`,
        [requestId]
      );
      if (existing.rows.length > 0) {
        const operation = toAdminOperation(existing.rows[0]);
        await client.query("COMMIT");
        return {
          kind: operation.semanticSha256 === semanticSha256 ? "existing" : "conflict",
          operation
        };
      }

      const inserted = await client.query(
        `INSERT INTO admin_operation_requests (
           operation_id, request_id, actor_admin_id, actor_subject, permission_key, risk_level,
           authorization_scope_json, requested_scope_json, scope_sha256,
           target_summary_json, target_sha256, payload_sha256, semantic_sha256,
           reason, trace_id, status, approval_status
         ) VALUES (
           $1::uuid, $2, $3, $4, $5, $6,
           $7::jsonb, $8::jsonb, $9,
           $10::jsonb, $11, $12, $13,
           $14, $15, 'preflighted', $16
         )
         RETURNING *`,
        [
          operationId,
          requestId,
          actorAdminId,
          actorSubject,
          permissionKey,
          riskLevel,
          toRequiredJsonb(authorizationScope),
          toRequiredJsonb(requestedScope),
          scopeSha256,
          toRequiredJsonb(targetSummary),
          targetSha256,
          payloadSha256,
          semanticSha256,
          reason,
          traceId,
          approvalStatus
        ]
      );
      const operation = toAdminOperation(inserted.rows[0]);
      await client.query(
        `INSERT INTO admin_operation_previews (
           preview_id, operation_id, nonce_sha256, impact_summary_json, summary_sha256,
           target_sha256, payload_sha256, expires_at
         ) VALUES ($1::uuid, $2::uuid, $3, $4::jsonb, $5, $6, $7, $8::timestamptz)`,
        [
          preview.previewId,
          operationId,
          preview.nonceSha256,
          toRequiredJsonb(preview.impactSummary),
          preview.summarySha256,
          targetSha256,
          payloadSha256,
          preview.expiresAt
        ]
      );
      await client.query(
        `INSERT INTO admin_operation_approvals (operation_id, status, evidence_summary_json)
         VALUES ($1::uuid, $2, $3::jsonb)`,
        [operationId, approvalStatus, toRequiredJsonb({ requirement: approvalStatus })]
      );
      await this.insertAdminOperationAuditEvent(client, {
        operation,
        eventType: "preflight_created",
        actorAdminId,
        actorSubject,
        reason,
        targetSummary,
        details: {
          previewId: preview.previewId,
          previewExpiresAt: preview.expiresAt,
          approvalStatus,
          payloadSha256
        }
      });
      await client.query("COMMIT");
      return {
        kind: "created",
        operation: {
          ...operation,
          preview: {
            previewId: preview.previewId,
            summarySha256: preview.summarySha256,
            expiresAt: preview.expiresAt,
            consumedAt: null
          }
        }
      };
    } catch (error) {
      await client.query("ROLLBACK").catch(() => undefined);
      if (error.code === UNIQUE_VIOLATION) {
        const { rows } = await this.pool.query(
          `SELECT * FROM admin_operation_requests WHERE request_id = $1 LIMIT 1`,
          [requestId]
        );
        if (rows.length > 0) {
          const operation = toAdminOperation(rows[0]);
          return { kind: operation.semanticSha256 === semanticSha256 ? "existing" : "conflict", operation };
        }
      }
      throw error;
    } finally {
      client.release();
    }
  }

  async getAdminOperationByRequestId(requestId) {
    const { rows } = await this.pool.query(
      `SELECT r.*,
              p.preview_id, p.summary_sha256, p.expires_at AS preview_expires_at,
              p.consumed_at AS preview_consumed_at,
              p.impact_summary_json AS preview_impact_summary_json,
              a.status AS approval_record_status,
              a.requested_at AS approval_requested_at,
              a.decided_at AS approval_decided_at,
              a.decided_by_admin_id AS approval_decided_by_admin_id,
              a.decided_by_subject AS approval_decided_by_subject,
              a.evidence_summary_json AS approval_evidence_summary_json,
              a.rejection_reason AS approval_rejection_reason
       FROM admin_operation_requests r
       LEFT JOIN admin_operation_previews p ON p.operation_id = r.operation_id
       LEFT JOIN admin_operation_approvals a ON a.operation_id = r.operation_id
       WHERE r.request_id = $1
       LIMIT 1`,
      [requestId]
    );
    return rows.length > 0 ? toAdminOperation(rows[0]) : null;
  }

  async listPendingAdminOperations({ limit = 100 } = {}) {
    const boundedLimit = Math.max(1, Math.min(Number(limit) || 1, 100));
    const { rows } = await this.pool.query(
      `SELECT r.*,
              p.preview_id, p.summary_sha256, p.expires_at AS preview_expires_at,
              p.consumed_at AS preview_consumed_at,
              p.impact_summary_json AS preview_impact_summary_json,
              a.status AS approval_record_status,
              a.requested_at AS approval_requested_at,
              a.decided_at AS approval_decided_at,
              a.decided_by_admin_id AS approval_decided_by_admin_id,
              a.decided_by_subject AS approval_decided_by_subject,
              a.evidence_summary_json AS approval_evidence_summary_json,
              a.rejection_reason AS approval_rejection_reason
       FROM admin_operation_requests r
       JOIN admin_operation_approvals a ON a.operation_id = r.operation_id
       LEFT JOIN admin_operation_previews p ON p.operation_id = r.operation_id
       WHERE r.approval_status = 'pending'
         AND r.status = 'preflighted'
         AND a.status = 'pending'
       ORDER BY r.created_at DESC
       LIMIT $1`,
      [boundedLimit]
    );
    return rows.map(toAdminOperation);
  }

  async claimAdminOperationExecution({ requestId, semanticSha256, nonceSha256, summarySha256, now = new Date() }) {
    const client = await this.pool.connect();
    try {
      await client.query("BEGIN");
      const selected = await client.query(
        `SELECT r.*,
                p.preview_id, p.nonce_sha256, p.summary_sha256, p.expires_at AS preview_expires_at,
                p.consumed_at AS preview_consumed_at,
                a.status AS approval_record_status
         FROM admin_operation_requests r
         JOIN admin_operation_previews p ON p.operation_id = r.operation_id
         JOIN admin_operation_approvals a ON a.operation_id = r.operation_id
         WHERE r.request_id = $1
         FOR UPDATE OF r, p, a`,
        [requestId]
      );
      if (selected.rows.length === 0) {
        await client.query("COMMIT");
        return { kind: "not_found" };
      }

      const row = selected.rows[0];
      const operation = toAdminOperation(row);
      if (operation.semanticSha256 !== semanticSha256) {
        await client.query("COMMIT");
        return { kind: "conflict", operation };
      }
      if (operationIsTerminal(operation.status)) {
        await client.query("COMMIT");
        return { kind: "terminal", operation };
      }
      if (operation.status === "executing") {
        await client.query("COMMIT");
        return { kind: "in_progress", operation };
      }
      if (!(["preflighted", "approved"].includes(operation.status)) ||
          operation.approvalStatus !== row.approval_record_status) {
        await client.query("COMMIT");
        return { kind: "state_conflict", operation };
      }
      if (operation.approvalStatus === "pending") {
        await client.query("COMMIT");
        return { kind: "approval_pending", operation };
      }
      if (operation.approvalStatus === "rejected") {
        await client.query("COMMIT");
        return { kind: "approval_rejected", operation };
      }
      if (new Date(row.preview_expires_at).getTime() <= new Date(now).getTime()) {
        await client.query("COMMIT");
        return { kind: "preview_expired", operation };
      }
      if (row.preview_consumed_at) {
        await client.query("COMMIT");
        return { kind: "nonce_replayed", operation };
      }
      if (row.nonce_sha256 !== nonceSha256 || row.summary_sha256 !== summarySha256) {
        await client.query("COMMIT");
        return { kind: "preview_mismatch", operation };
      }

      const previewUpdate = await client.query(
        `UPDATE admin_operation_previews
         SET consumed_at = $2::timestamptz
         WHERE preview_id = $1::uuid AND consumed_at IS NULL`,
        [row.preview_id, now]
      );
      if (previewUpdate.rowCount !== 1) {
        await client.query("COMMIT");
        return { kind: "nonce_replayed", operation };
      }
      const claimed = await client.query(
        `UPDATE admin_operation_requests
         SET status = 'executing', execution_claimed_at = $2::timestamptz, updated_at = $2::timestamptz
         WHERE operation_id = $1::uuid AND status IN ('preflighted', 'approved')
         RETURNING *`,
        [operation.operationId, now]
      );
      if (claimed.rows.length === 0) {
        await client.query("COMMIT");
        return { kind: "state_conflict", operation };
      }
      const claimedOperation = toAdminOperation({
        ...claimed.rows[0],
        preview_id: row.preview_id,
        summary_sha256: row.summary_sha256,
        preview_expires_at: row.preview_expires_at,
        preview_consumed_at: now
      });
      await this.insertAdminOperationAuditEvent(client, {
        operation: claimedOperation,
        eventType: "execution_claimed",
        actorAdminId: claimedOperation.actorAdminId,
        actorSubject: claimedOperation.actorSubject,
        reason: claimedOperation.reason,
        targetSummary: claimedOperation.targetSummary,
        details: { previewId: row.preview_id }
      });
      await client.query("COMMIT");
      return { kind: "claimed", operation: claimedOperation };
    } catch (error) {
      await client.query("ROLLBACK").catch(() => undefined);
      throw error;
    } finally {
      client.release();
    }
  }

  async completeAdminOperation({ operationId, status, resultSummary = null, errorSummary = null, details = {}, now = new Date() }) {
    const eventTypes = {
      succeeded: "execution_succeeded",
      failed: "execution_failed",
      execution_uncertain: "execution_uncertain",
      cancelled: "execution_cancelled"
    };
    if (!Object.prototype.hasOwnProperty.call(eventTypes, status)) {
      throw operationStoreError("ADMIN_OPERATION_RESULT_STATUS_INVALID", "Operation result status is invalid", { status });
    }

    const client = await this.pool.connect();
    try {
      await client.query("BEGIN");
      const existing = await client.query(
        `SELECT * FROM admin_operation_requests WHERE operation_id = $1::uuid FOR UPDATE`,
        [operationId]
      );
      if (existing.rows.length === 0) {
        throw operationStoreError("ADMIN_OPERATION_NOT_FOUND", "Operation does not exist", { operationId });
      }
      const prior = toAdminOperation(existing.rows[0]);
      if (operationIsTerminal(prior.status)) {
        await client.query("COMMIT");
        return { kind: "terminal", operation: prior };
      }
      if (prior.status !== "executing") {
        await client.query("COMMIT");
        return { kind: "state_conflict", operation: prior };
      }
      const updated = await client.query(
        `UPDATE admin_operation_requests
         SET status = $2,
             result_summary_json = $3::jsonb,
             error_summary_json = $4::jsonb,
             completed_at = $5::timestamptz,
             updated_at = $5::timestamptz
         WHERE operation_id = $1::uuid AND status = 'executing'
         RETURNING *`,
        [operationId, status, resultSummary === null ? null : toRequiredJsonb(resultSummary), errorSummary === null ? null : toRequiredJsonb(errorSummary), now]
      );
      if (updated.rows.length === 0) {
        await client.query("COMMIT");
        return { kind: "state_conflict", operation: prior };
      }
      const operation = toAdminOperation(updated.rows[0]);
      await this.insertAdminOperationAuditEvent(client, {
        operation,
        eventType: eventTypes[status],
        actorAdminId: operation.actorAdminId,
        actorSubject: operation.actorSubject,
        reason: operation.reason,
        targetSummary: operation.targetSummary,
        resultSummary: status === "succeeded" ? resultSummary : errorSummary,
        details
      });
      await client.query("COMMIT");
      return { kind: "completed", operation };
    } catch (error) {
      await client.query("ROLLBACK").catch(() => undefined);
      throw error;
    } finally {
      client.release();
    }
  }

  async markAdminOperationExecutionUncertain({ operationId, errorSummary, now = new Date() }) {
    const { rows } = await this.pool.query(
      `UPDATE admin_operation_requests
       SET status = 'execution_uncertain',
           error_summary_json = $2::jsonb,
           completed_at = $3::timestamptz,
           updated_at = $3::timestamptz
       WHERE operation_id = $1::uuid AND status = 'executing'
       RETURNING *`,
      [operationId, toRequiredJsonb(errorSummary), now]
    );
    if (rows.length > 0) {
      return { kind: "marked_uncertain", operation: toAdminOperation(rows[0]) };
    }
    const existing = await this.pool.query(
      `SELECT * FROM admin_operation_requests WHERE operation_id = $1::uuid LIMIT 1`,
      [operationId]
    );
    if (existing.rows.length === 0) {
      throw operationStoreError("ADMIN_OPERATION_NOT_FOUND", "Operation does not exist", { operationId });
    }
    return { kind: "terminal_or_conflict", operation: toAdminOperation(existing.rows[0]) };
  }

  async decideAdminOperationApproval({
    requestId,
    status,
    decidedByAdminId = null,
    decidedBySubject,
    evidenceSummary = {},
    rejectionReason = null,
    now = new Date()
  }) {
    if (!["approved", "rejected"].includes(status)) {
      throw operationStoreError("ADMIN_OPERATION_APPROVAL_STATUS_INVALID", "Approval status is invalid", { status });
    }

    const client = await this.pool.connect();
    try {
      await client.query("BEGIN");
      const selected = await client.query(
        `SELECT r.*,
                a.status AS approval_record_status
         FROM admin_operation_requests r
         JOIN admin_operation_approvals a ON a.operation_id = r.operation_id
         WHERE r.request_id = $1
         FOR UPDATE OF r, a`,
        [requestId]
      );
      if (selected.rows.length === 0) {
        throw operationStoreError("ADMIN_OPERATION_NOT_FOUND", "Operation does not exist", { requestId });
      }
      const prior = toAdminOperation(selected.rows[0]);
      if (prior.approvalStatus !== "pending" || selected.rows[0].approval_record_status !== "pending" || prior.status !== "preflighted") {
        await client.query("COMMIT");
        return { kind: "state_conflict", operation: prior };
      }
      const nextOperationStatus = status === "approved" ? "approved" : "cancelled";
      const next = await client.query(
        `UPDATE admin_operation_requests
         SET approval_status = $2::varchar,
             status = $3::varchar,
             completed_at = CASE WHEN $3::varchar = 'cancelled' THEN $4::timestamptz ELSE NULL END,
             error_summary_json = CASE WHEN $3::varchar = 'cancelled' THEN $5::jsonb ELSE NULL END,
             updated_at = $4::timestamptz
         WHERE operation_id = $1::uuid
         RETURNING *`,
        [
          prior.operationId,
          status,
          nextOperationStatus,
          now,
          status === "rejected" ? toRequiredJsonb({ code: "ADMIN_OPERATION_APPROVAL_REJECTED", reason: rejectionReason }) : null
        ]
      );
      await client.query(
        `UPDATE admin_operation_approvals
         SET status = $2::varchar,
             decided_at = $3::timestamptz,
             decided_by_admin_id = $4,
             decided_by_subject = $5,
             evidence_summary_json = $6::jsonb,
             rejection_reason = $7,
             updated_at = $3::timestamptz
         WHERE operation_id = $1::uuid AND status = 'pending'`,
        [prior.operationId, status, now, decidedByAdminId, decidedBySubject, toRequiredJsonb(evidenceSummary), rejectionReason]
      );
      const operation = toAdminOperation(next.rows[0]);
      await this.insertAdminOperationAuditEvent(client, {
        operation,
        eventType: status === "approved" ? "approval_approved" : "approval_rejected",
        actorAdminId: decidedByAdminId,
        actorSubject: decidedBySubject,
        reason: status === "approved" ? operation.reason : rejectionReason,
        targetSummary: operation.targetSummary,
        resultSummary: status === "approved" ? evidenceSummary : { code: "ADMIN_OPERATION_APPROVAL_REJECTED" },
        details: { approvalStatus: status }
      });
      await client.query("COMMIT");
      return { kind: status, operation };
    } catch (error) {
      await client.query("ROLLBACK").catch(() => undefined);
      throw error;
    } finally {
      client.release();
    }
  }
  async listAdminOperationAuditEvents({
    limit = 100,
    from,
    to,
    cursor = null,
    actorAdminId,
    permissionKey,
    eventType,
    target,
    requestId,
    traceId,
    riskLevel,
    result
  } = {}) {
    const params = [from, to];
    let query = `SELECT e.*, r.status AS operation_status
                 FROM admin_operation_audit_events e
                 LEFT JOIN admin_operation_requests r ON r.operation_id = e.operation_id
                 WHERE e.created_at >= $1::timestamptz AND e.created_at < $2::timestamptz`;
    const add = (value) => {
      params.push(value);
      return `$${params.length}`;
    };

    if (actorAdminId !== undefined && actorAdminId !== null) {
      query += ` AND e.actor_admin_id = ${add(actorAdminId)}`;
    }
    if (permissionKey) {
      query += ` AND e.permission_key = ${add(permissionKey)}`;
    }
    if (eventType) {
      query += ` AND e.event_type = ${add(eventType)}`;
    }
    if (target) {
      const targetParam = add(target);
      query += ` AND (e.target_summary_json -> 'targetIds' ? ${targetParam} OR e.target_summary_json ->> 'targetId' = ${targetParam})`;
    }
    if (requestId) {
      query += ` AND e.request_id = ${add(requestId)}`;
    }
    if (traceId) {
      query += ` AND e.trace_id = ${add(traceId)}`;
    }
    if (riskLevel) {
      query += ` AND e.risk_level = ${add(riskLevel)}`;
    }
    if (result) {
      query += ` AND r.status = ${add(result)}`;
    }
    if (cursor) {
      const createdAt = add(cursor.createdAt);
      const id = add(cursor.id);
      query += ` AND (e.created_at, e.id) < (${createdAt}::timestamptz, ${id}::bigint)`;
    }
    query += ` ORDER BY e.created_at DESC, e.id DESC LIMIT ${add(Math.max(1, Math.min(Number(limit) || 1, 5001)))}`;
    const { rows } = await this.pool.query(query, params);
    return rows.map(toAdminOperationAuditEvent);
  }
}
