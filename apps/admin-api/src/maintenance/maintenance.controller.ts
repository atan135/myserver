import { Body, Controller, Get, HttpCode, HttpStatus, Inject, Post, Req, UseGuards } from "@nestjs/common";
import { ApiBearerAuth, ApiTags } from "@nestjs/swagger";

import { JwtAuthGuard } from "../auth/jwt-auth.guard.js";
import { Roles } from "../auth/roles.decorator.js";
import { RolesGuard } from "../auth/roles.guard.js";
import { getClientIp } from "../common/client-ip.js";
import { ADMIN_CONFIG, ADMIN_STORE } from "../tokens.js";

@ApiTags("maintenance")
@ApiBearerAuth()
@UseGuards(JwtAuthGuard, RolesGuard)
@Controller("/api/v1/maintenance")
export class MaintenanceController {
  constructor(
    @Inject(ADMIN_CONFIG) private readonly config: any,
    @Inject(ADMIN_STORE) private readonly adminStore: any
  ) {}

  @Get()
  @Roles("viewer", "operator", "admin")
  async getStatus() {
    const status = await this.adminStore.getMaintenanceStatus();
    return { ok: true, ...status };
  }

  @Post()
  @Roles("admin")
  @HttpCode(HttpStatus.OK)
  async setStatus(@Body() body: any, @Req() req: any) {
    const { enabled, reason } = body || {};

    await this.adminStore.setMaintenanceMode(enabled, reason || "");

    await this.adminStore.appendAuditLog({
      adminId: req.admin.sub,
      adminUsername: req.admin.username,
      action: enabled ? "maintenance_enabled" : "maintenance_disabled",
      targetType: "system",
      targetValue: "maintenance",
      details: { reason },
      ip: getClientIp(req, this.config)
    });

    return { ok: true, message: enabled ? "Maintenance mode enabled" : "Maintenance mode disabled" };
  }
}
