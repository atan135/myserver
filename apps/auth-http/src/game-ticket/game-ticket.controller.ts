import { Body, Controller, HttpCode, HttpStatus, Inject, Post, Req } from "@nestjs/common";
import { ApiOperation, ApiTags } from "@nestjs/swagger";

import { unauthorized, badRequest, forbidden, serviceUnavailable } from "../common/http-exception.js";
import { getClientIp } from "../common/client-ip.js";
import { log } from "../logger.js";
import { AuthService } from "../auth/auth.service.js";
import { AUTH_BLOCKLIST, AUTH_CHARACTER_STORE, AUTH_CONFIG, AUTH_DB_STORE, AUTH_STORE } from "../tokens.js";

const LOGINABLE_CHARACTER_STATUSES = new Set(["active"]);
const CHARACTER_ID_PATTERN = /^chr_[0-9a-hjkmnp-tv-z]+$/;

function logSecurity(level: string, message: string, extra: Record<string, unknown>) {
  try {
    log(level, message, extra);
  } catch {
    // Focused tests may instantiate the controller before logger bootstrap.
  }
}

function getBearerToken(req: any): string | null {
  const authorization = req.headers.authorization;
  if (!authorization?.startsWith("Bearer ")) {
    return null;
  }

  return authorization.slice("Bearer ".length).trim();
}

function normalizeCharacterId(input: unknown): string {
  if (typeof input !== "string" || input.trim().length === 0) {
    throw badRequest("MISSING_CHARACTER_ID", "character_id must be a non-empty string");
  }

  const characterId = input.trim();
  if (!CHARACTER_ID_PATTERN.test(characterId)) {
    throw badRequest("INVALID_CHARACTER_ID", "character_id has invalid format");
  }

  return characterId;
}

@ApiTags("game-ticket")
@Controller("/api/v1/game-ticket")
export class GameTicketController {
  constructor(
    @Inject(AUTH_STORE) private readonly authStore: any,
    @Inject(AUTH_CONFIG) private readonly config: any,
    @Inject(AUTH_BLOCKLIST) private readonly blocklist: any,
    @Inject(AUTH_DB_STORE) private readonly dbStore: any,
    @Inject(AUTH_CHARACTER_STORE) private readonly characterStore: any,
    private readonly authService: AuthService
  ) {}

  @Post("issue")
  @ApiOperation({ summary: "Issue a character-bound game ticket for the current session" })
  async issue(@Req() req: any, @Body() body: any = {}) {
    const accessToken = getBearerToken(req);
    if (!accessToken) {
      throw unauthorized("MISSING_BEARER_TOKEN");
    }

    const session = await this.authStore.getSessionByAccessToken(accessToken);
    if (!session) {
      throw unauthorized("INVALID_ACCESS_TOKEN");
    }

    await this.authService.assertNotInMaintenance();
    const characterId = normalizeCharacterId(body?.character_id ?? body?.characterId);

    const clientIp = getClientIp(req, this.config);
    try {
      await this.authStore.assertPlayerCanIssueTicket(session.playerId, clientIp);
    } catch (error: any) {
      if (error.code === "ACCOUNT_DISABLED") {
        throw forbidden("ACCOUNT_DISABLED", "Account is disabled");
      }
      throw error;
    }

    const decision = await this.blocklist.checkPlayer(session.playerId);
    if (decision.unavailable) {
      logSecurity("warn", "security.blocklist_unavailable", {
        targetType: "player",
        playerId: session.playerId,
        clientIp,
        path: req.url
      });
      await this.dbStore?.appendSecurityAudit?.({
        eventType: "blocklist_unavailable",
        targetType: "player",
        targetValue: session.playerId,
        clientIp,
        severity: "critical",
        details: { path: req.url, source: "game_ticket_issue" }
      });
      throw serviceUnavailable("BLOCKLIST_UNAVAILABLE", "redis blocklist is unavailable");
    }
    if (decision.blocked) {
      logSecurity("warn", "security.player_blocked", {
        playerId: session.playerId,
        clientIp,
        path: req.url
      });
      await this.dbStore?.appendSecurityAudit?.({
        eventType: "player_blocked",
        targetType: "player",
        targetValue: session.playerId,
        clientIp,
        severity: "critical",
        details: { path: req.url, source: "game_ticket_issue" }
      });
      throw forbidden("PLAYER_BLOCKED", "player is blocked");
    }

    if (!this.characterStore?.enabled) {
      throw serviceUnavailable("CHARACTER_STORE_UNAVAILABLE", "character store is unavailable");
    }

    const character = await this.characterStore.getByCharacterId(characterId);
    if (!character || character.deletedAt) {
      throw forbidden("CHARACTER_NOT_FOUND", "character is not available to the current account");
    }
    if (character.accountPlayerId !== session.playerId) {
      throw forbidden("CHARACTER_OWNER_MISMATCH", "character does not belong to current account");
    }
    if (!LOGINABLE_CHARACTER_STATUSES.has(character.status)) {
      throw forbidden("CHARACTER_NOT_LOGINABLE", "character status does not allow login");
    }

    const ticket = await this.authStore.issueGameTicket(session.playerId, clientIp, {
      characterId: character.characterId,
      worldId: character.worldId
    });
    const services = await this.authService.buildServicePayload();
    const gameProxy = this.authService.getGameProxyDescriptor(services);
    if (!gameProxy) {
      throw serviceUnavailable("SERVICE_DISCOVERY_UNAVAILABLE", "game-proxy client endpoint is unavailable");
    }

    return {
      ok: true,
      playerId: session.playerId,
      characterId: character.characterId,
      worldId: character.worldId,
      ticket: ticket.value,
      ticketExpiresAt: ticket.expiresAt,
      gameProxyHost: gameProxy.host,
      gameProxyPort: gameProxy.port,
      services
    };
  }

