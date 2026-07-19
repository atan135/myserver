import { Body, Controller, HttpCode, HttpStatus, Inject, Param, Post, Req, UseGuards } from "@nestjs/common";
import { ApiBearerAuth, ApiTags } from "@nestjs/swagger";

import { JwtAuthGuard } from "../auth/jwt-auth.guard.js";
import { AdminPolicyGuard } from "../auth/admin-policy.guard.js";
import { extractAdminPolicyScope } from "../auth/admin-policy.guard.js";
import { Permissions } from "../auth/roles.decorator.js";
import { getClientIp } from "../common/client-ip.js";
import { appendSecurityAuditLog, getSecurityAuditClientIp } from "../common/security-audit.js";
import { ApiHttpException, badRequest, notFound } from "../common/http-exception.js";
import { encodeSubjectToken } from "../nats-client.js";
import { ADMIN_CONFIG, ADMIN_GAME_ADMIN_CLIENT, ADMIN_HIGH_RISK_OPERATIONS, ADMIN_NATS, ADMIN_STORE } from "../tokens.js";
import { computeBanExpiresAt } from "../ban-utils.js";
import { createHash, randomUUID } from "node:crypto";
import { getTitleDefinitions } from "../players/title-table.js";

const GM_BAN_DURATION_MAX_SECONDS = 31_536_000;
const ASSET_GRANT_LARGE_QUANTITY = 10_000;
const ASSET_GRANT_HIGH_FREQUENCY_LIMIT = 10;
const ASSET_GRANT_HIGH_FREQUENCY_WINDOW_MS = 60_000;
const GM_BROADCAST_SUBJECT = "myserver.gm.broadcast";
const GAME_ADMIN_ACTOR_PATTERN = /^[A-Za-z0-9._@-]{1,128}$/;
const ELEMENT_KEYS = ["earth", "fire", "water", "wind"] as const;
const AFFINITY_TOTAL = 10000;
const TITLE_ACTIONS = ["grant", "revoke", "equip", "unequip"] as const;
const DISCIPLINE_TIERS = ["novice", "apprentice", "adept", "expert", "master", "grandmaster"];

function gameServerError(error: any) {
  const code = error?.code || "GAME_SERVER_ERROR";
  const statusCode = code === "GAME_SERVER_ADMIN_TARGET_REQUIRED"
    ? 400
    : code === "GAME_SERVER_ADMIN_TARGET_NOT_FOUND"
      ? 404
      : 502;
  return new ApiHttpException(statusCode, {
    ok: false,
    error: code,
    message: error.message
  });
}

function adminStoreError(error: any) {
  if (typeof error?.getStatus === "function") {
    return error;
  }

  const code = error?.code || "GM_CHARACTER_OPERATION_FAILED";
  const statusCode = code === "CHARACTER_NOT_FOUND" ||
    code === "TITLE_NOT_OWNED" ||
    code === "TITLE_CONFIG_NOT_FOUND"
    ? 404
    : 400;
  return new ApiHttpException(statusCode, {
    ok: false,
    error: code,
    message: error?.message || code
  });
}

function gameServerFailure(error: any) {
  const failure: any = {
    ok: false,
    error: error?.code || "GAME_SERVER_ERROR",
    message: error?.message || "game-server error"
  };
  if (error?.gameAdminEndpoint) {
    failure.endpoint = error.gameAdminEndpoint;
    failure.instanceId = error.gameAdminEndpoint.instanceId;
  }
  if (error?.gameAdminInstances) {
    failure.instances = error.gameAdminInstances;
  }
  return failure;
}

function isGameAdminTargetSelectionError(error: any) {
  return error?.code === "GAME_SERVER_ADMIN_TARGET_REQUIRED" ||
    error?.code === "GAME_SERVER_ADMIN_TARGET_NOT_FOUND";
}

function sessionKickPublishError(globalKick: any, legacyResult: any) {
  return new ApiHttpException(502, {
    ok: false,
    error: "SESSION_KICK_PUBLISH_FAILED",
    message: globalKick.message || "failed to publish global session kick",
    globalKick,
    legacyResult
  });
}

function globalBroadcastPublishError(globalBroadcast: any, legacyBroadcast: any) {
  return new ApiHttpException(502, {
    ok: false,
    error: "GM_BROADCAST_PUBLISH_FAILED",
    message:
      globalBroadcast.message ||
      "failed to publish global GM broadcast; legacy single-instance fallback result is attached",
    partialDelivered: legacyBroadcast?.ok === true,
    globalBroadcast,
    legacyBroadcast
  });
}

function normalizePlayerId(playerId: any) {
  if (!playerId || typeof playerId !== "string" || playerId.trim().length === 0) {
    throw badRequest("INVALID_PLAYER_ID", "playerId is required");
  }
  return playerId.trim();
}

function normalizeCharacterId(characterId: any) {
  if (!characterId || typeof characterId !== "string" || characterId.trim().length === 0) {
    throw badRequest("INVALID_CHARACTER_ID", "characterId is required");
  }
  return characterId.trim();
}

