import { Controller, Get, Inject, Query, UseGuards } from "@nestjs/common";
import { ApiBearerAuth, ApiTags } from "@nestjs/swagger";

import { JwtAuthGuard } from "../auth/jwt-auth.guard.js";
import { AdminPolicyGuard } from "../auth/admin-policy.guard.js";
import { Permissions } from "../auth/roles.decorator.js";
import { badRequest } from "../common/http-exception.js";
import { ADMIN_STORE } from "../tokens.js";
import { decodeGlobalId } from "./global-id-decoder.js";

const MAX_LIMIT = 100;

function pageLimit(value: any) {
  const limit = Number(value);
  if (!Number.isFinite(limit) || limit <= 0) {
    return 50;
  }
  return Math.min(Math.floor(limit), MAX_LIMIT);
}

function pageOffset(value: any) {
  const offset = Number(value);
  if (!Number.isFinite(offset) || offset < 0) {
    return 0;
  }
  return Math.floor(offset);
}

function optionalPositiveInteger(value: any, field: string) {
  if (value === undefined || value === null || value === "") {
    return null;
  }

  const text = String(value).trim();
  if (!/^\d+$/.test(text)) {
    throw badRequest(`INVALID_${field.toUpperCase()}`, `${field} must be a non-negative integer`);
  }
  return text;
}

function optionalSearchString(value: any, field: string) {
  if (value === undefined || value === null) {
    return null;
  }

  if (typeof value !== "string") {
    throw badRequest(`INVALID_${field.toUpperCase()}`, `${field} must be a string`);
  }

  const normalized = value.trim();
  if (normalized.length === 0) {
    return null;
  }
  if (normalized.length > 64) {
    throw badRequest(`INVALID_${field.toUpperCase()}`, `${field} must be 64 characters or fewer`);
  }
  return normalized;
}

function requiredId(value: any) {
  if (typeof value !== "string") {
    throw badRequest("INVALID_GLOBAL_ID", "id is required");
  }

  const id = value.trim();
  if (id.length === 0) {
    throw badRequest("INVALID_GLOBAL_ID", "id is required");
  }
  if (id.length > 128) {
    throw badRequest("INVALID_GLOBAL_ID", "id must be 128 characters or fewer");
  }
  return id;
}

function pickDecodedNumber(decoded: any, names: string[]) {
  for (const name of names) {
    const value = decoded?.[name];
    if (value !== undefined && value !== null) {
      return toJsonSafe(value);
    }
  }
  return null;
}

function toJsonSafe(value: any): any {
  if (typeof value === "bigint") {
    return value.toString();
  }
  if (value instanceof Date) {
    return value.toISOString();
  }
  if (Array.isArray(value)) {
    return value.map(toJsonSafe);
  }
  if (value && typeof value === "object") {
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, toJsonSafe(item)]));
  }
  return value;
}

function toTimeString(value: any) {
  if (value === undefined || value === null || value === "") {
    return null;
  }
  if (value instanceof Date) {
    return value.toISOString();
  }
  return String(value);
}

function pickDecodedTime(decoded: any) {
  const direct = decoded?.created_at || decoded?.createdAt;
  if (direct !== undefined && direct !== null && direct !== "") {
    return toTimeString(direct);
  }

  const createdAtMs = pickDecodedNumber(decoded, ["created_at_ms", "createdAtMs", "unix_ms", "unixMs"]);
  if (createdAtMs !== null) {
    return new Date(Number(createdAtMs)).toISOString();
  }

  return null;
}

function normalizeDecoded(rawId: string, decoded: any) {
  const createdAt = pickDecodedTime(decoded);

  return {
    raw_id: toJsonSafe(decoded?.raw_id || decoded?.rawId || rawId),
    normalized_id: toJsonSafe(decoded?.normalized_id || decoded?.normalizedId || rawId),
    id_kind: toJsonSafe(decoded?.id_kind || decoded?.idKind || decoded?.kind || null),
    numeric_id: toJsonSafe(decoded?.numeric_id || decoded?.numericId || decoded?.id || null),
    created_at: createdAt,
    created_at_ms: pickDecodedNumber(decoded, ["created_at_ms", "createdAtMs", "unix_ms", "unixMs"]),
    origin_id: pickDecodedNumber(decoded, ["origin_id", "originId"]),
    worker_id: pickDecodedNumber(decoded, ["worker_id", "workerId"]),
    sequence: toJsonSafe(decoded?.sequence ?? null),
    decoder: toJsonSafe(decoded)
  };
}

