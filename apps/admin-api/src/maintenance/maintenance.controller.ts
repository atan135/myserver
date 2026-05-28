import { Body, Controller, Get, HttpCode, HttpStatus, Inject, Post, Req, UseGuards } from "@nestjs/common";
import { ApiBearerAuth, ApiTags } from "@nestjs/swagger";

import { JwtAuthGuard } from "../auth/jwt-auth.guard.js";
import { ADMIN_STORE } from "../tokens.js";

function getClientIp(req: any): string | null {
  const forwardedFor = req.headers["x-forwarded-for"];
  if (typeof forwardedFor === "string" && forwardedFor.length > 0) {
    return forwardedFor.split(",")[0].trim();
  }
  return req.socket.remoteAddress || null;
}

@ApiTags("maintenance")
@ApiBearerAuth()
@UseGuards(JwtAuthGuard)
@Controller("/api/v1/maintenance")
export class MaintenanceController {
  constructor(@Inject(ADMIN_STORE) private readonly adminStore: any) {}

  @Get()
  async getStatus() {
    const status = await this.adminStore.getMaintenanceStatus();
    return { ok: true, ...status };
  }

  @Post()
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
      ip: getClientIp(req)
    });

    return { ok: true, message: enabled ? "Maintenance mode enabled" : "Maintenance mode disabled" };
  }
}
