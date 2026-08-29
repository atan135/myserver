import { redactAuditReason } from "../../operations/audit-reason.js";

import { toIsoString, toNumericId, normalizeJson, redactOperationAuditValue } from "../formatters.js";

export function toAdmin(row) {
  return {
    id: toNumericId(row.id),
    username: row.username,
    displayName: row.display_name,
    role: row.role,
    status: row.status,
    passwordAlgo: row.password_algo,
    passwordSalt: row.password_salt,
    passwordHash: row.password_hash
  };
}

export function toPlayer(row) {
  return {
    player_id: row.player_id,
    guest_id: row.guest_id,
    login_name: row.login_name,
    display_name: row.display_name,
    account_type: row.account_type,
    status: row.status,
    ban_expires_at: toIsoString(row.ban_expires_at),
    banExpiresAt: toIsoString(row.ban_expires_at),
    created_at: toIsoString(row.created_at),
    last_login_at: toIsoString(row.last_login_at)
  };
}

export function toAdminOperation(row) {
  if (!row) {
    return null;
  }

  return {
    operationId: row.operation_id,
    requestId: row.request_id,
    actorAdminId: toNumericId(row.actor_admin_id),
    actorSubject: row.actor_subject,
    permissionKey: row.permission_key,
    riskLevel: row.risk_level,
    authorizationScope: normalizeJson(row.authorization_scope_json),
    requestedScope: normalizeJson(row.requested_scope_json),
    scopeSha256: row.scope_sha256,
    targetSummary: normalizeJson(row.target_summary_json),
    targetSha256: row.target_sha256,
    payloadSha256: row.payload_sha256,
    semanticSha256: row.semantic_sha256,
    reason: redactAuditReason(row.reason),
    traceId: row.trace_id,
    status: row.status,
    approvalStatus: row.approval_status,
    executionClaimedAt: toIsoString(row.execution_claimed_at),
    completedAt: toIsoString(row.completed_at),
    resultSummary: normalizeJson(row.result_summary_json),
    errorSummary: normalizeJson(row.error_summary_json),
    createdAt: toIsoString(row.created_at),
    updatedAt: toIsoString(row.updated_at),
    preview: row.preview_id ? {
      previewId: row.preview_id,
      summarySha256: row.summary_sha256,
      impactSummary: redactOperationAuditValue(normalizeJson(row.preview_impact_summary_json) || {}),
      expiresAt: toIsoString(row.preview_expires_at),
      consumedAt: toIsoString(row.preview_consumed_at)
    } : null,
    approval: row.approval_record_status ? {
      status: row.approval_record_status,
      requestedAt: toIsoString(row.approval_requested_at),
      decidedAt: toIsoString(row.approval_decided_at),
      decidedByAdminId: toNumericId(row.approval_decided_by_admin_id),
      decidedBySubject: row.approval_decided_by_subject || null,
      evidenceSummary: redactOperationAuditValue(normalizeJson(row.approval_evidence_summary_json) || {}),
      rejectionReason: redactAuditReason(row.approval_rejection_reason)
    } : null
  };
}

export function toAdminOperationAuditEvent(row) {
  return {
    id: toNumericId(row.id),
    operationId: row.operation_id || null,
    breakglassGrantId: row.breakglass_grant_id || null,
    eventType: row.event_type,
    actorAdminId: toNumericId(row.actor_admin_id),
    actorSubject: row.actor_subject,
    requestId: row.request_id || null,
    permissionKey: row.permission_key || null,
    riskLevel: row.risk_level || null,
    traceId: row.trace_id || null,
    reason: redactAuditReason(row.reason),
    targetSummary: redactOperationAuditValue(normalizeJson(row.target_summary_json)),
    resultSummary: redactOperationAuditValue(normalizeJson(row.result_summary_json)),
    details: redactOperationAuditValue(normalizeJson(row.details_json) || {}),
    result: row.operation_status || null,
    createdAt: toIsoString(row.created_at)
  };
}

export function toBreakglassGrant(row) {
  if (!row) {
    return null;
  }

  return {
    grantId: row.grant_id,
    activationRequestId: row.activation_request_id,
    actorAdminId: toNumericId(row.actor_admin_id),
    actorSubject: row.actor_subject,
    permissionKey: row.permission_key,
    scope: normalizeJson(row.scope_json),
    scopeSha256: row.scope_sha256,
    targetSummary: normalizeJson(row.target_summary_json),
    targetSha256: row.target_sha256,
    semanticSha256: row.semantic_sha256,
    reason: row.reason,
    activatedAt: toIsoString(row.activated_at),
    expiresAt: toIsoString(row.expires_at),
    revokedAt: toIsoString(row.revoked_at),
    revokedByAdminId: toNumericId(row.revoked_by_admin_id),
    revokedBySubject: row.revoked_by_subject || null,
    revocationReason: row.revocation_reason || null
  };
}

