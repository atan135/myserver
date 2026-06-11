import { Controller, Get, Inject, Query, Req, UseGuards } from "@nestjs/common";
import { ApiBearerAuth, ApiTags } from "@nestjs/swagger";

import { JwtAuthGuard } from "../auth/jwt-auth.guard.js";
import { Permissions } from "../auth/roles.decorator.js";
import { RolesGuard } from "../auth/roles.guard.js";
import { ADMIN_STORE } from "../tokens.js";

function pageLimit(value: any) {
  return Math.min(Number(value) || 50, 100);
}

function pageOffset(value: any) {
  return Number(value) || 0;
}

@ApiTags("audit")
@ApiBearerAuth()
@UseGuards(JwtAuthGuard, RolesGuard)
@Controller("/api/v1")
export class AuditController {
  constructor(@Inject(ADMIN_STORE) private readonly adminStore: any) {}

  @Get("audit-logs")
  @Permissions("audit.read")
  async auditLogs(@Query() query: any) {
    const { limit = 50, offset = 0, action, target_type } = query;
    const logs = await this.adminStore.getAuditLogs({
      limit: pageLimit(limit),
      offset: pageOffset(offset),
      action,
      targetType: target_type
    });

    const total = await this.adminStore.countAuditLogs({ action, targetType: target_type });

    return {
      ok: true,
      logs,
      total,
      limit: pageLimit(limit),
      offset: pageOffset(offset)
    };
  }

  @Get("security-logs")
  @Permissions("security.read")
  async securityLogs(@Query() query: any, @Req() _req: any) {
    const { limit = 50, offset = 0, event_type, target_type, severity, client_ip } = query;
    const logs = await this.adminStore.getSecurityLogs({
      limit: pageLimit(limit),
      offset: pageOffset(offset),
      eventType: event_type,
      targetType: target_type,
      severity,
      clientIp: client_ip
    });

    const total = await this.adminStore.countSecurityLogs({
      eventType: event_type,
      targetType: target_type,
      severity,
      clientIp: client_ip
    });

    return {
      ok: true,
      logs,
      total,
      limit: pageLimit(limit),
      offset: pageOffset(offset)
    };
  }
}
