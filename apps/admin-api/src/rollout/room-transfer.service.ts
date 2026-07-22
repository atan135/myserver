import { createHash } from "node:crypto";

import { Inject, Injectable } from "@nestjs/common";

import { discoverGameProxyAdminEndpoints, discoverGameServerAdminEndpoints } from "../registry-client.js";
import { ADMIN_CONFIG, ADMIN_OPERATION_ASSERTIONS, ADMIN_POLICY, ADMIN_REDIS } from "../tokens.js";

type RoomTransferInput = {
  worldId: string;
  rolloutEpoch: string;
  roomId: string;
  oldServerId: string;
  newServerId: string;
  proxyInstanceId: string;
  backupReference: string;
  requestId: string;
  actorId: number | string;
};

const IDENTIFIER = /^[A-Za-z0-9][A-Za-z0-9._:@-]{0,127}$/;

function controlPlaneError(code: string, message = code) {
  const error: any = new Error(message);
  error.code = code;
  return error;
}

function requireIdentifier(value: unknown, field: string) {
  const normalized = typeof value === "string" || typeof value === "number" ? String(value).trim() : "";
  if (!IDENTIFIER.test(normalized)) {
    throw controlPlaneError("ROLLOUT_INPUT_INVALID", `${field} is invalid`);
  }
  return normalized;
}

export function downstreamRequestId(rootRequestId: string, stage: string, instanceId: string) {
  const digest = createHash("sha256")
    .update(`${rootRequestId}:${stage}:${instanceId}`, "utf8")
    .digest("hex")
    .slice(0, 40);
  return `rollout-${digest}`;
}

export function roomTransferAssertionStage(messageType: unknown) {
  return `game-message-${requireIdentifier(messageType, "game-server message type")}`;
}

function endpointUrl(endpoint: any) {
  const protocol = endpoint?.protocol === "https" ? "https" : "http";
  return `${protocol}://${endpoint.host}:${endpoint.port}`;
}

@Injectable()
export class RoomTransferService {
  constructor(
    @Inject(ADMIN_CONFIG) private readonly config: any,
    @Inject(ADMIN_REDIS) private readonly redis: any,
    @Inject(ADMIN_POLICY) private readonly policy: any,
    @Inject(ADMIN_OPERATION_ASSERTIONS) private readonly assertions: any
  ) {}

  normalizeInput(body: any, actorId: number | string): RoomTransferInput {
    return {
      worldId: requireIdentifier(body?.worldId ?? body?.world_id, "worldId"),
      rolloutEpoch: requireIdentifier(body?.rolloutEpoch ?? body?.rollout_epoch, "rolloutEpoch"),
      roomId: requireIdentifier(body?.roomId ?? body?.room_id, "roomId"),
      oldServerId: requireIdentifier(body?.oldServerId ?? body?.old_server_id, "oldServerId"),
      newServerId: requireIdentifier(body?.newServerId ?? body?.new_server_id, "newServerId"),
      proxyInstanceId: requireIdentifier(body?.proxyInstanceId ?? body?.proxy_instance_id, "proxyInstanceId"),
      backupReference: requireIdentifier(body?.backupReference ?? body?.backup_reference, "backupReference"),
      requestId: requireIdentifier(body?.requestId ?? body?.request_id, "requestId"),
      actorId: requireIdentifier(actorId, "actorId")
    };
  }

  async validate(input: RoomTransferInput) {
    if (input.oldServerId === input.newServerId) {
      throw controlPlaneError("ROLLOUT_INPUT_INVALID", "oldServerId and newServerId must differ");
    }
    const targets = await this.resolveTargets(input);
    const routeScope = {
      worldId: input.worldId,
      serviceName: "game-proxy",
      instanceId: targets.proxy.instanceId,
      targetType: "room",
      targetIds: [input.roomId],
      targetCount: 1
    };
    const decision = await this.policy.authorize(input.actorId, "proxy.route.write", routeScope);
    if (!decision?.allowed) {
      throw controlPlaneError(
        decision?.code === "SCOPE_DENIED" || decision?.code === "SCOPE_REQUIRED"
          ? "ADMIN_OPERATION_SCOPE_DENIED"
          : "ADMIN_OPERATION_PERMISSION_DENIED"
      );
    }
    return targets;
  }

