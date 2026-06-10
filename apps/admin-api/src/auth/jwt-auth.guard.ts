import { CanActivate, ExecutionContext, Inject, Injectable } from "@nestjs/common";
import { JwtService } from "@nestjs/jwt";

import { ADMIN_CONFIG, ADMIN_STORE } from "../tokens.js";
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
    @Inject(ADMIN_STORE) private readonly adminStore: any
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

    const admin = await this.adminStore.findAdminByUsername(payload.username);
    if (!admin) {
      throw unauthorized("ADMIN_NOT_FOUND", "Admin account no longer exists");
    }

    if (admin.status !== "active") {
      throw forbidden("ACCOUNT_DISABLED", "Account is disabled");
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
