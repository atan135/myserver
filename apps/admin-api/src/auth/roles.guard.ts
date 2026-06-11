import { CanActivate, ExecutionContext, Injectable } from "@nestjs/common";
import { Reflector } from "@nestjs/core";

import { forbidden } from "../common/http-exception.js";
import { log } from "../logger.js";
import { AdminPermission, AdminRole, PERMISSIONS_KEY, ROLES_KEY, roleHasAllPermissions } from "./roles.decorator.js";

@Injectable()
export class RolesGuard implements CanActivate {
  constructor(private readonly reflector: Reflector) {}

  canActivate(context: ExecutionContext): boolean {
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

      this.logPermissionDenied(context, req, permissions, roles || []);
      throw forbidden("INSUFFICIENT_PERMISSION", "Insufficient permission");
    }

    if (roles?.includes(role)) {
      return true;
    }

    this.logPermissionDenied(context, req, [], roles || []);
    throw forbidden("INSUFFICIENT_PERMISSION", "Insufficient permission");
  }

  private logPermissionDenied(
    context: ExecutionContext,
    req: any,
    permissions: readonly AdminPermission[],
    roles: readonly AdminRole[]
  ) {
    log("warn", "admin_api.permission_denied", {
      adminId: req.admin?.sub ?? null,
      username: req.admin?.username ?? null,
      role: req.admin?.role ?? null,
      method: req.method,
      path: req.url || req.raw?.url || null,
      handler: context.getHandler()?.name,
      permissions,
      roles
    });
  }
}
