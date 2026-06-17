import { Inject, Injectable, NestMiddleware } from "@nestjs/common";

import { isRequestSecure, getClientIp } from "./client-ip.js";
import { ApiHttpException } from "./http-exception.js";
import { log } from "../logger.js";
import { AUTH_CONFIG, AUTH_DB_STORE } from "../tokens.js";

function logSecurity(level: string, message: string, extra: Record<string, unknown>) {
  try {
    log(level, message, extra);
  } catch {
    // Focused tests may instantiate middleware before logger bootstrap.
  }
}

@Injectable()
export class TlsRequiredMiddleware implements NestMiddleware {
  constructor(
    @Inject(AUTH_CONFIG) private readonly config: any,
    @Inject(AUTH_DB_STORE) private readonly dbStore: any
  ) {}

  async use(req: any, _res: any, next: () => void) {
    if (!this.config.authRequireTls || isRequestSecure(req, this.config)) {
      next();
      return;
    }

    const clientIp = getClientIp(req, this.config);
    logSecurity("warn", "security.auth_tls_required", {
      method: req.method,
      path: req.url,
      clientIp
    });
    await this.dbStore?.appendSecurityAudit?.({
      eventType: "auth_tls_required",
      targetType: "ip",
      targetValue: clientIp,
      clientIp,
      severity: "warning",
      details: { method: req.method, path: req.url }
    });

    throw new ApiHttpException(426, {
      ok: false,
      error: "AUTH_TLS_REQUIRED",
      message: "HTTPS is required"
    });
  }
}
