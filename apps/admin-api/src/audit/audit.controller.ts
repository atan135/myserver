import { Controller, Get, Inject, Query, Req, UseGuards } from "@nestjs/common";
import { ApiBearerAuth, ApiTags } from "@nestjs/swagger";

import { JwtAuthGuard } from "../auth/jwt-auth.guard.js";
import { AdminPolicyGuard } from "../auth/admin-policy.guard.js";
import { Permissions } from "../auth/roles.decorator.js";
import { ApiHttpException } from "../common/http-exception.js";
import { ADMIN_STORE } from "../tokens.js";

const MAX_AUDIT_WINDOW_MS = 31 * 24 * 60 * 60 * 1000;
const MAX_AUDIT_EXPORT_WINDOW_MS = 7 * 24 * 60 * 60 * 1000;
const MAX_AUDIT_PAGE_SIZE = 100;
const MAX_AUDIT_EXPORT_ROWS = 5_000;
const IDENTIFIER = /^[A-Za-z0-9][A-Za-z0-9._:@-]{0,127}$/;
const RISK_LEVELS = new Set(["low", "medium", "high", "emergency"]);
const OPERATION_RESULTS = new Set(["succeeded", "failed", "execution_uncertain", "cancelled"]);

function pageLimit(value: any) {
  return Math.min(Number(value) || 50, 100);
}

function pageOffset(value: any) {
  return Number(value) || 0;
}

function auditQueryError(code: string) {
  return new ApiHttpException(400, {
    ok: false,
    error: code,
    message: "Audit query is invalid"
  });
}

function optionalIdentifier(value: unknown, field: string) {
  if (value === undefined || value === null || value === "") {
    return null;
  }
  const normalized = String(value).trim();
  if (!IDENTIFIER.test(normalized)) {
    throw auditQueryError(`AUDIT_${field}_INVALID`);
  }
  return normalized;
}

function optionalActor(value: unknown) {
  if (value === undefined || value === null || value === "") {
    return null;
  }
  const actor = Number(value);
  if (!Number.isSafeInteger(actor) || actor < 1) {
    throw auditQueryError("AUDIT_ACTOR_INVALID");
  }
  return actor;
}

function optionalTimestamp(value: unknown, field: string) {
  if (value === undefined || value === null || value === "") {
    return null;
  }
  if (typeof value !== "string" || value.length > 64) {
    throw auditQueryError(`AUDIT_${field}_INVALID`);
  }
  const parsed = new Date(value);
  if (Number.isNaN(parsed.getTime()) || !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,3})?(?:Z|[+-]\d{2}:\d{2})$/.test(value)) {
    throw auditQueryError(`AUDIT_${field}_INVALID`);
  }
  return parsed;
}

function decodeCursor(value: unknown) {
  if (value === undefined || value === null || value === "") {
    return null;
  }
  if (typeof value !== "string" || value.length > 256 || !/^[A-Za-z0-9_-]+$/.test(value)) {
    throw auditQueryError("AUDIT_CURSOR_INVALID");
  }
  try {
    const decoded = JSON.parse(Buffer.from(value, "base64url").toString("utf8"));
    const createdAt = optionalTimestamp(decoded?.createdAt, "CURSOR");
    const id = Number(decoded?.id);
    if (!createdAt || !Number.isSafeInteger(id) || id < 1) {
      throw new Error("invalid cursor");
    }
    return { createdAt: createdAt.toISOString(), id };
  } catch {
    throw auditQueryError("AUDIT_CURSOR_INVALID");
  }
}

function encodeCursor(event: any) {
  return Buffer.from(JSON.stringify({ createdAt: event.createdAt, id: event.id }), "utf8").toString("base64url");
}

function operationAuditQuery(query: any, { exporting = false } = {}) {
  const now = new Date();
  const to = optionalTimestamp(query?.to ?? query?.end_at, "TO") || now;
  const from = optionalTimestamp(query?.from ?? query?.start_at, "FROM") || new Date(to.getTime() - MAX_AUDIT_WINDOW_MS);
  const maxWindow = exporting ? MAX_AUDIT_EXPORT_WINDOW_MS : MAX_AUDIT_WINDOW_MS;
  if (from.getTime() >= to.getTime() || to.getTime() - from.getTime() > maxWindow) {
    throw auditQueryError(exporting ? "AUDIT_EXPORT_WINDOW_INVALID" : "AUDIT_TIME_WINDOW_INVALID");
  }
  const riskLevel = optionalIdentifier(query?.risk ?? query?.risk_level, "RISK");
  if (riskLevel && !RISK_LEVELS.has(riskLevel)) {
    throw auditQueryError("AUDIT_RISK_INVALID");
  }
  const result = optionalIdentifier(query?.result ?? query?.status, "RESULT");
  if (result && !OPERATION_RESULTS.has(result)) {
    throw auditQueryError("AUDIT_RESULT_INVALID");
  }
  const pageSize = Number(query?.limit ?? 50);
  if (!Number.isSafeInteger(pageSize) || pageSize < 1 || pageSize > MAX_AUDIT_PAGE_SIZE) {
    throw auditQueryError("AUDIT_LIMIT_INVALID");
  }
  return {
    from: from.toISOString(),
    to: to.toISOString(),
    cursor: decodeCursor(query?.cursor),
    limit: pageSize,
    actorAdminId: optionalActor(query?.actor ?? query?.admin_id ?? query?.actor_admin_id),
    permissionKey: optionalIdentifier(query?.permission, "PERMISSION"),
    eventType: optionalIdentifier(query?.event ?? query?.action, "EVENT"),
    target: optionalIdentifier(query?.target, "TARGET"),
    requestId: optionalIdentifier(query?.request_id ?? query?.requestId, "REQUEST_ID"),
    traceId: optionalIdentifier(query?.trace_id ?? query?.traceId, "TRACE_ID"),
    riskLevel,
    result
  };
}

@ApiTags("audit")
@ApiBearerAuth()
@UseGuards(JwtAuthGuard, AdminPolicyGuard)
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

  @Get("operation-audit-events")
  @Permissions("audit.read")
  async operationAuditEvents(@Query() query: any) {
    const filter = operationAuditQuery(query);
    const rows = await this.adminStore.listAdminOperationAuditEvents({
      ...filter,
      limit: filter.limit + 1
    });
    const hasMore = rows.length > filter.limit;
    const events = hasMore ? rows.slice(0, filter.limit) : rows;
    return {
      ok: true,
      events,
      from: filter.from,
      to: filter.to,
      limit: filter.limit,
      nextCursor: hasMore ? encodeCursor(events[events.length - 1]) : null
    };
  }

  @Get("operation-audit-events/export")
  @Permissions("audit.read")
  async exportOperationAuditEvents(@Query() query: any) {
    const filter = operationAuditQuery(query, { exporting: true });
    if (filter.cursor) {
      throw auditQueryError("AUDIT_EXPORT_CURSOR_FORBIDDEN");
    }
    const rows = await this.adminStore.listAdminOperationAuditEvents({
      ...filter,
      limit: MAX_AUDIT_EXPORT_ROWS + 1
    });
    if (rows.length > MAX_AUDIT_EXPORT_ROWS) {
      throw new ApiHttpException(413, {
        ok: false,
        error: "AUDIT_EXPORT_LIMIT_EXCEEDED",
        message: "Audit export exceeds the row limit"
      });
    }
    return {
      ok: true,
      events: rows,
      exported: rows.length,
      from: filter.from,
      to: filter.to
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
