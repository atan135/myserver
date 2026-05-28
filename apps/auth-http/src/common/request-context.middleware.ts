import crypto from "node:crypto";
import { Injectable, NestMiddleware } from "@nestjs/common";

import { log, requestContext } from "../logger.js";

@Injectable()
export class RequestContextMiddleware implements NestMiddleware {
  use(req: any, res: any, next: () => void) {
    const requestId = req.headers["x-request-id"] || crypto.randomBytes(8).toString("hex");
    req.requestId = requestId;
    res.setHeader("X-Request-Id", requestId);

    requestContext.run({ requestId }, () => {
      log("info", "http.request", {
        method: req.method,
        path: req.path
      });
      next();
    });
  }
}
