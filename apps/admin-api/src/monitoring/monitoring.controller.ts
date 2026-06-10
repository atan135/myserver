import { Controller, Get, HttpCode, HttpStatus, Param, Post, Query, UseGuards } from "@nestjs/common";
import { ApiBearerAuth, ApiTags } from "@nestjs/swagger";

import { JwtAuthGuard } from "../auth/jwt-auth.guard.js";
import { Roles } from "../auth/roles.decorator.js";
import { RolesGuard } from "../auth/roles.guard.js";
import { MonitoringService } from "./monitoring.service.js";

@ApiTags("monitoring")
@ApiBearerAuth()
@UseGuards(JwtAuthGuard, RolesGuard)
@Controller("/api/admin/monitoring")
export class MonitoringController {
  constructor(private readonly monitoringService: MonitoringService) {}

  @Get("services")
  @Roles("viewer", "operator", "admin")
  services() {
    return this.monitoringService.services();
  }

  @Get("services/:name/metrics")
  @Roles("viewer", "operator", "admin")
  metrics(@Param("name") name: string, @Query("window") window = "5m") {
    return this.monitoringService.metrics(name, window);
  }

  @Post("archive")
  @Roles("admin")
  @HttpCode(HttpStatus.OK)
  archive() {
    return this.monitoringService.archive();
  }
}
