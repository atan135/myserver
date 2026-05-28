import { ArgumentsHost, Catch, ExceptionFilter, HttpException, HttpStatus } from "@nestjs/common";

import { log } from "../logger.js";

@Catch()
export class HttpExceptionFilter implements ExceptionFilter {
  catch(exception: unknown, host: ArgumentsHost) {
    const ctx = host.switchToHttp();
    const res = ctx.getResponse();

    if (exception instanceof HttpException) {
      const status = exception.getStatus();
      const response = exception.getResponse();
      if (typeof response === "object" && response !== null && "ok" in response) {
        return res.status(status).json(response);
      }

      if (status === HttpStatus.NOT_FOUND) {
        return res.status(status).json({
          ok: false,
          error: "NOT_FOUND"
        });
      }

      return res.status(status).json(response);
    }

    const error = exception as Error;
    log("error", "http.unhandled_error", { error: error?.message });
    return res.status(500).json({ ok: false, error: "INTERNAL_ERROR" });
  }
}
