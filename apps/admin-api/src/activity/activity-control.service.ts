import { createHash, randomUUID } from "node:crypto";

import { createActivityTypeRegistry, validateActivityTypeConfig } from "../activity-types.js";

export interface ActivityControlService {
  list(query: Record<string, unknown>): Promise<unknown>;
  detail(activityId: string): Promise<unknown>;
  createDraft(command: Record<string, unknown>): Promise<unknown>;
  createDraftFromPublished(activityId: string, command: Record<string, unknown>): Promise<unknown>;
  updateDraft(activityId: string, command: Record<string, unknown>): Promise<unknown>;
  preflight(activityId: string, command: Record<string, unknown>): Promise<unknown>;
  publish(activityId: string, command: Record<string, unknown>): Promise<unknown>;
  offline(activityId: string, command: Record<string, unknown>): Promise<unknown>;
  records(activityId: string, query: ActivityRecordQuery): Promise<unknown>;
}

export interface ActivityRecordQuery {
  actorId?: string;
  version?: number;
  characterId?: string;
  status?: string;
  from?: string;
  to?: string;
  requestId?: string;
  limit?: number;
  offset?: number;
}

export interface ActivityRecord {
  recordId: string;
  activityId: string;
  version: number;
  recordType: "claim" | "draw" | "reward_grant";
  characterId?: string;
  requestId?: string;
  status: string;
  createdAt: string;
  details: Record<string, unknown>;
}

export interface ActivityAuditEvent {
  action: "draft_created" | "draft_updated" | "draft_forked" | "preflight" | "published" | "offlined" | "records_read";
  activityId?: string;
  actorId: string;
  reason?: string;
  version?: number;
  result: "success" | "failure";
  errorCode?: string;
  summary?: Record<string, unknown>;
}

export interface ActivityAuditSink {
  write(event: ActivityAuditEvent): Promise<void>;
}

export class NoopActivityAuditSink implements ActivityAuditSink {
  async write(_event: ActivityAuditEvent): Promise<void> {}
}

export class ActivityControlError extends Error {
  constructor(public readonly code: string, message: string, public readonly details?: unknown) {
    super(message);
    this.name = "ActivityControlError";
  }
}

export interface ActivityRefreshEvent {
  activityId: string;
  version: number;
  action: "published" | "offline";
  digest?: string;
}

export interface ActivityRefreshNotifier {
  notify(event: ActivityRefreshEvent): Promise<void>;
}

export class NoopActivityRefreshNotifier implements ActivityRefreshNotifier {
  async notify(_event: ActivityRefreshEvent): Promise<void> {}
}

export interface ActivityControlRepository {
  list(query: Record<string, unknown>): Promise<unknown>;
  detail(activityId: string): Promise<unknown>;
  createDraft(command: Record<string, unknown>): Promise<any>;
  createDraftFromPublished(activityId: string, command: Record<string, unknown>): Promise<any>;
  updateDraft(activityId: string, command: Record<string, unknown>): Promise<any>;
  publish(activityId: string, command: Record<string, unknown>): Promise<any>;
  offline(activityId: string, command: Record<string, unknown>): Promise<any>;
  records(activityId: string, query: ActivityRecordQuery): Promise<unknown>;
}

type ActivityStatus = "draft" | "published" | "offline";
interface StoredActivity {
  activityId: string;
  key: string;
  activityType: string;
  status: ActivityStatus;
  revision: number;
  draftVersion: number;
  draft: Record<string, unknown>;
  currentVersion?: number;
  versions: Map<number, Record<string, unknown>>;
  versionDigests: Map<number, string>;
  sourceSnapshot?: Record<string, unknown>;
  offlineReason?: string;
}

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T;
}

function canonicalJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (value && typeof value === "object") {
    return `{${Object.keys(value as Record<string, unknown>)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${canonicalJson((value as Record<string, unknown>)[key])}`)
      .join(",")}}`;
  }
  return JSON.stringify(value) ?? "null";
}

function activityConfigDigest(command: Record<string, unknown>): string {
  const payload = {
    public_config: command.publicConfig,
    type_config: command.typeConfig
  };
  return `sha256:${createHash("sha256").update(canonicalJson(payload)).digest("hex")}`;
}

function etag(activity: StoredActivity): string {
  return `W/"activity-${activity.activityId}-${activity.revision}"`;
}

function expectedMatches(activity: StoredActivity, value: unknown): boolean {
  if (value === undefined || value === null || value === "") return true;
  return String(value) === etag(activity) || String(value) === String(activity.revision);
}

function requireActivity(state: Map<string, StoredActivity>, activityId: string): StoredActivity {
  const activity = state.get(activityId);
  if (!activity) throw new ActivityControlError("ACTIVITY_NOT_FOUND", "activity was not found");
  return activity;
}

/** Offline adapter for contract tests. Production keeps the unavailable provider. */
export class InMemoryActivityControlRepository implements ActivityControlRepository {
  private readonly state = new Map<string, StoredActivity>();
  private readonly activityRecords: ActivityRecord[] = [];

  async list(query: Record<string, unknown>): Promise<unknown> {
    const status = typeof query.status === "string" ? query.status : undefined;
    const activityType = typeof query.activityType === "string" ? query.activityType : undefined;
    const key = typeof query.key === "string" ? query.key : undefined;
    const limit = Number(query.limit ?? 50);
    const offset = Number(query.offset ?? 0);
    const rows = [...this.state.values()]
      .filter((item) => (!status || item.status === status) && (!activityType || item.activityType === activityType) && (!key || item.key === key))
      .sort((left, right) => left.activityId.localeCompare(right.activityId));
    return { items: rows.slice(offset, offset + limit).map((item) => this.summary(item)), total: rows.length, limit, offset };
  }

  async detail(activityId: string): Promise<unknown> {
    return this.summary(requireActivity(this.state, activityId), true);
  }

  async records(activityId: string, query: ActivityRecordQuery): Promise<unknown> {
    requireActivity(this.state, activityId);
    const from = query.from ? Date.parse(query.from) : Number.NEGATIVE_INFINITY;
    const to = query.to ? Date.parse(query.to) : Number.POSITIVE_INFINITY;
    const rows = this.activityRecords
      .filter((record) => record.activityId === activityId)
      .filter((record) => query.version === undefined || record.version === query.version)
      .filter((record) => !query.characterId || record.characterId === query.characterId)
      .filter((record) => !query.status || record.status === query.status)
      .filter((record) => !query.requestId || record.requestId === query.requestId)
      .filter((record) => {
        const timestamp = Date.parse(record.createdAt);
        return Number.isFinite(timestamp) && timestamp >= from && timestamp < to;
      })
      .sort((left, right) => right.createdAt.localeCompare(left.createdAt) || right.recordId.localeCompare(left.recordId));
    const limit = Number(query.limit ?? 50);
    const offset = Number(query.offset ?? 0);
    return { items: rows.slice(offset, offset + limit).map(clone), total: rows.length, limit, offset };
  }

  /** Test-only fixture hook; production adapters must source append-only tables. */
  appendRecordForTest(record: ActivityRecord): void {
    if (this.activityRecords.some((item) => item.recordId === record.recordId)) throw new ActivityControlError("ACTIVITY_RECORD_CONFLICT", "record id already exists");
    this.activityRecords.push(clone(record));
  }

  async createDraft(command: Record<string, unknown>): Promise<any> {
    const activityId = typeof command.activityId === "string" && command.activityId.trim() ? command.activityId : randomUUID();
    if (this.state.has(activityId)) throw new ActivityControlError("ACTIVITY_KEY_CONFLICT", "activity id already exists");
    if ([...this.state.values()].some((item) => item.key === String(command.key))) throw new ActivityControlError("ACTIVITY_KEY_CONFLICT", "activity key already exists");
    const item: StoredActivity = {
      activityId,
      key: String(command.key),
      activityType: String(command.activityType),
      status: "draft",
      revision: 1,
      draftVersion: 1,
      draft: clone(command),
      versions: new Map(),
      versionDigests: new Map(),
    };
    this.state.set(activityId, item);
    return this.summary(item, true);
  }

  async updateDraft(activityId: string, command: Record<string, unknown>): Promise<any> {
    const item = requireActivity(this.state, activityId);
    if (item.status === "published") throw new ActivityControlError("ACTIVITY_PUBLISHED_IMMUTABLE", "published activity versions are immutable; create a new draft version");
    if (item.status !== "draft") throw new ActivityControlError("ACTIVITY_INVALID_STATE", "only draft activities can be edited");
    if (!expectedMatches(item, command.ifMatch)) throw new ActivityControlError("ACTIVITY_VERSION_CONFLICT", "draft was changed by another operator");
    item.revision += 1;
    item.draftVersion += 1;
    const { ifMatch: _ifMatch, ...nextDraft } = command;
    item.draft = clone(nextDraft);
    item.key = String(command.key);
    item.activityType = String(command.activityType);
    return this.summary(item, true);
  }

  async publish(activityId: string, command: Record<string, unknown>): Promise<any> {
    const item = requireActivity(this.state, activityId);
    if (item.status === "published") throw new ActivityControlError("ACTIVITY_ALREADY_PUBLISHED", "activity is already published");
    if (item.status === "offline") throw new ActivityControlError("ACTIVITY_INVALID_STATE", "offline activity requires a new version");
    const version = Number(command.version);
    if (!Number.isInteger(version) || version !== item.draftVersion) throw new ActivityControlError("ACTIVITY_VERSION_CONFLICT", "draft version is stale");
    if (!expectedMatches(item, command.ifMatch)) throw new ActivityControlError("ACTIVITY_VERSION_CONFLICT", "activity was changed by another operator");
    if (item.versions.has(version)) throw new ActivityControlError("ACTIVITY_VERSION_CONFLICT", "published version is immutable");
    const snapshot = clone(item.draft);
    const configDigest = activityConfigDigest(snapshot);
    item.versions.set(version, snapshot);
    item.versionDigests.set(version, configDigest);
    item.currentVersion = version;
    item.status = "published";
    item.revision += 1;
    return { ...this.summary(item, true), version, snapshot: clone(snapshot), configDigest, etag: etag(item) };
  }

  async offline(activityId: string, command: Record<string, unknown>): Promise<any> {
    const item = requireActivity(this.state, activityId);
    if (item.status === "offline") throw new ActivityControlError("ACTIVITY_ALREADY_OFFLINE", "activity is already offline");
    if (item.status !== "published") throw new ActivityControlError("ACTIVITY_INVALID_STATE", "only published activities can be taken offline");
    const version = Number(command.version);
    if (version !== item.currentVersion || !expectedMatches(item, command.ifMatch)) throw new ActivityControlError("ACTIVITY_VERSION_CONFLICT", "activity version is stale");
    item.status = "offline";
    item.offlineReason = String(command.reason ?? "");
    item.revision += 1;
    return { ...this.summary(item, true), version, offlineReason: item.offlineReason, etag: etag(item) };
  }

  private summary(item: StoredActivity, includeDraft = false): Record<string, unknown> {
    return {
      activityId: item.activityId,
      key: item.key,
      activityType: item.activityType,
      status: item.status,
      revision: item.revision,
      version: item.status === "draft" ? item.draftVersion : item.currentVersion ?? item.draftVersion,
      configDigest: item.status === "draft"
        ? activityConfigDigest(item.draft)
        : item.currentVersion === undefined ? undefined : item.versionDigests.get(item.currentVersion),
      ...(item.currentVersion !== undefined && includeDraft ? { snapshot: clone(item.versions.get(item.currentVersion)) } : {}),
      ...(item.sourceSnapshot !== undefined && includeDraft ? { sourceSnapshot: clone(item.sourceSnapshot) } : {}),
      etag: etag(item),
      ...(includeDraft ? { draft: clone(item.draft), offlineReason: item.offlineReason } : {}),
    };
  }

  async createDraftFromPublished(activityId: string, command: Record<string, unknown>): Promise<any> {
    const item = requireActivity(this.state, activityId);
    if (item.status !== "published") throw new ActivityControlError("ACTIVITY_INVALID_STATE", "only published activities can create a new draft");
    const sourceVersion = Number(command.sourceVersion);
    if (sourceVersion !== item.currentVersion || !expectedMatches(item, command.ifMatch)) throw new ActivityControlError("ACTIVITY_VERSION_CONFLICT", "published source version is stale");
    const source = item.versions.get(sourceVersion);
    if (!source) throw new ActivityControlError("ACTIVITY_NOT_FOUND", "published source version was not found");
    const overrides = command.overrides && typeof command.overrides === "object" && !Array.isArray(command.overrides) ? command.overrides as Record<string, unknown> : {};
    if ("activityId" in overrides || "key" in overrides || "activityType" in overrides) throw new ActivityControlError("ACTIVITY_INVALID_CONFIG", "activity identity cannot be overridden");
    const { sourceVersion: _sourceVersion, ifMatch: _ifMatch, overrides: _overrides, ...metadata } = command;
    const draft = { ...clone(source), ...clone(overrides), reason: metadata.reason };
    item.status = "draft";
    item.currentVersion = undefined;
    item.draftVersion = sourceVersion + 1;
    item.revision += 1;
    item.draft = draft;
    item.sourceSnapshot = clone(source);
    return this.summary(item, true);
  }
}

export interface ActivityPreflightError { path: string; code: string; message: string; }

function pushError(errors: ActivityPreflightError[], path: string, code: string, message: string): void {
  errors.push({ path, code, message });
}

function lotteryPoolDigest(typeConfig: Record<string, unknown>): string | undefined {
  if (typeConfig.pool_version === undefined || !Array.isArray(typeConfig.pool_items)) return undefined;
  const chunks: Buffer[] = [Buffer.from("lottery-pool-weight-v1")];
  const poolVersion = Buffer.alloc(4);
  poolVersion.writeUInt32LE(Number(typeConfig.pool_version));
  chunks.push(poolVersion);
  for (const item of typeConfig.pool_items as any[]) {
    const itemId = Buffer.alloc(4);
    itemId.writeInt32LE(Number(item?.item_id));
    const quantity = Buffer.alloc(4);
    quantity.writeUInt32LE(Number(item?.quantity));
    const weight = Buffer.alloc(8);
    weight.writeBigUInt64LE(BigInt(item?.weight));
    chunks.push(itemId, quantity, weight);
  }
  return `sha256:${createHash("sha256").update(Buffer.concat(chunks)).digest("hex")}`;
}

function lotteryPoolFreezeErrors(previous: Record<string, unknown> | undefined, current: Record<string, unknown>): ActivityPreflightError[] {
  if (current.activityType !== "lottery" || !previous || previous.activityType !== "lottery") return [];
  const previousTypeConfig = previous.typeConfig && typeof previous.typeConfig === "object" && !Array.isArray(previous.typeConfig)
    ? previous.typeConfig as Record<string, unknown>
    : undefined;
  const currentTypeConfig = current.typeConfig && typeof current.typeConfig === "object" && !Array.isArray(current.typeConfig)
    ? current.typeConfig as Record<string, unknown>
    : undefined;
  if (!previousTypeConfig || !currentTypeConfig) return [];
  const previousVersion = Number(previousTypeConfig.pool_version);
  const currentVersion = Number(currentTypeConfig.pool_version);
  const previousDigest = lotteryPoolDigest(previousTypeConfig);
  const currentDigest = lotteryPoolDigest(currentTypeConfig);
  const errors: ActivityPreflightError[] = [];
  if (Number.isInteger(previousVersion) && Number.isInteger(currentVersion) && currentVersion < previousVersion) {
    pushError(errors, "typeConfig.pool_version", "POOL_VERSION_ROLLBACK", "lottery pool_version cannot move backwards after publish");
  }
  if (previousVersion === currentVersion && previousDigest && currentDigest && previousDigest !== currentDigest) {
    pushError(errors, "typeConfig.pool_items", "POOL_VERSION_IMMUTABLE", "lottery pool items and weights require a new pool_version");
  }
  return errors;
}

function validateDraft(command: Record<string, unknown>): ActivityPreflightError[] {
  const errors: ActivityPreflightError[] = [];
  for (const field of ["key", "activityType", "startAt", "endAt", "claimDeadline", "timezone", "reason"]) {
    if (typeof command[field] !== "string" || !String(command[field]).trim()) pushError(errors, field, "REQUIRED", `${field} is required`);
  }
  if (!Number.isInteger(Number(command.schemaVersion)) || Number(command.schemaVersion) < 1) pushError(errors, "schemaVersion", "INVALID", "schemaVersion must be a positive integer");
  const start = Date.parse(String(command.startAt ?? ""));
  const end = Date.parse(String(command.endAt ?? ""));
  const deadline = Date.parse(String(command.claimDeadline ?? ""));
  if (!Number.isFinite(start)) pushError(errors, "startAt", "INVALID_TIME", "startAt must be an ISO timestamp");
  if (!Number.isFinite(end)) pushError(errors, "endAt", "INVALID_TIME", "endAt must be an ISO timestamp");
  if (Number.isFinite(start) && Number.isFinite(end) && start >= end) pushError(errors, "endAt", "INVALID_WINDOW", "endAt must be after startAt");
  if (!Number.isFinite(deadline)) pushError(errors, "claimDeadline", "INVALID_TIME", "claimDeadline must be an ISO timestamp");
  if (Number.isFinite(end) && Number.isFinite(deadline) && deadline < end) pushError(errors, "claimDeadline", "INVALID_WINDOW", "claimDeadline must be at or after endAt");
  if (typeof command.timezone === "string") {
    try { new Intl.DateTimeFormat("en-US", { timeZone: command.timezone }).format(); }
    catch { pushError(errors, "timezone", "INVALID_TIMEZONE", "timezone must be a valid IANA timezone"); }
  }
  if (!command.publicConfig || typeof command.publicConfig !== "object" || Array.isArray(command.publicConfig)) pushError(errors, "publicConfig", "INVALID_OBJECT", "publicConfig must be an object");
  if (!command.typeConfig || typeof command.typeConfig !== "object" || Array.isArray(command.typeConfig)) pushError(errors, "typeConfig", "INVALID_OBJECT", "typeConfig must be an object");
  if (typeof command.activityType === "string" && command.typeConfig && typeof command.typeConfig === "object") {
    const typeSchemaVersion = Number((command.typeConfig as Record<string, unknown>).schema_version);
    if (typeSchemaVersion !== Number(command.schemaVersion)) {
      pushError(errors, "typeConfig.schema_version", "SCHEMA_VERSION_MISMATCH", "typeConfig.schema_version must match schemaVersion");
    }
    try { validateActivityTypeConfig(createActivityTypeRegistry(), command.activityType, command.typeConfig); }
    catch (error: any) { pushError(errors, "typeConfig", error.code || "INVALID_CONFIG", error.message || "type config is invalid"); }
  }
  const groups = Array.isArray(command.rewardGroups) ? command.rewardGroups : [];
  const groupKeys = new Set<string>();
  groups.forEach((group: any, index) => {
    const path = `rewardGroups[${index}]`;
    if (!group || typeof group !== "object") return pushError(errors, path, "INVALID_OBJECT", "reward group must be an object");
    if (typeof group.key !== "string" || !group.key.trim()) pushError(errors, `${path}.key`, "REQUIRED", "reward group key is required");
    else if (groupKeys.has(group.key)) pushError(errors, `${path}.key`, "DUPLICATE", "reward group key must be unique");
    else groupKeys.add(group.key);
    if (!["fixed", "weighted"].includes(String(group.selectionMode))) pushError(errors, `${path}.selectionMode`, "INVALID", "selectionMode must be fixed or weighted");
    if (!Array.isArray(group.items) || group.items.length === 0) pushError(errors, `${path}.items`, "REQUIRED", "reward group must contain items");
    for (const [itemIndex, item] of (Array.isArray(group.items) ? group.items : []).entries()) {
      if (!item || typeof item !== "object") pushError(errors, `${path}.items[${itemIndex}]`, "INVALID_OBJECT", "reward item must be an object");
      else if (!Number.isInteger(Number((item as any).quantity)) || Number((item as any).quantity) <= 0) pushError(errors, `${path}.items[${itemIndex}].quantity`, "INVALID", "quantity must be positive");
    }
  });
  const stages = Array.isArray(command.stages) ? command.stages : [];
  const stageIds = new Set<string>();
  const stageNos = new Set<number>();
  stages.forEach((stage: any, index) => {
    const path = `stages[${index}]`;
    if (!stage || typeof stage !== "object") return pushError(errors, path, "INVALID_OBJECT", "stage must be an object");
    if (typeof stage.stageId !== "string" || !stage.stageId.trim()) pushError(errors, `${path}.stageId`, "REQUIRED", "stageId is required");
    else if (stageIds.has(stage.stageId)) pushError(errors, `${path}.stageId`, "DUPLICATE", "stageId must be unique");
    else stageIds.add(stage.stageId);
    const stageNo = Number(stage.stageNo);
    if (!Number.isInteger(stageNo) || stageNo <= 0) pushError(errors, `${path}.stageNo`, "INVALID", "stageNo must be positive");
    else if (stageNos.has(stageNo)) pushError(errors, `${path}.stageNo`, "DUPLICATE", "stageNo must be unique");
    else stageNos.add(stageNo);
    if (typeof stage.rewardGroupKey !== "string" || !groupKeys.has(stage.rewardGroupKey)) pushError(errors, `${path}.rewardGroupKey`, "UNKNOWN_REFERENCE", "rewardGroupKey must reference a reward group");
    if (!stage.qualification || typeof stage.qualification !== "object" || Array.isArray(stage.qualification)) pushError(errors, `${path}.qualification`, "INVALID_OBJECT", "qualification must be an object");
  });
  if (command.activityType === "login_reward" && stages.length > 0) {
    const typedStages = command.typeConfig && typeof command.typeConfig === "object" && !Array.isArray(command.typeConfig)
      ? Array.isArray((command.typeConfig as any).stages) ? (command.typeConfig as any).stages : []
      : [];
    const configuredByNo = new Map<number, any>(typedStages.map((stage: any) => [Number(stage.stage_no), stage]));
    const draftNos = stages.map((stage: any) => Number(stage.stageNo));
    if (draftNos.some((stageNo, index) => stageNo !== [...draftNos].sort((left, right) => left - right)[index])) {
      pushError(errors, "stages", "UNSORTED", "login_reward stages must be sorted by stageNo");
    }
    stages.forEach((stage: any, index) => {
      const typed = configuredByNo.get(Number(stage.stageNo));
      if (!typed) return;
      if (typed.reward_group_key !== stage.rewardGroupKey) {
        pushError(errors, `stages[${index}].rewardGroupKey`, "MISMATCH", "stage reward group must match typeConfig");
      }
    });
  }
  if (command.activityType === "lottery") {
    const typeConfig = command.typeConfig && typeof command.typeConfig === "object" && !Array.isArray(command.typeConfig)
      ? command.typeConfig as Record<string, unknown>
      : undefined;
    const poolItems = typeConfig && Array.isArray(typeConfig.pool_items) ? typeConfig.pool_items : [];
    const catalogItemIds = new Set<number>();
    groups.forEach((group: any) => {
      for (const item of Array.isArray(group?.items) ? group.items : []) {
        const rawId = item?.item_id ?? item?.itemId ?? item?.asset_id ?? item?.assetId ?? item?.id;
        const itemId = Number(rawId);
        if (Number.isInteger(itemId) && itemId > 0) catalogItemIds.add(itemId);
      }
    });
    if (poolItems.length > 0 && groups.length === 0) {
      pushError(errors, "rewardGroups", "REQUIRED", "lottery rewardGroups must provide the pool item catalog");
    }
    poolItems.forEach((item: any, index: number) => {
      const path = `typeConfig.pool_items[${index}]`;
      if (!item || typeof item !== "object" || Array.isArray(item)) return;
      if (!Number.isInteger(item.quantity) || item.quantity <= 0) pushError(errors, `${path}.quantity`, "INVALID", "quantity must be positive");
      if (!Number.isInteger(item.weight) || item.weight <= 0) pushError(errors, `${path}.weight`, "INVALID", "weight must be positive");
      if (groups.length > 0 && !catalogItemIds.has(Number(item.item_id))) {
        pushError(errors, `${path}.item_id`, "UNKNOWN_REFERENCE", "pool item must reference an item in rewardGroups");
      }
    });
  }
  if (!Array.isArray(command.stages)) pushError(errors, "stages", "REQUIRED", "stages must be an array");
  if (!Array.isArray(command.rewardGroups)) pushError(errors, "rewardGroups", "REQUIRED", "rewardGroups must be an array");
  return errors;
}

export class ActivityControlDomainService implements ActivityControlService {
  constructor(private readonly repository: ActivityControlRepository = new InMemoryActivityControlRepository(), private readonly notifier: ActivityRefreshNotifier = new NoopActivityRefreshNotifier(), private readonly audit: ActivityAuditSink = new NoopActivityAuditSink()) {}
  list(query: Record<string, unknown>): Promise<unknown> { return this.repository.list(query); }
  detail(activityId: string): Promise<unknown> { return this.repository.detail(activityId); }
  async createDraft(command: Record<string, unknown>): Promise<unknown> {
    try {
      const errors = validateDraft(command);
      if (errors.length) throw new ActivityControlError("ACTIVITY_INVALID_CONFIG", "draft failed preflight", errors);
      const result = await this.repository.createDraft(command);
      return this.withAudit(result, { action: "draft_created", activityId: String((result as any).activityId), actorId: String(command.actorId || "admin-api"), reason: String(command.reason), version: Number((result as any).version) });
    } catch (error: any) {
      await this.auditFailure("draft_created", command, error);
      throw error;
    }
  }
  async createDraftFromPublished(activityId: string, command: Record<string, unknown>): Promise<unknown> {
    try {
      const detail: any = await this.repository.detail(activityId);
      if (detail?.status !== "published") throw new ActivityControlError("ACTIVITY_INVALID_STATE", "only published activities can create a new draft");
      const source = detail?.snapshot as Record<string, unknown> | undefined;
      if (!source) throw new ActivityControlError("ACTIVITY_NOT_FOUND", "published source version was not found");
      const overrides = command.overrides && typeof command.overrides === "object" && !Array.isArray(command.overrides) ? command.overrides as Record<string, unknown> : {};
      const candidate = { ...clone(source), ...clone(overrides), reason: command.reason };
      const errors = validateDraft(candidate);
      if (errors.length) throw new ActivityControlError("ACTIVITY_INVALID_CONFIG", "new draft failed preflight", errors);
      const result = await this.repository.createDraftFromPublished(activityId, command);
      return this.withAudit(result, { action: "draft_forked", activityId, actorId: String(command.actorId || "admin-api"), reason: String(command.reason), version: Number((result as any).version) });
    } catch (error: any) {
      await this.auditFailure("draft_forked", { ...command, activityId }, error);
      throw error;
    }
  }
  async updateDraft(activityId: string, command: Record<string, unknown>): Promise<unknown> {
    try {
      const errors = validateDraft(command);
      if (errors.length) throw new ActivityControlError("ACTIVITY_INVALID_CONFIG", "draft failed preflight", errors);
      const result = await this.repository.updateDraft(activityId, command);
      return this.withAudit(result, { action: "draft_updated", activityId, actorId: String(command.actorId || "admin-api"), reason: String(command.reason), version: Number((result as any).version) });
    } catch (error: any) {
      await this.auditFailure("draft_updated", { ...command, activityId }, error);
      throw error;
    }
  }
  async preflight(activityId: string, command: Record<string, unknown>): Promise<unknown> {
    try {
      const detail: any = await this.repository.detail(activityId);
      const draft = detail?.draft as Record<string, unknown> | undefined;
      if (!draft) throw new ActivityControlError("ACTIVITY_NOT_FOUND", "activity draft was not found");
      const requestedVersion = command.version === undefined ? detail.version : Number(command.version);
      const errors = requestedVersion !== detail.version ? [{ path: "version", code: "ACTIVITY_VERSION_CONFLICT", message: "draft version is stale" }] : validateDraft(draft);
      const result = { activityId, version: requestedVersion, valid: errors.length === 0, errors };
      return this.withAudit(result, { action: "preflight", activityId, actorId: String(command.actorId || "admin-api"), reason: String(command.reason || "preflight"), version: requestedVersion });
    } catch (error: any) {
      await this.auditFailure("preflight", { ...command, activityId }, error);
      throw error;
    }
  }
  async publish(activityId: string, command: Record<string, unknown>): Promise<unknown> {
    try {
      if (typeof command.reason !== "string" || !command.reason.trim()) throw new ActivityControlError("ACTIVITY_INVALID_REQUEST", "reason is required");
      const detail: any = await this.repository.detail(activityId);
      const draft = detail?.draft as Record<string, unknown> | undefined;
      if (!draft) throw new ActivityControlError("ACTIVITY_NOT_FOUND", "activity draft was not found");
      const errors = validateDraft(draft);
      errors.push(...lotteryPoolFreezeErrors((detail?.snapshot || detail?.sourceSnapshot) as Record<string, unknown> | undefined, draft));
      if (errors.length) throw new ActivityControlError("ACTIVITY_PRECHECK_FAILED", "activity cannot be published", errors);
      const result: any = await this.repository.publish(activityId, command);
      let notification: Record<string, unknown> = { status: "sent" };
      try { await this.notifier.notify({ activityId, version: Number(result.version), action: "published", digest: typeof result.configDigest === "string" ? result.configDigest : undefined }); }
      catch (error: any) { notification = { status: "failed", error: String(error?.message || "refresh notification failed") }; }
      return this.withAudit({ ...result, notification }, { action: "published", activityId, actorId: String(command.actorId || "admin-api"), reason: String(command.reason), version: Number(result.version) });
    } catch (error: any) {
      await this.auditFailure("published", { ...command, activityId }, error);
      throw error;
    }
  }
  async offline(activityId: string, command: Record<string, unknown>): Promise<unknown> {
    try {
      if (typeof command.reason !== "string" || !command.reason.trim()) throw new ActivityControlError("ACTIVITY_INVALID_REQUEST", "reason is required");
      const result: any = await this.repository.offline(activityId, command);
      let notification: Record<string, unknown> = { status: "sent" };
      try { await this.notifier.notify({ activityId, version: Number(result.version), action: "offline", digest: typeof result.configDigest === "string" ? result.configDigest : undefined }); }
      catch (error: any) { notification = { status: "failed", error: String(error?.message || "refresh notification failed") }; }
      return this.withAudit({ ...result, notification }, { action: "offlined", activityId, actorId: String(command.actorId || "admin-api"), reason: String(command.reason), version: Number(result.version) });
    } catch (error: any) {
      await this.auditFailure("offlined", { ...command, activityId }, error);
      throw error;
    }
  }
  records(activityId: string, query: ActivityRecordQuery): Promise<unknown> {
    return this.repository.records(activityId, query)
      .then((result) => this.withAudit(result, { action: "records_read", activityId, actorId: query.actorId || "admin-api" }))
      .catch(async (error) => { await this.auditFailure("records_read", { ...query, activityId, actorId: query.actorId }, error); throw error; });
  }

  private async withAudit<T>(result: T, event: Omit<ActivityAuditEvent, "result">): Promise<T> {
    let auditStatus: Record<string, unknown> = { status: "sent" };
    try { await this.audit.write({ ...event, result: "success", summary: summarize(result) }); }
    catch (error: any) { auditStatus = { status: "failed", error: String(error?.message || "audit storage unavailable") }; }
    return result && typeof result === "object" ? { ...(result as Record<string, unknown>), audit: auditStatus } as T : result;
  }

  private async auditFailure(action: ActivityAuditEvent["action"], command: Record<string, unknown>, error: any): Promise<void> {
    try { await this.audit.write({ action, activityId: command.activityId ? String(command.activityId) : undefined, actorId: String(command.actorId || "admin-api"), reason: typeof command.reason === "string" ? command.reason : undefined, result: "failure", errorCode: error?.code || "ACTIVITY_UNKNOWN" }); }
    catch { /* preserve original operation error */ }
  }
}

function summarize(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object") return {};
  const object = value as Record<string, unknown>;
  return Object.fromEntries(["activityId", "version", "status", "total", "valid", "notification"].filter((key) => key in object).map((key) => [key, object[key]]));
}

export class ActivityControlUnavailableService implements ActivityControlService {
  private unavailable(): never { const error: any = new Error("ACTIVITY_CONTROL_UNAVAILABLE"); error.code = "ACTIVITY_CONTROL_UNAVAILABLE"; throw error; }
  async list(): Promise<unknown> { return this.unavailable(); }
  async detail(): Promise<unknown> { return this.unavailable(); }
  async createDraft(): Promise<unknown> { return this.unavailable(); }
  async createDraftFromPublished(): Promise<unknown> { return this.unavailable(); }
  async updateDraft(): Promise<unknown> { return this.unavailable(); }
  async preflight(): Promise<unknown> { return this.unavailable(); }
  async publish(): Promise<unknown> { return this.unavailable(); }
  async offline(): Promise<unknown> { return this.unavailable(); }
  async records(): Promise<unknown> { return this.unavailable(); }
}