function normalizeRequiredText(value: any, code: string, fieldName: string, maxLength = 255) {
  if (typeof value !== "string") {
    throw badRequest(code, `${fieldName} is required`);
  }
  const normalized = value.trim();
  if (normalized.length === 0) {
    throw badRequest(code, `${fieldName} is required`);
  }
  if (normalized.length > maxLength) {
    throw badRequest(code, `${fieldName} must be ${maxLength} characters or fewer`);
  }
  return normalized;
}

function normalizeOptionalText(value: any, code: string, fieldName: string, maxLength = 128) {
  if (value === undefined || value === null || value === "") {
    return null;
  }
  if (typeof value !== "string") {
    throw badRequest(code, `${fieldName} must be a string`);
  }
  const normalized = value.trim();
  if (normalized.length === 0) {
    return null;
  }
  if (normalized.length > maxLength) {
    throw badRequest(code, `${fieldName} must be ${maxLength} characters or fewer`);
  }
  return normalized;
}

function normalizeElementValues(value: any, fieldName: "affinity" | "mastery", { required = false } = {}) {
  if (value === undefined || value === null) {
    if (required) {
      throw badRequest("INVALID_CHARACTER_ELEMENT_PAYLOAD", `${fieldName} is required`);
    }
    return null;
  }
  if (typeof value !== "object" || Array.isArray(value)) {
    throw badRequest("INVALID_CHARACTER_ELEMENT_PAYLOAD", `${fieldName} must be an object`);
  }

  const normalized: Record<string, number> = {};
  for (const key of ELEMENT_KEYS) {
    const raw = value[key];
    if (raw === undefined || raw === null || raw === "") {
      throw badRequest("INVALID_CHARACTER_ELEMENT_PAYLOAD", `${fieldName}.${key} is required`);
    }
    const numberValue = Number(raw);
    if (!Number.isSafeInteger(numberValue)) {
      throw badRequest("INVALID_CHARACTER_ELEMENT_PAYLOAD", `${fieldName}.${key} must be an integer`);
    }
    if (numberValue < 0) {
      throw badRequest(
        fieldName === "affinity" ? "NEGATIVE_AFFINITY" : "NEGATIVE_MASTERY",
        `${fieldName}.${key} must be non-negative`
      );
    }
    normalized[key] = numberValue;
  }

  if (fieldName === "affinity") {
    const total = ELEMENT_KEYS.reduce((sum, key) => sum + normalized[key], 0);
    if (total !== AFFINITY_TOTAL) {
      throw badRequest("INVALID_AFFINITY_TOTAL", `affinity total must be ${AFFINITY_TOTAL}`);
    }
  }

  return normalized;
}

function normalizeTitleAction(value: any) {
  if (typeof value !== "string" || !(TITLE_ACTIONS as readonly string[]).includes(value.trim())) {
    throw badRequest("INVALID_GM_TITLE_ACTION", "action must be grant, revoke, equip, or unequip");
  }
  return value.trim();
}

function normalizeTitleId(value: any) {
  return normalizeRequiredText(value, "INVALID_TITLE_ID", "titleId", 64);
}

function normalizeExpiresAt(value: any) {
  if (value === undefined || value === null || value === "") {
    return null;
  }
  if (typeof value !== "string") {
    throw badRequest("INVALID_EXPIRES_AT", "expiresAt must be an ISO timestamp string");
  }
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    throw badRequest("INVALID_EXPIRES_AT", "expiresAt must be an ISO timestamp string");
  }
  return date.toISOString();
}

function normalizeDisciplineId(value: any) {
  return normalizeRequiredText(value, "INVALID_DISCIPLINE_ID", "disciplineId", 64);
}

function normalizeDisciplinePoints(value: any) {
  const numberValue = Number(value);
  if (!Number.isSafeInteger(numberValue) || numberValue < 0) {
    throw badRequest("INVALID_DISCIPLINE_POINTS", "points must be a non-negative integer");
  }
  return numberValue;
}

function normalizeDisciplineTier(value: any) {
  if (typeof value !== "string" || !DISCIPLINE_TIERS.includes(value.trim())) {
    throw badRequest("INVALID_DISCIPLINE_TIER", "tier is invalid");
  }
  return value.trim();
}

function normalizeBoolean(value: any, fieldName: string) {
  if (typeof value !== "boolean") {
    throw badRequest("INVALID_DISCIPLINE_PAYLOAD", `${fieldName} must be a boolean`);
  }
  return value;
}

function normalizeReason(value: any) {
  return normalizeRequiredText(value, "MISSING_REASON", "reason", 255);
}

function broadcastPayloadAuditSummary(title: string, content: string, sender: string) {
  const combined = `${title}\n${content}\n${sender}`;
  return {
    payloadSha256: createHash("sha256").update(combined, "utf8").digest("hex"),
    titleBytes: Buffer.byteLength(title, "utf8"),
    contentBytes: Buffer.byteLength(content, "utf8"),
    senderBytes: Buffer.byteLength(sender, "utf8")
  };
}

