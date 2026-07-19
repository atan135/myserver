import { Controller, Get, Inject, Query, Req, UseGuards } from "@nestjs/common";
import { ApiBearerAuth, ApiTags } from "@nestjs/swagger";

import { JwtAuthGuard } from "../auth/jwt-auth.guard.js";
import { AdminPolicyGuard } from "../auth/admin-policy.guard.js";
import { Permissions } from "../auth/roles.decorator.js";
import { getClientIp } from "../common/client-ip.js";
import { ApiHttpException, badRequest } from "../common/http-exception.js";
import { ADMIN_CONFIG, ADMIN_STORE } from "../tokens.js";

const MAX_LIMIT = 100;
const MAX_UNSCOPED_WINDOW_MS = 31 * 24 * 60 * 60 * 1000;

function optionalText(value: unknown, field: string, maxLength = 128) {
  if (value === undefined || value === null || value === "") return undefined;
  if (typeof value !== "string") {
    throw badRequest("INVALID_ASSET_LEDGER_QUERY", `${field} must be a string`);
  }
  const normalized = value.trim();
  if (!normalized || normalized.length > maxLength) {
    throw badRequest("INVALID_ASSET_LEDGER_QUERY", `${field} is invalid`);
  }
  return normalized;
}

function pageLimit(value: unknown) {
  if (value === undefined || value === null || value === "") return 50;
  if (!/^\d+$/.test(String(value))) {
    throw badRequest("INVALID_ASSET_LEDGER_QUERY", "limit must be an integer");
  }
  return Math.min(Number(value), MAX_LIMIT);
}

function pageOffset(value: unknown) {
  if (value === undefined || value === null || value === "") return 0;
  if (!/^\d+$/.test(String(value))) {
    throw badRequest("INVALID_ASSET_LEDGER_QUERY", "offset must be an integer");
  }
  return Number(value);
}

function timestamp(value: unknown, field: string) {
  const normalized = optionalText(value, field, 64);
  if (!normalized) return undefined;
  const parsed = Date.parse(normalized);
  if (Number.isNaN(parsed)) {
    throw badRequest("INVALID_ASSET_LEDGER_QUERY", `${field} must be an ISO timestamp`);
  }
  return new Date(parsed).toISOString();
}

function normalizeFilters(query: any) {
  const filters = {
    characterId: optionalText(query.character_id ?? query.characterId, "character_id"),
    requestId: optionalText(query.request_id ?? query.requestId, "request_id"),
    originType: optionalText(query.origin_type ?? query.originType ?? query.origin, "origin_type", 32),
    originId: optionalText(query.origin_id ?? query.originId, "origin_id"),
    deliveryId: optionalText(query.delivery_id ?? query.deliveryId, "delivery_id"),
    from: timestamp(query.from, "from"),
    to: timestamp(query.to, "to")
  };

  if (!Object.values(filters).some(Boolean)) {
    throw badRequest(
      "ASSET_LEDGER_FILTER_REQUIRED",
      "character_id, request_id, origin, delivery_id, or time range is required"
    );
  }
  if (filters.from && filters.to && Date.parse(filters.to) < Date.parse(filters.from)) {
    throw badRequest("INVALID_ASSET_LEDGER_QUERY", "to must not be before from");
  }
  if (!filters.characterId && !filters.requestId && filters.from && filters.to &&
      Date.parse(filters.to) - Date.parse(filters.from) > MAX_UNSCOPED_WINDOW_MS) {
    throw badRequest("ASSET_LEDGER_TIME_RANGE_TOO_LARGE", "unscoped time range must be 31 days or less");
  }
  return filters;
}

@ApiTags("assets")
@ApiBearerAuth()
@UseGuards(JwtAuthGuard, AdminPolicyGuard)
@Controller("/api/v1/assets")
export class AssetLedgerController {
  constructor(
    @Inject(ADMIN_CONFIG) private readonly config: any,
    @Inject(ADMIN_STORE) private readonly adminStore: any
  ) {}

  @Get("ledger")
  @Permissions("assets.ledger.read")
  async ledger(@Query() query: any, @Req() req: any) {
    let filters: any = null;
    try {
      filters = normalizeFilters(query || {});
      const limit = pageLimit(query?.limit);
      const offset = pageOffset(query?.offset);
      const [entries, total] = await Promise.all([
        this.adminStore.getAssetLedger({ ...filters, limit, offset }),
        this.adminStore.countAssetLedger(filters)
      ]);

      await this.appendQueryAudit(req, filters, "success", { limit, offset, resultCount: entries.length });
      return { ok: true, entries, total, limit, offset };
    } catch (error: any) {
      await this.appendQueryAudit(req, filters, "failed", {
        error: error?.getResponse?.().error || error?.code || "ASSET_LEDGER_QUERY_FAILED"
      });
      if (error?.message === "GAME_DATABASE_UNAVAILABLE") {
        throw new ApiHttpException(503, {
          ok: false,
          error: "GAME_DATABASE_UNAVAILABLE",
          message: "game database is unavailable"
        });
      }
      throw error;
    }
  }

  private async appendQueryAudit(req: any, filters: any, result: string, details: Record<string, unknown>) {
    await this.adminStore.appendAuditLog({
      adminId: req.admin.sub,
      adminUsername: req.admin.username,
      action: "asset_ledger_query",
      targetType: filters?.characterId ? "character" : "asset_ledger",
      targetValue: filters?.characterId || filters?.requestId || filters?.deliveryId || null,
      details: {
        result,
        characterId: filters?.characterId || null,
        requestId: filters?.requestId || null,
        originType: filters?.originType || null,
        originId: filters?.originId || null,
        deliveryId: filters?.deliveryId || null,
        from: filters?.from || null,
        to: filters?.to || null,
        ...details
      },
      ip: getClientIp(req, this.config)
    });
  }
}
