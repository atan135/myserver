import { randomUUID } from "node:crypto";

import { Inject, Injectable } from "@nestjs/common";

import { ADMIN_POLICY, ADMIN_STORE } from "../tokens.js";
import { AdminPolicyScopeRequest } from "../auth/admin-policy.service.js";
import { adminOperationSha256 } from "./admin-operation.service.js";
import { containsSensitiveAuditReason } from "./audit-reason.js";

const IDENTIFIER_PATTERN = /^[A-Za-z0-9][A-Za-z0-9._:@-]{0,127}$/;
const MAX_BREAKGLASS_TTL_MS = 15 * 60 * 1000;
const SENSITIVE_SUMMARY_KEY = /password|token|secret|private.?key|authorization|cookie|ticket/i;

type BreakglassActor = {
  adminId: number | string;
  subject: string;
};

function breakglassError(code: string, message = code, details: Record<string, unknown> = {}) {
  const error: any = new Error(message);
  error.code = code;
  Object.assign(error, details);
  return error;
}

function identifier(value: unknown, field: string) {
  const normalized = typeof value === "string" || typeof value === "number" ? String(value).trim() : "";
  if (!IDENTIFIER_PATTERN.test(normalized)) {
    throw breakglassError("ADMIN_BREAKGLASS_INPUT_INVALID", `${field} is invalid`, { field });
  }
  return normalized;
}

function requiredReason(value: unknown, field = "reason") {
  const normalized = typeof value === "string" ? value.trim() : "";
  if (!normalized || Buffer.byteLength(normalized, "utf8") > 512 || /[\u0000-\u001f\u007f]/.test(normalized)) {
    throw breakglassError("ADMIN_BREAKGLASS_INPUT_INVALID", `${field} is invalid`, { field });
  }
  if (containsSensitiveAuditReason(normalized)) {
    throw breakglassError("ADMIN_BREAKGLASS_SENSITIVE_REASON", `${field} contains a credential-like value`, { field });
  }
  return normalized;
}

function canonicalSummary(value: unknown, field: string, depth = 0): Record<string, unknown> | unknown[] | string | number | boolean | null {
  if (depth > 8) throw breakglassError("ADMIN_BREAKGLASS_INPUT_INVALID", `${field} is too deep`, { field });
  if (value === null || typeof value === "string" || typeof value === "boolean") return value as string | boolean | null;
  if (typeof value === "number") {
    if (!Number.isFinite(value)) throw breakglassError("ADMIN_BREAKGLASS_INPUT_INVALID", `${field} contains an invalid number`, { field });
    return value;
  }
  if (Array.isArray(value)) return value.map((entry) => canonicalSummary(entry, field, depth + 1));
  if (!value || typeof value !== "object" || Object.getPrototypeOf(value) !== Object.prototype) {
    throw breakglassError("ADMIN_BREAKGLASS_INPUT_INVALID", `${field} must be JSON-compatible`, { field });
  }
  return Object.fromEntries(
    Object.entries(value as Record<string, unknown>)
      .sort(([left], [right]) => left.localeCompare(right))
      .map(([key, entry]) => {
        if (SENSITIVE_SUMMARY_KEY.test(key)) {
          throw breakglassError("ADMIN_BREAKGLASS_SENSITIVE_SUMMARY", `${field} contains a sensitive key`, { field, key });
        }
        return [key, canonicalSummary(entry, field, depth + 1)];
      })
  );
}

function summary(value: unknown, field: string) {
  const normalized = canonicalSummary(value, field);
  if (!normalized || typeof normalized !== "object" || Array.isArray(normalized)) {
    throw breakglassError("ADMIN_BREAKGLASS_INPUT_INVALID", `${field} must be an object`, { field });
  }
  if (Buffer.byteLength(JSON.stringify(normalized), "utf8") > 16 * 1024) {
    throw breakglassError("ADMIN_BREAKGLASS_INPUT_INVALID", `${field} is too large`, { field });
  }
  return normalized as Record<string, unknown>;
}

