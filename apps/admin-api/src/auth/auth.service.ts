import { Inject, Injectable } from "@nestjs/common";
import { JwtService } from "@nestjs/jwt";

import { ADMIN_CONFIG, ADMIN_POLICY, ADMIN_SESSION_STORE, ADMIN_STORE } from "../tokens.js";
import { badRequest, forbidden, notFound, unauthorized } from "../common/http-exception.js";
import { getClientIp } from "../common/client-ip.js";
import { appendSecurityAuditLog } from "../common/security-audit.js";
import type { LoginDto } from "./dto/login.dto.js";

function toAdminDto(admin: any, capabilities: Map<string, Array<{ scope: unknown }>>) {
  const entries = [...capabilities.entries()];
  return {
    id: admin.id,
    username: admin.username,
    displayName: admin.displayName,
    role: admin.role,
    permissions: entries.map(([permissionKey]) => permissionKey),
    permissionScopes: Object.fromEntries(entries.map(([permissionKey, grants]) => [
      permissionKey,
      grants.map((grant) => grant.scope)
    ]))
  };
}

async function recordLoginFailure(
  adminStore: any,
  username: string,
  clientIp: string | null,
  reason: string,
  details: Record<string, unknown> = {}
) {
  await appendSecurityAuditLog(adminStore, {
    eventType: "admin_login_failed",
    targetType: "admin",
    targetValue: username,
    severity: "warning",
    clientIp,
    details: { reason, ...details }
  });
}

@Injectable()
export class AuthService {
  constructor(
    private readonly jwtService: JwtService,
    @Inject(ADMIN_CONFIG) private readonly config: any,
    @Inject(ADMIN_STORE) private readonly adminStore: any,
    @Inject(ADMIN_SESSION_STORE) private readonly sessionStore: any,
    @Inject(ADMIN_POLICY) private readonly policy: any
  ) {}

  private async effectiveAdminDto(admin: any) {
    const capabilities = await this.policy.effectiveCapabilities(admin.id);
    return toAdminDto(admin, capabilities);
  }

  async login(dto: LoginDto, req: any) {
    const { username, password } = dto || {};

    if (!username || typeof username !== "string" || username.trim().length === 0) {
      throw badRequest("INVALID_USERNAME", "username is required");
    }

    if (!password || typeof password !== "string" || password.length === 0) {
      throw badRequest("INVALID_PASSWORD", "password is required");
    }

    const normalizedUsername = username.trim();
    const clientIp = getClientIp(req, this.config);
    const lockStatus = await this.sessionStore.getLoginLock(normalizedUsername, clientIp);
    if (lockStatus.locked) {
      await appendSecurityAuditLog(this.adminStore, {
        eventType: "admin_login_locked",
        targetType: "admin",
        targetValue: normalizedUsername,
        severity: "warning",
        clientIp,
        details: { remainingSeconds: lockStatus.remainingSeconds }
      });
      throw forbidden("ADMIN_LOGIN_LOCKED", `Admin login is locked. Try again in ${lockStatus.remainingSeconds} seconds`);
    }

    const admin = await this.adminStore.findAdminByUsername(normalizedUsername);
    if (!admin) {
      const attempts = await this.sessionStore.recordLoginFailure(normalizedUsername, clientIp, this.config);
      await recordLoginFailure(this.adminStore, normalizedUsername, clientIp, "admin_not_found", { attempts });
      if (attempts >= this.config.adminLoginMaxFailures) {
        await appendSecurityAuditLog(this.adminStore, {
          eventType: "admin_login_locked",
          targetType: "admin",
          targetValue: normalizedUsername,
          severity: "critical",
          clientIp,
          details: { attempts, lockSeconds: this.config.adminLoginLockSeconds }
        });
      }
      throw unauthorized("INVALID_CREDENTIALS", "Invalid username or password");
    }

    if (admin.status !== "active") {
      const attempts = await this.sessionStore.recordLoginFailure(admin.username, clientIp, this.config);
      await recordLoginFailure(this.adminStore, admin.username, clientIp, "account_disabled", { attempts });
      throw forbidden("ACCOUNT_DISABLED", "Account is disabled");
    }

    const passwordValid = await this.adminStore.verifyPassword(password, admin.passwordHash);
    if (!passwordValid) {
      const attempts = await this.sessionStore.recordLoginFailure(admin.username, clientIp, this.config);
      await recordLoginFailure(this.adminStore, admin.username, clientIp, "invalid_password", { attempts });
      if (attempts >= this.config.adminLoginMaxFailures) {
        await appendSecurityAuditLog(this.adminStore, {
          eventType: "admin_login_locked",
          targetType: "admin",
          targetValue: admin.username,
          severity: "critical",
          clientIp,
          details: { attempts, lockSeconds: this.config.adminLoginLockSeconds }
        });
      }
      throw unauthorized("INVALID_CREDENTIALS", "Invalid username or password");
    }

    const adminDto = await this.effectiveAdminDto(admin);
    const jti = this.sessionStore.createJti();
    const tokenVersion = await this.sessionStore.getTokenVersion(admin.id);
    const tokenPayload = {
      sub: admin.id,
      username: admin.username,
      role: admin.role,
      jti,
      tokenVersion
    };
    const accessToken = await this.jwtService.signAsync(tokenPayload, {
      secret: this.config.jwtSecret,
      expiresIn: this.config.jwtExpiresIn
    });

    await this.sessionStore.createSession({
      adminId: admin.id,
      username: admin.username,
      role: admin.role,
      jti,
      tokenVersion,
      clientIp,
      ttlSeconds: this.config.adminSessionTtlSeconds
    });
    await this.sessionStore.clearLoginFailures(admin.username, clientIp);
    await this.adminStore.updateLastLogin(admin.id);
    await this.adminStore.appendAuditLog({
      adminId: admin.id,
      adminUsername: admin.username,
      action: "admin_login",
      ip: clientIp
    });

    return {
      ok: true,
      accessToken,
      expiresIn: this.config.jwtExpiresIn,
      admin: adminDto
    };
  }

  async me(req: any) {
    const admin = await this.adminStore.findAdminByUsername(req.admin.username);
    if (!admin) {
      throw notFound("ADMIN_NOT_FOUND");
    }

    return {
      ok: true,
      admin: await this.effectiveAdminDto(admin)
    };
  }

  async logout(req: any) {
    if (req.admin?.jti) {
      await this.sessionStore.deleteSession(req.admin.jti);
    }

    await this.adminStore.appendAuditLog({
      adminId: req.admin.sub,
      adminUsername: req.admin.username,
      action: "admin_logout",
      ip: getClientIp(req, this.config)
    });

    return { ok: true, message: "Logged out" };
  }
}
