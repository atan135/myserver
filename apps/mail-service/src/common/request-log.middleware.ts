import { Injectable, NestMiddleware } from "@nestjs/common";

import { log } from "../logger.js";

@Injectable()
export class RequestLogMiddleware implements NestMiddleware {
  use(req: any, _res: any, next: () => void) {
    log("info", "http.request", {
      method: req.method,
      path: req.url
    });
    next();
  }
}
