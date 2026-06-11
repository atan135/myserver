import { getClientIp } from "./client-ip.js";
import { log } from "../logger.js";

export type SecurityAuditSeverity = "info" | "warning" | "critical";

export interface SecurityAuditEvent {
  eventType: string;
  targetType?: string | null;
  targetValue?: string | null;
  severity?: SecurityAuditSeverity;
  clientIp?: string | null;
  details?: Record<string, unknown>;
}

export async function appendSecurityAuditLog(adminStore: any, event: SecurityAuditEvent): Promise<void> {
  if (typeof adminStore?.appendSecurityAuditLog !== "function") {
    return;
  }

  try {
    await adminStore.appendSecurityAuditLog(event);
  } catch (err: any) {
    log("warn", "admin_api.security_audit_write_failed", {
      eventType: event.eventType,
      reason: err?.code || err?.name || "unknown_error",
      message: err?.message
    });
  }
}

export function getSecurityAuditClientIp(req: any, config: any): string | null {
  return getClientIp(req, config);
}

function getRequestPath(req: any): string | null {
  const rawPath = req?.url || req?.raw?.url || null;
  if (!rawPath) {
    return null;
  }

  return String(rawPath).split(/[?#]/, 1)[0] || null;
}

export function getRequestAuditDetails(req: any, extra: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    method: req?.method || null,
    path: getRequestPath(req),
    ...extra
  };
}
