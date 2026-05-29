import { Body, Controller, Delete, Get, HttpCode, HttpStatus, Param, Post, Put, Query } from "@nestjs/common";
import { ApiCreatedResponse, ApiOkResponse, ApiOperation, ApiTags } from "@nestjs/swagger";

import { AnnouncementsService } from "./announcements.service.js";

@ApiTags("announcements")
@Controller("/api/v1/announcements")
export class AnnouncementsController {
  constructor(private readonly announcementsService: AnnouncementsService) {}

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
  create(@Body() body: any) {
    return this.announcementsService.create(body);
  }

  @Put(":announceId")
  @ApiOperation({ summary: "Update announcement" })
  update(@Param("announceId") announceId: string, @Body() body: any) {
    return this.announcementsService.update(announceId, body);
  }

  @Delete(":announceId")
  @HttpCode(HttpStatus.OK)
  @ApiOperation({ summary: "Delete announcement" })
  delete(@Param("announceId") announceId: string) {
    return this.announcementsService.delete(announceId);
  }
}
