import { Body, Controller, Get, HttpCode, HttpStatus, Inject, Post, Req } from "@nestjs/common";
import { ApiOperation, ApiTags } from "@nestjs/swagger";

import { badRequest, ApiHttpException } from "../common/http-exception.js";
import { AUTH_CONFIG, AUTH_GAME_ADMIN_CLIENT } from "../tokens.js";

function verifyInternalToken(req: any, config: any) {
  const token = config.internalApiToken;
  if (!token) {
    if (config.strictSecurity) {
      throw new ApiHttpException(503, {
        ok: false,
        error: "INTERNAL_API_TOKEN_REQUIRED",
        message: "INTERNAL_API_TOKEN is required when strict security is enabled"
      });
    }
    return;
  }

  const provided = req.headers["x-service-token"];
  if (provided !== token) {
    throw new ApiHttpException(401, {
      ok: false,
      error: "INVALID_SERVICE_TOKEN",
      message: "Missing or invalid X-Service-Token header"
    });
  }
}

function gameServerError(error: any) {
  return new ApiHttpException(502, {
    ok: false,
    error: error.code || "GAME_SERVER_UNAVAILABLE",
    message: error.message
  });
}

@ApiTags("internal-game-server")
@Controller("/api/v1/internal/game-server")
export class InternalController {
  constructor(
    @Inject(AUTH_CONFIG) private readonly config: any,
    @Inject(AUTH_GAME_ADMIN_CLIENT) private readonly gameAdminClient: any
  ) {}

  @Get("status")
  @ApiOperation({ summary: "Get game-server admin status" })
  async status(@Req() req: any) {
    verifyInternalToken(req, this.config);

    try {
      const status = await this.gameAdminClient.getServerStatus();
      return {
        ok: true,
        ...status
      };
    } catch (error: any) {
      throw gameServerError(error);
    }
  }

  @Get("rollout-drain-status")
  @ApiOperation({ summary: "Get game-server rollout drain status" })
  async rolloutDrainStatus(@Req() req: any) {
    verifyInternalToken(req, this.config);

    try {
      return await this.gameAdminClient.getRolloutDrainStatus();
    } catch (error: any) {
      throw gameServerError(error);
    }
  }

  @Post("config")
  @HttpCode(HttpStatus.OK)
  @ApiOperation({ summary: "Update game-server runtime config" })
  async updateConfig(@Req() req: any, @Body() body: any) {
    verifyInternalToken(req, this.config);

    const key = body?.key;
    const value = body?.value;

    if (typeof key !== "string" || key.length === 0) {
      throw badRequest("INVALID_CONFIG_KEY", "key must be a non-empty string");
    }

    if (typeof value !== "string" || value.length === 0) {
      throw badRequest("INVALID_CONFIG_VALUE", "value must be a non-empty string");
    }

    try {
      const result = await this.gameAdminClient.updateConfig(key, value);
      return {
        ok: result.ok,
        errorCode: result.errorCode
      };
    } catch (error: any) {
      throw gameServerError(error);
    }
  }
}
