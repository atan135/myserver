import { CanActivate, ExecutionContext, Injectable } from "@nestjs/common";
import { Reflector } from "@nestjs/core";

import { forbidden } from "../common/http-exception.js";
import { AdminRole, ROLES_KEY } from "./roles.decorator.js";

@Injectable()
export class RolesGuard implements CanActivate {
  constructor(private readonly reflector: Reflector) {}

  canActivate(context: ExecutionContext): boolean {
    const roles = this.reflector.getAllAndOverride<AdminRole[]>(ROLES_KEY, [
      context.getHandler(),
      context.getClass()
    ]);

    if (!roles || roles.length === 0) {
      return true;
    }

    const req = context.switchToHttp().getRequest();
    const role = req.admin?.role;
    if (roles.includes(role)) {
      return true;
    }

    throw forbidden("INSUFFICIENT_ROLE", "Insufficient role");
  }
}