function broadcastDeliveryAuditSummary(globalBroadcast: any, legacyBroadcast: any) {
  const summarizeInstances = (instances: unknown) => Array.isArray(instances)
    ? instances.slice(0, 100).map((instance: any) => ({
        ok: instance?.ok === true,
        instanceId: typeof instance?.instanceId === "string" ? instance.instanceId : null
      }))
    : [];
  return {
    global: {
      ok: globalBroadcast?.ok === true,
      subject: typeof globalBroadcast?.subject === "string" ? globalBroadcast.subject : null,
      broadcastId: typeof globalBroadcast?.payload?.broadcast_id === "string" ? globalBroadcast.payload.broadcast_id : null,
      error: typeof globalBroadcast?.error === "string" ? globalBroadcast.error : null
    },
    legacy: {
      ok: legacyBroadcast?.ok === true,
      skipped: legacyBroadcast?.skipped === true,
      fallback: legacyBroadcast?.fallback === true,
      instances: summarizeInstances(legacyBroadcast?.instances)
    }
  };
}

function auditErrorCode(error: any) {
  return error?.getResponse?.().error || error?.code || error?.message || "UNKNOWN_ERROR";
}

function normalizeGmReason(reason: any, prefix: "gm_kick" | "gm_ban") {
  if (reason === undefined || reason === null) {
    return prefix;
  }
  if (typeof reason !== "string") {
    throw badRequest("INVALID_REASON", "reason must be a string");
  }
  const normalized = reason.trim();
  return normalized.length > 0 ? `${prefix}:${normalized}` : prefix;
}

function normalizeGameAdminActor(req: any) {
  const admin = req?.admin || {};
  const candidates = [admin.username, admin.sub !== undefined && admin.sub !== null ? `admin-${admin.sub}` : null];

  for (const candidate of candidates) {
    if (candidate === undefined || candidate === null) {
      continue;
    }
    const actor = String(candidate).trim();
    if (GAME_ADMIN_ACTOR_PATTERN.test(actor)) {
      return actor;
    }
  }

  return undefined;
}

function normalizeTargetInstanceId(value: any) {
  if (value === undefined || value === null || value === "") {
    return undefined;
  }
  if (typeof value !== "string") {
    throw badRequest("INVALID_TARGET_INSTANCE_ID", "targetInstanceId must be a string");
  }
  const normalized = value.trim();
  return normalized.length > 0 ? normalized : undefined;
}

function createGameAdminOptions(req: any, body: any = {}): any {
  return {
    actor: normalizeGameAdminActor(req),
    targetInstanceId: normalizeTargetInstanceId(body?.targetInstanceId ?? body?.target_instance_id)
  };
}

async function createGameAssertionContext(
  req: any,
  body: any,
  permission: string,
  adminStore: any,
  targetType: string,
  targetIds: string[]
) {
  return {
    actorId: req?.admin?.sub,
    permission,
    scope: await extractAdminPolicyScope(req, permission, adminStore),
    target: { targetType, targetIds },
    requestId: body?.requestId ?? body?.request_id,
    traceId: req?.headers?.["x-request-id"] ?? req?.headers?.["x-trace-id"]
  };
}

async function preflightSingleTarget(gameAdminClient: any, gameAdminOptions: any) {
  try {
    return await gameAdminClient.resolveAdminEndpoint({
      ...gameAdminOptions,
      requireExplicitTarget: true
    });
  } catch (error: any) {
    if (isGameAdminTargetSelectionError(error)) {
      throw gameServerError(error);
    }
    return null;
  }
}

@ApiTags("gm")
@ApiBearerAuth()
@UseGuards(JwtAuthGuard, AdminPolicyGuard)
@Controller("/api/v1/gm")
export class GmController {
  constructor(
    @Inject(ADMIN_CONFIG) private readonly config: any,
    @Inject(ADMIN_STORE) private readonly adminStore: any,
    @Inject(ADMIN_NATS) private readonly nats: any,
    @Inject(ADMIN_GAME_ADMIN_CLIENT) private readonly gameAdminClient: any,
    @Inject(ADMIN_HIGH_RISK_OPERATIONS) private readonly highRiskOperations: any
  ) {}

  private async runHighRiskOperation(input: any) {
    if (typeof this.highRiskOperations?.run !== "function") {
      throw new ApiHttpException(503, {
        ok: false,
        error: "ADMIN_OPERATION_SERVICE_UNAVAILABLE",
        message: "High-risk operation service is unavailable"
      });
    }
    const outcome = await this.highRiskOperations.run(input);
    return outcome.state === "executed" ? outcome.result : outcome.response;
  }

  private async characterOperationScope(characterId: string) {
    if (typeof this.adminStore?.findCharacterById !== "function") {
      throw new ApiHttpException(503, {
        ok: false,
        error: "ADMIN_OPERATION_TARGET_RESOLUTION_UNAVAILABLE",
        message: "High-risk target resolution is unavailable"
      });
    }
    const character = await this.adminStore.findCharacterById(characterId, { includeDeleted: true });
    const worldId = character?.worldId ?? character?.world_id;
    if (worldId === undefined || worldId === null || String(worldId).trim() === "") {
      throw notFound("CHARACTER_NOT_FOUND", "Character not found");
    }
    return {
      worldId: String(worldId),
      serviceName: "game-server",
      targetType: "character",
      targetIds: [characterId],
      targetCount: 1
    };
  }

  private async publishGlobalSessionKick(playerId: string, reason: string) {
    const subject = `myserver.session.kick.${encodeSubjectToken(playerId)}`;
    const payload = { player_id: playerId, reason };

    try {
      await this.nats.publishJson(subject, payload);
      return { ok: true, subject, payload };
    } catch (error: any) {
      return {
        ok: false,
        error: error?.code || "NATS_PUBLISH_FAILED",
        message: error?.message || "failed to publish global session kick",
        subject,
        payload
      };
    }
  }

