import { Injectable, NestMiddleware } from "@nestjs/common";

import { log } from "../logger.js";

@Injectable()
export class RequestLogMiddleware implements NestMiddleware {
  use(req: any, _res: any, next: () => void) {
    const pathname = String(req.url || "").split("?", 1)[0];
    const isMailNamespace = pathname === "/api/v1/mails" || pathname.startsWith("/api/v1/mails/");
    if (isMailNamespace) {
      // Player handlers emit a validated request-id and fixed route template after
      // authentication/limiting; do not create a second raw-path access record.
      next();
      return;
    }
    log("info", "http.request", {
      method: req.method,
      path: pathname
    });
    next();
  }
}
