import { Body, Controller, Get, HttpCode, HttpStatus, Param, Post, Put, Query } from "@nestjs/common";
import { ApiCreatedResponse, ApiOkResponse, ApiOperation, ApiTags } from "@nestjs/swagger";

import { MailsService } from "./mails.service.js";

@ApiTags("mails")
@Controller("/api/v1/mails")
export class MailsController {
  constructor(private readonly mailsService: MailsService) {}

  @Get()
  @ApiOperation({ summary: "List player mails" })
  @ApiOkResponse({ schema: { example: { ok: true, mails: [], unread_count: 0 } } })
  list(@Query() query: any) {
    return this.mailsService.list(query);
  }

  @Get(":mailId")
  @ApiOperation({ summary: "Get mail detail" })
  get(@Param("mailId") mailId: string) {
    return this.mailsService.get(mailId);
  }

  @Post()
  @ApiOperation({ summary: "Send mail" })
  @ApiCreatedResponse({ schema: { example: { ok: true, mail_id: "mail-001" } } })
  create(@Body() body: any) {
    return this.mailsService.create(body);
  }

  @Put(":mailId/read")
  @HttpCode(HttpStatus.OK)
  @ApiOperation({ summary: "Mark mail as read" })
  markRead(@Param("mailId") mailId: string, @Body() body: any) {
    return this.mailsService.markRead(mailId, body);
  }

  @Post(":mailId/claim")
  @HttpCode(HttpStatus.OK)
  @ApiOperation({ summary: "Claim mail attachment" })
  claim(@Param("mailId") mailId: string, @Body() body: any) {
    return this.mailsService.claim(mailId, body);
  }
}
