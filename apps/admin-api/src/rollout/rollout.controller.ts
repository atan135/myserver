import { Body, Controller, Get, HttpCode, HttpStatus, Inject, Param, Post, Req, UseGuards } from "@nestjs/common";
import { ApiBearerAuth, ApiTags } from "@nestjs/swagger";

import { JwtAuthGuard } from "../auth/jwt-auth.guard.js";
import { AdminPolicyGuard } from "../auth/admin-policy.guard.js";
import { Permissions, PolicyScopeResolver } from "../auth/roles.decorator.js";
import { ApiHttpException } from "../common/http-exception.js";
import { ADMIN_GAME_ADMIN_CLIENT, ADMIN_HIGH_RISK_OPERATIONS, ADMIN_POLICY } from "../tokens.js";
import { RoomTransferService } from "./room-transfer.service.js";

function rolloutError(error: any) {
  if (typeof error?.getStatus === "function") return error;
  const code = typeof error?.code === "string" ? error.code : "ROLLOUT_OPERATION_FAILED";
  const status = code === "ADMIN_OPERATION_PERMISSION_DENIED" || code === "ADMIN_OPERATION_SCOPE_DENIED"
    ? 403
    : code === "ROLLOUT_TARGET_NOT_FOUND" || code === "GAME_SERVER_ADMIN_TARGET_NOT_FOUND"
      ? 404
      : code === "SERVICE_DISCOVERY_REQUIRED" || code === "SERVICE_DISCOVERY_UNAVAILABLE" ||
          code === "GAME_SERVER_ADMIN_ENDPOINT_NOT_FOUND"
        ? 503
        : code.startsWith("GAME_ADMIN_")
          ? 502
        : 400;
  return new ApiHttpException(status, { ok: false, error: code, message: error?.message || code });
}

const INSTANCE_ID = /^[A-Za-z0-9][A-Za-z0-9._:@-]{0,127}$/;

function requireInstanceId(value: unknown) {
  const normalized = typeof value === "string" ? value.trim() : "";
  if (!INSTANCE_ID.test(normalized)) {
    const error: any = new Error("game-server instanceId is invalid");
    error.code = "ROLLOUT_INPUT_INVALID";
    throw error;
  }
  return normalized;
}

function requestId(body: any) {
  const value = body?.requestId ?? body?.request_id;
  return typeof value === "string" ? value.trim() : "";
}

function gameServerDrainPolicyScope(request: any) {
  const instanceId = requireInstanceId(request?.params?.instanceId);
  return {
    worldId: "*",
    serviceName: "game-server",
    instanceId,
    targetType: "config",
    targetIds: ["drain_mode"],
    targetCount: 1
  };
}

function gameServerShutdownPolicyScope(request: any) {
  const instanceId = requireInstanceId(request?.params?.instanceId);
  return {
    serviceName: "game-server",
    instanceId,
    targetType: "service",
    targetIds: [instanceId],
    targetCount: 1
  };
}

function gameServerInstanceListPolicyScope() {
  return {
    worldId: "*",
    serviceName: "game-server",
    instanceId: "*",
    targetType: "config",
    targetIds: ["drain_mode"],
    targetCount: 1
  };
}

function gameServerAssertionContext(
  req: any,
  body: any,
  permission: "game.config.write" | "service.shutdown",
  instanceId: string,
  targetType: "config" | "service",
  targetIds: string[],
  worldId?: string
) {
  const id = requestId(body);
  return {
    actorId: req.admin?.sub,
    permission,
    scope: {
      ...(worldId ? { worldId } : {}),
      serviceName: "game-server",
      instanceId,
      targetType,
      targetIds,
      targetCount: targetIds.length
    },
    target: { targetType, targetIds },
    requestId: id,
    traceId: `trace-${id}`
  };
}

@ApiTags("rollouts")
@ApiBearerAuth()
@Controller("/api/v1/rollouts")
export class RolloutController {
  constructor(
    private readonly roomTransfer: RoomTransferService,
    @Inject(ADMIN_HIGH_RISK_OPERATIONS) private readonly highRiskOperations: any,
    @Inject(ADMIN_GAME_ADMIN_CLIENT) private readonly gameAdminClient: any,
    @Inject(ADMIN_POLICY) private readonly adminPolicy: any
  ) {}

  @Get("game-server/instances")
  @UseGuards(JwtAuthGuard)
  @Permissions("game.config.write")
  @PolicyScopeResolver(gameServerInstanceListPolicyScope)
  @HttpCode(HttpStatus.OK)
  async listGameServerInstances(@Req() req: any) {
    try {
      const endpoints = await this.gameAdminClient.listAdminEndpoints();
      const instances = [];
      for (const endpoint of Array.isArray(endpoints) ? endpoints : []) {
        let instanceId = "";
        try {
          instanceId = requireInstanceId(endpoint?.instanceId || endpoint?.instance_id);
        } catch {
          continue;
        }
        if (!instanceId || endpoint?.source === "fallback" || endpoint?.fallback === true) continue;
        const decision = await this.adminPolicy.authorize(req.admin?.sub, "game.config.write", {
          worldId: "*",
          serviceName: "game-server",
          instanceId,
          targetType: "config",
          targetIds: ["drain_mode"],
          targetCount: 1
        });
        if (!decision?.allowed) continue;
        instances.push({
          instanceId,
          status: endpoint.healthy === false ? "unhealthy" : "healthy",
          healthy: endpoint.healthy !== false
        });
      }
      return { ok: true, instances };
    } catch (error: any) {
      throw rolloutError(error);
    }
  }