  private async publishGlobalBroadcast(title: string, content: string, sender: string) {
    const payload = {
      broadcast_id: randomUUID(),
      title,
      content,
      sender,
      created_at: new Date().toISOString()
    };

    try {
      await this.nats.publishJson(GM_BROADCAST_SUBJECT, payload);
      return { ok: true, subject: GM_BROADCAST_SUBJECT, payload };
    } catch (error: any) {
      return {
        ok: false,
        error: error?.code || "NATS_PUBLISH_FAILED",
        message: error?.message || "failed to publish global GM broadcast",
        subject: GM_BROADCAST_SUBJECT,
        payload
      };
    }
  }

  @Post("broadcast")
  @Permissions("gm.broadcast")
  @HttpCode(HttpStatus.OK)
  async broadcast(@Body() body: any, @Req() req: any) {
    const { title, content, sender, reason } = body || {};

    if (!title || typeof title !== "string" || title.trim().length === 0) {
      throw badRequest("INVALID_TITLE", "title is required");
    }

    if (!content || typeof content !== "string" || content.trim().length === 0) {
      throw badRequest("INVALID_CONTENT", "content is required");
    }

    const normalizedTitle = title.trim();
    const normalizedContent = content.trim();
    const normalizedSender = typeof sender === "string" && sender.trim().length > 0 ? sender.trim() : "System";
    const normalizedReason = normalizeReason(reason);
    const broadcastEvidence = broadcastPayloadAuditSummary(normalizedTitle, normalizedContent, normalizedSender);
    return this.runHighRiskOperation({
      request: req,
      permission: "gm.broadcast",
      scope: {
        worldId: "*",
        serviceName: "game-server",
        targetType: "world",
        targetIds: ["all"],
        targetCount: 1
      },
      targetSummary: { targetType: "world", targetIds: ["all"] },
      payload: { title: normalizedTitle, content: normalizedContent, sender: normalizedSender },
      impactSummary: { targetType: "world", targetCount: 1, delivery: "global_broadcast" },
      reason: normalizedReason,
      execute: async () => {
        const gameAdminOptions = createGameAdminOptions(req, body);
        gameAdminOptions.assertionContext = await createGameAssertionContext(
          req,
          body,
          "gm.broadcast",
          this.adminStore,
          "world",
          ["all"]
        );
        const globalBroadcast = await this.publishGlobalBroadcast(
          normalizedTitle,
          normalizedContent,
          normalizedSender,
        );
        let legacyBroadcast: any = {
          ok: true,
          skipped: true,
          reason: "global_broadcast_published"
        };
        if (!globalBroadcast.ok) {
          try {
            const result = await this.gameAdminClient.broadcast(
              normalizedTitle,
              normalizedContent,
              normalizedSender,
              gameAdminOptions
            );
            legacyBroadcast = { ok: true, ...(result ?? {}), fallback: true };
          } catch (error: any) {
            legacyBroadcast = gameServerFailure(error);
            legacyBroadcast.fallback = true;
          }
        }

        await this.adminStore.appendAuditLog({
          adminId: req.admin.sub,
          adminUsername: req.admin.username,
          action: "gm_broadcast",
          targetType: "system",
          targetValue: "all",
          details: {
            broadcast: broadcastEvidence,
            requestedTargetInstanceId: gameAdminOptions.targetInstanceId,
            delivery: broadcastDeliveryAuditSummary(globalBroadcast, legacyBroadcast)
          },
          ip: getClientIp(req, this.config)
        });

        if (!globalBroadcast.ok) {
          throw globalBroadcastPublishError(globalBroadcast, legacyBroadcast);
        }

        return {
          ok: true,
          message: "Broadcast sent",
          globalBroadcast,
          legacyBroadcast
        };
      },
      resultSummary: () => ({ action: "gm.broadcast", outcome: "succeeded", broadcast: broadcastEvidence })
    });
  }

