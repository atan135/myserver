import {
  Body,
  Controller,
  Get,
  HttpCode,
  HttpException,
  Inject,
  Param,
  Post,
  Query,
  Req,
  UseGuards
} from "@nestjs/common";
import { ApiBearerAuth, ApiTags } from "@nestjs/swagger";

import { Permissions } from "../auth/roles.decorator.js";
import { JwtAuthGuard } from "../auth/jwt-auth.guard.js";
import { AdminPolicyGuard } from "../auth/admin-policy.guard.js";
import { getClientIp } from "../common/client-ip.js";
import { ApiHttpException } from "../common/http-exception.js";
import { ADMIN_CONFIG, ADMIN_HIGH_RISK_OPERATIONS, MYFORGE_ORCHESTRATOR } from "../tokens.js";

const HTTP_STATUS_BY_ERROR: Record<string, number> = {
  INVALID_REQUEST: 400,
  MYFORGE_TARGET_PATH_INVALID: 400,
  MYFORGE_PROMPT_INVALID: 400,
  MYFORGE_PROMPT_TOO_LARGE: 413,
  MYFORGE_AGENT_NOT_FOUND: 404,
  MYFORGE_TASK_NOT_FOUND: 404,
  MYFORGE_AGENT_PROJECT_MISMATCH: 409,
  MYFORGE_TASK_NOT_CANCELLABLE: 409,
  MYFORGE_DISABLED: 503
};

function toHttpException(error: any): Error {
  if (error instanceof HttpException) return error;
  const status = error?.statusCode ?? HTTP_STATUS_BY_ERROR[error?.code];
  if (!Number.isInteger(status)) return error;
  return new ApiHttpException(status, {
    ok: false,
    error: error.code,
    message: error.message
  });
}

@ApiTags("myforge")
@ApiBearerAuth()
@UseGuards(JwtAuthGuard, AdminPolicyGuard)
@Controller("/api/v1/myforge")
export class MyforgeController {
  constructor(
    @Inject(ADMIN_CONFIG) private readonly config: any,
    @Inject(MYFORGE_ORCHESTRATOR) private readonly orchestrator: any,
    @Inject(ADMIN_HIGH_RISK_OPERATIONS) private readonly highRiskOperations: any
  ) {}

  @Get("agents")
  @Permissions("myforge.agent.read")
  async listAgents(@Query() query: any) {
    return this.call(() => this.orchestrator.listAgents(query));
  }

  @Get("tasks")
  @Permissions("myforge.task.read")
  async listTasks(@Query() query: any) {
    return this.call(() => this.orchestrator.listTasks(query));
  }

  @Get("tasks/:requestId")
  @Permissions("myforge.task.read")
  async getTask(@Param("requestId") requestId: string) {
    return this.call(() => this.orchestrator.getTask(requestId));
  }

  @Post("tasks/fangyuan-blueprint")
  @HttpCode(202)
  @Permissions("myforge.task.create")
  async createFangyuanBlueprint(@Body() body: any, @Req() req: any) {
    if (typeof this.highRiskOperations?.run !== "function") {
      throw new ApiHttpException(503, {
        ok: false,
        error: "ADMIN_OPERATION_SERVICE_UNAVAILABLE",
        message: "High-risk operation service is unavailable"
      });
    }
    const source = body && typeof body === "object" && !Array.isArray(body) ? body : {};
    const {
      requestId,
      request_id,
      preflightNonce,
      preflight_nonce,
      preflightSummarySha256,
      preflight_summary_sha256,
      reason,
      ...taskBody
    } = source;
    // Keep protocol fields on req.body for the operation service but never forward
    // them to MyForge's exact task payload validator.
    req.body = {
      ...taskBody,
      requestId: requestId ?? request_id,
      preflightNonce: preflightNonce ?? preflight_nonce,
      preflightSummarySha256: preflightSummarySha256 ?? preflight_summary_sha256,
      reason
    };
    const outcome = await this.highRiskOperations.run({
      request: req,
      permission: "myforge.task.create",
      scope: { serviceName: "myforge", targetType: "myforge_task", targetIds: ["fangyuan-blueprint"], targetCount: 1 },
      targetSummary: { targetType: "myforge_task", targetIds: ["fangyuan-blueprint"] },
      payload: taskBody,
      impactSummary: { targetType: "myforge_task", targetCount: 1, taskType: "fangyuan-blueprint" },
      reason,
      execute: () => this.call(() => this.orchestrator.createFangyuanBlueprint(taskBody, this.actor(req))),
      resultSummary: () => ({ action: "myforge.task.create", targetCount: 1, outcome: "submitted" })
    });
    return outcome.state === "executed" ? outcome.result : outcome.response;
  }

  @Post("tasks/:requestId/cancel")
  @HttpCode(200)
  @Permissions("myforge.task.cancel")
  async cancelTask(@Param("requestId") requestId: string, @Body() body: any, @Req() req: any) {
    return this.call(() => this.orchestrator.cancelTask(requestId, body, this.actor(req)));
  }

  private actor(req: any) {
    return {
      adminId: req.admin?.sub ?? null,
      adminUsername: req.admin?.username ?? null,
      ip: getClientIp(req, this.config)
    };
  }

  private async call(callback: () => Promise<any>) {
    try {
      return await callback();
    } catch (error) {
      throw toHttpException(error);
    }
  }
}

export { toHttpException };
