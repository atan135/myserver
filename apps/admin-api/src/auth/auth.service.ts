import { Inject, Injectable } from "@nestjs/common";
import { JwtService } from "@nestjs/jwt";

import { ADMIN_CONFIG, ADMIN_STORE } from "../tokens.js";
import { badRequest, forbidden, notFound, unauthorized } from "../common/http-exception.js";
import type { LoginDto } from "./dto/login.dto.js";

function getClientIp(req: any): string | null {
  const forwardedFor = req.headers["x-forwarded-for"];
  if (typeof forwardedFor === "string" && forwardedFor.length > 0) {
    return forwardedFor.split(",")[0].trim();
  }
  return req.ip || req.socket?.remoteAddress || null;
}

function toAdminDto(admin: any) {
  return {
    id: admin.id,
    username: admin.username,
    displayName: admin.displayName,
    role: admin.role
  };
}

@Injectable()
export class AuthService {
  constructor(
    private readonly jwtService: JwtService,
    @Inject(ADMIN_CONFIG) private readonly config: any,
    @Inject(ADMIN_STORE) private readonly adminStore: any
  ) {}

  async login(dto: LoginDto, req: any) {
    const { username, password } = dto || {};

    if (!username || typeof username !== "string" || username.trim().length === 0) {
      throw badRequest("INVALID_USERNAME", "username is required");
    }

    if (!password || typeof password !== "string" || password.length === 0) {
      throw badRequest("INVALID_PASSWORD", "password is required");
    }

    const admin = await this.adminStore.findAdminByUsername(username.trim());
    if (!admin) {
      throw unauthorized("INVALID_CREDENTIALS", "Invalid username or password");
    }

    if (admin.status !== "active") {
      throw forbidden("ACCOUNT_DISABLED", "Account is disabled");
    }

    const passwordValid = await this.adminStore.verifyPassword(password, admin.passwordHash);
    if (!passwordValid) {
      throw unauthorized("INVALID_CREDENTIALS", "Invalid username or password");
    }

    const tokenPayload = {
      sub: admin.id,
      username: admin.username,
      role: admin.role
    };
    const accessToken = await this.jwtService.signAsync(tokenPayload, {
      secret: this.config.jwtSecret,
      expiresIn: this.config.jwtExpiresIn
    });

    await this.adminStore.updateLastLogin(admin.id);
    await this.adminStore.appendAuditLog({
      adminId: admin.id,
      adminUsername: admin.username,
      action: "admin_login",
      ip: getClientIp(req)
    });

    return {
      ok: true,
      accessToken,
      expiresIn: this.config.jwtExpiresIn,
      admin: toAdminDto(admin)
    };
  }

  async me(req: any) {
    const admin = await this.adminStore.findAdminByUsername(req.admin.username);
    if (!admin) {
      throw notFound("ADMIN_NOT_FOUND");
    }

    return {
      ok: true,
      admin: toAdminDto(admin)
    };
  }

  async logout(req: any) {
    await this.adminStore.appendAuditLog({
      adminId: req.admin.sub,
      adminUsername: req.admin.username,
      action: "admin_logout",
      ip: getClientIp(req)
    });

    return { ok: true, message: "Logged out" };
  }
}
