import { ArgumentsHost, Catch, ExceptionFilter, HttpException, HttpStatus } from "@nestjs/common";

import { log } from "../logger.js";

function sendJson(response: any, status: number, body: Record<string, unknown>) {
  if (typeof response.status === "function") {
    const reply = response.status(status);
    if (typeof reply?.send === "function") {
      return reply.send(body);
    }
  } else if (typeof response.code === "function") {
    const reply = response.code(status);
    if (typeof reply?.send === "function") {
      return reply.send(body);
    }
  }

  if (typeof response.send === "function") {
    return response.send(body);
  }

  response.statusCode = status;
  response.setHeader?.("content-type", "application/json");
  response.end?.(JSON.stringify(body));
  return response;
}

@Catch()
export class HttpExceptionFilter implements ExceptionFilter {
  catch(exception: unknown, host: ArgumentsHost) {
    const ctx = host.switchToHttp();
    const res = ctx.getResponse();

    if (exception instanceof HttpException) {
      const status = exception.getStatus();
      const response = exception.getResponse();
      if (typeof response === "object" && response !== null && "ok" in response) {
        return sendJson(res, status, response as Record<string, unknown>);
      }

      if (status === HttpStatus.NOT_FOUND) {
        return sendJson(res, status, {
          ok: false,
          error: "NOT_FOUND"
        });
      }

      return sendJson(res, status, typeof response === "object" ? response as Record<string, unknown> : { message: response });
    }

    const error = exception as Error;
    log("error", "http.unhandled_error", { error: error?.message });
    return sendJson(res, 500, { ok: false, error: "INTERNAL_ERROR" });
  }
}
