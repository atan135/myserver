import { Inject, Injectable } from "@nestjs/common";

import { ADMIN_STORE } from "../tokens.js";

const SCOPE_KEYS = [
  "world_ids",
  "service_names",
  "instance_ids",
  "field_allowlist",
  "target_types",
  "target_ids"
] as const;

const SCOPE_DIMENSIONS = new Set(SCOPE_KEYS);
const PERMISSION_KEY_PATTERN = /^[a-z][a-z0-9]*(?:[._][a-z0-9]+)*$/;
const MAX_SCOPE_TARGETS = 10_000;

export type AdminPolicyScope = {
  worldIds: string[];
  serviceNames: string[];
  instanceIds: string[];
  fieldAllowlist: string[];
  targetTypes: string[];
  targetIds: string[];
  maxTargets: number;
};

export type AdminPolicyScopeRequest = {
  worldId?: string | null;
  serviceName?: string | null;
  instanceId?: string | null;
  fields?: readonly string[];
  targetType?: string | null;
  targetIds?: readonly string[];
  targetCount?: number;
};

export type AdminPolicyDecision = {
  allowed: boolean;
  code:
    | "ALLOWED"
    | "INVALID_PERMISSION_KEY"
    | "UNKNOWN_PERMISSION"
    | "PERMISSION_INACTIVE"
    | "INVALID_PERMISSION_CATALOG"
    | "SCOPE_REQUIRED"
    | "PERMISSION_DENIED"
    | "SCOPE_DENIED";
  permissionKey: string;
  matchedGrant?: {
    source: "role" | "direct";
    sourceId: number | string;
    scope: AdminPolicyScope;
  };
};

type PolicyPermission = {
  permission_key: string;
  active: boolean;
  scope_dimensions: unknown;
};

type PolicyGrant = PolicyPermission & {
  scope_json: unknown;
  grant_source: "role" | "direct";
  source_id: number | string;
};

function stringValue(value: unknown, field: string): string {
  if (typeof value !== "string") {
    throw new Error(`${field} must contain strings`);
  }
  const normalized = value.trim();
  if (!normalized || normalized.length > 256 || /[\u0000-\u001f\u007f]/.test(normalized)) {
    throw new Error(`${field} contains an invalid value`);
  }
  return normalized;
}

function normalizeStringList(value: unknown, field: string): string[] {
  if (!Array.isArray(value) || value.length === 0) {
    throw new Error(`${field} must be a non-empty array`);
  }
  const values = [...new Set(value.map((item) => stringValue(item, field)))];
  if (values.includes("*") && values.length !== 1) {
    throw new Error(`${field} cannot combine wildcard with named values`);
  }
  return values;
}

function objectValue(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("scope must be an object");
  }
  return value as Record<string, unknown>;
}

function valueFor(object: Record<string, unknown>, snakeCase: string, camelCase: string): unknown {
  const snake = object[snakeCase];
  const camel = object[camelCase];
  if (snake !== undefined && camel !== undefined) {
    throw new Error(`${snakeCase} must not be supplied twice`);
  }
  return snake ?? camel;
}

