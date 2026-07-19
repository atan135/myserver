import { Body, Controller, Get, HttpCode, HttpStatus, Inject, Post, Req, UseGuards } from "@nestjs/common";
import { ApiBearerAuth, ApiTags } from "@nestjs/swagger";

import { JwtAuthGuard } from "../auth/jwt-auth.guard.js";
import { AdminPolicyGuard } from "../auth/admin-policy.guard.js";
import { Permissions } from "../auth/roles.decorator.js";
import { getClientIp } from "../common/client-ip.js";
import { ApiHttpException, badRequest } from "../common/http-exception.js";
import { ADMIN_CONFIG, ADMIN_HIGH_RISK_OPERATIONS, ADMIN_STORE } from "../tokens.js";

function normalizeReason(value: unknown): string | null {
  if (value === undefined || value === null) {
    return null;
  }
  if (typeof value !== "string") {
    throw badRequest("INVALID_MAINTENANCE_REASON", "reason must be a string");
  }

  const reason = value.trim();
  if (reason.length === 0) {
    return null;
  }
  if (reason.length > 512) {
    throw badRequest("INVALID_MAINTENANCE_REASON", "reason must be 512 characters or fewer");
  }
  return reason;
}

@ApiTags("maintenance")
@ApiBearerAuth()
@UseGuards(JwtAuthGuard, AdminPolicyGuard)
@Controller("/api/v1/maintenance")
export class MaintenanceController {
  constructor(
    @Inject(ADMIN_CONFIG) private readonly config: any,
    @Inject(ADMIN_STORE) private readonly adminStore: any,
    @Inject(ADMIN_HIGH_RISK_OPERATIONS) private readonly highRiskOperations: any
  ) {}

  @Get()
  @Permissions("maintenance.read")
  async getStatus() {
    const status = await this.adminStore.getMaintenanceStatus();
    return { ok: true, ...status };
  }

  @Post()
  @Permissions("maintenance.write")
  @HttpCode(HttpStatus.OK)
  async setStatus(@Body() body: any, @Req() req: any) {
    const { enabled, reason } = body || {};
    if (typeof enabled !== "boolean") {
      throw badRequest("INVALID_MAINTENANCE_ENABLED", "enabled must be a boolean");
    }

    const normalizedReason = normalizeReason(reason);
    if (typeof this.highRiskOperations?.run !== "function") {
      throw new ApiHttpException(503, {
        ok: false,
        error: "ADMIN_OPERATION_SERVICE_UNAVAILABLE",
        message: "High-risk operation service is unavailable"
      });
    }
    const outcome = await this.highRiskOperations.run({
      request: req,
      permission: "maintenance.write",
      scope: { serviceName: "control-plane", targetType: "maintenance", targetIds: ["global"], targetCount: 1 },
      targetSummary: { targetType: "maintenance", targetIds: ["global"] },
      payload: { enabled },
      impactSummary: { targetType: "maintenance", targetCount: 1, nextState: enabled ? "enabled" : "disabled" },
      reason: normalizedReason,
      execute: async () => {
        const updatedAt = new Date().toISOString();
        const updatedBy = req.admin.username || String(req.admin.sub);
        const status = await this.adminStore.setMaintenanceMode(enabled, {
          reason: normalizedReason,
          updatedAt,
          updatedBy
        });

        await this.adminStore.appendAuditLog({
          adminId: req.admin.sub,
          adminUsername: req.admin.username,
          action: enabled ? "maintenance_enabled" : "maintenance_disabled",
          targetType: "system",
          targetValue: "maintenance",
          details: { reason: normalizedReason },
          ip: getClientIp(req, this.config)
        });

        return {
          ok: true,
          message: enabled ? "Maintenance mode enabled" : "Maintenance mode disabled",
          ...status
        };
      },
      resultSummary: () => ({ action: "maintenance.write", outcome: "succeeded" })
    });
    return outcome.state === "executed" ? outcome.result : outcome.response;
  }
}