  @Post("send-item")
  @Permissions("gm.send_item")
  @HttpCode(HttpStatus.OK)
  async sendItem(@Body() body: any, @Req() req: any) {
    const { characterId, itemId, itemCount, reason } = body || {};
    const normalizedCharacterId = normalizeCharacterId(characterId);

    if (!itemId || typeof itemId !== "string") {
      throw badRequest("INVALID_ITEM_ID", "itemId is required");
    }

    if (!itemCount || typeof itemCount !== "number" || !Number.isSafeInteger(itemCount) || itemCount <= 0) {
      throw badRequest("INVALID_ITEM_COUNT", "itemCount must be a positive number");
    }
    const normalizedReason = normalizeReason(reason);
    const scope = await this.characterOperationScope(normalizedCharacterId);
    const assetEvidence = {
      itemId,
      itemDelta: itemCount,
      ledgerReference: "unavailable: game-server admin response does not return an asset ledger id"
    };

    return this.runHighRiskOperation({
      request: req,
      permission: "gm.send_item",
      scope,
      targetSummary: { targetType: "character", targetIds: [normalizedCharacterId] },
      payload: { characterId: normalizedCharacterId, itemId, itemCount },
      impactSummary: { targetType: "character", targetCount: 1, assetChange: "item_grant" },
      reason: normalizedReason,
      execute: async () => {
        try {
          const gameAdminOptions = createGameAdminOptions(req, body);
          gameAdminOptions.assertionContext = await createGameAssertionContext(
            req,
            body,
            "gm.send_item",
            this.adminStore,
            "character",
            [normalizedCharacterId]
          );
          const gameAdminResult = await this.gameAdminClient.sendItem(
            normalizedCharacterId,
            itemId,
            itemCount,
            normalizedReason,
            gameAdminOptions
          );

          await this.adminStore.appendAuditLog({
            adminId: req.admin.sub,
            adminUsername: req.admin.username,
            action: "gm_send_item",
            targetType: "character",
            targetValue: normalizedCharacterId,
            details: {
              itemId,
              itemCount,
              reason: normalizedReason,
              requestedTargetInstanceId: gameAdminOptions.targetInstanceId,
              gameAdmin: {
                ok: gameAdminResult?.ok === true,
                instanceId: gameAdminResult?.instanceId,
                endpoint: gameAdminResult?.endpoint
              }
            },
            ip: getClientIp(req, this.config)
          });
          await this.appendAssetGrantSecurityAudit(req, {
            action: "gm_send_item",
            characterId: normalizedCharacterId,
            itemId,
            itemCount,
            emergency: false
          });

          return { ok: true, message: "Item sent" };
        } catch (error: any) {
          throw gameServerError(error);
        }
      },
      resultSummary: () => ({
        action: "gm.send_item",
        targetCount: 1,
        outcome: "succeeded",
        asset: assetEvidence
      })
    });
  }

  @Post("emergency-compensate-item")
  @Permissions("gm.asset_correction.emergency")
  @HttpCode(HttpStatus.OK)
  async emergencyCompensateItem(@Body() body: any, @Req() req: any) {
    const characterId = normalizeCharacterId(body?.characterId);
    const itemId = normalizeRequiredText(body?.itemId, "INVALID_ITEM_ID", "itemId", 32);
    const itemCount = Number(body?.itemCount);
    const reason = normalizeReason(body?.reason);
    if (!Number.isSafeInteger(itemCount) || itemCount <= 0) {
      throw badRequest("INVALID_ITEM_COUNT", "itemCount must be a positive integer");
    }
    const scope = await this.characterOperationScope(characterId);
    const assetEvidence = {
      itemId,
      itemDelta: itemCount,
      ledgerReference: "unavailable: game-server admin response does not return an asset ledger id"
    };

    return this.runHighRiskOperation({
      request: req,
      permission: "gm.asset_correction.emergency",
      scope,
      targetSummary: { targetType: "character", targetIds: [characterId] },
      payload: { characterId, itemId, itemCount },
      impactSummary: { targetType: "character", targetCount: 1, assetChange: "emergency_item_correction" },
      reason,
      emergency: true,
      execute: async () => {
        const gameAdminOptions = {
          ...createGameAdminOptions(req, body),
          requestId: body?.requestId ?? body?.request_id,
          source: "gm-emergency-correction"
        };
        gameAdminOptions.assertionContext = await createGameAssertionContext(
          req,
          body,
          "gm.asset_correction.emergency",
          this.adminStore,
          "character",
          [characterId]
        );
        gameAdminOptions.assertionContext.requestId = gameAdminOptions.requestId;
        try {
          const gameAdminResult = await this.gameAdminClient.sendItem(
            characterId,
            itemId,
            itemCount,
            reason,
            gameAdminOptions
          );
          await this.adminStore.appendAuditLog({
            adminId: req.admin.sub,
            adminUsername: req.admin.username,
            action: "gm_emergency_asset_correction",
            targetType: "character",
            targetValue: characterId,
            details: {
              itemId,
              itemCount,
              reason,
              requestId: gameAdminOptions.requestId,
              permission: "gm.asset_correction.emergency",
              gameAdmin: {
                ok: gameAdminResult?.ok === true,
                instanceId: gameAdminResult?.instanceId,
                endpoint: gameAdminResult?.endpoint
              }
            },
            ip: getClientIp(req, this.config)
          });
          await this.appendAssetGrantSecurityAudit(req, {
            action: "gm_emergency_asset_correction",
            characterId,
            itemId,
            itemCount,
            emergency: true
          });
          return { ok: true, message: "Emergency compensation submitted", requestId: gameAdminOptions.requestId };
        } catch (error: any) {
          throw gameServerError(error);
        }
      },
      resultSummary: () => ({
        action: "gm.asset_correction.emergency",
        targetCount: 1,
        outcome: "succeeded",
        asset: assetEvidence
      })
    });
  }