function list(value: unknown, field: string, wildcardWhenEmpty = true) {
  if (value === undefined || value === null) return wildcardWhenEmpty ? ["*"] : [];
  if (!Array.isArray(value)) throw breakglassError("ADMIN_BREAKGLASS_INPUT_INVALID", `${field} must be an array`, { field });
  const values = [...new Set(value.map((entry) => identifier(entry, field)))].sort();
  return values.length > 0 ? values : wildcardWhenEmpty ? ["*"] : [];
}

function scopeConstraint(scope: AdminPolicyScopeRequest = {}) {
  const scalar = (value: unknown, field: string) => value === undefined || value === null || value === "" ? "*" : identifier(value, field);
  const targetIds = list(scope.targetIds, "targetIds");
  const targetCount = scope.targetCount === undefined ? Math.max(targetIds[0] === "*" ? 1 : targetIds.length, 1) : scope.targetCount;
  if (!Number.isSafeInteger(targetCount) || targetCount < 1 || targetCount > 10_000) {
    throw breakglassError("ADMIN_BREAKGLASS_INPUT_INVALID", "targetCount is invalid", { field: "targetCount" });
  }
  return {
    world_ids: [scalar(scope.worldId, "worldId")],
    service_names: [scalar(scope.serviceName, "serviceName")],
    instance_ids: [scalar(scope.instanceId, "instanceId")],
    field_allowlist: list(scope.fields, "fields"),
    target_types: [scalar(scope.targetType, "targetType")],
    target_ids: targetIds,
    max_targets: targetCount
  };
}

function listAllows(granted: unknown, requested: readonly string[]) {
  return Array.isArray(granted) && requested.every((value) => granted.includes("*") || granted.includes(value));
}

function requestedDimensions(scope: AdminPolicyScopeRequest) {
  const scalar = (value: unknown, field: string) => value === undefined || value === null || value === "" ? [] : [identifier(value, field)];
  return {
    world_ids: scalar(scope.worldId, "worldId"),
    service_names: scalar(scope.serviceName, "serviceName"),
    instance_ids: scalar(scope.instanceId, "instanceId"),
    field_allowlist: list(scope.fields, "fields", false),
    target_types: scalar(scope.targetType, "targetType"),
    target_ids: list(scope.targetIds, "targetIds", false),
    targetCount: scope.targetCount === undefined ? Math.max(Array.isArray(scope.targetIds) ? scope.targetIds.length : 0, 1) : scope.targetCount
  };
}

function grantMatchesScope(grant: any, scope: AdminPolicyScopeRequest, dimensions: readonly string[]) {
  const requested = requestedDimensions(scope);
  if (!Number.isSafeInteger(requested.targetCount) || requested.targetCount < 1 || requested.targetCount > Number(grant.scope?.max_targets || 0)) {
    return false;
  }
  return dimensions.every((dimension) => {
    const values = requested[dimension as keyof typeof requested];
    return Array.isArray(values) && values.length > 0 && listAllows(grant.scope?.[dimension], values);
  });
}

@Injectable()
export class AdminBreakglassService {
  constructor(
    @Inject(ADMIN_POLICY) private readonly policy: any,
    @Inject(ADMIN_STORE) private readonly adminStore: any
  ) {}

