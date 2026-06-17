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

import { authenticateAnnounceReadHeaders } from "../announce-auth.js";
import { forbidden, unauthorized } from "../common/http-exception.js";
import { ANNOUNCE_CONFIG, ANNOUNCE_READ_AUTH } from "../tokens.js";
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
    @Inject(ANNOUNCE_CONFIG) private readonly config: any,
    @Inject(ANNOUNCE_READ_AUTH) private readonly readAuth: any
  ) {}

  @Get()
  @ApiOperation({ summary: "List announcements" })
  @ApiOkResponse({ schema: { example: { ok: true, announcements: [], limit: 50, offset: 0 } } })
  async list(
    @Headers() headers: Record<string, string | string[] | undefined>,
    @Query() query: any
  ) {
    await this.requireReadAccess(headers || {});
    return this.announcementsService.list(query);
  }

  @Get(":announceId")
  @ApiOperation({ summary: "Get announcement detail" })
  async get(
    @Param("announceId") announceId: string,
    @Headers() headers: Record<string, string | string[] | undefined>
  ) {
    await this.requireReadAccess(headers || {});
    return this.announcementsService.get(announceId);
  }

  @Post()
  @ApiOperation({ summary: "Create announcement" })
  @ApiCreatedResponse({ schema: { example: { ok: true, announcement: { announce_id: "ann_1j7qv8m4x2" } } } })
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

  private async requireReadAccess(headers: Record<string, string | string[] | undefined>) {
    try {
      await authenticateAnnounceReadHeaders(headers, this.readAuth, this.config);
    } catch (error: any) {
      if (error?.statusCode === 403) {
        throw forbidden(
          error.code || "ANNOUNCE_READ_TOKEN_INVALID",
          error.message || "Announcement read token is invalid"
        );
      }

      throw unauthorized(
        error?.code || "ANNOUNCE_READ_AUTH_REQUIRED",
        error?.message || "Announcement read APIs require a read token or game ticket"
      );
    }
  }
}
