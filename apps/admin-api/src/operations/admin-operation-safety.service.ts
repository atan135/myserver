import { createHash } from "node:crypto";

import { Inject, Injectable } from "@nestjs/common";

import { ADMIN_CONFIG, ADMIN_REDIS, ADMIN_STORE } from "../tokens.js";
import { AdminPolicyScopeRequest } from "../auth/admin-policy.service.js";

const RATE_LIMIT_SCRIPT = `
local request_state = redis.call('GET', KEYS[2])
if request_state == 'allow' then
  return 0
end
if request_state == 'deny' then
  return -1
end
local count = redis.call('INCR', KEYS[1])
if count == 1 then
  redis.call('PEXPIRE', KEYS[1], ARGV[1])
end
if count <= tonumber(ARGV[3]) then
  redis.call('SET', KEYS[2], 'allow', 'PX', ARGV[2])
else
  local remaining_ttl = redis.call('PTTL', KEYS[1])
  if remaining_ttl < 1 then
    remaining_ttl = tonumber(ARGV[1])
  end
  redis.call('SET', KEYS[2], 'deny', 'PX', remaining_ttl)
end
return count
`;
const REQUEST_PERMIT_TTL_MS = 24 * 60 * 60 * 1000;

function safetyError(code: string, message = code) {
  const error: any = new Error(message);
  error.code = code;
  return error;
}

function identifier(value: unknown) {
  return typeof value === "string" || typeof value === "number" ? String(value).trim() : "";
}

function scopeFingerprint(scope: AdminPolicyScopeRequest) {
  const normalized = JSON.stringify({
    worldId: scope?.worldId ?? null,
    serviceName: scope?.serviceName ?? null,
    instanceId: scope?.instanceId ?? null,
    targetType: scope?.targetType ?? null,
    targetIds: Array.isArray(scope?.targetIds) ? [...scope.targetIds].map(String).sort() : [],
    targetCount: scope?.targetCount ?? null
  });
  return createHash("sha256").update(normalized).digest("hex").slice(0, 24);
}

function safeErrorCode(error: any) {
  return typeof error?.code === "string" ? error.code : "UNKNOWN_ERROR";
}

@Injectable()
export class AdminOperationSafetyService {
  private readonly windowMs: number;
  private readonly limit: number;

  constructor(
    @Inject(ADMIN_REDIS) private readonly redis: any,
    @Inject(ADMIN_STORE) private readonly adminStore: any,
    @Inject(ADMIN_CONFIG) config: any
  ) {
    this.windowMs = Number(config?.adminOperationRateLimitWindowMs ?? 60_000);
    this.limit = Number(config?.adminOperationRateLimitMax ?? 20);
  }

  async enforceExecutionRateLimit({
    actorAdminId,
    permission,
    scope,
    requestId
  }: {
    actorAdminId: number | string;
    permission: string;
    scope: AdminPolicyScopeRequest;
    requestId: string;
  }) {
    const actor = identifier(actorAdminId);
    const permissionKey = identifier(permission);
    const normalizedRequestId = identifier(requestId);
    if (!actor || !permissionKey || !normalizedRequestId || !this.redis || typeof this.redis.eval !== "function") {
      throw safetyError("ADMIN_RATE_LIMIT_DEPENDENCY_UNAVAILABLE", "Operation rate limiting is unavailable");
    }

    const fingerprint = scopeFingerprint(scope);
    const key = `admin-operation-rate:${actor}:${permissionKey}:${fingerprint}`;
    const requestPermitKey = `admin-operation-rate-request:${actor}:${permissionKey}:${fingerprint}:${normalizedRequestId}`;
    let count: number;
    try {
      const result = await this.redis.eval(
        RATE_LIMIT_SCRIPT,
        2,
        key,
        requestPermitKey,
        String(this.windowMs),
        String(REQUEST_PERMIT_TTL_MS),
        String(this.limit)
      );
      count = Number(result);
    } catch (error: any) {
      throw safetyError("ADMIN_RATE_LIMIT_DEPENDENCY_UNAVAILABLE", safeErrorCode(error));
    }
    if (!Number.isSafeInteger(count) || count < -1) {
      throw safetyError("ADMIN_RATE_LIMIT_DEPENDENCY_UNAVAILABLE", "Rate limiter returned an invalid count");
    }
    if (count === 0) return { count, limit: this.limit, windowMs: this.windowMs, duplicate: true };
    if (count === -1) {
      throw safetyError("ADMIN_OPERATION_RATE_LIMITED", "Operation rate limit exceeded");
    }
    if (count <= this.limit) {
      return { count, limit: this.limit, windowMs: this.windowMs };
    }

    await this.recordSecurityEvent({
      eventType: "admin_operation_rate_limited",
      severity: "critical",
      actorAdminId: actor,
      permission: permissionKey,
      scope,
      requestId: normalizedRequestId,
      details: { count, limit: this.limit, windowMs: this.windowMs }
    });
    throw safetyError("ADMIN_OPERATION_RATE_LIMITED", "Operation rate limit exceeded");
  }

  async recordSecurityEvent({
    eventType,
    severity = "critical",
    actorAdminId = null,
    permission = null,
    scope = {},
    requestId = null,
    details = {}
  }: {
    eventType: string;
    severity?: "info" | "warning" | "critical";
    actorAdminId?: number | string | null;
    permission?: string | null;
    scope?: AdminPolicyScopeRequest;
    requestId?: string | null;
    details?: Record<string, unknown>;
  }) {
    if (typeof this.adminStore?.appendSecurityAuditLog !== "function") {
      throw safetyError("ADMIN_SECURITY_AUDIT_UNAVAILABLE", "Security audit storage is unavailable");
    }
    try {
      await this.adminStore.appendSecurityAuditLog({
        eventType,
        targetType: permission ? "admin_operation" : "admin",
        targetValue: permission || (actorAdminId === null ? null : String(actorAdminId)),
        severity,
        details: {
          actorAdminId: actorAdminId === null ? null : String(actorAdminId),
          permission,
          requestId,
          scopeFingerprint: scopeFingerprint(scope),
          ...details
        }
      });
    } catch (error: any) {
      throw safetyError("ADMIN_SECURITY_AUDIT_UNAVAILABLE", safeErrorCode(error));
    }
  }
}
