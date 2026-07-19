import { Body, Controller, Get, HttpCode, HttpStatus, Inject, Post, Req, UseGuards } from "@nestjs/common";
import { ApiBearerAuth, ApiTags } from "@nestjs/swagger";

import { JwtAuthGuard } from "../auth/jwt-auth.guard.js";
import { AdminPolicyGuard } from "../auth/admin-policy.guard.js";
import { Permissions } from "../auth/roles.decorator.js";
import { getClientIp } from "../common/client-ip.js";
import { badRequest } from "../common/http-exception.js";
import { ADMIN_CONFIG, ADMIN_STORE } from "../tokens.js";

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
    @Inject(ADMIN_STORE) private readonly adminStore: any
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
  }
}
