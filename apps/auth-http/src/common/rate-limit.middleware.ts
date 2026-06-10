import { Inject, Injectable, NestMiddleware } from "@nestjs/common";

import { rateLimited } from "./http-exception.js";
import { getClientIp } from "./client-ip.js";
import { AUTH_CONFIG, AUTH_MYSQL_STORE, AUTH_RATE_LIMITER } from "../tokens.js";

@Injectable()
export class RateLimitMiddleware implements NestMiddleware {
  constructor(
    @Inject(AUTH_CONFIG) private readonly config: any,
    @Inject(AUTH_RATE_LIMITER) private readonly rateLimiter: any,
    @Inject(AUTH_MYSQL_STORE) private readonly mysqlStore: any
  ) {}

  async use(req: any, res: any, next: () => void) {
    const clientIp = getClientIp(req, this.config);

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
