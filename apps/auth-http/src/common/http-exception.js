import { HttpException } from "@nestjs/common";

export class ApiHttpException extends HttpException {
  constructor(statusCode, body) {
    super(body, statusCode);
  }
}

export function badRequest(error, message) {
  return new ApiHttpException(400, { ok: false, error, message });
}

export function unauthorized(error = "UNAUTHORIZED") {
  return new ApiHttpException(401, { ok: false, error });
}

export function forbidden(error = "FORBIDDEN", message) {
  return new ApiHttpException(403, { ok: false, error, message });
}

export function rateLimited(error = "RATE_LIMITED", message) {
  return new ApiHttpException(429, { ok: false, error, message });
}

export function serviceUnavailable(error = "SERVICE_UNAVAILABLE", message, extra = {}) {
  return new ApiHttpException(503, { ok: false, error, message, ...extra });
}

export function notFound(path) {
  return new ApiHttpException(404, { ok: false, error: "NOT_FOUND", path });
}
