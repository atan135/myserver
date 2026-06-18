import { Controller, Get, HttpCode, HttpStatus, Param, Post, Query, UseGuards } from "@nestjs/common";
import { ApiBearerAuth, ApiTags } from "@nestjs/swagger";

import { JwtAuthGuard } from "../auth/jwt-auth.guard.js";
import { Permissions } from "../auth/roles.decorator.js";
import { RolesGuard } from "../auth/roles.guard.js";
import { MonitoringService } from "./monitoring.service.js";

@ApiTags("monitoring")
@ApiBearerAuth()
@UseGuards(JwtAuthGuard, RolesGuard)
@Controller("/api/admin/monitoring")
export class MonitoringController {
  constructor(private readonly monitoringService: MonitoringService) {}

  @Get("services")
  @Permissions("monitoring.read")
  services() {
    return this.monitoringService.services();
  }

  @Get("registry")
  @Permissions("monitoring.read")
  registry() {
    return this.monitoringService.registry();
  }

  @Get("services/:name/metrics")
  @Permissions("monitoring.read")
  metrics(@Param("name") name: string, @Query("window") window = "5m") {
    return this.monitoringService.metrics(name, window);
  }

  @Get("rollout-drain")
  @Permissions("monitoring.read")
  rolloutDrain() {
    return this.monitoringService.rolloutDrain();
  }

  @Post("archive")
  @Permissions("monitoring.archive")
  @HttpCode(HttpStatus.OK)
  archive() {
    return this.monitoringService.archive();
  }
}
