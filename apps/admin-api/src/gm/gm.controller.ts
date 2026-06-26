import { Body, Controller, HttpCode, HttpStatus, Inject, Post, Req, UseGuards } from "@nestjs/common";
import { ApiBearerAuth, ApiTags } from "@nestjs/swagger";

import { JwtAuthGuard } from "../auth/jwt-auth.guard.js";
import { Permissions } from "../auth/roles.decorator.js";
import { RolesGuard } from "../auth/roles.guard.js";
import { getClientIp } from "../common/client-ip.js";
import { ApiHttpException, badRequest, notFound } from "../common/http-exception.js";
import { encodeSubjectToken } from "../nats-client.js";
import { ADMIN_CONFIG, ADMIN_GAME_ADMIN_CLIENT, ADMIN_NATS, ADMIN_STORE } from "../tokens.js";
import { computeBanExpiresAt } from "../ban-utils.js";
import { randomUUID } from "node:crypto";

const GM_BAN_DURATION_MAX_SECONDS = 31_536_000;
const GM_BROADCAST_SUBJECT = "myserver.gm.broadcast";
const GAME_ADMIN_ACTOR_PATTERN = /^[A-Za-z0-9._@-]{1,128}$/;

function gameServerError(error: any) {
  const code = error?.code || "GAME_SERVER_ERROR";
  const statusCode = code === "GAME_SERVER_ADMIN_TARGET_REQUIRED"
    ? 400
    : code === "GAME_SERVER_ADMIN_TARGET_NOT_FOUND"
      ? 404
      : 502;
  return new ApiHttpException(statusCode, {
    ok: false,
    error: code,
    message: error.message
  });
}

function gameServerFailure(error: any) {
  const failure: any = {
    ok: false,
    error: error?.code || "GAME_SERVER_ERROR",
    message: error?.message || "game-server error"
  };
  if (error?.gameAdminEndpoint) {
    failure.endpoint = error.gameAdminEndpoint;
    failure.instanceId = error.gameAdminEndpoint.instanceId;
  }
  if (error?.gameAdminInstances) {
    failure.instances = error.gameAdminInstances;
  }
  return failure;
}

function isGameAdminTargetSelectionError(error: any) {
  return error?.code === "GAME_SERVER_ADMIN_TARGET_REQUIRED" ||
    error?.code === "GAME_SERVER_ADMIN_TARGET_NOT_FOUND";
}

function sessionKickPublishError(globalKick: any, legacyResult: any) {
  return new ApiHttpException(502, {
    ok: false,
    error: "SESSION_KICK_PUBLISH_FAILED",
    message: globalKick.message || "failed to publish global session kick",
    globalKick,
    legacyResult
  });
}

function globalBroadcastPublishError(globalBroadcast: any, legacyBroadcast: any) {
  return new ApiHttpException(502, {
    ok: false,
    error: "GM_BROADCAST_PUBLISH_FAILED",
    message:
      globalBroadcast.message ||
      "failed to publish global GM broadcast; legacy single-instance fallback result is attached",
    partialDelivered: legacyBroadcast?.ok === true,
    globalBroadcast,
    legacyBroadcast
  });
}

function normalizePlayerId(playerId: any) {
  if (!playerId || typeof playerId !== "string" || playerId.trim().length === 0) {
    throw badRequest("INVALID_PLAYER_ID", "playerId is required");
  }
  return playerId.trim();
}

function normalizeCharacterId(characterId: any) {
  if (!characterId || typeof characterId !== "string" || characterId.trim().length === 0) {
    throw badRequest("INVALID_CHARACTER_ID", "characterId is required");
  }
  return characterId.trim();
}

function normalizeGmReason(reason: any, prefix: "gm_kick" | "gm_ban") {
  if (reason === undefined || reason === null) {
    return prefix;
  }
  if (typeof reason !== "string") {
    throw badRequest("INVALID_REASON", "reason must be a string");
  }
  const normalized = reason.trim();
  return normalized.length > 0 ? `${prefix}:${normalized}` : prefix;
}

function normalizeGameAdminActor(req: any) {
  const admin = req?.admin || {};
  const candidates = [admin.username, admin.sub !== undefined && admin.sub !== null ? `admin-${admin.sub}` : null];

  for (const candidate of candidates) {
    if (candidate === undefined || candidate === null) {
      continue;
    }
    const actor = String(candidate).trim();
    if (GAME_ADMIN_ACTOR_PATTERN.test(actor)) {
      return actor;
    }
  }

  return undefined;
}

function normalizeTargetInstanceId(value: any) {
  if (value === undefined || value === null || value === "") {
    return undefined;
  }
  if (typeof value !== "string") {
    throw badRequest("INVALID_TARGET_INSTANCE_ID", "targetInstanceId must be a string");
  }
  const normalized = value.trim();
  return normalized.length > 0 ? normalized : undefined;
}

function createGameAdminOptions(req: any, body: any = {}) {
  return {
    actor: normalizeGameAdminActor(req),
    targetInstanceId: normalizeTargetInstanceId(body?.targetInstanceId ?? body?.target_instance_id)
  };
}

