import { CanActivate, ExecutionContext, Inject, Injectable, Optional } from "@nestjs/common";
import { Reflector } from "@nestjs/core";

import { ADMIN_CONFIG, ADMIN_STORE } from "../tokens.js";
import { forbidden } from "../common/http-exception.js";
import { log } from "../logger.js";
import {
  appendSecurityAuditLog,
  getRequestAuditDetails,
  getSecurityAuditClientIp
} from "../common/security-audit.js";
import { AdminPermission, AdminRole, PERMISSIONS_KEY, ROLES_KEY, roleHasAllPermissions } from "./roles.decorator.js";

@Injectable()
export class RolesGuard implements CanActivate {
  constructor(
    private readonly reflector: Reflector,
    @Optional() @Inject(ADMIN_STORE) private readonly adminStore?: any,
    @Optional() @Inject(ADMIN_CONFIG) private readonly config?: any
  ) {}

  async canActivate(context: ExecutionContext): Promise<boolean> {
    const permissions = this.reflector.getAllAndOverride<AdminPermission[]>(PERMISSIONS_KEY, [
      context.getHandler(),
      context.getClass()
    ]);
    const roles = this.reflector.getAllAndOverride<AdminRole[]>(ROLES_KEY, [
      context.getHandler(),
      context.getClass()
    ]);

    if ((!permissions || permissions.length === 0) && (!roles || roles.length === 0)) {
      return true;
    }

    const req = context.switchToHttp().getRequest();
    const role = req.admin?.role;
    if (permissions?.length) {
      if (roleHasAllPermissions(role, permissions)) {
        return true;
      }

      await this.recordPermissionDenied(context, req, permissions, roles || []);
      throw forbidden("INSUFFICIENT_PERMISSION", "Insufficient permission");
    }

    if (roles?.includes(role)) {
      return true;
    }

    await this.recordPermissionDenied(context, req, [], roles || []);
    throw forbidden("INSUFFICIENT_PERMISSION", "Insufficient permission");
  }

  private async recordPermissionDenied(
    context: ExecutionContext,
    req: any,
    permissions: readonly AdminPermission[],
    roles: readonly AdminRole[]
  ) {
    const handler = context.getHandler()?.name || null;

    log("warn", "admin_api.permission_denied", {
      adminId: req.admin?.sub ?? null,
      username: req.admin?.username ?? null,
      role: req.admin?.role ?? null,
      method: req.method,
      path: req.url || req.raw?.url || null,
      handler,
      permissions,
      roles
    });

    await appendSecurityAuditLog(this.adminStore, {
      eventType: "admin_permission_denied",
      targetType: req.admin?.username || req.admin?.sub ? "admin" : null,
      targetValue: req.admin?.username || (req.admin?.sub ? String(req.admin.sub) : null),
      severity: "critical",
      clientIp: getSecurityAuditClientIp(req, this.config),
      details: getRequestAuditDetails(req, {
        reason: "INSUFFICIENT_PERMISSION",
        errorCode: "INSUFFICIENT_PERMISSION",
        adminId: req.admin?.sub ?? null,
        username: req.admin?.username ?? null,
        role: req.admin?.role ?? null,
        requiredPermissions: permissions,
        requiredRoles: roles,
        handler
      })
    });
  }
}
