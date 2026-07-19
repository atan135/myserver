import { createHash, randomBytes, randomUUID } from "node:crypto";

import { Inject, Injectable } from "@nestjs/common";

import { ADMIN_CONFIG, ADMIN_POLICY, ADMIN_STORE } from "../tokens.js";
import { adminPolicyScopeToDatabase, AdminPolicyScopeRequest } from "../auth/admin-policy.service.js";

const IDENTIFIER_PATTERN = /^[A-Za-z0-9][A-Za-z0-9._:@-]{0,127}$/;
const HASH_PATTERN = /^[0-9a-f]{64}$/;
const MAX_SUMMARY_BYTES = 16 * 1024;
const MAX_SUMMARY_DEPTH = 8;
const MAX_PREFLIGHT_TTL_MS = 15 * 60 * 1000;
const MIN_PREFLIGHT_TTL_MS = 10 * 1000;
const SENSITIVE_SUMMARY_KEY = /password|token|secret|private.?key|authorization|cookie|ticket/i;

const APPROVAL_REQUIRED_PERMISSIONS = new Set([
  "gm.send_item",
  "gm.asset_correction.emergency",
  "players.ban",
  "gm.ban_player",
  "game.config.write",
  "proxy.maintenance.write",
  "proxy.rollout.write",
  "proxy.route.write",
  "service.shutdown"
]);

export type AdminOperationActor = {
  adminId: number | string;
  subject: string;
};

export type AdminOperationPreflightInput = {
  actor: AdminOperationActor;
  permission: string;
  scope: AdminPolicyScopeRequest;
  requestId: string;
  reason: string;
  targetSummary: Record<string, unknown>;
  payload: unknown;
  impactSummary: Record<string, unknown>;
  traceId?: string;
  approvalRequired?: boolean;
};

export type AdminOperationExecuteInput = Omit<AdminOperationPreflightInput, "impactSummary" | "approvalRequired"> & {
  nonce: string;
  preflightSummarySha256: string;
};

function operationError(code: string, message = code, details: Record<string, unknown> = {}) {
  const error: any = new Error(message);
  error.code = code;
  Object.assign(error, details);
  return error;
}

function identifier(value: unknown, field: string) {
  const normalized = typeof value === "string" || typeof value === "number" ? String(value).trim() : "";
  if (!IDENTIFIER_PATTERN.test(normalized)) {
    throw operationError("ADMIN_OPERATION_INPUT_INVALID", `${field} is invalid`, { field });
  }
  return normalized;
}

function reason(value: unknown, field = "reason") {
  const normalized = typeof value === "string" ? value.trim() : "";
  if (!normalized || Buffer.byteLength(normalized, "utf8") > 512 || /[\u0000-\u001f\u007f]/.test(normalized)) {
    throw operationError("ADMIN_OPERATION_INPUT_INVALID", `${field} is invalid`, { field });
  }
  return normalized;
}

function canonicalValue(value: unknown, depth = 0): unknown {
  if (depth > MAX_SUMMARY_DEPTH) {
    throw operationError("ADMIN_OPERATION_INPUT_INVALID", "JSON nesting is too deep");
  }
  if (value === null || typeof value === "string" || typeof value === "boolean") {
    return value;
  }
  if (typeof value === "number") {
    if (!Number.isFinite(value)) {
      throw operationError("ADMIN_OPERATION_INPUT_INVALID", "JSON number is invalid");
    }
    return value;
  }
  if (Array.isArray(value)) {
    return value.map((entry) => canonicalValue(entry, depth + 1));
  }
  if (!value || typeof value !== "object" || Object.getPrototypeOf(value) !== Object.prototype) {
    throw operationError("ADMIN_OPERATION_INPUT_INVALID", "value must be JSON-compatible");
  }
  return Object.fromEntries(
    Object.entries(value as Record<string, unknown>)
      .sort(([left], [right]) => left.localeCompare(right))
      .map(([key, entry]) => [key, canonicalValue(entry, depth + 1)])
  );
}

export function canonicalAdminOperationJson(value: unknown) {
  return JSON.stringify(canonicalValue(value));
}