  async execute(input: RoomTransferInput, resolvedTargets?: any) {
    const targets = resolvedTargets || await this.validate(input);
    try {
      const rollout = await import(new URL("../../../../tools/rollout/rollout-transfer.js", import.meta.url).href);
      const gameAssertionProvider = (endpoint: any) => async ({ messageType, payload }: any) => {
        const instanceId = requireIdentifier(endpoint.instanceId, "game-server instanceId");
        return this.assertions.issue({
          actorId: input.actorId,
          permission: "game.room.transfer",
          scope: {
            worldId: input.worldId,
            serviceName: "game-server",
            instanceId,
            targetType: "room",
            targetIds: [input.roomId],
            targetCount: 1
          },
          target: { targetType: "room", targetIds: [input.roomId], worldId: input.worldId },
          requestId: downstreamRequestId(input.requestId, roomTransferAssertionStage(messageType), instanceId),
          traceId: `trace-${input.requestId}`
        }, "game-server", instanceId, payload);
      };
      const proxyAssertionProvider = async ({ method, path }: any) => {
        const instanceId = requireIdentifier(targets.proxy.instanceId, "game-proxy instanceId");
        const payload = Buffer.from(`${String(method).toUpperCase()}\n${path}`, "utf8");
        return this.assertions.issue({
          actorId: input.actorId,
          permission: "proxy.route.write",
          scope: {
            worldId: input.worldId,
            serviceName: "game-proxy",
            instanceId,
            targetType: "room",
            targetIds: [input.roomId],
            targetCount: 1
          },
          target: { targetType: "room", targetIds: [input.roomId], worldId: input.worldId },
          requestId: downstreamRequestId(input.requestId, "proxy_route_upsert", instanceId),
          traceId: `trace-${input.requestId}`
        }, "game-proxy", instanceId, payload);
      };
      const oldServer = new rollout.GameServerTransferClient({
        host: targets.old.host,
        port: targets.old.port,
        assertionProvider: gameAssertionProvider(targets.old)
      });
      const newServer = new rollout.GameServerTransferClient({
        host: targets.new.host,
        port: targets.new.port,
        assertionProvider: gameAssertionProvider(targets.new)
      });
      const proxy = new rollout.ProxyAdminClient({
        baseUrl: endpointUrl(targets.proxy),
        token: this.config.gameProxyAdminReadToken || this.config.gameProxyAdminToken,
        assertionProvider: proxyAssertionProvider,
        timeoutMs: this.config.gameProxyAdminRequestTimeoutMs
      });
      return await rollout.orchestrateRoomTransfer({
        rolloutEpoch: input.rolloutEpoch,
        roomId: input.roomId,
        oldServerId: input.oldServerId,
        newServerId: input.newServerId,
        requireExistingRouteMetadata: false
      }, { oldServer, newServer, proxy });
    } catch (error: any) {
      if (typeof error?.code === "string" && error.code) {
        throw error;
      }
      throw controlPlaneError("ROLLOUT_DOWNSTREAM_FAILED", error?.message || "Room transfer downstream request failed");
    }
  }

  private async resolveTargets(input: RoomTransferInput) {
    if (!this.config.registryDiscoveryEnabled || !this.redis) {
      throw controlPlaneError("SERVICE_DISCOVERY_REQUIRED", "Room transfer requires registry discovery");
    }
    const gameServers = await discoverGameServerAdminEndpoints(this.redis, this.config);
    const proxyEndpoints = await discoverGameProxyAdminEndpoints(this.redis, this.config);
    const old = gameServers.find((endpoint: any) => endpoint.instanceId === input.oldServerId && endpoint.healthy !== false);
    const next = gameServers.find((endpoint: any) => endpoint.instanceId === input.newServerId && endpoint.healthy !== false);
    const proxy = proxyEndpoints.find((endpoint: any) => endpoint.instanceId === input.proxyInstanceId && endpoint.healthy !== false);
    if (!old || !next || !proxy) {
      throw controlPlaneError("ROLLOUT_TARGET_NOT_FOUND", "One or more rollout targets are not discoverable");
    }
    return { old, new: next, proxy };
  }
}