export function normalizeAdminPolicyScope(value: unknown): AdminPolicyScope {
  if (typeof value === "string") {
    try {
      value = JSON.parse(value);
    } catch {
      throw new Error("scope must be valid JSON");
    }
  }
  const scope = objectValue(value);
  const allowed = new Set([
    ...SCOPE_KEYS,
    "worldIds",
    "serviceNames",
    "instanceIds",
    "fieldAllowlist",
    "targetTypes",
    "targetIds",
    "max_targets",
    "maxTargets"
  ]);
  const unknown = Object.keys(scope).find((key) => !allowed.has(key));
  if (unknown) {
    throw new Error(`scope contains unknown key ${unknown}`);
  }

  const maxTargets = valueFor(scope, "max_targets", "maxTargets");
  if (!Number.isSafeInteger(maxTargets) || Number(maxTargets) < 1 || Number(maxTargets) > MAX_SCOPE_TARGETS) {
    throw new Error(`max_targets must be an integer between 1 and ${MAX_SCOPE_TARGETS}`);
  }

  return {
    worldIds: normalizeStringList(valueFor(scope, "world_ids", "worldIds"), "world_ids"),
    serviceNames: normalizeStringList(valueFor(scope, "service_names", "serviceNames"), "service_names"),
    instanceIds: normalizeStringList(valueFor(scope, "instance_ids", "instanceIds"), "instance_ids"),
    fieldAllowlist: normalizeStringList(valueFor(scope, "field_allowlist", "fieldAllowlist"), "field_allowlist"),
    targetTypes: normalizeStringList(valueFor(scope, "target_types", "targetTypes"), "target_types"),
    targetIds: normalizeStringList(valueFor(scope, "target_ids", "targetIds"), "target_ids"),
    maxTargets: Number(maxTargets)
  };
}

export function adminPolicyScopeToDatabase(scope: AdminPolicyScope): Record<string, unknown> {
  return {
    world_ids: scope.worldIds,
    service_names: scope.serviceNames,
    instance_ids: scope.instanceIds,
    field_allowlist: scope.fieldAllowlist,
    target_types: scope.targetTypes,
    target_ids: scope.targetIds,
    max_targets: scope.maxTargets
  };
}

function normalizeRequestedValue(value: unknown): string | null {
  if (value === undefined || value === null || value === "") {
    return null;
  }
  return stringValue(value, "requested scope");
}

function normalizeRequestedList(value: unknown): string[] {
  if (value === undefined || value === null) {
    return [];
  }
  if (!Array.isArray(value)) {
    throw new Error("requested scope list must be an array");
  }
  return [...new Set(value.map((item) => stringValue(item, "requested scope")))];
}

function listMatches(granted: readonly string[], requested: readonly string[]): boolean {
  if (requested.length === 0) {
    return granted.length === 1 && granted[0] === "*";
  }
  return requested.every((value) => granted.includes("*") || granted.includes(value));
}

function requiredDimensions(value: unknown): string[] | null {
  if (!Array.isArray(value) || value.some((item) => typeof item !== "string" || !SCOPE_DIMENSIONS.has(item as typeof SCOPE_KEYS[number]))) {
    return null;
  }
  return [...new Set(value)];
}

function hasRequiredDimension(request: {
  worldIds: string[];
  serviceNames: string[];
  instanceIds: string[];
  fields: string[];
  targetTypes: string[];
  targetIds: string[];
}, dimension: string): boolean {
  const values = {
    world_ids: request.worldIds,
    service_names: request.serviceNames,
    instance_ids: request.instanceIds,
    field_allowlist: request.fields,
    target_types: request.targetTypes,
    target_ids: request.targetIds
  }[dimension];
  return Array.isArray(values) && values.length > 0;
}

function normalizeRequest(scope: AdminPolicyScopeRequest = {}) {
  const worldId = normalizeRequestedValue(scope.worldId);
  const serviceName = normalizeRequestedValue(scope.serviceName);
  const instanceId = normalizeRequestedValue(scope.instanceId);
  const targetType = normalizeRequestedValue(scope.targetType);
  const fields = normalizeRequestedList(scope.fields);
  const targetIds = normalizeRequestedList(scope.targetIds);
  const targetCount = scope.targetCount === undefined
    ? Math.max(targetIds.length, 1)
    : scope.targetCount;
  if (!Number.isSafeInteger(targetCount) || targetCount < 1 || targetCount > MAX_SCOPE_TARGETS) {
    throw new Error(`targetCount must be an integer between 1 and ${MAX_SCOPE_TARGETS}`);
  }
  return {
    worldIds: worldId ? [worldId] : [],
    serviceNames: serviceName ? [serviceName] : [],
    instanceIds: instanceId ? [instanceId] : [],
    fields,
    targetTypes: targetType ? [targetType] : [],
    targetIds,
    targetCount
  };
}

