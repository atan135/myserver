import { Inject, Injectable, NestMiddleware } from "@nestjs/common";

import { rateLimited } from "./http-exception.js";
import { AUTH_CONFIG, AUTH_MYSQL_STORE, AUTH_RATE_LIMITER } from "../tokens.js";

function getClientIp(req: any): string | null {
  const forwardedFor = req.headers["x-forwarded-for"];
  if (typeof forwardedFor === "string" && forwardedFor.length > 0) {
    return forwardedFor.split(",")[0].trim();
  }

  return req.ip || req.socket?.remoteAddress || null;
}

@Injectable()
export class RateLimitMiddleware implements NestMiddleware {
  constructor(
    @Inject(AUTH_CONFIG) private readonly config: any,
    @Inject(AUTH_RATE_LIMITER) private readonly rateLimiter: any,
    @Inject(AUTH_MYSQL_STORE) private readonly mysqlStore: any
  ) {}

  async use(req: any, res: any, next: () => void) {
    const clientIp = getClientIp(req);

    if (this.config.ratelimitEnabled && this.rateLimiter) {
      const { limited, retryAfterSeconds } = await this.rateLimiter.isIpRateLimited(clientIp);
      if (limited) {
        this.mysqlStore?.appendSecurityAudit?.({
          eventType: "ip_rate_limited",
          targetType: "ip",
          targetValue: clientIp,
          clientIp,
          severity: "warning",
          details: { path: req.url, retryAfterSeconds }
        });

        if (typeof res.setHeader === "function") {
          res.setHeader("Retry-After", String(retryAfterSeconds));
        } else {
          res.header("Retry-After", String(retryAfterSeconds));
        }
        throw rateLimited("IP_RATE_LIMITED", "Too many requests from this IP");
      }
    }

    next();
  }
}
