import { ArgumentsHost, Catch, ExceptionFilter, HttpException, HttpStatus } from "@nestjs/common";

import { log } from "../logger.js";
import { publicMailRouteTemplate } from "../mails/public-mail-request.js";

function sendJson(response: any, status: number, body: Record<string, unknown>) {
  if (typeof response.status === "function") {
    response.status(status);
  } else if (typeof response.code === "function") {
    response.code(status);
  }

  return response.send(body);
}

const PUBLIC_ERROR_MESSAGES: Record<number, string> = {
  400: "Mail request is invalid",
  401: "Player ticket is invalid",
  403: "Mail request is not allowed",
  404: "Mail was not found",
  409: "Mail action cannot be completed in the current state",
  410: "Mail has expired",
  422: "Mail action cannot be completed",
  429: "Too many mail requests",
  503: "Mail service is temporarily unavailable"
};

function publicStatus(status: number) {
  return Object.hasOwn(PUBLIC_ERROR_MESSAGES, status) ? status : 503;
}

function stablePublicError(status: number, response: any) {
  if (status === 401) return "MAIL_PLAYER_TICKET_INVALID";
  if (status === 404) return "MAIL_NOT_FOUND";
  if (status === 429) return "MAIL_RATE_LIMITED";
  const candidate = typeof response?.error === "string" ? response.error : "";
  return /^[A-Z][A-Z0-9_]{0,127}$/.test(candidate)
    ? candidate
    : (status === 400 ? "INVALID_PLAYER_MAIL_REQUEST" : "MAIL_UNAVAILABLE");
}

function publicErrorResponse(status: number, response: any) {
  return {
    ok: false,
    error: stablePublicError(status, response),
    message: PUBLIC_ERROR_MESSAGES[status]
  };
}

@Catch()
export class HttpExceptionFilter implements ExceptionFilter {
  catch(exception: unknown, host: ArgumentsHost) {
    const ctx = host.switchToHttp();
    const req = ctx.getRequest();
    const res = ctx.getResponse();
    const isPublicMailRoute = Boolean(publicMailRouteTemplate(req?.url));

    if (exception instanceof HttpException) {
      const status = exception.getStatus();
      const response = exception.getResponse();
      if (isPublicMailRoute) {
        const sanitizedStatus = publicStatus(status);
        return sendJson(res, sanitizedStatus, publicErrorResponse(sanitizedStatus, response));
      }
      if (typeof response === "object" && response !== null && "ok" in response) {
        return sendJson(res, status, response as Record<string, unknown>);
      }

      if (status === HttpStatus.NOT_FOUND) {
        return sendJson(res, status, {
          ok: false,
          error: "NOT_FOUND",
          path: req.url
        });
      }

      return sendJson(res, status, typeof response === "object" ? response as Record<string, unknown> : { message: response });
    }

    const error = exception as Error;
    log("error", "http.unhandled_error", { error: error?.message });
    if (isPublicMailRoute) {
      return sendJson(res, 503, publicErrorResponse(503, null));
    }
    return sendJson(res, 500, { ok: false, error: "INTERNAL_ERROR" });
  }
}