  @Get("game-server/:instanceId/drain-status")
  @UseGuards(JwtAuthGuard, AdminPolicyGuard)
  @Permissions("game.config.write")
  @PolicyScopeResolver(gameServerDrainPolicyScope)
  @HttpCode(HttpStatus.OK)
  async getGameServerDrainStatus(@Param("instanceId") rawInstanceId: string) {
    try {
      const instanceId = requireInstanceId(rawInstanceId);
      const status = await this.gameAdminClient.getRolloutDrainStatus({
        targetInstanceId: instanceId,
        requireRegistryTarget: true
      });
      return {
        ok: status?.ok === true,
        instanceId,
        errorCode: status?.errorCode || "",
        rolloutEpoch: status?.rolloutEpoch || "",
        ownedRoomCount: status?.ownedRoomCount || 0,
        migratingRoomCount: status?.migratingRoomCount || 0,
        connectionCount: status?.connectionCount || 0,
        drainModeEnabled: status?.drainModeEnabled === true,
        drainModeEnteredAtMs: status?.drainModeEnteredAtMs || 0,
        transferableEmptyRoomCount: status?.transferableEmptyRoomCount || 0,
        retiredRoomCount: status?.retiredRoomCount || 0,
        drainModeReason: status?.drainModeReason || "",
        drainModeSource: status?.drainModeSource || "",
        routeCount: Array.isArray(status?.routes) ? status.routes.length : 0,
        transferableEmptyRoomSampleCount: Array.isArray(status?.transferableEmptyRoomSamples)
          ? status.transferableEmptyRoomSamples.length
          : 0
      };
    } catch (error: any) {
      throw rolloutError(error);
    }
  }

  @Post("game-server/:instanceId/drain")
  @UseGuards(JwtAuthGuard, AdminPolicyGuard)
  @Permissions("game.config.write")
  @PolicyScopeResolver(gameServerDrainPolicyScope)
  @HttpCode(HttpStatus.OK)
  async setGameServerDrain(
    @Param("instanceId") rawInstanceId: string,
    @Body() body: any,
    @Req() req: any
  ) {
    try {
      const instanceId = requireInstanceId(rawInstanceId);
      if (typeof body?.enabled !== "boolean") {
        const error: any = new Error("enabled must be a boolean");
        error.code = "ROLLOUT_INPUT_INVALID";
        throw error;
      }
      const enabled = body.enabled;
      const outcome = await this.highRiskOperations.run({
        request: req,
        permission: "game.config.write",
        scope: {
          worldId: "*",
          serviceName: "game-server",
          instanceId,
          targetType: "config",
          targetIds: ["drain_mode"],
          targetCount: 1
        },
        targetSummary: { targetType: "config", targetIds: ["drain_mode"], instanceId },
        payload: { action: "game_server_drain", enabled, instanceId },
        impactSummary: { action: "game_server_drain", enabled, instanceId, targetCount: 1 },
        reason: body?.reason,
        execute: () => this.gameAdminClient.updateConfig("drain_mode", enabled ? "on" : "off", {
          targetInstanceId: instanceId,
          requireRegistryTarget: true,
          assertionContext: gameServerAssertionContext(
            req,
            body,
            "game.config.write",
            instanceId,
            "config",
            ["drain_mode"],
            "*"
          )
        }),
        resultSummary: (result: any) => ({
          action: "game_server_drain",
          instanceId,
          enabled,
          ok: result?.ok === true,
          errorCode: result?.errorCode || ""
        })
      });
      return outcome.state === "executed" ? outcome.result : outcome.response;
    } catch (error: any) {
      throw rolloutError(error);
    }
  }

  @Post("game-server/:instanceId/shutdown")
  @UseGuards(JwtAuthGuard, AdminPolicyGuard)
  @Permissions("service.shutdown")
  @PolicyScopeResolver(gameServerShutdownPolicyScope)
  @HttpCode(HttpStatus.OK)
  async shutdownGameServer(
    @Param("instanceId") rawInstanceId: string,
    @Body() body: any,
    @Req() req: any
  ) {
    try {
      const instanceId = requireInstanceId(rawInstanceId);
      const outcome = await this.highRiskOperations.run({
        request: req,
        permission: "service.shutdown",
        scope: {
          serviceName: "game-server",
          instanceId,
          targetType: "service",
          targetIds: [instanceId],
          targetCount: 1
        },
        targetSummary: {
          targetType: "service",
          targetIds: [instanceId],
          serviceName: "game-server",
          instanceId,
          worldId: null
        },
        payload: { action: "game_server_shutdown", instanceId },
        impactSummary: { action: "game_server_shutdown", instanceId, targetCount: 1 },
        reason: body?.reason,
        emergency: true,
        execute: () => this.gameAdminClient.requestServerShutdown(body?.reason, {
          targetInstanceId: instanceId,
          requireRegistryTarget: true,
          allowLiveUnhealthyAdminTarget: true,
          assertionContext: gameServerAssertionContext(
            req,
            body,
            "service.shutdown",
            instanceId,
            "service",
            [instanceId]
          )
        }),
        resultSummary: (result: any) => ({
          action: "game_server_shutdown",
          instanceId,
          ok: result?.ok === true,
          errorCode: result?.error_code || "",
          shutdownArmed: result?.shutdown_armed === true,
          connectionCount: result?.connection_count || 0,
          ownedRoomCount: result?.owned_room_count || 0,
          migratingRoomCount: result?.migrating_room_count || 0
        })
      });
      return outcome.state === "executed" ? outcome.result : outcome.response;
    } catch (error: any) {
      throw rolloutError(error);
    }
  }

  @Post("room-transfer")
  @UseGuards(JwtAuthGuard, AdminPolicyGuard)
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
