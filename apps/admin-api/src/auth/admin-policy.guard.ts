import { CanActivate, ExecutionContext, Inject, Injectable } from "@nestjs/common";
import { Reflector } from "@nestjs/core";

import { ADMIN_CONFIG, ADMIN_POLICY, ADMIN_STORE } from "../tokens.js";
import { forbidden, unauthorized } from "../common/http-exception.js";
import {
  appendSecurityAuditLog,
  getRequestAuditDetails,
  getSecurityAuditClientIp
} from "../common/security-audit.js";
import {
  AdminPermission,
  AdminPermissionResolver,
  PERMISSIONS_KEY,
  POLICY_PERMISSION_RESOLVER_KEY
} from "./roles.decorator.js";
import { AdminPolicyScopeRequest } from "./admin-policy.service.js";

type RequestSource = Record<string, unknown>;

function objectSource(value: unknown): RequestSource {
  return value && typeof value === "object" && !Array.isArray(value) ? value as RequestSource : {};
}

function firstText(sources: RequestSource[], keys: string[]): string | undefined {
  for (const source of sources) {
    for (const key of keys) {
      const value = source[key];
      if (typeof value === "string" || typeof value === "number") {
        const normalized = String(value).trim();
        if (normalized) {
          return normalized;
        }
      }
    }
  }
  return undefined;
}

function texts(value: unknown): string[] {
  const values = Array.isArray(value) ? value : [value];
  return values
    .filter((item) => typeof item === "string" || typeof item === "number")
    .map((item) => String(item).trim())
    .filter(Boolean);
}

function targetIds(sources: RequestSource[]): string[] {
  const ids = new Set<string>();
  const pluralKeys = [
    "targetIds", "target_ids", "playerIds", "player_ids", "characterIds", "character_ids",
    "adminIds", "admin_ids", "requestIds", "request_ids"
  ];
  const singularKeys = [
    "targetId", "target_id", "playerId", "player_id", "characterId", "character_id",
    "adminId", "admin_id", "requestId", "request_id", "agentId", "agent_id"
  ];

  for (const source of sources) {
    for (const key of pluralKeys) {
      for (const value of texts(source[key])) {
        ids.add(value);
      }
    }
    for (const key of singularKeys) {
      for (const value of texts(source[key])) {
        ids.add(value);
      }
    }
  }
  return [...ids];
}

function requestedFields(body: RequestSource): string[] {
  const fields = new Set<string>();
  for (const value of texts(body.fields ?? body.fieldAllowlist ?? body.field_allowlist)) {
    fields.add(value);
  }
  for (const field of ["affinity", "mastery"]) {
    if (body[field] !== undefined) {
      fields.add(field);
    }
  }
  return [...fields];
}

async function resolveGmWorldId(permission: string, ids: readonly string[], adminStore: any): Promise<string | undefined> {
  if (!permission.startsWith("gm.") || ids.length !== 1 || typeof adminStore?.findCharacterById !== "function") {
    return undefined;
  }

  const character = await adminStore.findCharacterById(ids[0], { includeDeleted: true });
  const worldId = character?.worldId ?? character?.world_id;
  if (worldId === undefined || worldId === null || String(worldId).trim() === "") {
    return undefined;
  }
  return String(worldId);
}

export async function extractAdminPolicyScope(
  request: any,
  permission: string,
  adminStore?: any
): Promise<AdminPolicyScopeRequest> {
  const params = objectSource(request?.params);
  const body = objectSource(request?.body);
  const query = objectSource(request?.query);
  const sources = [params, body, query];
  const ids = targetIds(sources);
  const resolvedWorldId = await resolveGmWorldId(permission, ids, adminStore);

  // Missing values represent an operation over the complete dimension.  They are
  // deliberately rendered as "*", so only an explicit wildcard grant can match.
  return {
    worldId: resolvedWorldId || firstText(sources, ["worldId", "world_id"]) || "*",
    serviceName: firstText(sources, ["serviceName", "service_name", "service", "name"]) || "*",
    instanceId: firstText(sources, ["targetInstanceId", "target_instance_id", "instanceId", "instance_id"]) || "*",
    fields: requestedFields(body).length > 0 ? requestedFields(body) : ["*"],
    targetType: firstText(sources, ["targetType", "target_type"]) || "*",
    targetIds: ids.length > 0 ? ids : ["*"],
    targetCount: ids.length > 0 ? ids.length : 1
  };
}

