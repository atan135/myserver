import { Body, Controller, Get, HttpCode, HttpStatus, Inject, Param, Post, Query, Req, UseGuards } from "@nestjs/common";
import { ApiBearerAuth, ApiTags } from "@nestjs/swagger";

import { JwtAuthGuard } from "../auth/jwt-auth.guard.js";
import { AdminPolicyGuard } from "../auth/admin-policy.guard.js";
import { Permissions, PolicyScopeResolver } from "../auth/roles.decorator.js";
import { ApiHttpException } from "../common/http-exception.js";
import { ADMIN_BREAKGLASS, ADMIN_OPERATIONS, ADMIN_POLICY, ADMIN_STORE } from "../tokens.js";
import { containsSensitiveAuditReason } from "./audit-reason.js";

function approvalError(error: any) {
  const code = typeof error?.code === "string" ? error.code : "ADMIN_OPERATION_APPROVAL_FAILED";
  const statusCode = code === "ADMIN_OPERATION_NOT_FOUND" ? 404
    : code === "ADMIN_OPERATION_SELF_APPROVAL_FORBIDDEN" ? 403
    : code === "ADMIN_OPERATION_STATE_CONFLICT" ? 409
      : 400;
  return new ApiHttpException(statusCode, {
    ok: false,
    error: code,
    message: "Operation approval was rejected"
  });
}

function requiredIdentifier(value: unknown, field: string) {
  const normalized = typeof value === "string" || typeof value === "number" ? String(value).trim() : "";
  if (!/^[A-Za-z0-9][A-Za-z0-9._:@-]{0,127}$/.test(normalized)) {
    throw new ApiHttpException(400, { ok: false, error: "ADMIN_BREAKGLASS_INPUT_INVALID", message: `${field} is invalid` });
  }
  return normalized;
}

function requiredTargetIds(body: any) {
  const raw = body?.targetIds ?? body?.target_ids ?? body?.targetId ?? body?.target_id;
  const values = Array.isArray(raw) ? raw : [raw];
  const ids = [...new Set(values.map((value) => requiredIdentifier(value, "targetId")))];
  if (ids.length === 0) {
    throw new ApiHttpException(400, { ok: false, error: "ADMIN_BREAKGLASS_INPUT_INVALID", message: "targetId is required" });
  }
  return ids;
}

function requiredReason(value: unknown) {
  const normalized = typeof value === "string" ? value.trim() : "";
  if (!normalized || Buffer.byteLength(normalized, "utf8") > 512 || /[\u0000-\u001f\u007f]/.test(normalized)) {
    throw new ApiHttpException(400, { ok: false, error: "ADMIN_BREAKGLASS_INPUT_INVALID", message: "reason is invalid" });
  }
  return normalized;
}

const SENSITIVE_EVIDENCE_KEY = /password|passwd|pwd|token|secret|api[-_]?key|private[-_]?key|authorization|cookie|session(?:[-_]?id)?|nonce|payload|assertion|credential/i;
const SENSITIVE_EVIDENCE_VALUE = /\b(?:password|passwd|pwd|token|secret|api[-_]?key|private[-_]?key|authorization|cookie|session(?:[-_]?id)?)\b\s*(?:=|:)|\b(?:bearer|basic)\s+[A-Za-z0-9._~+/=-]{8,}\b|-----BEGIN(?: [A-Z0-9]+)* PRIVATE KEY-----|\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b/i;

