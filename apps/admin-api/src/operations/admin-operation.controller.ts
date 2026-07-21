import { Body, Controller, HttpCode, HttpStatus, Inject, Param, Post, Req, UseGuards } from "@nestjs/common";
import { ApiBearerAuth, ApiTags } from "@nestjs/swagger";

import { JwtAuthGuard } from "../auth/jwt-auth.guard.js";
import { AdminPolicyGuard } from "../auth/admin-policy.guard.js";
import { Permissions } from "../auth/roles.decorator.js";
import { ApiHttpException } from "../common/http-exception.js";
import { ADMIN_BREAKGLASS, ADMIN_OPERATIONS, ADMIN_STORE } from "../tokens.js";

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

@ApiTags("admin-operations")
@ApiBearerAuth()
@UseGuards(JwtAuthGuard, AdminPolicyGuard)
@Controller("/api/v1/admin-operations")
export class AdminOperationController {
  constructor(
    @Inject(ADMIN_OPERATIONS) private readonly operations: any,
    @Inject(ADMIN_BREAKGLASS) private readonly breakglass: any,
    @Inject(ADMIN_STORE) private readonly adminStore: any
  ) {}

  @Post(":requestId/approval")
  @Permissions("admin.permissions.manage")
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
        evidenceSummary: body?.evidenceSummary ?? body?.evidence_summary ?? {},
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
