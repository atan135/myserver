import { HttpException } from "@nestjs/common";

export class ApiHttpException extends HttpException {
  constructor(statusCode: number, body: Record<string, unknown>) {
    super(body, statusCode);
  }
}

export function badRequest(error: string, message?: string): ApiHttpException {
  return new ApiHttpException(400, { ok: false, error, message: message || error });
}

export function unauthorized(error: string, message?: string): ApiHttpException {
  return new ApiHttpException(401, { ok: false, error, message: message || error });
}

export function forbidden(error: string, message?: string): ApiHttpException {
  return new ApiHttpException(403, { ok: false, error, message: message || error });
}

export function notFound(error: string, message?: string): ApiHttpException {
  return new ApiHttpException(404, { ok: false, error, message: message || error });
}

export function conflict(error: string, message?: string): ApiHttpException {
  return new ApiHttpException(409, { ok: false, error, message: message || error });
}

export function gone(error: string, message?: string): ApiHttpException {
  return new ApiHttpException(410, { ok: false, error, message: message || error });
}

export function badGateway(error: string, message?: string): ApiHttpException {
  return new ApiHttpException(502, { ok: false, error, message: message || error });
}
