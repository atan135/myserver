import { Body, Controller, HttpCode, HttpStatus, Inject, Post, Req, UseGuards } from "@nestjs/common";
import { ApiBearerAuth, ApiTags } from "@nestjs/swagger";

import { JwtAuthGuard } from "../auth/jwt-auth.guard.js";
import { AdminPolicyGuard } from "../auth/admin-policy.guard.js";
import { Permissions } from "../auth/roles.decorator.js";
import { ApiHttpException } from "../common/http-exception.js";
import { ADMIN_HIGH_RISK_OPERATIONS } from "../tokens.js";
import { RoomTransferService } from "./room-transfer.service.js";

function rolloutError(error: any) {
  if (typeof error?.getStatus === "function") return error;
  const code = typeof error?.code === "string" ? error.code : "ROLLOUT_OPERATION_FAILED";
  const status = code === "ADMIN_OPERATION_PERMISSION_DENIED" || code === "ADMIN_OPERATION_SCOPE_DENIED"
    ? 403
    : code === "ROLLOUT_TARGET_NOT_FOUND"
      ? 404
      : code === "SERVICE_DISCOVERY_REQUIRED"
        ? 503
        : 400;
  return new ApiHttpException(status, { ok: false, error: code, message: error?.message || code });
}

@ApiTags("rollouts")
@ApiBearerAuth()
@UseGuards(JwtAuthGuard, AdminPolicyGuard)
@Controller("/api/v1/rollouts")
export class RolloutController {
  constructor(
    private readonly roomTransfer: RoomTransferService,
    @Inject(ADMIN_HIGH_RISK_OPERATIONS) private readonly highRiskOperations: any
  ) {}

  @Post("room-transfer")
  @Permissions("game.room.transfer")
  @HttpCode(HttpStatus.OK)
  async transferRoom(@Body() body: any, @Req() req: any) {
    try {
      const input = this.roomTransfer.normalizeInput(body, req.admin?.sub);
      const targets = await this.roomTransfer.validate(input);
      const outcome = await this.highRiskOperations.run({
        request: req,
        permission: "game.room.transfer",
        scope: {
          worldId: input.worldId,
          serviceName: "game-server",
          instanceId: input.oldServerId,
          targetType: "room",
          targetIds: [input.roomId],
          targetCount: 1
        },
        targetSummary: {
          targetType: "room",
          targetIds: [input.roomId],
          worldId: input.worldId,
          oldServerId: input.oldServerId,
          newServerId: input.newServerId,
          proxyInstanceId: input.proxyInstanceId,
          backupReference: input.backupReference
        },
        payload: {
          worldId: input.worldId,
          rolloutEpoch: input.rolloutEpoch,
          roomId: input.roomId,
          oldServerId: input.oldServerId,
          newServerId: input.newServerId,
          proxyInstanceId: input.proxyInstanceId,
          backupReference: input.backupReference
        },
        impactSummary: {
          targetType: "room",
          targetCount: 1,
          oldServerId: targets.old.instanceId,
          newServerId: targets.new.instanceId,
          proxyInstanceId: targets.proxy.instanceId,
          operation: "room_transfer"
        },
        reason: body?.reason,
        execute: () => this.roomTransfer.execute(input, targets),
        resultSummary: (result: any) => ({
          action: "game.room.transfer",
          outcome: result?.ok === true ? "succeeded" : "execution_uncertain",
          stage: result?.stage || "complete",
          completedStages: Array.isArray(result?.completedStages) ? result.completedStages : []
        })
      });
      return outcome.state === "executed" ? outcome.result : outcome.response;
    } catch (error: any) {
      throw rolloutError(error);
    }
  }
}
