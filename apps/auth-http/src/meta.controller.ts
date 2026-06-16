import { Controller, Get, HttpCode, HttpStatus, Inject } from "@nestjs/common";
import { ApiTags } from "@nestjs/swagger";

import { ApiHttpException } from "./common/http-exception.js";
import { AUTH_CONFIG, AUTH_DB_STORE, AUTH_STORE } from "./tokens.js";

@ApiTags("meta")
@Controller()
export class MetaController {
  constructor(
    @Inject(AUTH_CONFIG) private readonly config: any,
    @Inject(AUTH_STORE) private readonly authStore: any,
    @Inject(AUTH_DB_STORE) private readonly dbStore: any
  ) {}

  @Get("/healthz")
  @HttpCode(HttpStatus.OK)
  async healthz() {
    const checks = { redis: "ok", db: "skipped" };
    let healthy = true;

    try {
      await this.authStore.redis.ping();
    } catch {
      checks.redis = "error";
      healthy = false;
    }

    if (this.config.dbEnabled && this.dbStore?.enabled) {
      try {
        await this.dbStore.pool.query("SELECT 1");
        checks.db = "ok";
      } catch {
        checks.db = "error";
        healthy = false;
      }
    }

    if (!healthy) {
      throw new ApiHttpException(503, {
        ok: false,
        service: this.config.appName,
        env: this.config.env,
        storage: this.config.dbEnabled ? "redis+postgresql" : "redis",
        checks
      });
    }

    return {
      ok: healthy,
      service: this.config.appName,
      env: this.config.env,
      storage: this.config.dbEnabled ? "redis+postgresql" : "redis",
      checks
    };
  }

  @Get("/api/v1/meta")
  meta() {
    return {
      project: "MyServer",
      service: this.config.appName,
      stage: "minimum-flow",
      protocol: "json",
      internalProtocol: "protobuf+tcp",
      storage: this.config.dbEnabled ? "redis+postgresql" : "redis",
      nextSteps: ["room-game-loop", "rate-limit", "admin-control-plane"]
    };
  }
}