function approvalEvidenceSummary(value: unknown) {
  if (!value || typeof value !== "object" || Array.isArray(value) || Object.keys(value).length === 0) {
    throw Object.assign(new Error("Approval evidence summary is required"), {
      code: "ADMIN_OPERATION_APPROVAL_EVIDENCE_REQUIRED"
    });
  }
  const inspect = (entry: unknown, depth = 0) => {
    if (depth > 6) throw Object.assign(new Error("Approval evidence is too deep"), { code: "ADMIN_OPERATION_APPROVAL_EVIDENCE_INVALID" });
    if (typeof entry === "string") {
      if (!entry.trim() || SENSITIVE_EVIDENCE_VALUE.test(entry)) {
        throw Object.assign(new Error("Approval evidence contains a credential-like value"), { code: "ADMIN_OPERATION_APPROVAL_EVIDENCE_INVALID" });
      }
      return;
    }
    if (entry === null || typeof entry === "boolean" || typeof entry === "number") return;
    if (Array.isArray(entry)) {
      entry.forEach((item) => inspect(item, depth + 1));
      return;
    }
    if (!entry || typeof entry !== "object") {
      throw Object.assign(new Error("Approval evidence is invalid"), { code: "ADMIN_OPERATION_APPROVAL_EVIDENCE_INVALID" });
    }
    for (const [key, nested] of Object.entries(entry as Record<string, unknown>)) {
      if (SENSITIVE_EVIDENCE_KEY.test(key)) {
        throw Object.assign(new Error("Approval evidence contains a sensitive key"), { code: "ADMIN_OPERATION_APPROVAL_EVIDENCE_INVALID" });
      }
      inspect(nested, depth + 1);
    }
  };
  inspect(value);
  return value as Record<string, unknown>;
}

function pendingLimit(value: unknown) {
  if (value === undefined || value === null || value === "") return 50;
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < 1 || parsed > 100) {
    throw new ApiHttpException(400, { ok: false, error: "ADMIN_OPERATION_LIST_LIMIT_INVALID", message: "limit is invalid" });
  }
  return parsed;
}

const SENSITIVE_SUMMARY_KEY = /password|token|secret|private.?key|authorization|cookie|ticket|nonce|payload|assertion|endpoint|host|port|credential/i;

function safeOperationSummary(value: unknown, depth = 0): unknown {
  if (depth > 6) return "[TRUNCATED]";
  if (value === null || typeof value === "string" || typeof value === "boolean" || typeof value === "number") return value;
  if (Array.isArray(value)) return value.slice(0, 100).map((entry) => safeOperationSummary(entry, depth + 1));
  if (!value || typeof value !== "object") return "[REDACTED]";
  return Object.fromEntries(
    Object.entries(value as Record<string, unknown>)
      .slice(0, 100)
      .map(([key, entry]) => [key, SENSITIVE_SUMMARY_KEY.test(key) ? "[REDACTED]" : safeOperationSummary(entry, depth + 1)])
  );
}

function operationReadView(operation: any) {
  return {
    operationId: operation?.operationId || null,
    requestId: operation?.requestId || null,
    requester: {
      adminId: operation?.actorAdminId ?? null,
      subject: operation?.actorSubject || null
    },
    permissionKey: operation?.permissionKey || null,
    riskLevel: operation?.riskLevel || null,
    status: operation?.status || null,
    approvalStatus: operation?.approvalStatus || null,
    reason: containsSensitiveAuditReason(operation?.reason) ? "[REDACTED: potential credential]" : operation?.reason || null,
    targetSummary: safeOperationSummary(operation?.targetSummary || {}),
    impactSummary: safeOperationSummary(operation?.preview?.impactSummary || {}),
    preview: operation?.preview ? {
      expiresAt: operation.preview.expiresAt || null,
      consumedAt: operation.preview.consumedAt || null
    } : null,
    approval: operation?.approval ? {
      status: operation.approval.status || null,
      requestedAt: operation.approval.requestedAt || null,
      decidedAt: operation.approval.decidedAt || null,
      decidedByAdminId: operation.approval.decidedByAdminId ?? null,
      decidedBySubject: operation.approval.decidedBySubject || null,
      evidenceSummary: safeOperationSummary(operation.approval.evidenceSummary || {}),
      rejectionReason: operation.approval.rejectionReason || null
    } : null,
    resultSummary: operation?.resultSummary === null || operation?.resultSummary === undefined
      ? null
      : safeOperationSummary(operation.resultSummary),
    errorSummary: operation?.errorSummary === null || operation?.errorSummary === undefined
      ? null
      : safeOperationSummary(operation.errorSummary),
    createdAt: operation?.createdAt || null,
    updatedAt: operation?.updatedAt || null,
    completedAt: operation?.completedAt || null
  };
}