  @Post("kick-player")
  @Permissions("gm.kick_player")
  @HttpCode(HttpStatus.OK)
  async kickPlayer(@Body() body: any, @Req() req: any) {
    const { playerId, reason } = body || {};
    const normalizedPlayerId = normalizePlayerId(playerId);
    const normalizedReason = normalizeGmReason(normalizeReason(reason), "gm_kick");
    const scope = { targetType: "player", targetIds: [normalizedPlayerId], targetCount: 1 };
    return this.runHighRiskOperation({
      request: req,
      permission: "gm.kick_player",
      scope,
      targetSummary: { targetType: "player", targetIds: [normalizedPlayerId] },
      payload: { playerId: normalizedPlayerId },
      impactSummary: { targetType: "player", targetCount: 1, action: "session_kick" },
      reason: normalizedReason,
      execute: async () => {
        const gameAdminOptions = createGameAdminOptions(req, body);
        gameAdminOptions.assertionContext = await createGameAssertionContext(
          req,
          body,
          "gm.kick_player",
          this.adminStore,
          "player",
          [normalizedPlayerId]
        );
        const targetEndpoint = await preflightSingleTarget(this.gameAdminClient, gameAdminOptions);

        const globalKick = await this.publishGlobalSessionKick(normalizedPlayerId, normalizedReason);

        let legacyKick: any = { ok: true };
        try {
          const result = await this.gameAdminClient.kickPlayer(normalizedPlayerId, normalizedReason, {
            ...gameAdminOptions,
            endpoint: targetEndpoint
          });
          legacyKick = { ok: true, ...(result ?? {}) };
        } catch (error: any) {
          if (isGameAdminTargetSelectionError(error)) {
            throw gameServerError(error);
          }
          legacyKick = gameServerFailure(error);
        }

        await this.adminStore.appendAuditLog({
          adminId: req.admin.sub,
          adminUsername: req.admin.username,
          action: "gm_kick_player",
          targetType: "player",
          targetValue: normalizedPlayerId,
          details: {
            reason: normalizedReason,
            globalKick,
            legacyKick,
            requestedTargetInstanceId: gameAdminOptions.targetInstanceId
          },
          ip: getClientIp(req, this.config)
        });

        if (!globalKick.ok) {
          throw sessionKickPublishError(globalKick, legacyKick);
        }

        return { ok: true, message: "Player kicked", globalKick, legacyKick };
      },
      resultSummary: () => ({ action: "gm.kick_player", targetCount: 1, outcome: "succeeded" })
    });
  }

  @Post("ban-player")
  @Permissions("gm.ban_player")
  @HttpCode(HttpStatus.OK)
  async banPlayer(@Body() body: any, @Req() req: any) {
    const { playerId, durationSeconds, reason } = body || {};

    if (
      !durationSeconds ||
      typeof durationSeconds !== "number" ||
      !Number.isInteger(durationSeconds) ||
      durationSeconds <= 0 ||
      durationSeconds > GM_BAN_DURATION_MAX_SECONDS
    ) {
      throw badRequest("INVALID_DURATION", "durationSeconds must be a positive integer no greater than 31536000");
    }

    const normalizedPlayerId = normalizePlayerId(playerId);
    const normalizedReason = normalizeGmReason(normalizeReason(reason), "gm_ban");
    const scope = { targetType: "player", targetIds: [normalizedPlayerId], targetCount: 1 };
    return this.runHighRiskOperation({
      request: req,
      permission: "gm.ban_player",
      scope,
      targetSummary: { targetType: "player", targetIds: [normalizedPlayerId] },
      payload: { playerId: normalizedPlayerId, durationSeconds },
      impactSummary: { targetType: "player", targetCount: 1, action: "player_ban" },
      reason: normalizedReason,
      execute: async () => {
        const gameAdminOptions = createGameAdminOptions(req, body);
        gameAdminOptions.assertionContext = await createGameAssertionContext(
          req,
          body,
          "gm.ban_player",
          this.adminStore,
          "player",
          [normalizedPlayerId]
        );
        const targetEndpoint = await preflightSingleTarget(this.gameAdminClient, gameAdminOptions);
        const player = await this.adminStore.findPlayerById(normalizedPlayerId);
        if (!player) {
          throw notFound("PLAYER_NOT_FOUND", "Player not found");
        }

        const banExpiresAt = computeBanExpiresAt(durationSeconds);
        const updated = await this.adminStore.updatePlayerStatus(normalizedPlayerId, "banned", { banExpiresAt });
        if (!updated) {
          throw notFound("PLAYER_NOT_FOUND", "Player not found");
        }

        const globalKick = await this.publishGlobalSessionKick(normalizedPlayerId, normalizedReason);

        let legacyBan: any = { ok: true };
        try {
          const result = await this.gameAdminClient.banPlayer(
            normalizedPlayerId,
            durationSeconds,
            normalizedReason,
            {
              ...gameAdminOptions,
              endpoint: targetEndpoint
            }
          );
          legacyBan = { ok: true, ...(result ?? {}) };
        } catch (error: any) {
          if (isGameAdminTargetSelectionError(error)) {
            throw gameServerError(error);
          }
          legacyBan = gameServerFailure(error);
        }

        await this.adminStore.appendAuditLog({
          adminId: req.admin.sub,
          adminUsername: req.admin.username,
          action: "gm_ban_player",
          targetType: "player",
          targetValue: normalizedPlayerId,
          details: {
            from: player.status,
            to: "banned",
            durationSeconds,
            banExpiresAt,
            reason: normalizedReason,
            globalKick,
            legacyBan,
            requestedTargetInstanceId: gameAdminOptions.targetInstanceId
          },
          ip: getClientIp(req, this.config)
        });

        if (!globalKick.ok) {
          return {
            ok: false,
            error: "SESSION_KICK_PUBLISH_FAILED",
            message: globalKick.message || "Player banned, but global session kick failed",
            banStatus: "banned",
            banExpiresAt,
            globalKick,
            legacyBan
          };
        }

        return { ok: true, message: "Player banned", banExpiresAt, globalKick, legacyBan };
      },
      resultSummary: () => ({ action: "gm.ban_player", targetCount: 1, outcome: "succeeded" })
    });
  }

