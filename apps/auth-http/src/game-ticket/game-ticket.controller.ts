import { Body, Controller, HttpCode, HttpStatus, Inject, Post, Req } from "@nestjs/common";
import { ApiOperation, ApiTags } from "@nestjs/swagger";

import { unauthorized, badRequest, forbidden } from "../common/http-exception.js";
import { getClientIp } from "../common/client-ip.js";
import { AUTH_CONFIG, AUTH_STORE } from "../tokens.js";
import { AuthService } from "../auth/auth.service.js";

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

    const ticket = await this.authStore.issueGameTicket(session.playerId, getClientIp(req, this.config));
    const services = await this.authService.buildServicePayload();

    return {
      ok: true,
      playerId: session.playerId,
      ticket: ticket.value,
      ticketExpiresAt: ticket.expiresAt,
      gameProxyHost: this.authService.gameProxyHost,
      gameProxyPort: this.authService.gameProxyPort,
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
