import { Body, Controller, Get, Inject, Param, Patch, Post, Query, UseGuards } from "@nestjs/common";
import { ApiBearerAuth, ApiTags } from "@nestjs/swagger";
import { createActivityTypeRegistry, validateActivityTypeConfig } from "../activity-types.js";
import { JwtAuthGuard } from "../auth/jwt-auth.guard.js";
import { AdminPolicyGuard } from "../auth/admin-policy.guard.js";
import { Permissions } from "../auth/roles.decorator.js";
import { ApiHttpException, badRequest } from "../common/http-exception.js";
import { ADMIN_ACTIVITY_CONTROL } from "../tokens.js";
import { assertActivityDraftShape, assertStrictJson } from "./activity.dto.js";
import type { ActivityControlService } from "./activity-control.service.js";

const DRAFT_FIELDS = ["key", "activityType", "schemaVersion", "startAt", "endAt", "claimDeadline", "timezone", "publicConfig", "typeConfig", "stages", "rewardGroups", "reason"] as const;
const VERSION_FIELDS = ["version", "ifMatch", "reason"] as const;

function text(value: unknown, name: string, max = 128): string {
  if (typeof value !== "string" || !value.trim() || value.length > max) throw badRequest("ACTIVITY_INVALID_REQUEST", `${name} is invalid`);
  return value.trim();
}

function page(query: any) {
  const limit = query?.limit === undefined ? 50 : Number(query.limit);
  const offset = query?.offset === undefined ? 0 : Number(query.offset);
  if (!Number.isInteger(limit) || limit < 1 || limit > 100 || !Number.isInteger(offset) || offset < 0) {
    throw badRequest("ACTIVITY_INVALID_PAGE", "limit or offset is invalid");
  }
  return { limit, offset };
}

@ApiTags("activities")
@ApiBearerAuth()
@UseGuards(JwtAuthGuard, AdminPolicyGuard)
@Controller("/api/v1/activities")
export class ActivityController {
  private readonly schemas = createActivityTypeRegistry();
  constructor(@Inject(ADMIN_ACTIVITY_CONTROL) private readonly service: ActivityControlService) {}

  @Get()
  @Permissions("activities.read")
  async list(@Query() query: any) {
    try { assertStrictJson(query ?? {}, ["status", "activityType", "key", "limit", "offset"], "query"); }
    catch (error: any) { throw this.contractError(error); }
    return this.invoke(() => this.service.list({ ...page(query), status: query?.status, activityType: query?.activityType, key: query?.key }));
  }

  @Get(":activityId")
  @Permissions("activities.read")
  async detail(@Param("activityId") activityId: string) {
    return this.invoke(() => this.service.detail(text(activityId, "activityId")));
  }

  @Post("drafts")
  @Permissions("activities.write")
  async createDraft(@Body() body: any) {
    const command = this.draft(body);
    return this.invoke(() => this.service.createDraft(command));
  }

  @Patch(":activityId/drafts")
  @Permissions("activities.write")
  async updateDraft(@Param("activityId") activityId: string, @Body() body: any) {
    const command = this.draft(body);
    return this.invoke(() => this.service.updateDraft(text(activityId, "activityId"), command));
  }

  @Post(":activityId/preflight")
  @Permissions("activities.publish")
  async preflight(@Param("activityId") activityId: string, @Body() body: any) {
    return this.invoke(() => this.service.preflight(text(activityId, "activityId"), this.versionCommand(body)));
  }

  @Post(":activityId/publish")
  @Permissions("activities.publish")
  async publish(@Param("activityId") activityId: string, @Body() body: any) {
    return this.invoke(() => this.service.publish(text(activityId, "activityId"), this.versionCommand(body)));
  }

  @Post(":activityId/offline")
  @Permissions("activities.publish")
  async offline(@Param("activityId") activityId: string, @Body() body: any) {
    return this.invoke(() => this.service.offline(text(activityId, "activityId"), this.versionCommand(body)));
  }

  @Get(":activityId/records")
  @Permissions("activities.records.read")
  async records(@Param("activityId") activityId: string, @Query() query: any) {
    try { assertStrictJson(query ?? {}, ["status", "characterId", "limit", "offset"], "query"); }
    catch (error: any) { throw this.contractError(error); }
    return this.invoke(() => this.service.records(text(activityId, "activityId"), { ...page(query), status: query?.status, characterId: query?.characterId }));
  }

  private draft(body: any) {
    try {
      assertStrictJson(body, DRAFT_FIELDS);
      assertActivityDraftShape(body);
      const type = text(body.activityType, "activityType", 64);
      const schemaVersion = Number(body.schemaVersion);
      if (!Number.isInteger(schemaVersion) || schemaVersion < 1) throw new Error("ACTIVITY_SCHEMA_VERSION_UNSUPPORTED");
      validateActivityTypeConfig(this.schemas, type, { ...body.typeConfig, schema_version: schemaVersion });
      return { ...body, key: text(body.key, "key", 64), activityType: type, schemaVersion, reason: text(body.reason, "reason", 512) };
    } catch (error: any) {
      throw this.contractError(error);
    }
  }

  private versionCommand(body: any) {
    try {
      assertStrictJson(body, VERSION_FIELDS);
      const version = Number(body.version);
      if (!Number.isInteger(version) || version < 1) throw new Error("ACTIVITY_VERSION_CONFLICT");
      return { version, ifMatch: body.ifMatch ? text(body.ifMatch, "ifMatch", 128) : undefined, reason: text(body.reason, "reason", 512) };
    } catch (error: any) { throw this.contractError(error); }
  }

  private contractError(error: any) {
    const code = error?.code || String(error?.message || "ACTIVITY_INVALID_REQUEST").split(":")[0];
    return badRequest(code, "activity request does not satisfy the control-plane contract");
  }

  private async invoke(operation: () => Promise<unknown>) {
    try { return { ok: true, ...(await operation() as object) }; }
    catch (error: any) {
      if (error?.code === "ACTIVITY_CONTROL_UNAVAILABLE") {
        throw new ApiHttpException(503, { ok: false, error: error.code, message: "activity control persistence is not enabled" });
      }
      throw error;
    }
  }
}