function operationApprovalPolicyScope(request: any) {
  const requestId = typeof request?.params?.requestId === "string" ? request.params.requestId.trim() : "";
  return { targetIds: requestId ? [requestId] : ["*"], targetCount: 1 };
}

@ApiTags("admin-operations")
@ApiBearerAuth()
@Controller("/api/v1/admin-operations")
export class AdminOperationController {
  constructor(
    @Inject(ADMIN_OPERATIONS) private readonly operations: any,
    @Inject(ADMIN_BREAKGLASS) private readonly breakglass: any,
    @Inject(ADMIN_STORE) private readonly adminStore: any,
    @Inject(ADMIN_POLICY) private readonly adminPolicy: any
  ) {}

  @Get("pending-approvals")
  @UseGuards(JwtAuthGuard)
  @Permissions("admin.permissions.manage")
  @HttpCode(HttpStatus.OK)
  async listPendingApprovals(@Query() query: any, @Req() req: any) {
    const operations = await this.adminStore.listPendingAdminOperations({ limit: pendingLimit(query?.limit) });
    const visible = [];
    for (const operation of operations) {
      const decision = await this.adminPolicy?.authorize?.(
        req.admin?.sub,
        "admin.permissions.manage",
        { targetIds: [operation.requestId], targetCount: 1 }
      );
      if (decision?.allowed) visible.push(operationReadView(operation));
    }
    return { ok: true, operations: visible };
  }

  @Get(":requestId")
  @UseGuards(JwtAuthGuard, AdminPolicyGuard)
  @Permissions("admin.permissions.manage")
  @PolicyScopeResolver(operationApprovalPolicyScope)
  @HttpCode(HttpStatus.OK)
  async getOperation(@Param("requestId") requestId: string) {
    const operation = await this.adminStore.getAdminOperationByRequestId(requestId);
    if (!operation) {
      throw new ApiHttpException(404, { ok: false, error: "ADMIN_OPERATION_NOT_FOUND", message: "Operation was not found" });
    }
    return { ok: true, operation: operationReadView(operation) };
  }

  @Post(":requestId/approval")
  @UseGuards(JwtAuthGuard, AdminPolicyGuard)
  @Permissions("admin.permissions.manage")
  @PolicyScopeResolver(operationApprovalPolicyScope)
  @HttpCode(HttpStatus.OK)
  async decideApproval(@Param("requestId") requestId: string, @Body() body: any, @Req() req: any) {
    try {
      const operation = await this.adminStore.getAdminOperationByRequestId(requestId);
      if (!operation) {
        throw Object.assign(new Error("Operation does not exist"), { code: "ADMIN_OPERATION_NOT_FOUND" });
      }
      if (String(operation.actorAdminId) === String(req.admin?.sub)) {
        throw Object.assign(new Error("Self approval is forbidden"), { code: "ADMIN_OPERATION_SELF_APPROVAL_FORBIDDEN" });
      }
      const decision = await this.operations.decideApproval({
        requestId,
        actor: {
          adminId: req.admin?.sub,
          subject: `admin:${String(req.admin?.sub ?? "").trim()}`
        },
        status: body?.status,
        evidenceSummary: approvalEvidenceSummary(body?.evidenceSummary ?? body?.evidence_summary),
        rejectionReason: body?.rejectionReason ?? body?.rejection_reason ?? null
      });
      return {
        ok: true,
        decision: decision.kind,
        operation: {
          operationId: decision.operation?.operationId || null,
          requestId: decision.operation?.requestId || null,
          status: decision.operation?.status || null,
          approvalStatus: decision.operation?.approvalStatus || null
        }
      };
    } catch (error: any) {
      throw approvalError(error);
    }
  }

