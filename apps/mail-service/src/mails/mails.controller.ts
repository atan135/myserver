import { Body, Controller, Get, Headers, HttpCode, HttpStatus, Inject, Param, Post, Put, Query } from "@nestjs/common";
import { ApiCreatedResponse, ApiOkResponse, ApiOperation, ApiTags } from "@nestjs/swagger";

import { forbidden, unauthorized } from "../common/http-exception.js";
import { authenticatePlayerHeaders, validateServiceToken } from "../mail-auth.js";
import { MAIL_CONFIG, MAIL_PLAYER_AUTH } from "../tokens.js";
import { MailsService } from "./mails.service.js";

@ApiTags("mails")
@Controller("/api/v1/mails")
export class MailsController {
  constructor(
    private readonly mailsService: MailsService,
    @Inject(MAIL_CONFIG) private readonly config: any,
    @Inject(MAIL_PLAYER_AUTH) private readonly playerAuth: any
  ) {}

  private async authenticatePlayer(headers: any, queryOrBody: any = {}) {
    if (!this.config.mailPlayerAuthRequired) {
      return { playerId: queryOrBody?.player_id };
    }

    try {
      return await authenticatePlayerHeaders(headers, this.playerAuth);
    } catch (error: any) {
      throw unauthorized(error?.code || "INVALID_TICKET", error?.message || "invalid ticket");
    }
  }

  private authenticateService(headers: any) {
    try {
      validateServiceToken(headers, this.config);
    } catch (error: any) {
      if (error?.statusCode === 403) {
        throw forbidden(error.code || "MAIL_SERVICE_TOKEN_INVALID", error.message || "mail service token is invalid");
      }
      throw unauthorized(error?.code || "MAIL_SERVICE_TOKEN_REQUIRED", error?.message || "mail service token is required");
    }
  }

  @Get()
  @ApiOperation({ summary: "List player mails" })
  @ApiOkResponse({ schema: { example: { ok: true, mails: [], unread_count: 0 } } })
  async list(@Headers() headers: any, @Query() query: any) {
    const auth = await this.authenticatePlayer(headers, query);
    return this.mailsService.list(auth.playerId, query);
  }

  @Get(":mailId")
  @ApiOperation({ summary: "Get mail detail" })
  async get(@Param("mailId") mailId: string, @Headers() headers: any, @Query() query: any) {
    const auth = await this.authenticatePlayer(headers, query);
    return this.mailsService.get(mailId, auth.playerId, query);
  }

  @Post()
  @ApiOperation({ summary: "Send mail" })
  @ApiCreatedResponse({ schema: { example: { ok: true, mail_id: "mail-001" } } })
  create(@Headers() headers: any, @Body() body: any) {
    this.authenticateService(headers);
    return this.mailsService.create(body);
  }

  @Put(":mailId/read")
  @HttpCode(HttpStatus.OK)
  @ApiOperation({ summary: "Mark mail as read" })
  async markRead(@Param("mailId") mailId: string, @Headers() headers: any, @Body() body: any) {
    const auth = await this.authenticatePlayer(headers, body);
    return this.mailsService.markRead(mailId, auth.playerId, body);
  }

  @Post(":mailId/claim")
  @HttpCode(HttpStatus.OK)
  @ApiOperation({ summary: "Claim mail attachment" })
  async claim(@Param("mailId") mailId: string, @Headers() headers: any, @Body() body: any) {
    const auth = await this.authenticatePlayer(headers, body);
    return this.mailsService.claim(mailId, auth.playerId, body);
  }
}