  @Post("characters/:characterId/elements")
  @Permissions("gm.character_elements.write")
  @HttpCode(HttpStatus.OK)
  async setCharacterElements(@Param("characterId") characterIdParam: string, @Body() body: any, @Req() req: any) {
    let characterId = typeof characterIdParam === "string" ? characterIdParam.trim() : "";
    let reason: string | null = null;

    try {
      characterId = normalizeCharacterId(characterIdParam);
      reason = normalizeReason(body?.reason);
      const affinity = normalizeElementValues(body?.affinity, "affinity");
      const mastery = normalizeElementValues(body?.mastery, "mastery");

      if (!affinity && !mastery) {
        throw badRequest("INVALID_CHARACTER_ELEMENT_PAYLOAD", "affinity or mastery is required");
      }

      const result = await this.adminStore.setCharacterElementsForAdmin({
        characterId,
        affinity,
        mastery,
        operatorType: "admin",
        operatorId: String(req.admin.sub),
        sourceType: "gm",
        sourceId: "admin-api-character-elements",
        reason
      });

      await this.appendGmCharacterAudit(req, {
        action: "gm_character_elements_set",
        characterId,
        details: {
          result: "success",
          reason,
          changed: result.changed,
          before: result.before,
          after: result.after,
          affinityDelta: result.affinityDelta,
          masteryDelta: result.masteryDelta,
          permission: "gm.character_elements.write"
        }
      });

      return {
        ok: true,
        message: result.changed ? "Character elements updated" : "Character elements unchanged",
        character: result.character,
        before: result.before,
        after: result.after,
        affinityDelta: result.affinityDelta,
        masteryDelta: result.masteryDelta,
        changed: result.changed
      };
    } catch (error: any) {
      await this.appendGmCharacterAudit(req, {
        action: "gm_character_elements_set_failed",
        characterId,
        details: {
          result: "failed",
          reason,
          error: auditErrorCode(error),
          permission: "gm.character_elements.write"
        }
      });
      throw adminStoreError(error);
    }
  }

  @Post("characters/:characterId/titles")
  @Permissions("gm.character_titles.write")
  @HttpCode(HttpStatus.OK)
  async applyCharacterTitle(@Param("characterId") characterIdParam: string, @Body() body: any, @Req() req: any) {
    let characterId = typeof characterIdParam === "string" ? characterIdParam.trim() : "";
    let action: string | null = null;
    let titleId: string | null = null;
    let reason: string | null = null;
    let expiresAt: string | null = null;

    try {
      characterId = normalizeCharacterId(characterIdParam);
      action = normalizeTitleAction(body?.action);
      titleId = normalizeTitleId(body?.titleId ?? body?.title_id);
      reason = normalizeReason(body?.reason);
      expiresAt = normalizeExpiresAt(body?.expiresAt ?? body?.expires_at);

      const titleDefinition = getTitleDefinitions()[titleId];
      if (!titleDefinition) {
        throw notFound("TITLE_CONFIG_NOT_FOUND", "title config not found");
      }

      if (action === "grant" && titleDefinition.limited === true && !expiresAt) {
        throw badRequest("LIMITED_TITLE_REQUIRES_EXPIRES_AT", "limited title requires expiresAt");
      }

      const result = await this.adminStore.applyCharacterTitleForAdmin({
        characterId,
        action,
        titleId,
        expiresAt,
        operatorType: "admin",
        operatorId: String(req.admin.sub),
        sourceType: "gm",
        sourceId: "admin-api-character-titles",
        reason
      });

      await this.appendGmCharacterAudit(req, {
        action: "gm_character_title_apply",
        characterId,
        details: {
          result: "success",
          reason,
          gmAction: action,
          titleId,
          expiresAt,
          status: result.status,
          changed: result.changed,
          before: result.before,
          after: result.after,
          permission: "gm.character_titles.write"
        }
      });

      return {
        ok: true,
        message: `Title ${result.status}`,
        ...result
      };
    } catch (error: any) {
      await this.appendGmCharacterAudit(req, {
        action: "gm_character_title_apply_failed",
        characterId,
        details: {
          result: "failed",
          reason,
          gmAction: action,
          titleId,
          expiresAt,
          error: auditErrorCode(error),
          permission: "gm.character_titles.write"
        }
      });
      throw adminStoreError(error);
    }
  }