  @Post("breakglass/activate")
  @UseGuards(JwtAuthGuard, AdminPolicyGuard)
  @Permissions("breakglass.activate")
  @HttpCode(HttpStatus.CREATED)
  async activateBreakglass(@Body() body: any, @Req() req: any) {
    try {
      const targetIds = requiredTargetIds(body);
      const targetType = requiredIdentifier(body?.targetType ?? body?.target_type, "targetType");
      const serviceName = requiredIdentifier(body?.serviceName ?? body?.service_name, "serviceName");
      const instanceId = body?.instanceId ?? body?.instance_id;
      const worldId = body?.worldId ?? body?.world_id;
      const permission = requiredIdentifier(body?.permission, "permission");
      const requestId = requiredIdentifier(body?.requestId ?? body?.request_id, "requestId");
      const normalizedInstanceId = instanceId === undefined || instanceId === null || instanceId === ""
        ? undefined
        : requiredIdentifier(instanceId, "instanceId");
      const normalizedWorldId = worldId === undefined || worldId === null || worldId === ""
        ? undefined
        : requiredIdentifier(worldId, "worldId");
      const targetSummary = {
        targetType,
        targetIds,
        serviceName,
        instanceId: normalizedInstanceId || null,
        worldId: normalizedWorldId || null
      };
      const activation = await this.breakglass.activate({
        actor: { adminId: req.admin?.sub, subject: `admin:${String(req.admin?.sub ?? "").trim()}` },
        requestId,
        permission,
        scope: {
          worldId: normalizedWorldId,
          serviceName,
          instanceId: normalizedInstanceId,
          targetType,
          targetIds,
          targetCount: targetIds.length
        },
        targetSummary,
        reason: requiredReason(body?.reason),
        ttlMs: body?.ttlMs ?? body?.ttl_ms ?? 300000
      });
      return {
        ok: true,
        state: activation.kind,
        grant: {
          grantId: activation.grant?.grantId || null,
          permission: activation.grant?.permissionKey || permission,
          expiresAt: activation.grant?.expiresAt || null
        }
      };
    } catch (error: any) {
      const code = typeof error?.code === "string" ? error.code : "ADMIN_BREAKGLASS_ACTIVATION_FAILED";
      throw new ApiHttpException(code === "ADMIN_BREAKGLASS_ACTIVATE_DENIED" ? 403 : 400, {
        ok: false,
        error: code,
        message: "Break-glass activation was rejected"
      });
    }
  }

  @Post("breakglass/:grantId/revoke")
  @UseGuards(JwtAuthGuard, AdminPolicyGuard)
  @Permissions("breakglass.activate")
  @HttpCode(HttpStatus.OK)
  async revokeBreakglass(@Param("grantId") grantId: string, @Body() body: any, @Req() req: any) {
    try {
      // grantId is the immutable activation record. The store appends the revocation audit event.
      const grant = await this.breakglass.revoke({
        grantId: requiredIdentifier(grantId, "grantId"),
        actor: { adminId: req.admin?.sub, subject: `admin:${String(req.admin?.sub ?? "").trim()}` },
        reason: requiredReason(body?.reason)
      });
      return { ok: true, grantId: grant.grantId, revokedAt: grant.revokedAt };
    } catch (error: any) {
      const code = typeof error?.code === "string" ? error.code : "ADMIN_BREAKGLASS_REVOCATION_FAILED";
      throw new ApiHttpException(code === "ADMIN_BREAKGLASS_GRANT_NOT_ACTIVE" ? 409 : 400, {
        ok: false,
        error: code,
        message: "Break-glass revocation was rejected"
      });
    }
  }
}