export function adminOperationSha256(value: unknown) {
  const payload = Buffer.isBuffer(value) || value instanceof Uint8Array
    ? Buffer.from(value)
    : Buffer.from(canonicalAdminOperationJson(value), "utf8");
  return createHash("sha256").update(payload).digest("hex");
}

function safeSummary(value: unknown, field: string): Record<string, unknown> {
  const normalized = canonicalValue(value);
  if (!normalized || typeof normalized !== "object" || Array.isArray(normalized)) {
    throw operationError("ADMIN_OPERATION_INPUT_INVALID", `${field} must be an object`, { field });
  }
  const inspect = (entry: unknown, path: string) => {
    if (Array.isArray(entry)) {
      entry.forEach((item, index) => inspect(item, `${path}[${index}]`));
      return;
    }
    if (!entry || typeof entry !== "object") {
      return;
    }
    for (const [key, valueAtKey] of Object.entries(entry as Record<string, unknown>)) {
      if (SENSITIVE_SUMMARY_KEY.test(key)) {
        throw operationError("ADMIN_OPERATION_SENSITIVE_SUMMARY", `${field} contains a sensitive key`, { field, path: `${path}.${key}` });
      }
      inspect(valueAtKey, `${path}.${key}`);
    }
  };
  inspect(normalized, field);
  if (Buffer.byteLength(JSON.stringify(normalized), "utf8") > MAX_SUMMARY_BYTES) {
    throw operationError("ADMIN_OPERATION_INPUT_INVALID", `${field} is too large`, { field });
  }
  return normalized as Record<string, unknown>;
}

function requestScopeSnapshot(scope: AdminPolicyScopeRequest = {}) {
  const normalizedList = (value: unknown, field: string) => {
    if (value === undefined || value === null) return [];
    if (!Array.isArray(value)) {
      throw operationError("ADMIN_OPERATION_INPUT_INVALID", `${field} must be an array`, { field });
    }
    return [...new Set(value.map((entry) => identifier(entry, field)))].sort();
  };
  const optionalIdentifier = (value: unknown, field: string) => value === undefined || value === null || value === ""
    ? null
    : identifier(value, field);
  const targetIds = normalizedList(scope.targetIds, "targetIds");
  const targetCount = scope.targetCount === undefined ? Math.max(targetIds.length, 1) : scope.targetCount;
  if (!Number.isSafeInteger(targetCount) || targetCount < 1 || targetCount > 10_000) {
    throw operationError("ADMIN_OPERATION_INPUT_INVALID", "targetCount is invalid", { field: "targetCount" });
  }
  return {
    worldId: optionalIdentifier(scope.worldId, "worldId"),
    serviceName: optionalIdentifier(scope.serviceName, "serviceName"),
    instanceId: optionalIdentifier(scope.instanceId, "instanceId"),
    fields: normalizedList(scope.fields, "fields"),
    targetType: optionalIdentifier(scope.targetType, "targetType"),
    targetIds,
    targetCount
  };
}

function requiresApproval(permission: string, riskLevel: string, requested: boolean | undefined) {
  return requested === true || riskLevel === "emergency" || APPROVAL_REQUIRED_PERMISSIONS.has(permission);
}

function policyFailure(decision: any) {
  return operationError(
    decision?.code === "SCOPE_DENIED" || decision?.code === "SCOPE_REQUIRED"
      ? "ADMIN_OPERATION_SCOPE_DENIED"
      : "ADMIN_OPERATION_PERMISSION_DENIED",
    "Admin operation is not authorized",
    { policyCode: decision?.code || "PERMISSION_DENIED" }
  );
}

@Injectable()
export class AdminOperationService {
  private readonly preflightTtlMs: number;

  constructor(
    @Inject(ADMIN_CONFIG) private readonly config: any,
    @Inject(ADMIN_POLICY) private readonly policy: any,
    @Inject(ADMIN_STORE) private readonly adminStore: any
  ) {
    const configured = Number(config?.adminOperationPreflightTtlMs ?? 120_000);
    this.preflightTtlMs = Number.isFinite(configured)
      ? Math.min(Math.max(Math.floor(configured), MIN_PREFLIGHT_TTL_MS), MAX_PREFLIGHT_TTL_MS)
      : 120_000;
  }

