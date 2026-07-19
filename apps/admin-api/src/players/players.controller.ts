import { Body, Controller, Get, Inject, Param, Post, Put, Query, Req, UseGuards } from "@nestjs/common";
import { ApiBearerAuth, ApiTags } from "@nestjs/swagger";

import { JwtAuthGuard } from "../auth/jwt-auth.guard.js";
import { PermissionResolver, Permissions } from "../auth/roles.decorator.js";
import { AdminPolicyGuard } from "../auth/admin-policy.guard.js";
import { getClientIp } from "../common/client-ip.js";
import { badRequest, notFound } from "../common/http-exception.js";
import { ADMIN_CONFIG, ADMIN_STORE } from "../tokens.js";
import { getTitleDefinitions } from "./title-table.js";

const PLAYER_STATUSES = ["active", "disabled", "banned", "pending_review"];
const DEFAULT_TITLE_LOG_LIMIT = 20;
const MAX_TITLE_LOG_LIMIT = 100;
const CHARACTER_ID_PATTERN = /^chr_[0-9a-hjkmnp-tv-z]+$/;
const CHARACTER_NAME_PATTERN = /^[\p{Script=Han}A-Za-z0-9_-]+$/u;

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

function requiredGlobalCharacterId(value: any) {
  const characterId = requiredCharacterId(value);
  if (!CHARACTER_ID_PATTERN.test(characterId)) {
    throw badRequest("INVALID_CHARACTER_ID", "characterId has invalid format");
  }
  return characterId;
}

function requiredReason(value: any) {
  if (typeof value !== "string") {
    throw badRequest("MISSING_REASON", "reason is required");
  }

  const reason = value.trim();
  if (reason.length === 0) {
    throw badRequest("MISSING_REASON", "reason is required");
  }
  if (reason.length > 255) {
    throw badRequest("INVALID_REASON", "reason must be 255 characters or fewer");
  }
  return reason;
}

function optionalSafeObject(value: any, fallback: Record<string, unknown> = {}) {
  if (value === undefined || value === null) {
    return fallback;
  }

  if (typeof value !== "object" || Array.isArray(value)) {
    throw badRequest("INVALID_CHARACTER_PAYLOAD", "character object fields must be JSON objects");
  }
  return value;
}

function optionalInteger(value: any, fallback: number, fieldName: string) {
  if (value === undefined || value === null || value === "") {
    return fallback;
  }

  const parsed = Number.parseInt(String(value), 10);
  if (!Number.isSafeInteger(parsed)) {
    throw badRequest("INVALID_CHARACTER_PAYLOAD", `${fieldName} must be an integer`);
  }
  return parsed;
}

function normalizeCharacterName(value: any) {
  if (typeof value !== "string") {
    throw badRequest("INVALID_CHARACTER_NAME", "name is required");
  }

  const name = value.trim();
  if (name.length === 0) {
    throw badRequest("INVALID_CHARACTER_NAME", "name is required");
  }
  if (Array.from(name).length > 64) {
    throw badRequest("INVALID_CHARACTER_NAME", "name must be 64 characters or fewer");
  }
  if (/\s/u.test(name) || !CHARACTER_NAME_PATTERN.test(name)) {
    throw badRequest("INVALID_CHARACTER_NAME", "name may only contain Chinese characters, letters, numbers, underscore, and hyphen");
  }
  return name;
}

