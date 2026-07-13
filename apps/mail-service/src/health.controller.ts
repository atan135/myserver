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
      storage: this.config.dbEnabled ? "postgresql" : "memory",
      mail_claim_new_requests_enabled: this.config.claimNewRequestsEnabled !== false,
      mail_claim_recovery_enabled: this.config.claimRecoveryEnabled !== false
    };
  }
}
