import { Controller, Get, Inject, Res } from "@nestjs/common";
import { ApiTags } from "@nestjs/swagger";

import { AUTH_CONFIG, AUTH_MYSQL_STORE, AUTH_STORE } from "./tokens.js";

@ApiTags("meta")
@Controller()
export class MetaController {
  constructor(
    @Inject(AUTH_CONFIG) private readonly config: any,
    @Inject(AUTH_STORE) private readonly authStore: any,
    @Inject(AUTH_MYSQL_STORE) private readonly mysqlStore: any
  ) {}

  @Get("/healthz")
  async healthz(@Res({ passthrough: true }) res: any) {
    const checks = { redis: "ok", mysql: "skipped" };
    let healthy = true;

    try {
      await this.authStore.redis.ping();
    } catch {
      checks.redis = "error";
      healthy = false;
    }

    if (this.config.mysqlEnabled && this.mysqlStore?.enabled) {
      try {
        await this.mysqlStore.pool.execute("SELECT 1");
        checks.mysql = "ok";
      } catch {
        checks.mysql = "error";
        healthy = false;
      }
    }

    res.status(healthy ? 200 : 503);
    return {
      ok: healthy,
      service: this.config.appName,
      env: this.config.env,
      storage: this.config.mysqlEnabled ? "redis+mysql" : "redis",
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
      storage: this.config.mysqlEnabled ? "redis+mysql" : "redis",
      nextSteps: ["room-game-loop", "rate-limit", "admin-control-plane"]
    };
  }
}