  @Post("validate")
  @HttpCode(HttpStatus.OK)
  @ApiOperation({ summary: "Validate a character-bound game ticket" })
  async validate(@Body() body: any) {
    const { ticket } = body || {};
    if (!ticket || typeof ticket !== "string") {
      throw badRequest("INVALID_TICKET", "ticket must be a non-empty string");
    }

    try {
      const payload = await this.authStore.validateGameTicket(ticket);
      return {
        ok: true,
        playerId: payload.playerId,
        characterId: payload.characterId,
        worldId: payload.worldId ?? null,
        exp: payload.exp,
        ver: payload.ver ?? 1
      };
    } catch (error: any) {
      if (error.code === "MISSING_CHARACTER_ID") {
        throw unauthorized("MISSING_CHARACTER_ID");
      }
      if (
        error.code === "INVALID_TICKET_FORMAT" ||
        error.code === "INVALID_TICKET_SIGNATURE" ||
        error.code === "INVALID_TICKET_PAYLOAD" ||
        error.code === "INVALID_TICKET_EXP" ||
        error.code === "TICKET_EXPIRED" ||
        error.code === "TICKET_NOT_FOUND" ||
        error.code === "TICKET_REVOKED"
      ) {
        throw unauthorized(error.code);
      }
      throw error;
    }
  }

  @Post("revoke")
  @HttpCode(HttpStatus.OK)
  @ApiOperation({ summary: "Revoke a game ticket" })
  async revoke(@Req() req: any, @Body() body: any) {
    const accessToken = getBearerToken(req);
    if (!accessToken) {
      throw unauthorized("MISSING_BEARER_TOKEN");
    }

    const session = await this.authStore.getSessionByAccessToken(accessToken);
    if (!session) {
      throw unauthorized("INVALID_ACCESS_TOKEN");
    }

    const { ticket } = body || {};
    if (!ticket || typeof ticket !== "string") {
      throw badRequest("INVALID_TICKET", "ticket must be a non-empty string");
    }

    try {
      await this.authStore.revokeTicket(ticket, getClientIp(req, this.config), {
        expectedPlayerId: session.playerId
      });
    } catch (error: any) {
      if (error.code === "TICKET_OWNER_MISMATCH") {
        throw forbidden("TICKET_OWNER_MISMATCH", "ticket does not belong to current player");
      }
      throw error;
    }

    return {
      ok: true,
      message: "Ticket revoked"
    };
  }
}
