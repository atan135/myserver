import { Controller, Get, HttpCode, HttpStatus, Param, Post, Query } from "@nestjs/common";
import { ApiTags } from "@nestjs/swagger";

import { MonitoringService } from "./monitoring.service.js";

@ApiTags("monitoring")
@Controller("/api/admin/monitoring")
export class MonitoringController {
  constructor(private readonly monitoringService: MonitoringService) {}

  @Get("services")
  services() {
    return this.monitoringService.services();
  }

  @Get("services/:name/metrics")
  metrics(@Param("name") name: string, @Query("window") window = "5m") {
    return this.monitoringService.metrics(name, window);
  }

  @Post("archive")
  @HttpCode(HttpStatus.OK)
  archive() {
    return this.monitoringService.archive();
  }
}