  @Post("characters/:characterId/disciplines")
  @Permissions("gm.character_disciplines.write")
  @HttpCode(HttpStatus.OK)
  async setCharacterDiscipline(@Param("characterId") characterIdParam: string, @Body() body: any, @Req() req: any) {
    let characterId = typeof characterIdParam === "string" ? characterIdParam.trim() : "";
    let disciplineId: string | null = null;
    let points: number | null = null;
    let tier: string | null = null;
    let active: boolean | null = null;
    let reason: string | null = null;

    try {
      characterId = normalizeCharacterId(characterIdParam);
      disciplineId = normalizeDisciplineId(body?.disciplineId ?? body?.discipline_id);
      points = normalizeDisciplinePoints(body?.points);
      tier = normalizeDisciplineTier(body?.tier);
      active = normalizeBoolean(body?.active, "active");
      reason = normalizeReason(body?.reason);

      const result = await this.adminStore.setCharacterDisciplineForAdmin({
        characterId,
        disciplineId,
        points,
        tier,
        active,
        operatorType: "admin",
        operatorId: String(req.admin.sub),
        sourceType: "gm",
        sourceId: "admin-api-character-disciplines",
        reason
      });

      await this.appendGmCharacterAudit(req, {
        action: "gm_character_discipline_set",
        characterId,
        details: {
          result: "success",
          reason,
          disciplineId,
          points,
          tier,
          active,
          status: result.status,
          changed: result.changed,
          before: result.before,
          after: result.after,
          permission: "gm.character_disciplines.write"
        }
      });

      return {
        ok: true,
        message: result.changed ? "Character discipline updated" : "Character discipline unchanged",
        ...result
      };
    } catch (error: any) {
      await this.appendGmCharacterAudit(req, {
        action: "gm_character_discipline_set_failed",
        characterId,
        details: {
          result: "failed",
          reason,
          disciplineId,
          points,
          tier,
          active,
          error: auditErrorCode(error),
          permission: "gm.character_disciplines.write"
        }
      });
      throw adminStoreError(error);
    }
  }

  @Post("characters/:characterId/unlock-check")
  @Permissions("gm.character_titles.write", "gm.character_disciplines.write")
  @HttpCode(HttpStatus.OK)
  async runCharacterUnlockCheck(@Param("characterId") characterIdParam: string, @Body() body: any, @Req() req: any) {
    let characterId = typeof characterIdParam === "string" ? characterIdParam.trim() : "";
    let reason: string | null = null;

    try {
      characterId = normalizeCharacterId(characterIdParam);
      reason = normalizeReason(body?.reason);

      const result = await this.adminStore.runCharacterUnlockCheckForAdmin({
        characterId,
        titleDefinitions: getTitleDefinitions(),
        operatorType: "admin",
        operatorId: String(req.admin.sub),
        sourceType: "gm",
        sourceId: "admin-api-unlock-check",
        reason
      });

      await this.appendGmCharacterAudit(req, {
        action: "gm_character_unlock_check",
        characterId,
        details: {
          result: "success",
          reason,
          checked: result.checked,
          granted: result.granted,
          results: result.results,
          permission: ["gm.character_titles.write", "gm.character_disciplines.write"]
        }
      });

      return {
        ok: true,
        message: "Unlock check completed",
        ...result
      };
    } catch (error: any) {
      await this.appendGmCharacterAudit(req, {
        action: "gm_character_unlock_check_failed",
        characterId,
        details: {
          result: "failed",
          reason,
          error: auditErrorCode(error),
          permission: ["gm.character_titles.write", "gm.character_disciplines.write"]
        }
      });
      throw adminStoreError(error);
    }
  }

  private async appendGmCharacterAudit(
    req: any,
    {
      action,
      characterId,
      details
    }: {
      action: string;
      characterId: string;
      details: Record<string, unknown>;
    }
  ) {
    await this.adminStore.appendAuditLog({
      adminId: req.admin.sub,
      adminUsername: req.admin.username,
      action,
      targetType: "character",
      targetValue: characterId,
      details,
      ip: getClientIp(req, this.config)
    });
  }

  private async appendAssetGrantSecurityAudit(
    req: any,
    {
      action,
      characterId,
      itemId,
      itemCount,
      emergency
    }: {
      action: string;
      characterId: string;
      itemId: string;
      itemCount: number;
      emergency: boolean;
    }
  ) {
    const since = new Date(Date.now() - ASSET_GRANT_HIGH_FREQUENCY_WINDOW_MS).toISOString();
    const recentCount = typeof this.adminStore.countRecentAdminAuditActions === "function"
      ? await this.adminStore.countRecentAdminAuditActions({
        adminId: req.admin.sub,
        action,
        since
      })
      : 0;
    const large = itemCount >= ASSET_GRANT_LARGE_QUANTITY;
    const highFrequency = recentCount >= ASSET_GRANT_HIGH_FREQUENCY_LIMIT;
    if (!emergency && !large && !highFrequency) {
      return;
    }

    await appendSecurityAuditLog(this.adminStore, {
      eventType: emergency ? "asset_emergency_correction" : "asset_grant_anomaly",
      targetType: "character",
      targetValue: characterId,
      severity: emergency || large ? "critical" : "warning",
      clientIp: getSecurityAuditClientIp(req, this.config),
      details: {
        action,
        adminId: req.admin.sub,
        username: req.admin.username,
        itemId,
        itemCount,
        emergency,
        largeQuantity: large,
        highFrequency,
        recentActionCount: recentCount,
        windowMs: ASSET_GRANT_HIGH_FREQUENCY_WINDOW_MS
      }
    });
  }
}
