import { Inject, Injectable, NestMiddleware } from "@nestjs/common";

import { getClientIp } from "./client-ip.js";
import { forbidden, serviceUnavailable } from "./http-exception.js";
import { log } from "../logger.js";
import { AUTH_BLOCKLIST, AUTH_CONFIG, AUTH_MYSQL_STORE } from "../tokens.js";

function logSecurity(level: string, message: string, extra: Record<string, unknown>) {
  try {
    log(level, message, extra);
  } catch {
    // Focused tests may instantiate middleware before logger bootstrap.
  }
}

function shouldCheckIp(req: any): boolean {
  const path = String(req.url || "").split("?")[0];
  if (path.startsWith("/api/v1/auth/")) {
    return true;
  }
  return path === "/api/v1/game-ticket/issue";
}

@Injectable()
export class RedisBlocklistMiddleware implements NestMiddleware {
  constructor(
    @Inject(AUTH_CONFIG) private readonly config: any,
    @Inject(AUTH_BLOCKLIST) private readonly blocklist: any,
    @Inject(AUTH_MYSQL_STORE) private readonly mysqlStore: any
  ) {}

  async use(req: any, _res: any, next: () => void) {
    if (!shouldCheckIp(req)) {
      next();
      return;
    }

    const clientIp = getClientIp(req, this.config);
    const decision = await this.blocklist.checkIp(clientIp);

    if (!decision.blocked) {
      next();
      return;
    }

    if (decision.unavailable) {
      logSecurity("warn", "security.blocklist_unavailable", {
        targetType: "ip",
        clientIp,
        path: req.url
      });
      await this.mysqlStore?.appendSecurityAudit?.({
        eventType: "blocklist_unavailable",
        targetType: "ip",
        targetValue: clientIp,
        clientIp,
        severity: "critical",
        details: { path: req.url }
      });
      throw serviceUnavailable("BLOCKLIST_UNAVAILABLE", "redis blocklist is unavailable");
    }

    logSecurity("warn", "security.ip_blocked", {
      clientIp,
      path: req.url
    });
    await this.mysqlStore?.appendSecurityAudit?.({
      eventType: "ip_blocked",
      targetType: "ip",
      targetValue: clientIp,
      clientIp,
      severity: "critical",
      details: { path: req.url }
    });
    throw forbidden("IP_BLOCKED", "client IP is blocked");
  }
}