function errorCodeOf(error: any) {
  return error?.getResponse?.().error || error?.code || error?.message || "UNKNOWN_ERROR";
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
@UseGuards(JwtAuthGuard, AdminPolicyGuard)
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

  @Get(":playerId/characters")
  @Permissions("players.read")
  async playerCharacters(@Param("playerId") playerId: string, @Query() query: any) {
    const player = await this.adminStore.findPlayerById(playerId);
    if (!player) {
      throw notFound("PLAYER_NOT_FOUND", "Player not found");
    }

    const includeDeleted = query?.includeDeleted !== "false" && query?.include_deleted !== "false";
    const limit = pageLimit(query?.limit);
    const offset = pageOffset(query?.offset);
    const [characters, total] = await Promise.all([
      this.adminStore.findCharactersByAccountPlayerId(playerId, { includeDeleted, limit, offset }),
      this.adminStore.countCharactersByAccountPlayerId(playerId, { includeDeleted })
    ]);

    return {
      ok: true,
      playerId,
      characters,
      total,
      limit,
      offset
    };
  }

  @Get("characters/:characterId/profile")
  @Permissions("players.read")
  async characterProfile(
    @Param("characterId") characterIdParam: string,
    @Query("logLimit") logLimitParam: any,
    @Req() req: any
  ) {
    let characterId = typeof characterIdParam === "string" ? characterIdParam.trim() : "";

    try {
      characterId = requiredCharacterId(characterIdParam);
      const logLimit = titleLogLimit(logLimitParam);
      const overview = await this.adminStore.findCharacterProfileOverview({ characterId, logLimit });
      if (!overview) {
        throw notFound("CHARACTER_NOT_FOUND", "Character not found");
      }

      const titleDefinitions = getTitleDefinitions();
      const titles = overview.titles.map((title: any) => enrichTitle(title, titleDefinitions));
      const equippedTitle = overview.equippedTitle
        ? enrichTitle(overview.equippedTitle, titleDefinitions)
        : null;

      await this.appendCharacterProfileQueryAudit(req, {
        action: "character_profile_query",
        characterId,
        result: "success",
        logLimit,
        titleCount: titles.length,
        disciplineCount: overview.disciplines.length,
        elementLogCount: overview.elementLogs.length,
        titleLogCount: overview.titleLogs.length,
        disciplineLogCount: overview.disciplineLogs.length
      });

      return {
        ok: true,
        characterId,
        character: overview.character,
        attributes: overview.character.attributes,
        titles,
        equippedTitle,
        disciplines: overview.disciplines,
        logs: {
          elements: overview.elementLogs,
          titles: overview.titleLogs,
          disciplines: overview.disciplineLogs
        }
      };
    } catch (error: any) {
      await this.appendCharacterProfileQueryAudit(req, {
        action: "character_profile_query_failed",
        characterId: characterId || null,
        result: "failed",
        error: error?.getResponse?.().error || error?.code || error?.message || "UNKNOWN_ERROR"
      });
      throw error;
    }
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

  @Post(":playerId/characters")
  @Permissions("players.status.update")
  async createCharacterForPlayer(@Param("playerId") playerId: string, @Body() body: any, @Req() req: any) {
    let reason: string | null = null;
    let targetCharacterId: string | null = null;

    try {
      reason = this.getRequiredReasonForAudit(body);
      const player = await this.adminStore.findPlayerById(playerId);
      if (!player) {
        throw notFound("PLAYER_NOT_FOUND", "Player not found");
      }

      const input = this.normalizeAdminCharacterCreateInput(playerId, body);
      const character = await this.adminStore.createCharacterForAdmin(input);
      targetCharacterId = character.character_id || character.characterId;

      await this.appendCharacterLifecycleAudit(req, {
        action: "admin_character_create",
        targetValue: targetCharacterId,
        details: {
          result: "success",
          reason,
          targetAccountPlayerId: playerId,
          bypassCharacterLimit: true,
          characterId: targetCharacterId,
          characterName: character.name,
          worldId: character.world_id ?? character.worldId,
          permission: "players.status.update"
        }
      });

      return {
        ok: true,
        character,
        audit: {
          action: "admin_character_create",
          targetType: "character",
          targetValue: targetCharacterId
        }
      };
    } catch (error: any) {
      await this.appendCharacterLifecycleAudit(req, {
        action: "admin_character_create_failed",
        targetValue: targetCharacterId,
        details: {
          result: "failed",
          reason,
          targetAccountPlayerId: playerId,
          bypassCharacterLimit: true,
          error: errorCodeOf(error),
          permission: "players.status.update"
        }
      });
      throw error;
    }
  }

  @Post("characters/:characterId/restore")
  @Permissions("players.status.update")
  async restoreCharacter(@Param("characterId") characterIdParam: string, @Body() body: any, @Req() req: any) {
    let reason: string | null = null;
    let characterId: string | null = null;

    try {
      reason = this.getRequiredReasonForAudit(body);
      characterId = requiredGlobalCharacterId(characterIdParam);
      const existing = await this.adminStore.findCharacterById(characterId, { includeDeleted: true });
      if (!existing) {
        throw notFound("CHARACTER_NOT_FOUND", "Character not found");
      }

      if (!existing.deleted_at && !existing.deletedAt) {
        throw badRequest("CHARACTER_NOT_DELETED", "character is not deleted");
      }
      if (existing.status !== "deleted") {
        throw badRequest("CHARACTER_NOT_RESTORABLE", "character status is not restorable");
      }

      const fromStatus = existing.status;
      const restored = await this.adminStore.restoreCharacterForAdmin(characterId);
      if (!restored) {
        throw badRequest("CHARACTER_NOT_RESTORABLE", "character is not restorable");
      }

      await this.appendCharacterLifecycleAudit(req, {
        action: "admin_character_restore",
        targetValue: characterId,
        details: {
          result: "success",
          reason,
          targetAccountPlayerId: restored.account_player_id ?? restored.accountPlayerId,
          bypassCharacterLimit: true,
          characterId,
          fromStatus,
          toStatus: restored.status,
          permission: "players.status.update"
        }
      });

      return {
        ok: true,
        character: restored,
        audit: {
          action: "admin_character_restore",
          targetType: "character",
          targetValue: characterId
        }
      };
    } catch (error: any) {
      await this.appendCharacterLifecycleAudit(req, {
        action: "admin_character_restore_failed",
        targetValue: characterId,
        details: {
          result: "failed",
          reason,
          bypassCharacterLimit: true,
          characterId,
          error: errorCodeOf(error),
          permission: "players.status.update"
        }
      });
      throw error;
    }
  }

  @Put(":playerId/status")
  @PermissionResolver((request) => request?.body?.status === "banned"
    ? ["players.status.update", "players.ban"]
    : ["players.status.update"])
  async updateStatus(@Param("playerId") playerId: string, @Body() body: any, @Req() req: any) {
    const { status } = body || {};

    if (!status || !PLAYER_STATUSES.includes(status)) {
      throw badRequest("INVALID_STATUS", "status must be active, disabled, banned, or pending_review");
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

  private async appendCharacterProfileQueryAudit(
    req: any,
    {
      action,
      characterId,
      result,
      logLimit,
      titleCount,
      disciplineCount,
      elementLogCount,
      titleLogCount,
      disciplineLogCount,
      error
    }: {
      action: string;
      characterId: string | null;
      result: string;
      logLimit?: number;
      titleCount?: number;
      disciplineCount?: number;
      elementLogCount?: number;
      titleLogCount?: number;
      disciplineLogCount?: number;
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
        elementLogCount,
        titleLogCount,
        disciplineLogCount,
        error
      },
      ip: getClientIp(req, this.config)
    });
  }

  private getRequiredReasonForAudit(body: any) {
    return requiredReason(body?.reason);
  }

  private normalizeAdminCharacterCreateInput(playerId: string, body: any) {
    return {
      accountPlayerId: playerId,
      name: normalizeCharacterName(body?.name),
      worldId: optionalInteger(body?.worldId ?? body?.world_id, 0, "worldId"),
      appearance: optionalSafeObject(body?.appearance ?? body?.appearance_json, {}),
      position: optionalSafeObject(body?.position, {
        scene_id: 100,
        x: 0,
        y: 0,
        dir_x: 0,
        dir_y: 1
      }),
      affinity: optionalSafeObject(body?.affinity, {
        earth: 2500,
        fire: 2500,
        water: 2500,
        wind: 2500
      }),
      mastery: optionalSafeObject(body?.mastery, {
        earth: 0,
        fire: 0,
        water: 0,
        wind: 0
      })
    };
  }

  private async appendCharacterLifecycleAudit(
    req: any,
    {
      action,
      targetValue,
      details
    }: {
      action: string;
      targetValue: string | null;
      details: Record<string, unknown>;
    }
  ) {
    await this.adminStore.appendAuditLog({
      adminId: req.admin.sub,
      adminUsername: req.admin.username,
      action,
      targetType: "character",
      targetValue,
      details,
      ip: getClientIp(req, this.config)
    });
  }
}