function policyFailure(code: string) {
  if (code === "SCOPE_REQUIRED") {
    return forbidden("ADMIN_SCOPE_REQUIRED", "Operation requires a scoped target");
  }
  if (code === "SCOPE_DENIED") {
    return forbidden("ADMIN_SCOPE_DENIED", "Operation is outside the authorized scope");
  }
  if (code === "PERMISSION_DENIED") {
    return forbidden("ADMIN_PERMISSION_DENIED", "Permission is not granted");
  }
  return forbidden("ADMIN_PERMISSION_UNAVAILABLE", "Permission is not available");
}

@Injectable()
export class AdminPolicyGuard implements CanActivate {
  constructor(
    private readonly reflector: Reflector,
    @Inject(ADMIN_POLICY) private readonly policy: any,
    @Inject(ADMIN_STORE) private readonly adminStore: any,
    @Inject(ADMIN_CONFIG) private readonly config: any
  ) {}

  async canActivate(context: ExecutionContext): Promise<boolean> {
    const request = context.switchToHttp().getRequest();
    if (!request?.admin?.sub) {
      throw unauthorized("UNAUTHORIZED", "Admin authentication is required");
    }

    const permissions = this.permissionsFor(context, request);
    if (!permissions || permissions.length === 0) {
      await this.recordDenied(context, request, [], "ADMIN_PERMISSION_NOT_DECLARED");
      throw forbidden("ADMIN_PERMISSION_NOT_DECLARED", "Route permission is not declared");
    }

    for (const permission of permissions) {
      const scope = await extractAdminPolicyScope(request, permission, this.adminStore);
      const decision = await this.policy.authorize(request.admin.sub, permission, scope);
      if (!decision?.allowed) {
        const code = decision?.code || "ADMIN_PERMISSION_UNAVAILABLE";
        await this.recordDenied(context, request, permissions, code, permission, scope);
        throw policyFailure(code);
      }
    }

    return true;
  }

  private permissionsFor(context: ExecutionContext, request: any): readonly AdminPermission[] | null {
    const resolver = this.reflector.getAllAndOverride<AdminPermissionResolver>(POLICY_PERMISSION_RESOLVER_KEY, [
      context.getHandler(),
      context.getClass()
    ]);
    const permissions = resolver
      ? resolver(request)
      : this.reflector.getAllAndOverride<AdminPermission[]>(PERMISSIONS_KEY, [context.getHandler(), context.getClass()]);
    if (!Array.isArray(permissions) || permissions.length === 0 || permissions.some((permission) => typeof permission !== "string")) {
      return null;
    }
    return [...new Set(permissions)];
  }

  private async recordDenied(
    context: ExecutionContext,
    request: any,
    permissions: readonly AdminPermission[],
    code: string,
    permission?: string,
    scope?: AdminPolicyScopeRequest
  ) {
    await appendSecurityAuditLog(this.adminStore, {
      eventType: "admin_policy_denied",
      targetType: request.admin?.username || request.admin?.sub ? "admin" : null,
      targetValue: request.admin?.username || (request.admin?.sub ? String(request.admin.sub) : null),
      severity: "critical",
      clientIp: getSecurityAuditClientIp(request, this.config),
      details: getRequestAuditDetails(request, {
        errorCode: code,
        adminId: request.admin?.sub ?? null,
        username: request.admin?.username ?? null,
        handler: context.getHandler()?.name || null,
        requiredPermissions: permissions,
        permission: permission || null,
        scope: scope || null
      })
    });
  }
}