  async preflight(input: AdminOperationPreflightInput) {
    const requestId = identifier(input?.requestId, "requestId");
    const actorAdminId = identifier(input?.actor?.adminId, "actor id");
    const actorSubject = identifier(input?.actor?.subject, "actor subject");
    const permission = identifier(input?.permission, "permission");
    const operationReason = reason(input?.reason);
    const traceId = input?.traceId === undefined ? `trace-${randomUUID()}` : identifier(input.traceId, "traceId");
    const targetSummary = safeSummary(input?.targetSummary, "targetSummary");
    const impactSummary = safeSummary(input?.impactSummary, "impactSummary");
    const requestedScope = requestScopeSnapshot(input?.scope);
    const permissionRecord = await this.adminStore.findAdminPolicyPermission(permission);
    if (!permissionRecord || permissionRecord.active !== true) {
      throw operationError("ADMIN_OPERATION_PERMISSION_DENIED", "Operation permission is unavailable");
    }
    if (!(["high", "emergency"].includes(permissionRecord.risk_level))) {
      throw operationError("ADMIN_OPERATION_PREFLIGHT_NOT_REQUIRED", "Preflight is only valid for high-risk operations", {
        riskLevel: permissionRecord.risk_level
      });
    }
    const decision = await this.policy.authorize(actorAdminId, permission, input.scope);
    if (!decision?.allowed || !decision.matchedGrant) {
      throw policyFailure(decision);
    }

    const authorizationScope = adminPolicyScopeToDatabase(decision.matchedGrant.scope);
    const scopeSha256 = adminOperationSha256(requestedScope);
    const targetSha256 = adminOperationSha256(targetSummary);
    const payloadSha256 = adminOperationSha256(input.payload);
    const semanticSha256 = adminOperationSha256({
      actorAdminId,
      permission,
      scopeSha256,
      targetSha256,
      payloadSha256,
      reasonSha256: adminOperationSha256(operationReason)
    });
    const nonce = randomBytes(32).toString("base64url");
    const preview = {
      previewId: randomUUID(),
      nonceSha256: adminOperationSha256(nonce),
      impactSummary,
      summarySha256: adminOperationSha256(impactSummary),
      expiresAt: new Date(Date.now() + this.preflightTtlMs).toISOString()
    };
    const approvalStatus = requiresApproval(permission, permissionRecord.risk_level, input.approvalRequired)
      ? "pending"
      : "not_required";
    const reserved = await this.adminStore.reserveAdminOperationPreflight({
      operationId: randomUUID(),
      requestId,
      actorAdminId,
      actorSubject,
      permissionKey: permission,
      riskLevel: permissionRecord.risk_level,
      authorizationScope,
      requestedScope,
      scopeSha256,
      targetSummary,
      targetSha256,
      payloadSha256,
      semanticSha256,
      reason: operationReason,
      traceId,
      approvalStatus,
      preview
    });
    if (reserved.kind === "conflict") {
      throw operationError("ADMIN_OPERATION_REQUEST_CONFLICT", "requestId is already bound to a different operation", { requestId });
    }
    if (reserved.kind === "existing") {
      return {
        state: "existing",
        operation: reserved.operation,
        preflight: null
      };
    }
    return {
      state: "preflighted",
      operation: reserved.operation,
      preflight: {
        nonce,
        summarySha256: preview.summarySha256,
        expiresAt: preview.expiresAt,
        impactSummary,
        approvalStatus
      }
    };
  }