  async activate({
    actor,
    requestId,
    permission,
    scope,
    targetSummary,
    reason,
    ttlMs
  }: {
    actor: BreakglassActor;
    requestId: string;
    permission: string;
    scope: AdminPolicyScopeRequest;
    targetSummary: Record<string, unknown>;
    reason: string;
    ttlMs: number;
  }) {
    const actorAdminId = identifier(actor?.adminId, "actor id");
    const actorSubject = identifier(actor?.subject, "actor subject");
    const activationRequestId = identifier(requestId, "requestId");
    const permissionKey = identifier(permission, "permission");
    const activationReason = requiredReason(reason);
    if (!Number.isSafeInteger(ttlMs) || ttlMs < 1 || ttlMs > MAX_BREAKGLASS_TTL_MS) {
      throw breakglassError("ADMIN_BREAKGLASS_TTL_INVALID", "Break-glass TTL must be at most 15 minutes");
    }
    const activationDecision = await this.policy.authorize(actorAdminId, "breakglass.activate", scope);
    if (!activationDecision?.allowed) {
      throw breakglassError("ADMIN_BREAKGLASS_ACTIVATE_DENIED", "breakglass.activate permission is required", {
        policyCode: activationDecision?.code || "PERMISSION_DENIED"
      });
    }
    const permissionRecord = await this.adminStore.findAdminPolicyPermission(permissionKey);
    if (!permissionRecord || permissionRecord.active !== true || permissionRecord.risk_level !== "emergency" || permissionKey === "breakglass.activate") {
      throw breakglassError("ADMIN_BREAKGLASS_PERMISSION_INVALID", "Break-glass can grant only a distinct active emergency permission", { permissionKey });
    }
    const constrainedScope = scopeConstraint(scope);
    const normalizedTargetSummary = summary(targetSummary, "targetSummary");
    const scopeSha256 = adminOperationSha256(constrainedScope);
    const targetSha256 = adminOperationSha256(normalizedTargetSummary);
    const semanticSha256 = adminOperationSha256({
      actorAdminId,
      permissionKey,
      scopeSha256,
      targetSha256,
      reasonSha256: adminOperationSha256(activationReason),
      ttlMs
    });
    const grant = await this.adminStore.createAdminBreakglassGrant({
      grantId: randomUUID(),
      activationRequestId,
      actorAdminId,
      actorSubject,
      permissionKey,
      scope: constrainedScope,
      scopeSha256,
      targetSummary: normalizedTargetSummary,
      targetSha256,
      semanticSha256,
      reason: activationReason,
      expiresAt: new Date(Date.now() + ttlMs).toISOString()
    });
    if (grant.kind === "conflict") {
      throw breakglassError("ADMIN_BREAKGLASS_REQUEST_CONFLICT", "requestId is already bound to a different break-glass activation", { requestId: activationRequestId });
    }
    return grant;
  }

  async requireActiveGrant({
    actorAdminId,
    permission,
    scope,
    targetSummary
  }: {
    actorAdminId: number | string;
    permission: string;
    scope: AdminPolicyScopeRequest;
    targetSummary: Record<string, unknown>;
  }) {
    const normalizedActorId = identifier(actorAdminId, "actor id");
    const permissionKey = identifier(permission, "permission");
    const permissionRecord = await this.adminStore.findAdminPolicyPermission(permissionKey);
    if (!permissionRecord || permissionRecord.active !== true || permissionRecord.risk_level !== "emergency") {
      throw breakglassError("ADMIN_BREAKGLASS_PERMISSION_INVALID", "Operation is not an active emergency permission", { permissionKey });
    }
    const targetSha256 = adminOperationSha256(summary(targetSummary, "targetSummary"));
    const grants = await this.adminStore.listActiveAdminBreakglassGrants(normalizedActorId, permissionKey);
    const dimensions = Array.isArray(permissionRecord.scope_dimensions) ? permissionRecord.scope_dimensions : [];
    const grant = grants.find((candidate: any) => candidate.targetSha256 === targetSha256 && grantMatchesScope(candidate, scope, dimensions));
    if (!grant) {
      throw breakglassError("ADMIN_BREAKGLASS_GRANT_REQUIRED", "No active break-glass grant matches this action and target", { permissionKey });
    }
    return grant;
  }

  async revoke({ grantId, actor, reason }: { grantId: string; actor: BreakglassActor; reason: string }) {
    return this.adminStore.revokeAdminBreakglassGrant({
      grantId: identifier(grantId, "grantId"),
      revokedByAdminId: identifier(actor?.adminId, "actor id"),
      revokedBySubject: identifier(actor?.subject, "actor subject"),
      reason: requiredReason(reason, "revocation reason")
    });
  }
}
