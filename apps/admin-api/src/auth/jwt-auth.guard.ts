import { CanActivate, ExecutionContext, Inject, Injectable } from "@nestjs/common";
import { JwtService } from "@nestjs/jwt";

import { ADMIN_CONFIG, ADMIN_SESSION_STORE, ADMIN_STORE } from "../tokens.js";
import { forbidden, unauthorized } from "../common/http-exception.js";

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
      throw unauthorized("MISSING_TOKEN", "Authorization token required");
    }

    let payload: any;
    try {
      payload = await this.jwtService.verifyAsync(token, {
        secret: this.config.jwtSecret
      });
    } catch (err: any) {
      if (err.name === "TokenExpiredError") {
        throw unauthorized("TOKEN_EXPIRED", "Token has expired");
      }
      throw unauthorized("INVALID_TOKEN", "Invalid token");
    }

    if (!payload.jti) {
      throw unauthorized("SESSION_REQUIRED", "Admin session is required");
    }

    const admin = await this.adminStore.findAdminByUsername(payload.username);
    if (!admin) {
      throw unauthorized("ADMIN_NOT_FOUND", "Admin account no longer exists");
    }

    if (admin.status !== "active") {
      throw forbidden("ACCOUNT_DISABLED", "Account is disabled");
    }

    const session = await this.sessionStore.getSession(payload.jti);
    if (!session) {
      throw unauthorized("SESSION_REVOKED", "Admin session has been revoked");
    }

    if (String(session.adminId) !== String(admin.id)) {
      throw unauthorized("SESSION_MISMATCH", "Admin session does not match account");
    }

    const currentTokenVersion = await this.sessionStore.getTokenVersion(admin.id);
    if (Number(payload.tokenVersion || 0) !== currentTokenVersion || Number(session.tokenVersion || 0) !== currentTokenVersion) {
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
}
