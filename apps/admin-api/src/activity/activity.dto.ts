import { IsArray, IsInt, IsObject, IsOptional, IsString, Max, Min } from "class-validator";

export const ACTIVITY_JSON_MAX_BYTES = 64 * 1024;
export const ACTIVITY_JSON_MAX_DEPTH = 8;

export class ActivityStageDto {
  @IsString() stageId!: string;
  @IsInt() @Min(1) stageNo!: number;
  @IsString() rewardGroupKey!: string;
  @IsObject() qualification!: Record<string, unknown>;
}

export class ActivityRewardGroupDto {
  @IsString() key!: string;
  @IsString() selectionMode!: string;
  @IsArray() items!: unknown[];
}

export class ActivityDraftDto {
  @IsString() key!: string;
  @IsString() activityType!: string;
  @IsInt() @Min(1) schemaVersion!: number;
  @IsString() startAt!: string;
  @IsString() endAt!: string;
  @IsString() claimDeadline!: string;
  @IsString() timezone!: string;
  @IsObject() publicConfig!: Record<string, unknown>;
  @IsObject() typeConfig!: Record<string, unknown>;
  @IsArray() stages!: ActivityStageDto[];
  @IsArray() rewardGroups!: ActivityRewardGroupDto[];
  @IsString() reason!: string;
  @IsOptional() @IsString() ifMatch?: string;
}

export class ActivityVersionCommandDto {
  @IsInt() @Min(1) version!: number;
  @IsOptional() @IsString() ifMatch?: string;
  @IsString() reason!: string;
}

export class ActivityListQueryDto {
  @IsOptional() @IsString() status?: string;
  @IsOptional() @IsString() activityType?: string;
  @IsOptional() @IsString() key?: string;
  @IsOptional() @IsInt() @Min(1) @Max(100) limit?: number;
  @IsOptional() @IsInt() @Min(0) offset?: number;
}

export function assertStrictJson(value: unknown, allowed: readonly string[], path = "body", depth = 0): void {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`ACTIVITY_INVALID_REQUEST:${path} must be an object`);
  }
  if (depth > ACTIVITY_JSON_MAX_DEPTH) throw new Error("ACTIVITY_JSON_TOO_DEEP");
  const bytes = Buffer.byteLength(JSON.stringify(value));
  if (bytes > ACTIVITY_JSON_MAX_BYTES) throw new Error("ACTIVITY_JSON_TOO_LARGE");
  for (const [key, child] of Object.entries(value as Record<string, unknown>)) {
    if (!allowed.includes(key)) throw new Error(`ACTIVITY_UNKNOWN_FIELD:${path}.${key}`);
    if (child && typeof child === "object") assertJsonDepth(child, depth + 1);
  }
}

export function assertActivityDraftShape(body: Record<string, unknown>): void {
  assertJsonObject(body.typeConfig, "typeConfig");
  if (!Array.isArray(body.stages) || !Array.isArray(body.rewardGroups)) {
    throw new Error("ACTIVITY_INVALID_CONFIG:stages and rewardGroups must be arrays");
  }
  body.stages.forEach((stage, index) =>
    assertStrictJson(stage, ["stageId", "stageNo", "rewardGroupKey", "qualification"], `stages[${index}]`)
  );
  body.rewardGroups.forEach((group, index) =>
    assertStrictJson(group, ["key", "selectionMode", "items"], `rewardGroups[${index}]`)
  );
}

export function assertJsonObject(value: unknown, path: string): void {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`ACTIVITY_INVALID_CONFIG:${path} must be an object`);
  }
  const bytes = Buffer.byteLength(JSON.stringify(value));
  if (bytes > ACTIVITY_JSON_MAX_BYTES) throw new Error("ACTIVITY_JSON_TOO_LARGE");
  assertJsonDepth(value, 1);
}

function assertJsonDepth(value: object, depth: number): void {
  if (depth > ACTIVITY_JSON_MAX_DEPTH) throw new Error("ACTIVITY_JSON_TOO_DEEP");
  for (const child of Object.values(value)) {
    if (child && typeof child === "object") assertJsonDepth(child, depth + 1);
  }
}
