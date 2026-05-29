import { Controller, Get, Inject } from "@nestjs/common";
import { ApiTags } from "@nestjs/swagger";

import { MAIL_CONFIG } from "./tokens.js";

@ApiTags("health")
@Controller()
export class HealthController {
  constructor(@Inject(MAIL_CONFIG) private readonly config: any) {}

  @Get("/healthz")
  healthz() {
    return {
      ok: true,
      service: this.config.appName,
      env: this.config.env,
      storage: this.config.mysqlEnabled ? "mysql" : "memory"
    };
  }
}
