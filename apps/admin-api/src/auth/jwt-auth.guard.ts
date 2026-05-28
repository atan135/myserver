import { CanActivate, ExecutionContext, Inject, Injectable } from "@nestjs/common";
import { JwtService } from "@nestjs/jwt";

import { ADMIN_CONFIG } from "../tokens.js";
import { unauthorized } from "../common/http-exception.js";

function getTokenFromHeader(req: any): string | null {
  const auth = req.headers.authorization;
  if (!auth?.startsWith("Bearer ")) return null;
  return auth.slice("Bearer ".length).trim();
}

@Injectable()
export class JwtAuthGuard implements CanActivate {
  constructor(
    private readonly jwtService: JwtService,
    @Inject(ADMIN_CONFIG) private readonly config: any
  ) {}

  async canActivate(context: ExecutionContext): Promise<boolean> {
    const req = context.switchToHttp().getRequest();
    const token = getTokenFromHeader(req);
    if (!token) {
      throw unauthorized("MISSING_TOKEN", "Authorization token required");
    }

    try {
      req.admin = await this.jwtService.verifyAsync(token, {
        secret: this.config.jwtSecret
      });
      return true;
    } catch (err: any) {
      if (err.name === "TokenExpiredError") {
        throw unauthorized("TOKEN_EXPIRED", "Token has expired");
      }
      throw unauthorized("INVALID_TOKEN", "Invalid token");
    }
  }
}
