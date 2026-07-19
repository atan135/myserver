import {
  createHash,
  createPrivateKey,
  randomUUID,
  sign
} from "node:crypto";

import { Inject, Injectable } from "@nestjs/common";

import { ADMIN_CONFIG, ADMIN_POLICY } from "../tokens.js";
import { AdminPolicyScopeRequest } from "./admin-policy.service.js";

const ASSERTION_VERSION = 1;
const DEFAULT_TTL_MS = 60_000;
const MAX_TTL_MS = 300_000;
const IDENTIFIER_PATTERN = /^[A-Za-z0-9][A-Za-z0-9._:@-]{0,127}$/;

export type AssertionTarget = {
  targetType: string;
  targetIds?: readonly string[];
  worldId?: string | null;
};

export type AdminOperationAssertionContext = {
  actorId: number | string;
  permission: string;
  scope: AdminPolicyScopeRequest;
  target: AssertionTarget;
  operationId?: string;
  requestId?: string;
  traceId?: string;
};

type SignedAssertion = {
  version: number;
  operationId: string;
  requestId: string;
  traceId: string;
  issuer: string;
  keyId: string;
  actorId: string;
  permission: string;
  scope: Record<string, unknown>;
  target: Record<string, unknown>;
  service: string;
  instanceId: string;
  issuedAtMs: number;
  expiresAtMs: number;
  payloadSha256: string;
  signature: string;
};

function requireIdentifier(value: unknown, field: string): string {
  const normalized = typeof value === "string" || typeof value === "number" ? String(value).trim() : "";
  if (!IDENTIFIER_PATTERN.test(normalized)) {
    const error: any = new Error(`${field} is invalid`);
    error.code = "ADMIN_ASSERTION_INVALID";
    throw error;
  }
  return normalized;
}

function normalizeTargetIds(value: readonly string[] | undefined): string[] {
  const ids = [...new Set((value || []).map((item) => requireIdentifier(item, "target id")))];
  return ids.length > 0 ? ids : ["*"];
}

function normalizeFields(value: readonly string[] | undefined): string[] {
  const fields = [...new Set((value || ["*"]).map((item) => String(item).trim()).filter(Boolean))];
  return fields.length > 0 ? fields : ["*"];
}

function canonicalValue(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(canonicalValue);
  if (value && typeof value === "object") {
    return Object.fromEntries(
      Object.entries(value as Record<string, unknown>)
        .sort(([left], [right]) => left.localeCompare(right))
        .map(([key, entry]) => [key, canonicalValue(entry)])
    );
  }
  return value;
}

export function canonicalAdminOperationAssertionPayload(assertion: Omit<SignedAssertion, "signature">): Buffer {
  const fields = [
    assertion.version,
    assertion.operationId,
    assertion.requestId,
    assertion.traceId,
    assertion.issuer,
    assertion.keyId,
    assertion.actorId,
    assertion.permission,
    canonicalValue(assertion.scope),
    canonicalValue(assertion.target),
    assertion.service,
    assertion.instanceId,
    assertion.issuedAtMs,
    assertion.expiresAtMs,
    assertion.payloadSha256
  ];
  return Buffer.from(JSON.stringify(fields), "utf8");
}

function policyError(code: string) {
  const error: any = new Error("Admin operation is not authorized for the requested downstream target");
  error.code = code === "SCOPE_DENIED" || code === "SCOPE_REQUIRED"
    ? "ADMIN_ASSERTION_SCOPE_DENIED"
    : "ADMIN_ASSERTION_PERMISSION_DENIED";
  return error;
}

@Injectable()
export class AdminOperationAssertionService {
  constructor(
    @Inject(ADMIN_CONFIG) private readonly config: any,
    @Inject(ADMIN_POLICY) private readonly policy: any
  ) {}

  async issue(
    context: AdminOperationAssertionContext,
    service: string,
    instanceId: string,
    payload: Buffer | Uint8Array
  ): Promise<SignedAssertion> {
    const actorId = requireIdentifier(context?.actorId, "actor id");
    const permission = requireIdentifier(context?.permission, "permission");
    const targetType = requireIdentifier(context?.target?.targetType, "target type");
    const targetIds = normalizeTargetIds(context?.target?.targetIds);
    const normalizedService = requireIdentifier(service, "service");
    const normalizedInstanceId = requireIdentifier(instanceId, "instance id");
    const worldId = context?.target?.worldId ?? context?.scope?.worldId ?? "*";
    const effectiveScope: AdminPolicyScopeRequest = {
      ...context.scope,
      worldId: String(worldId || "*").trim() || "*",
      serviceName: normalizedService,
      instanceId: normalizedInstanceId,
      fields: normalizeFields(context?.scope?.fields),
      targetType,
      targetIds,
      targetCount: targetIds.length
    };
    const decision = await this.policy.authorize(actorId, permission, effectiveScope);
    if (!decision?.allowed) {
      throw policyError(decision?.code || "PERMISSION_DENIED");
    }

    const privateKeyPem = String(this.config.adminAssertionPrivateKey || "").trim();
    if (!privateKeyPem) {
      const error: any = new Error("ADMIN_ASSERTION_PRIVATE_KEY is required for internal admin writes");
      error.code = "ADMIN_ASSERTION_SIGNING_KEY_REQUIRED";
      throw error;
    }

    const ttlMs = Number(this.config.adminAssertionTtlMs || DEFAULT_TTL_MS);
    const issuedAtMs = Date.now();
    const expiresAtMs = issuedAtMs + Math.min(Math.max(ttlMs, 1), MAX_TTL_MS);
    const scope = {
      worldIds: [effectiveScope.worldId || "*"],
      serviceNames: [normalizedService],
      instanceIds: [normalizedInstanceId],
      fieldAllowlist: normalizeFields(effectiveScope.fields),
      targetTypes: [targetType],
      targetIds,
      maxTargets: targetIds.length
    };
    const target = {
      service: normalizedService,
      instanceId: normalizedInstanceId,
      worldId: effectiveScope.worldId || "*",
      targetType,
      targetIds
    };
    const unsigned = {
      version: ASSERTION_VERSION,
      operationId: requireIdentifier(context.operationId || `op-${randomUUID()}`, "operation id"),
      requestId: requireIdentifier(context.requestId || `req-${randomUUID()}`, "request id"),
      traceId: requireIdentifier(context.traceId || `trace-${randomUUID()}`, "trace id"),
      issuer: requireIdentifier(this.config.adminAssertionIssuer || "admin-api", "issuer"),
      keyId: requireIdentifier(this.config.adminAssertionKeyId || "admin-api-v1", "key id"),
      actorId,
      permission,
      scope,
      target,
      service: normalizedService,
      instanceId: normalizedInstanceId,
      issuedAtMs,
      expiresAtMs,
      payloadSha256: createHash("sha256").update(payload).digest("base64url")
    };
    const signature = sign(null, canonicalAdminOperationAssertionPayload(unsigned), createPrivateKey(privateKeyPem))
      .toString("base64url");
    return { ...unsigned, signature };
  }
}