function scopeMatches(
  granted: AdminPolicyScope,
  request: ReturnType<typeof normalizeRequest>,
  dimensions: readonly string[]
): boolean {
  if (request.targetCount > granted.maxTargets) {
    return false;
  }

  const matches = {
    world_ids: () => listMatches(granted.worldIds, request.worldIds),
    service_names: () => listMatches(granted.serviceNames, request.serviceNames),
    instance_ids: () => listMatches(granted.instanceIds, request.instanceIds),
    field_allowlist: () => listMatches(granted.fieldAllowlist, request.fields),
    target_types: () => listMatches(granted.targetTypes, request.targetTypes),
    target_ids: () => listMatches(granted.targetIds, request.targetIds)
  };
  return dimensions.every((dimension) => matches[dimension as keyof typeof matches]?.() === true);
}

@Injectable()
export class AdminPolicyService {
  constructor(@Inject(ADMIN_STORE) private readonly adminStore: any) {}

  async authorize(
    adminId: number | string,
    permissionKey: string,
    scopeRequest: AdminPolicyScopeRequest = {}
  ): Promise<AdminPolicyDecision> {
    if (typeof permissionKey !== "string" || !PERMISSION_KEY_PATTERN.test(permissionKey)) {
      return { allowed: false, code: "INVALID_PERMISSION_KEY", permissionKey: String(permissionKey || "") };
    }

    const permission = await this.adminStore.findAdminPolicyPermission(permissionKey) as PolicyPermission | null;
    if (!permission) {
      return { allowed: false, code: "UNKNOWN_PERMISSION", permissionKey };
    }
    if (permission.active !== true) {
      return { allowed: false, code: "PERMISSION_INACTIVE", permissionKey };
    }

    const dimensions = requiredDimensions(permission.scope_dimensions);
    if (!dimensions) {
      return { allowed: false, code: "INVALID_PERMISSION_CATALOG", permissionKey };
    }

    let request: ReturnType<typeof normalizeRequest>;
    try {
      request = normalizeRequest(scopeRequest);
    } catch {
      return { allowed: false, code: "SCOPE_DENIED", permissionKey };
    }
    if (dimensions.some((dimension) => !hasRequiredDimension(request, dimension))) {
      return { allowed: false, code: "SCOPE_REQUIRED", permissionKey };
    }

    const grants = await this.adminStore.listEffectiveAdminPolicyGrants(adminId, permissionKey) as PolicyGrant[];
    if (grants.length === 0) {
      return { allowed: false, code: "PERMISSION_DENIED", permissionKey };
    }
    for (const grant of grants) {
      try {
        const scope = normalizeAdminPolicyScope(grant.scope_json);
        if (scopeMatches(scope, request, dimensions)) {
          return {
            allowed: true,
            code: "ALLOWED",
            permissionKey,
            matchedGrant: {
              source: grant.grant_source,
              sourceId: grant.source_id,
              scope
            }
          };
        }
      } catch {
        // A malformed persisted scope is never widened into a usable grant.
      }
    }

    return { allowed: false, code: "SCOPE_DENIED", permissionKey };
  }

  async effectiveCapabilities(adminId: number | string) {
    const grants = await this.adminStore.listEffectiveAdminPolicyGrants(adminId);
    const capabilities = new Map<string, Array<{ source: "role" | "direct"; sourceId: number | string; scope: AdminPolicyScope }>>();
    for (const grant of grants as PolicyGrant[]) {
      try {
        const scope = normalizeAdminPolicyScope(grant.scope_json);
        const entries = capabilities.get(grant.permission_key) || [];
        entries.push({ source: grant.grant_source, sourceId: grant.source_id, scope });
        capabilities.set(grant.permission_key, entries);
      } catch {
        // Do not return malformed persisted scopes to a caller as a capability.
      }
    }
    return capabilities;
  }
}
