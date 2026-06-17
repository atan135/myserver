import { Body, Controller, Get, Inject, Param, Put, Query, Req, UseGuards } from "@nestjs/common";
import { ApiBearerAuth, ApiTags } from "@nestjs/swagger";

import { JwtAuthGuard } from "../auth/jwt-auth.guard.js";
import { Permissions, roleHasPermission } from "../auth/roles.decorator.js";
import { RolesGuard } from "../auth/roles.guard.js";
import { getClientIp } from "../common/client-ip.js";
import { badRequest, forbidden, notFound } from "../common/http-exception.js";
import { ADMIN_CONFIG, ADMIN_STORE } from "../tokens.js";

const PLAYER_STATUSES = ["active", "disabled", "banned", "pending_review"];

function pageLimit(value: any) {
  return Math.min(Number(value) || 50, 100);
}

function pageOffset(value: any) {
  return Number(value) || 0;
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
}
