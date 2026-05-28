import { Controller, Get } from "@nestjs/common";
import { ApiTags } from "@nestjs/swagger";

@ApiTags("health")
@Controller()
export class HealthController {
  @Get("/healthz")
  healthz() {
    return { ok: true, service: "admin-api" };
  }
}
