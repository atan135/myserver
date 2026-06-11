import { log } from "../logger.js";
import { getClientIp, ipMatchesAny, isRequestSecure } from "./client-ip.js";

function sendJson(reply: any, statusCode: number, body: Record<string, unknown>) {
  return reply.code(statusCode).type("application/json").send(body);
}

export function evaluateControlPlaneSecurity(req: any, config: any = {}) {
  const clientIp = getClientIp(req, config);

  if (config.adminApiRequireTls && !isRequestSecure(req, config)) {
    return {
      ok: false,
      statusCode: 426,
      error: "ADMIN_API_TLS_REQUIRED",
      message: "HTTPS is required",
      clientIp
    };
  }

  if (config.adminApiRequireIpAllowlist && !ipMatchesAny(clientIp, config.adminApiIpAllowlist || [])) {
    return {
      ok: false,
      statusCode: 403,
      error: "ADMIN_API_IP_NOT_ALLOWED",
      message: "Source IP is not allowed",
      clientIp
    };
  }

  return { ok: true, clientIp };
}

export function registerControlPlaneSecurityHook(fastify: any, config: any) {
  fastify.addHook("onRequest", async (request: any, reply: any) => {
    const result = evaluateControlPlaneSecurity(request, config);
    if (result.ok) {
      return;
    }

    log("warn", "security.control_plane_request_rejected", {
      error: result.error,
      method: request.method,
      path: request.url,
      clientIp: result.clientIp
    });

    return sendJson(reply, result.statusCode, {
      ok: false,
      error: result.error,
      message: result.message
    });
  });
}
