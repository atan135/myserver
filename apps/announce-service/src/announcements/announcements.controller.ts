import {
  Body,
  Controller,
  Delete,
  Get,
  Headers,
  HttpCode,
  HttpStatus,
  Inject,
  Param,
  Post,
  Put,
  Query
} from "@nestjs/common";
import { ApiCreatedResponse, ApiOkResponse, ApiOperation, ApiTags } from "@nestjs/swagger";

import { forbidden, unauthorized } from "../common/http-exception.js";
import { ANNOUNCE_CONFIG } from "../tokens.js";
import { AnnouncementsService } from "./announcements.service.js";

function firstHeaderValue(value: string | string[] | undefined): string {
  return Array.isArray(value) ? value[0] || "" : value || "";
}

function getHeaderValue(
  headers: Record<string, string | string[] | undefined>,
  name: string
): string {
  const direct = firstHeaderValue(headers[name]);
  if (direct) {
    return direct;
  }

  const lowerName = name.toLowerCase();
  for (const [key, value] of Object.entries(headers)) {
    if (key.toLowerCase() === lowerName) {
      return firstHeaderValue(value);
    }
  }

  return "";
}

function extractAdminToken(headers: Record<string, string | string[] | undefined>): string {
  const authorization = getHeaderValue(headers, "authorization").trim();
  if (authorization.toLowerCase().startsWith("bearer ")) {
    return authorization.slice(7).trim();
  }

  return getHeaderValue(headers, "x-admin-token").trim();
}

@ApiTags("announcements")
@Controller("/api/v1/announcements")
export class AnnouncementsController {
  constructor(
    private readonly announcementsService: AnnouncementsService,
    @Inject(ANNOUNCE_CONFIG) private readonly config: any
  ) {}

  @Get()
  @ApiOperation({ summary: "List announcements" })
  @ApiOkResponse({ schema: { example: { ok: true, announcements: [], limit: 50, offset: 0 } } })
  list(@Query() query: any) {
    return this.announcementsService.list(query);
  }

  @Get(":announceId")
  @ApiOperation({ summary: "Get announcement detail" })
  get(@Param("announceId") announceId: string) {
    return this.announcementsService.get(announceId);
  }

  @Post()
  @ApiOperation({ summary: "Create announcement" })
  @ApiCreatedResponse({ schema: { example: { ok: true, announcement: {} } } })
  create(@Headers() headers: Record<string, string | string[] | undefined>, @Body() body: any) {
    this.requireAdminToken(headers);
    return this.announcementsService.create(body);
  }

  @Put(":announceId")
  @ApiOperation({ summary: "Update announcement" })
  update(
    @Param("announceId") announceId: string,
    @Headers() headers: Record<string, string | string[] | undefined>,
    @Body() body: any
  ) {
    this.requireAdminToken(headers);
    return this.announcementsService.update(announceId, body);
  }

  @Delete(":announceId")
  @HttpCode(HttpStatus.OK)
  @ApiOperation({ summary: "Delete announcement" })
  delete(
    @Param("announceId") announceId: string,
    @Headers() headers: Record<string, string | string[] | undefined>
  ) {
    this.requireAdminToken(headers);
    return this.announcementsService.delete(announceId);
  }

  private requireAdminToken(headers: Record<string, string | string[] | undefined>) {
    const token = extractAdminToken(headers || {});
    if (!token) {
      throw unauthorized(
        "ANNOUNCE_ADMIN_TOKEN_REQUIRED",
        "Announcement write APIs require an admin token"
      );
    }

    if (token !== this.config.announceAdminToken) {
      throw forbidden(
        "ANNOUNCE_ADMIN_TOKEN_INVALID",
        "Announcement admin token is invalid"
      );
    }
  }
}