async function preflightSingleTarget(gameAdminClient: any, gameAdminOptions: any) {
  try {
    return await gameAdminClient.resolveAdminEndpoint({
      ...gameAdminOptions,
      requireExplicitTarget: true
    });
  } catch (error: any) {
    if (isGameAdminTargetSelectionError(error)) {
      throw gameServerError(error);
    }
    return null;
  }
}

@ApiTags("gm")
@ApiBearerAuth()
@UseGuards(JwtAuthGuard, RolesGuard)
@Controller("/api/v1/gm")
export class GmController {
  constructor(
    @Inject(ADMIN_CONFIG) private readonly config: any,
    @Inject(ADMIN_STORE) private readonly adminStore: any,
    @Inject(ADMIN_NATS) private readonly nats: any,
    @Inject(ADMIN_GAME_ADMIN_CLIENT) private readonly gameAdminClient: any
  ) {}

  private async publishGlobalSessionKick(playerId: string, reason: string) {
    const subject = `myserver.session.kick.${encodeSubjectToken(playerId)}`;
    const payload = { player_id: playerId, reason };

    try {
      await this.nats.publishJson(subject, payload);
      return { ok: true, subject, payload };
    } catch (error: any) {
      return {
        ok: false,
        error: error?.code || "NATS_PUBLISH_FAILED",
        message: error?.message || "failed to publish global session kick",
        subject,
        payload
      };
    }
  }

  private async publishGlobalBroadcast(title: string, content: string, sender: string) {
    const payload = {
      broadcast_id: randomUUID(),
      title,
      content,
      sender,
      created_at: new Date().toISOString()
    };

    try {
      await this.nats.publishJson(GM_BROADCAST_SUBJECT, payload);
      return { ok: true, subject: GM_BROADCAST_SUBJECT, payload };
    } catch (error: any) {
      return {
        ok: false,
        error: error?.code || "NATS_PUBLISH_FAILED",
        message: error?.message || "failed to publish global GM broadcast",
        subject: GM_BROADCAST_SUBJECT,
        payload
      };
    }
  }

  @Post("broadcast")
  @Permissions("gm.broadcast")
  @HttpCode(HttpStatus.OK)
  async broadcast(@Body() body: any, @Req() req: any) {
    const { title, content, sender } = body || {};

    if (!title || typeof title !== "string" || title.trim().length === 0) {
      throw badRequest("INVALID_TITLE", "title is required");
    }

    if (!content || typeof content !== "string" || content.trim().length === 0) {
      throw badRequest("INVALID_CONTENT", "content is required");
    }

    const normalizedTitle = title.trim();
    const normalizedContent = content.trim();
    const normalizedSender = typeof sender === "string" && sender.trim().length > 0 ? sender.trim() : "System";
    const gameAdminOptions = createGameAdminOptions(req, body);
    const globalBroadcast = await this.publishGlobalBroadcast(
      normalizedTitle,
      normalizedContent,
      normalizedSender
    );

    let legacyBroadcast: any = {
      ok: true,
      skipped: true,
      reason: "global_broadcast_published"
    };
    if (!globalBroadcast.ok) {
      try {
        const result = await this.gameAdminClient.broadcast(
          normalizedTitle,
          normalizedContent,
          normalizedSender,
          gameAdminOptions
        );
        legacyBroadcast = { ...result, fallback: true };
      } catch (error: any) {
        legacyBroadcast = gameServerFailure(error);
        legacyBroadcast.fallback = true;
      }
    }

    await this.adminStore.appendAuditLog({
      adminId: req.admin.sub,
      adminUsername: req.admin.username,
      action: "gm_broadcast",
      targetType: "system",
      targetValue: "all",
      details: {
        title: normalizedTitle,
        content: normalizedContent,
        sender: normalizedSender,
        requestedTargetInstanceId: gameAdminOptions.targetInstanceId,
        globalBroadcast,
        legacyBroadcast
      },
      ip: getClientIp(req, this.config)
    });

    if (!globalBroadcast.ok) {
      throw globalBroadcastPublishError(globalBroadcast, legacyBroadcast);
    }

    return {
      ok: true,
      message: "Broadcast sent",
      globalBroadcast,
      legacyBroadcast
    };
  }

  @Post("send-item")
  @Permissions("gm.send_item")
  @HttpCode(HttpStatus.OK)
  async sendItem(@Body() body: any, @Req() req: any) {
    const { characterId, itemId, itemCount, reason } = body || {};
    const normalizedCharacterId = normalizeCharacterId(characterId);

    if (!itemId || typeof itemId !== "string") {
      throw badRequest("INVALID_ITEM_ID", "itemId is required");
    }

    if (!itemCount || typeof itemCount !== "number" || itemCount <= 0) {
      throw badRequest("INVALID_ITEM_COUNT", "itemCount must be a positive number");
    }

    try {
      const gameAdminOptions = createGameAdminOptions(req, body);
      const gameAdminResult = await this.gameAdminClient.sendItem(
        normalizedCharacterId,
        itemId,
        itemCount,
        reason || "",
        gameAdminOptions
      );

      await this.adminStore.appendAuditLog({
        adminId: req.admin.sub,
        adminUsername: req.admin.username,
        action: "gm_send_item",
        targetType: "character",
        targetValue: normalizedCharacterId,
        details: {
          itemId,
          itemCount,
          reason,
          requestedTargetInstanceId: gameAdminOptions.targetInstanceId,
          gameAdmin: {
            ok: gameAdminResult?.ok === true,
            instanceId: gameAdminResult?.instanceId,
            endpoint: gameAdminResult?.endpoint
          }
        },
        ip: getClientIp(req, this.config)
      });

      return { ok: true, message: "Item sent" };
    } catch (error: any) {
      throw gameServerError(error);
    }
  }

