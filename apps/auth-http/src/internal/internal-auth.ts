import { ApiHttpException } from "../common/http-exception.js";

export function verifyInternalToken(req: any, config: any) {
  const token = config.internalApiToken;
  if (!token) {
    if (config.strictSecurity) {
      throw new ApiHttpException(503, {
        ok: false,
        error: "INTERNAL_API_TOKEN_REQUIRED",
        message: "INTERNAL_API_TOKEN is required when strict security is enabled"
      });
    }
    return;
  }

  const provided = req.headers["x-service-token"];
  if (provided !== token) {
    throw new ApiHttpException(401, {
      ok: false,
      error: "INVALID_SERVICE_TOKEN",
      message: "Missing or invalid X-Service-Token header"
    });
  }
}
