import { Body, Controller, Get, Inject, Param, Put, Query, Req, UseGuards } from "@nestjs/common";
import { ApiBearerAuth, ApiTags } from "@nestjs/swagger";

import { JwtAuthGuard } from "../auth/jwt-auth.guard.js";
import { Permissions, roleHasPermission } from "../auth/roles.decorator.js";
import { RolesGuard } from "../auth/roles.guard.js";
import { getClientIp } from "../common/client-ip.js";
import { badRequest, forbidden, notFound } from "../common/http-exception.js";
import { ADMIN_CONFIG, ADMIN_STORE } from "../tokens.js";
import { getTitleDefinitions } from "./title-table.js";

const PLAYER_STATUSES = ["active", "disabled", "banned", "pending_review"];
const DEFAULT_TITLE_LOG_LIMIT = 20;
const MAX_TITLE_LOG_LIMIT = 100;

function pageLimit(value: any) {
  return Math.min(Number(value) || 50, 100);
}

function pageOffset(value: any) {
  return Number(value) || 0;
}

function requiredCharacterId(value: any) {
  if (typeof value !== "string") {
    throw badRequest("INVALID_CHARACTER_ID", "characterId is required");
  }

  const characterId = value.trim();
  if (characterId.length === 0) {
    throw badRequest("INVALID_CHARACTER_ID", "characterId is required");
  }
  if (characterId.length > 64) {
    throw badRequest("INVALID_CHARACTER_ID", "characterId must be 64 characters or fewer");
  }
  return characterId;
}

function titleLogLimit(value: any) {
  if (value === undefined || value === null || value === "") {
    return DEFAULT_TITLE_LOG_LIMIT;
  }

  const text = String(value).trim();
  if (!/^\d+$/.test(text)) {
    throw badRequest("INVALID_LOG_LIMIT", "logLimit must be a positive integer");
  }

  const limit = Number.parseInt(text, 10);
  if (limit <= 0) {
    throw badRequest("INVALID_LOG_LIMIT", "logLimit must be a positive integer");
  }
  return Math.min(limit, MAX_TITLE_LOG_LIMIT);
}

function enrichTitle(title: any, titleDefinitions: Record<string, any>) {
  const definition = titleDefinitions[String(title.title_id)] || null;

  return {
    ...title,
    title_definition: definition,
    name: definition?.name || null,
    title_type: definition?.title_type || null,
    rarity: definition?.rarity || null,
    icon: definition?.icon || null,
    color: definition?.color || null,
    hidden: definition?.hidden === true,
    limited: definition?.limited === true,
    sort_order: definition?.sort_order ?? null
  };
}

@ApiTags("players")
@ApiBearerAuth()
@UseGuards(JwtAuthGuard, RolesGuard)
@Controller("/api/v1/players")
export class PlayersController {
  constructor(
    @Inject(ADMIN_CONFIG) private readonly config: any,
    @Inject(ADMIN_STORE) private readonly adminStore: any
  ) {}

  @Get()
  @Permissions("players.read")
  async list(@Query() query: any) {
    const { login_name, guest_id, status, limit = 50, offset = 0 } = query;

    const players = await this.adminStore.findPlayers({
      loginName: login_name,
      guestId: guest_id,
      status,
      limit: pageLimit(limit),
      offset: pageOffset(offset)
    });

    const total = await this.adminStore.countPlayers({
      loginName: login_name,
      guestId: guest_id,
      status
    });

    return {
      ok: true,
      players,
      total,
      limit: pageLimit(limit),
      offset: pageOffset(offset)
    };
  }

  @Get("characters/:characterId/titles")
  @Permissions("players.read")
  async characterTitles(
    @Param("characterId") characterIdParam: string,
    @Query("logLimit") logLimitParam: any,
    @Req() req: any
  ) {
    let characterId = typeof characterIdParam === "string" ? characterIdParam.trim() : "";

    try {
      characterId = requiredCharacterId(characterIdParam);
      const logLimit = titleLogLimit(logLimitParam);
      const overview = await this.adminStore.findCharacterTitleOverview({ characterId, logLimit });
      const titleDefinitions = getTitleDefinitions();
      const titles = overview.titles.map((title: any) => enrichTitle(title, titleDefinitions));
      const equippedTitle = overview.equippedTitle
        ? enrichTitle(overview.equippedTitle, titleDefinitions)
        : null;

      await this.appendCharacterTitleQueryAudit(req, {
        action: "character_titles_query",
        characterId,
        result: "success",
        logLimit,
        titleCount: titles.length,
        disciplineCount: overview.disciplines.length,
        titleLogCount: overview.titleLogs.length
      });

      return {
        ok: true,
        characterId,
        titles,
        equippedTitle,
        disciplines: overview.disciplines,
        titleLogs: overview.titleLogs
      };
    } catch (error: any) {
      await this.appendCharacterTitleQueryAudit(req, {
        action: "character_titles_query_failed",
        characterId: characterId || null,
        result: "failed",
        error: error?.getResponse?.().error || error?.code || error?.message || "UNKNOWN_ERROR"
      });
      throw error;
    }
  }

  @Get(":playerId")
  @Permissions("players.read")
  async detail(@Param("playerId") playerId: string) {
    const player = await this.adminStore.findPlayerById(playerId);
    if (!player) {
      throw notFound("PLAYER_NOT_FOUND", "Player not found");
    }

    return { ok: true, player };
  }

  @Put(":playerId/status")
  @Permissions("players.status.update")
  async updateStatus(@Param("playerId") playerId: string, @Body() body: any, @Req() req: any) {
    const { status } = body || {};

    if (!status || !PLAYER_STATUSES.includes(status)) {
      throw badRequest("INVALID_STATUS", "status must be active, disabled, banned, or pending_review");
    }

    if (status === "banned" && !roleHasPermission(req.admin.role, "players.ban")) {
      throw forbidden("INSUFFICIENT_PERMISSION", "Insufficient permission");
    }

    const player = await this.adminStore.findPlayerById(playerId);
    if (!player) {
      throw notFound("PLAYER_NOT_FOUND", "Player not found");
    }

    await this.adminStore.updatePlayerStatus(playerId, status);

    await this.adminStore.appendAuditLog({
      adminId: req.admin.sub,
      adminUsername: req.admin.username,
      action: "player_status_change",
      targetType: "player",
      targetValue: playerId,
      details: {
        from: player.status,
        to: status,
        previousBanExpiresAt: player.banExpiresAt || null,
        banExpiresAt: null
      },
      ip: getClientIp(req, this.config)
    });

    return { ok: true, message: "Player status updated", banExpiresAt: null };
  }

  private async appendCharacterTitleQueryAudit(
    req: any,
    {
      action,
      characterId,
      result,
      logLimit,
      titleCount,
      disciplineCount,
      titleLogCount,
      error
    }: {
      action: string;
      characterId: string | null;
      result: string;
      logLimit?: number;
      titleCount?: number;
      disciplineCount?: number;
      titleLogCount?: number;
      error?: string;
    }
  ) {
    await this.adminStore.appendAuditLog({
      adminId: req.admin.sub,
      adminUsername: req.admin.username,
      action,
      targetType: "character",
      targetValue: characterId,
      details: {
        result,
        logLimit,
        titleCount,
        disciplineCount,
        titleLogCount,
        error
      },
      ip: getClientIp(req, this.config)
    });
  }
}