  async claimExecution(input: AdminOperationExecuteInput) {
    const requestId = identifier(input?.requestId, "requestId");
    const actorAdminId = identifier(input?.actor?.adminId, "actor id");
    const permission = identifier(input?.permission, "permission");
    const operationReason = reason(input?.reason);
    const nonce = typeof input?.nonce === "string" && /^[A-Za-z0-9_-]{32,128}$/.test(input.nonce)
      ? input.nonce
      : "";
    if (!nonce) {
      throw operationError("ADMIN_OPERATION_NONCE_INVALID", "Preflight nonce is invalid");
    }
    const summarySha256 = typeof input?.preflightSummarySha256 === "string" && HASH_PATTERN.test(input.preflightSummarySha256)
      ? input.preflightSummarySha256
      : "";
    if (!summarySha256) {
      throw operationError("ADMIN_OPERATION_PREVIEW_INVALID", "Preflight summary hash is invalid");
    }
    const decision = await this.policy.authorize(actorAdminId, permission, input.scope);
    if (!decision?.allowed) {
      throw policyFailure(decision);
    }
    const requestedScope = requestScopeSnapshot(input?.scope);
    const semanticSha256 = adminOperationSha256({
      actorAdminId,
      permission,
      scopeSha256: adminOperationSha256(requestedScope),
      targetSha256: adminOperationSha256(safeSummary(input?.targetSummary, "targetSummary")),
      payloadSha256: adminOperationSha256(input.payload),
      reasonSha256: adminOperationSha256(operationReason)
    });
    const claimed = await this.adminStore.claimAdminOperationExecution({
      requestId,
      semanticSha256,
      nonceSha256: adminOperationSha256(nonce),
      summarySha256
    });
    if (claimed.kind === "claimed") return { state: "claimed", operation: claimed.operation };
    if (claimed.kind === "terminal") return { state: "terminal", operation: claimed.operation };
    if (claimed.kind === "in_progress") return { state: "in_progress", operation: claimed.operation };
    const codes: Record<string, string> = {
      not_found: "ADMIN_OPERATION_NOT_FOUND",
      conflict: "ADMIN_OPERATION_REQUEST_CONFLICT",
      state_conflict: "ADMIN_OPERATION_STATE_CONFLICT",
      approval_pending: "ADMIN_OPERATION_APPROVAL_REQUIRED",
      approval_rejected: "ADMIN_OPERATION_APPROVAL_REJECTED",
      preview_expired: "ADMIN_OPERATION_PREVIEW_EXPIRED",
      nonce_replayed: "ADMIN_OPERATION_NONCE_REPLAYED",
      preview_mismatch: "ADMIN_OPERATION_PREVIEW_MISMATCH"
    };
    throw operationError(codes[claimed.kind] || "ADMIN_OPERATION_STATE_CONFLICT", "Operation execution was rejected", {
      requestId,
      operation: claimed.operation || null
    });
  }

  async completeExecution({
    operationId,
    status,
    resultSummary = null,
    errorSummary = null,
    details = {}
  }: {
    operationId: string;
    status: "succeeded" | "failed" | "execution_uncertain" | "cancelled";
    resultSummary?: Record<string, unknown> | null;
    errorSummary?: Record<string, unknown> | null;
    details?: Record<string, unknown>;
  }) {
    return this.adminStore.completeAdminOperation({
      operationId: identifier(operationId, "operationId"),
      status,
      resultSummary: resultSummary === null ? null : safeSummary(resultSummary, "resultSummary"),
      errorSummary: errorSummary === null ? null : safeSummary(errorSummary, "errorSummary"),
      details: safeSummary(details, "details")
    });
  }

  async decideApproval({
    requestId,
    actor,
    status,
    evidenceSummary = {},
    rejectionReason = null
  }: {
    requestId: string;
    actor: AdminOperationActor;
    status: "approved" | "rejected";
    evidenceSummary?: Record<string, unknown>;
    rejectionReason?: string | null;
  }) {
    const normalizedStatus = status === "approved" || status === "rejected" ? status : null;
    if (!normalizedStatus) {
      throw operationError("ADMIN_OPERATION_APPROVAL_STATUS_INVALID", "Approval status is invalid");
    }
    const normalizedRejectionReason = normalizedStatus === "rejected"
      ? reason(rejectionReason, "rejectionReason")
      : null;
    return this.adminStore.decideAdminOperationApproval({
      requestId: identifier(requestId, "requestId"),
      status: normalizedStatus,
      decidedByAdminId: identifier(actor?.adminId, "actor id"),
      decidedBySubject: identifier(actor?.subject, "actor subject"),
      evidenceSummary: safeSummary(evidenceSummary, "evidenceSummary"),
      rejectionReason: normalizedRejectionReason
    });
  }
}
