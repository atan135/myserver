import { HttpException } from "@nestjs/common";

export class ApiHttpException extends HttpException {
  constructor(statusCode: number, body: Record<string, unknown>) {
    super(body, statusCode);
  }
}

export function badRequest(error: string, message?: string): ApiHttpException {
  return new ApiHttpException(400, { ok: false, error, message: message || error });
}

export function notFound(error: string, message?: string): ApiHttpException {
  return new ApiHttpException(404, { ok: false, error, message: message || error });
}
