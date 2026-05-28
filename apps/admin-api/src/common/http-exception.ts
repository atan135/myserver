import { HttpException } from "@nestjs/common";

export class ApiHttpException extends HttpException {
  constructor(statusCode: number, body: Record<string, unknown>) {
    super(body, statusCode);
  }
}

export function badRequest(error: string, message?: string): ApiHttpException {
  return new ApiHttpException(400, { ok: false, error, message });
}

export function unauthorized(error = "UNAUTHORIZED", message?: string): ApiHttpException {
  return new ApiHttpException(401, { ok: false, error, message });
}

export function forbidden(error = "FORBIDDEN", message?: string): ApiHttpException {
  return new ApiHttpException(403, { ok: false, error, message });
}

export function notFound(error = "NOT_FOUND", message?: string): ApiHttpException {
  return new ApiHttpException(404, { ok: false, error, message });
}
