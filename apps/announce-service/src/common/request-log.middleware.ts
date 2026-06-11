import { Injectable, NestMiddleware } from "@nestjs/common";

import { log } from "../logger.js";

export function sanitizeRequestPath(url: unknown): string {
  const value = typeof url === "string" && url.length > 0 ? url : "/";
  const queryIndex = value.indexOf("?");
  return queryIndex >= 0 ? value.slice(0, queryIndex) || "/" : value;
}

@Injectable()
export class RequestLogMiddleware implements NestMiddleware {
  use(req: any, _res: any, next: () => void) {
    log("info", "http.request", {
      method: req.method,
      path: sanitizeRequestPath(req.url)
    });
    next();
  }
}
