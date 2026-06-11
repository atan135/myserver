import { CanActivate, ExecutionContext, Inject, Injectable } from "@nestjs/common";
import { JwtService } from "@nestjs/jwt";

import { ADMIN_CONFIG, ADMIN_SESSION_STORE, ADMIN_STORE } from "../tokens.js";
import { forbidden, unauthorized } from "../common/http-exception.js";
import {
  appendSecurityAuditLog,
  getRequestAuditDetails,
  getSecurityAuditClientIp,
  SecurityAuditSeverity
} from "../common/security-audit.js";

function getTokenFromHeader(req: any): string | null {
  const auth = req.headers.authorization;
  if (!auth?.startsWith("Bearer ")) return null;
  return auth.slice("Bearer ".length).trim();
}

@Injectable()
export class JwtAuthGuard implements CanActivate {
  constructor(
    private readonly jwtService: JwtService,
    @Inject(ADMIN_CONFIG) private readonly config: any,
    @Inject(ADMIN_STORE) private readonly adminStore: any,
    @Inject(ADMIN_SESSION_STORE) private readonly sessionStore: any
  ) {}

  async canActivate(context: ExecutionContext): Promise<boolean> {
    const req = context.switchToHttp().getRequest();
    const token = getTokenFromHeader(req);
    if (!token) {
      await this.recordAuthDenied(context, req, "MISSING_TOKEN", "warning");
      throw unauthorized("MISSING_TOKEN", "Authorization token required");
    }

    let payload: any;
    try {
      payload = await this.jwtService.verifyAsync(token, {
        secret: this.config.jwtSecret
      });
    } catch (err: any) {
      if (err.name === "TokenExpiredError") {
        await this.recordAuthDenied(context, req, "TOKEN_EXPIRED", "warning");
        throw unauthorized("TOKEN_EXPIRED", "Token has expired");
      }
      await this.recordAuthDenied(context, req, "INVALID_TOKEN", "warning");
      throw unauthorized("INVALID_TOKEN", "Invalid token");
    }

    if (!payload.jti) {
      await this.recordAuthDenied(context, req, "SESSION_REQUIRED", "warning", payload);
      throw unauthorized("SESSION_REQUIRED", "Admin session is required");
    }

    const admin = await this.adminStore.findAdminByUsername(payload.username);
    if (!admin) {
      await this.recordAuthDenied(context, req, "ADMIN_NOT_FOUND", "critical", payload);
      throw unauthorized("ADMIN_NOT_FOUND", "Admin account no longer exists");
    }

    if (admin.status !== "active") {
      await this.recordAuthDenied(context, req, "ACCOUNT_DISABLED", "critical", payload, admin);
      throw forbidden("ACCOUNT_DISABLED", "Account is disabled");
    }

    const session = await this.sessionStore.getSession(payload.jti);
    if (!session) {
      await this.recordAuthDenied(context, req, "SESSION_REVOKED", "critical", payload, admin);
      throw unauthorized("SESSION_REVOKED", "Admin session has been revoked");
    }

    if (String(session.adminId) !== String(admin.id)) {
      await this.recordAuthDenied(context, req, "SESSION_MISMATCH", "critical", payload, admin);
      throw unauthorized("SESSION_MISMATCH", "Admin session does not match account");
    }

    const currentTokenVersion = await this.sessionStore.getTokenVersion(admin.id);
    if (Number(payload.tokenVersion || 0) !== currentTokenVersion || Number(session.tokenVersion || 0) !== currentTokenVersion) {
      await this.recordAuthDenied(context, req, "TOKEN_VERSION_REVOKED", "critical", payload, admin);
      throw unauthorized("TOKEN_VERSION_REVOKED", "Admin token version has been revoked");
    }

    req.admin = {
      ...payload,
      sub: admin.id,
      username: admin.username,
      displayName: admin.displayName,
      role: admin.role,
      status: admin.status
    };
    return true;
  }

  private async recordAuthDenied(
    context: ExecutionContext,
    req: any,
    errorCode: string,
    severity: SecurityAuditSeverity,
    payload?: any,
    admin?: any
  ) {
    const username = admin?.username || payload?.username || null;
    const adminId = admin?.id || payload?.sub || null;
    const role = admin?.role || payload?.role || null;

    await appendSecurityAuditLog(this.adminStore, {
      eventType: "admin_auth_denied",
      targetType: username || adminId ? "admin" : null,
      targetValue: username || (adminId ? String(adminId) : null),
      severity,
      clientIp: getSecurityAuditClientIp(req, this.config),
      details: getRequestAuditDetails(req, {
        reason: errorCode,
        errorCode,
        adminId,
        username,
        role,
        handler: context.getHandler()?.name || null,
        hasJti: Boolean(payload?.jti)
      })
    });
  }
}
