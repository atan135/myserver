import { Body, Controller, HttpCode, HttpStatus, Inject, Post, Req } from "@nestjs/common";
import { ApiOperation, ApiTags } from "@nestjs/swagger";

import { unauthorized, badRequest, forbidden, serviceUnavailable } from "../common/http-exception.js";
import { getClientIp } from "../common/client-ip.js";
import { log } from "../logger.js";
import { AuthService } from "../auth/auth.service.js";
import { AUTH_BLOCKLIST, AUTH_CONFIG, AUTH_DB_STORE, AUTH_STORE } from "../tokens.js";

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

@ApiTags("game-ticket")
@Controller("/api/v1/game-ticket")
export class GameTicketController {
  constructor(
    @Inject(AUTH_STORE) private readonly authStore: any,
    @Inject(AUTH_CONFIG) private readonly config: any,
    @Inject(AUTH_BLOCKLIST) private readonly blocklist: any,
    @Inject(AUTH_DB_STORE) private readonly dbStore: any,
    private readonly authService: AuthService
  ) {}

  @Post("issue")
  @ApiOperation({ summary: "Issue a game ticket for the current session" })
  async issue(@Req() req: any) {
    const accessToken = getBearerToken(req);
    if (!accessToken) {
      throw unauthorized("MISSING_BEARER_TOKEN");
    }

    const session = await this.authStore.getSessionByAccessToken(accessToken);
    if (!session) {
      throw unauthorized("INVALID_ACCESS_TOKEN");
    }

    await this.authService.assertNotInMaintenance();

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

    const ticket = await this.authStore.issueGameTicket(session.playerId, clientIp);
    const services = await this.authService.buildServicePayload();
    const gameProxy = this.authService.getGameProxyDescriptor(services);

    return {
      ok: true,
      playerId: session.playerId,
      ticket: ticket.value,
      ticketExpiresAt: ticket.expiresAt,
      gameProxyHost: gameProxy.host,
      gameProxyPort: gameProxy.port,
      services
    };
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
