import { Body, Controller, HttpCode, HttpStatus, Inject, Post, Req, UseGuards } from "@nestjs/common";
import { ApiBearerAuth, ApiTags } from "@nestjs/swagger";

import { JwtAuthGuard } from "../auth/jwt-auth.guard.js";
import { Roles } from "../auth/roles.decorator.js";
import { RolesGuard } from "../auth/roles.guard.js";
import { getClientIp } from "../common/client-ip.js";
import { ApiHttpException, badRequest, notFound } from "../common/http-exception.js";
import { ADMIN_CONFIG, ADMIN_GAME_ADMIN_CLIENT, ADMIN_STORE } from "../tokens.js";

const GM_BAN_DURATION_MAX_SECONDS = 31_536_000;

function gameServerError(error: any) {
  return new ApiHttpException(502, {
    ok: false,
    error: "GAME_SERVER_ERROR",
    message: error.message
  });
}

function gameServerFailure(error: any) {
  return {
    ok: false,
    error: error?.code || "GAME_SERVER_ERROR",
    message: error?.message || "game-server error"
  };
}

@ApiTags("gm")
@ApiBearerAuth()
@UseGuards(JwtAuthGuard, RolesGuard)
@Controller("/api/v1/gm")
export class GmController {
  constructor(
    @Inject(ADMIN_CONFIG) private readonly config: any,
    @Inject(ADMIN_STORE) private readonly adminStore: any,
    @Inject(ADMIN_GAME_ADMIN_CLIENT) private readonly gameAdminClient: any
  ) {}

  @Post("broadcast")
  @Roles("operator", "admin")
  @HttpCode(HttpStatus.OK)
  async broadcast(@Body() body: any, @Req() req: any) {
    const { title, content, sender } = body || {};

    if (!title || typeof title !== "string" || title.trim().length === 0) {
      throw badRequest("INVALID_TITLE", "title is required");
    }

    if (!content || typeof content !== "string" || content.trim().length === 0) {
      throw badRequest("INVALID_CONTENT", "content is required");
    }

    try {
      await this.gameAdminClient.broadcast(title.trim(), content.trim(), sender || "System");

      await this.adminStore.appendAuditLog({
        adminId: req.admin.sub,
        adminUsername: req.admin.username,
        action: "gm_broadcast",
        targetType: "system",
        targetValue: "all",
        details: { title, content, sender },
        ip: getClientIp(req, this.config)
      });

      return { ok: true, message: "Broadcast sent" };
    } catch (error: any) {
      throw gameServerError(error);
    }
  }

  @Post("send-item")
  @Roles("operator", "admin")
  @HttpCode(HttpStatus.OK)
  async sendItem(@Body() body: any, @Req() req: any) {
    const { playerId, itemId, itemCount, reason } = body || {};

    if (!playerId || typeof playerId !== "string") {
      throw badRequest("INVALID_PLAYER_ID", "playerId is required");
    }

    if (!itemId || typeof itemId !== "string") {
      throw badRequest("INVALID_ITEM_ID", "itemId is required");
    }

    if (!itemCount || typeof itemCount !== "number" || itemCount <= 0) {
      throw badRequest("INVALID_ITEM_COUNT", "itemCount must be a positive number");
    }

    try {
      await this.gameAdminClient.sendItem(playerId, itemId, itemCount, reason || "");

      await this.adminStore.appendAuditLog({
        adminId: req.admin.sub,
        adminUsername: req.admin.username,
        action: "gm_send_item",
        targetType: "player",
        targetValue: playerId,
        details: { itemId, itemCount, reason },
        ip: getClientIp(req, this.config)
      });

      return { ok: true, message: "Item sent" };
    } catch (error: any) {
      throw gameServerError(error);
    }
  }

  @Post("kick-player")
  @Roles("operator", "admin")
  @HttpCode(HttpStatus.OK)
  async kickPlayer(@Body() body: any, @Req() req: any) {
    const { playerId, reason } = body || {};

    if (!playerId || typeof playerId !== "string") {
      throw badRequest("INVALID_PLAYER_ID", "playerId is required");
    }

    try {
      await this.gameAdminClient.kickPlayer(playerId, reason || "");

      await this.adminStore.appendAuditLog({
        adminId: req.admin.sub,
        adminUsername: req.admin.username,
        action: "gm_kick_player",
        targetType: "player",
        targetValue: playerId,
        details: { reason },
        ip: getClientIp(req, this.config)
      });

      return { ok: true, message: "Player kicked" };
    } catch (error: any) {
      throw gameServerError(error);
    }
  }

  @Post("ban-player")
  @Roles("admin")
  @HttpCode(HttpStatus.OK)
  async banPlayer(@Body() body: any, @Req() req: any) {
    const { playerId, durationSeconds, reason } = body || {};

    if (!playerId || typeof playerId !== "string" || playerId.trim().length === 0) {
      throw badRequest("INVALID_PLAYER_ID", "playerId is required");
    }

    if (
      !durationSeconds ||
      typeof durationSeconds !== "number" ||
      !Number.isInteger(durationSeconds) ||
      durationSeconds <= 0 ||
      durationSeconds > GM_BAN_DURATION_MAX_SECONDS
    ) {
      throw badRequest("INVALID_DURATION", "durationSeconds must be a positive integer no greater than 31536000");
    }

    const normalizedPlayerId = playerId.trim();
    const normalizedReason = typeof reason === "string" ? reason.trim() : "";
    const player = await this.adminStore.findPlayerById(normalizedPlayerId);
    if (!player) {
      throw notFound("PLAYER_NOT_FOUND", "Player not found");
    }

    const updated = await this.adminStore.updatePlayerStatus(normalizedPlayerId, "banned");
    if (!updated) {
      throw notFound("PLAYER_NOT_FOUND", "Player not found");
    }

    let onlineKick: any = { ok: true };
    try {
      await this.gameAdminClient.banPlayer(normalizedPlayerId, durationSeconds, normalizedReason);
    } catch (error: any) {
      onlineKick = gameServerFailure(error);
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
        reason: normalizedReason,
        onlineKick
      },
      ip: getClientIp(req, this.config)
    });

    return { ok: true, message: "Player banned", onlineKick };
  }
}