function enrichDecodeResult(decoded: any, origin: any, worldAtCreate: any, currentWorld: any, mergeContext: any) {
  return {
    ...decoded,
    origin_key: origin?.origin_key || null,
    origin,
    world_at_create: worldAtCreate,
    current_world: currentWorld,
    merge_context: mergeContext
  };
}

@ApiTags("global-id")
@ApiBearerAuth()
@UseGuards(JwtAuthGuard, AdminPolicyGuard)
@Controller("/api/v1/global-id")
export class GlobalIdController {
  decodeGlobalIdInput = decodeGlobalId;

  constructor(@Inject(ADMIN_STORE) private readonly adminStore: any) {}

  @Get("decode")
  @Permissions("id.read")
  async decode(@Query("id") idParam: any) {
    const id = requiredId(idParam);
    const decoded = normalizeDecoded(id, await this.decodeGlobalIdInput(id));

    let origin = null;
    let worldAtCreate = null;
    let currentWorld = null;
    let mergeContext = null;
    if (decoded.origin_id !== null && decoded.origin_id !== undefined) {
      origin = await this.adminStore.findIdOrigin(decoded.origin_id);
      currentWorld = await this.adminStore.findCurrentWorldMembership(decoded.origin_id);

      if (decoded.created_at) {
        worldAtCreate = await this.adminStore.findWorldMembershipAt({
          originId: decoded.origin_id,
          createdAt: decoded.created_at
        });
        mergeContext = await this.adminStore.findMergeContext({
          originId: decoded.origin_id,
          createdAt: decoded.created_at,
          worldId: worldAtCreate?.world_id || currentWorld?.world_id || null
        });
      }
    }

    return {
      ok: true,
      decoded: enrichDecodeResult(decoded, origin, worldAtCreate, currentWorld, mergeContext)
    };
  }

  @Get("origins")
  @Permissions("id.read")
  async origins(@Query() query: any) {
    const filters = {
      originId: optionalPositiveInteger(query.origin_id, "origin_id"),
      originKey: optionalSearchString(query.origin_key, "origin_key"),
      limit: pageLimit(query.limit),
      offset: pageOffset(query.offset)
    };
    const origins = await this.adminStore.findIdOrigins(filters);
    const total = await this.adminStore.countIdOrigins(filters);

    return {
      ok: true,
      origins,
      total,
      limit: filters.limit,
      offset: filters.offset
    };
  }

  @Get("worlds")
  @Permissions("id.read")
  async worlds(@Query() query: any) {
    const filters = {
      worldId: optionalPositiveInteger(query.world_id, "world_id"),
      worldKey: optionalSearchString(query.world_key, "world_key"),
      originId: optionalPositiveInteger(query.origin_id, "origin_id"),
      limit: pageLimit(query.limit),
      offset: pageOffset(query.offset)
    };
    const worlds = await this.adminStore.findWorlds(filters);
    const total = await this.adminStore.countWorlds(filters);

    return {
      ok: true,
      worlds,
      total,
      limit: filters.limit,
      offset: filters.offset
    };
  }

  @Get("merge-events")
  @Permissions("id.read")
  async mergeEvents(@Query() query: any) {
    const filters = {
      worldId: optionalPositiveInteger(query.world_id, "world_id"),
      originId: optionalPositiveInteger(query.origin_id, "origin_id"),
      limit: pageLimit(query.limit),
      offset: pageOffset(query.offset)
    };
    const mergeEvents = await this.adminStore.findWorldMergeEvents(filters);
    const total = await this.adminStore.countWorldMergeEvents(filters);

    return {
      ok: true,
      mergeEvents,
      total,
      limit: filters.limit,
      offset: filters.offset
    };
  }
}