  @Post("kick-player")
  @Permissions("gm.kick_player")
  @HttpCode(HttpStatus.OK)
  async kickPlayer(@Body() body: any, @Req() req: any) {
    const { playerId, reason } = body || {};
    const normalizedPlayerId = normalizePlayerId(playerId);
    const normalizedReason = normalizeGmReason(reason, "gm_kick");
    const gameAdminOptions = createGameAdminOptions(req, body);
    const targetEndpoint = await preflightSingleTarget(this.gameAdminClient, gameAdminOptions);

    const globalKick = await this.publishGlobalSessionKick(normalizedPlayerId, normalizedReason);

    let legacyKick: any = { ok: true };
    try {
      legacyKick = await this.gameAdminClient.kickPlayer(normalizedPlayerId, normalizedReason, {
        ...gameAdminOptions,
        endpoint: targetEndpoint
      });
    } catch (error: any) {
      if (isGameAdminTargetSelectionError(error)) {
        throw gameServerError(error);
      }
      legacyKick = gameServerFailure(error);
    }

    await this.adminStore.appendAuditLog({
      adminId: req.admin.sub,
      adminUsername: req.admin.username,
      action: "gm_kick_player",
      targetType: "player",
      targetValue: normalizedPlayerId,
      details: {
        reason: normalizedReason,
        globalKick,
        legacyKick,
        requestedTargetInstanceId: gameAdminOptions.targetInstanceId
      },
      ip: getClientIp(req, this.config)
    });

    if (!globalKick.ok) {
      throw sessionKickPublishError(globalKick, legacyKick);
    }

    return { ok: true, message: "Player kicked", globalKick, legacyKick };
  }

  @Post("ban-player")
  @Permissions("gm.ban_player")
  @HttpCode(HttpStatus.OK)
  async banPlayer(@Body() body: any, @Req() req: any) {
    const { playerId, durationSeconds, reason } = body || {};

    if (
      !durationSeconds ||
      typeof durationSeconds !== "number" ||
      !Number.isInteger(durationSeconds) ||
      durationSeconds <= 0 ||
      durationSeconds > GM_BAN_DURATION_MAX_SECONDS
    ) {
      throw badRequest("INVALID_DURATION", "durationSeconds must be a positive integer no greater than 31536000");
    }

    const normalizedPlayerId = normalizePlayerId(playerId);
    const normalizedReason = normalizeGmReason(reason, "gm_ban");
    const gameAdminOptions = createGameAdminOptions(req, body);
    const targetEndpoint = await preflightSingleTarget(this.gameAdminClient, gameAdminOptions);
    const player = await this.adminStore.findPlayerById(normalizedPlayerId);
    if (!player) {
      throw notFound("PLAYER_NOT_FOUND", "Player not found");
    }

    const banExpiresAt = computeBanExpiresAt(durationSeconds);
    const updated = await this.adminStore.updatePlayerStatus(normalizedPlayerId, "banned", { banExpiresAt });
    if (!updated) {
      throw notFound("PLAYER_NOT_FOUND", "Player not found");
    }

    const globalKick = await this.publishGlobalSessionKick(normalizedPlayerId, normalizedReason);

    let legacyBan: any = { ok: true };
    try {
      legacyBan = await this.gameAdminClient.banPlayer(
        normalizedPlayerId,
        durationSeconds,
        normalizedReason,
        {
          ...gameAdminOptions,
          endpoint: targetEndpoint
        }
      );
    } catch (error: any) {
      if (isGameAdminTargetSelectionError(error)) {
        throw gameServerError(error);
      }
      legacyBan = gameServerFailure(error);
    }

    await this.adminStore.appendAuditLog({
      adminId: req.admin.sub,
      adminUsername: req.admin.username,
      action: "gm_ban_player",
      targetType: "player",
      targetValue: normalizedPlayerId,
      details: {
        from: player.status,
        to: "banned",
        durationSeconds,
        banExpiresAt,
        reason: normalizedReason,
        globalKick,
        legacyBan,
        requestedTargetInstanceId: gameAdminOptions.targetInstanceId
      },
      ip: getClientIp(req, this.config)
    });

    if (!globalKick.ok) {
      return {
        ok: false,
        error: "SESSION_KICK_PUBLISH_FAILED",
        message: globalKick.message || "Player banned, but global session kick failed",
        banStatus: "banned",
        banExpiresAt,
        globalKick,
        legacyBan
      };
    }

    return { ok: true, message: "Player banned", banExpiresAt, globalKick, legacyBan };
  }
}
