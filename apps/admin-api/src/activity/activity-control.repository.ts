import { createHash, randomUUID } from "node:crypto";

import {
  ActivityControlError,
  type ActivityAuditEvent,
  type ActivityAuditSink,
  type ActivityControlRepository,
  type ActivityRecord,
  type ActivityRecordQuery,
  type ActivityRefreshEvent,
  type ActivityRefreshNotifier
} from "./activity-control.service.js";

interface QueryResult<T = any> { rows: T[]; rowCount?: number | null; }
interface Queryable { query<T = any>(text: string, values?: unknown[]): Promise<QueryResult<T>>; }
interface PoolLike extends Queryable { connect?(): Promise<Queryable & { release?(): void }>; }

const REWARD_ITEM_I32_MAX = 0x7fffffff;
const REWARD_QUANTITY_U32_MAX = 0xffffffff;
const REWARD_ITEM_FIELDS = new Set(["item_id", "quantity", "weight", "binding"]);
const REWARD_GROUP_FIELDS = new Set(["key", "selectionMode", "items"]);
const REWARD_BINDINGS = new Set(["unbound", "character_bound"]);

interface ActivityRow {
  activity_id: string;
  activity_key: string;
  activity_type: string;
  status: string;
  current_version: number | null;
  start_at: Date | string;
  end_at: Date | string;
  claim_deadline: Date | string;
  timezone: string;
  offline_reason: string | null;
  revision: string | number;
  draft_version: number | null;
  draft_public_config: Record<string, unknown> | null;
  draft_type_config: Record<string, unknown> | null;
  draft_digest: string | null;
  draft_start_at: Date | string | null;
  draft_end_at: Date | string | null;
  draft_claim_deadline: Date | string | null;
  draft_timezone: string | null;
  draft_reason: string | null;
  current_public_config: Record<string, unknown> | null;
  current_type_config: Record<string, unknown> | null;
  current_digest: string | null;
  current_start_at: Date | string | null;
  current_end_at: Date | string | null;
  current_claim_deadline: Date | string | null;
  current_timezone: string | null;
  current_reason: string | null;
}

interface SnapshotColumns {
  version: number;
  publicConfig: Record<string, unknown>;
  typeConfig: Record<string, unknown>;
  digest: string;
  startAt: Date | string;
  endAt: Date | string;
  claimDeadline: Date | string;
  timezone: string;
  reason: string;
}

const ACTIVITY_SELECT = `
SELECT
  a.activity_id, a.activity_key, a.activity_type, a.status, a.current_version,
  a.start_at, a.end_at, a.claim_deadline, a.timezone, a.offline_reason,
  a.xmin::text AS revision,
  dv.version_no AS draft_version,
  dv.public_config_json AS draft_public_config,
  dv.type_config_json AS draft_type_config,
  dv.config_digest AS draft_digest,
  dv.start_at AS draft_start_at,
  dv.end_at AS draft_end_at,
  dv.claim_deadline AS draft_claim_deadline,
  dv.timezone AS draft_timezone,
  dv.change_reason AS draft_reason,
  cv.public_config_json AS current_public_config,
  cv.type_config_json AS current_type_config,
  cv.config_digest AS current_digest,
  cv.start_at AS current_start_at,
  cv.end_at AS current_end_at,
  cv.claim_deadline AS current_claim_deadline,
  cv.timezone AS current_timezone,
  cv.change_reason AS current_reason
FROM activity a
LEFT JOIN LATERAL (
  SELECT v.* FROM activity_version v
  WHERE v.activity_id = a.activity_id AND v.published_at IS NULL
  ORDER BY v.version_no DESC LIMIT 1
) dv ON true
LEFT JOIN activity_version cv
  ON cv.activity_id = a.activity_id AND cv.version_no = a.current_version`;

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

function invalidRewardCatalog(path: string, message: string): never {
  throw new ActivityControlError("ACTIVITY_INVALID_CONFIG", message, [{ path, code: "INVALID", message }]);
}

function normalizedRewardGroups(command: Record<string, unknown>): Array<Record<string, unknown>> {
  if (!Array.isArray(command.rewardGroups)) invalidRewardCatalog("rewardGroups", "rewardGroups must be an array");
  return command.rewardGroups.map((value, groupIndex) => {
    const groupPath = `rewardGroups[${groupIndex}]`;
    if (!value || typeof value !== "object" || Array.isArray(value)) invalidRewardCatalog(groupPath, "reward group must be an object");
    const group = value as Record<string, unknown>;
    for (const field of Object.keys(group)) {
      if (!REWARD_GROUP_FIELDS.has(field)) invalidRewardCatalog(`${groupPath}.${field}`, "reward group field is not allowed");
    }
    if (typeof group.key !== "string" || !group.key.trim()) invalidRewardCatalog(`${groupPath}.key`, "reward group key is required");
    if (!["fixed", "weighted"].includes(String(group.selectionMode))) invalidRewardCatalog(`${groupPath}.selectionMode`, "selectionMode must be fixed or weighted");
    if (!Array.isArray(group.items) || group.items.length === 0) invalidRewardCatalog(`${groupPath}.items`, "reward group must contain items");
    const weighted = group.selectionMode === "weighted";
    let totalWeight = 0;
    const items = group.items.map((value, itemIndex) => {
      const itemPath = `${groupPath}.items[${itemIndex}]`;
      if (!value || typeof value !== "object" || Array.isArray(value)) invalidRewardCatalog(itemPath, "reward item must be an object");
      const item = value as Record<string, unknown>;
      for (const field of Object.keys(item)) {
        if (!REWARD_ITEM_FIELDS.has(field)) invalidRewardCatalog(`${itemPath}.${field}`, "reward item field is not allowed");
      }
      if (!Number.isInteger(item.item_id) || Number(item.item_id) < 1 || Number(item.item_id) > REWARD_ITEM_I32_MAX) {
        invalidRewardCatalog(`${itemPath}.item_id`, "item_id must be a positive int32");
      }
      if (!Number.isInteger(item.quantity) || Number(item.quantity) < 1 || Number(item.quantity) > REWARD_QUANTITY_U32_MAX) {
        invalidRewardCatalog(`${itemPath}.quantity`, "quantity must be a positive uint32");
      }
      const hasWeight = Object.hasOwn(item, "weight");
      if (weighted && !hasWeight) invalidRewardCatalog(`${itemPath}.weight`, "weighted reward item requires weight");
      if (hasWeight && (!Number.isSafeInteger(item.weight) || Number(item.weight) < 1)) {
        invalidRewardCatalog(`${itemPath}.weight`, "weight must be a positive safe integer");
      }
      if (weighted) {
        totalWeight += Number(item.weight);
        if (!Number.isSafeInteger(totalWeight)) invalidRewardCatalog(`${groupPath}.items`, "reward group weights exceed safe integer range");
      }
      if (item.binding !== undefined && (typeof item.binding !== "string" || !REWARD_BINDINGS.has(item.binding))) {
        invalidRewardCatalog(`${itemPath}.binding`, "binding must be unbound or character_bound");
      }
      return {
        item_id: Number(item.item_id),
        quantity: Number(item.quantity),
        ...(hasWeight ? { weight: Number(item.weight) } : {}),
        ...(item.binding === undefined ? {} : { binding: item.binding })
      };
    });
    return { key: group.key, selectionMode: group.selectionMode, items };
  });
}

function persistedPublicConfig(command: Record<string, unknown>): Record<string, unknown> {
  const { reward_groups: _ignored, ...publicConfig } = jsonObject(command.publicConfig);
  const rewardGroups = normalizedRewardGroups(command);
  return {
    ...publicConfig,
    reward_groups: rewardGroups.map((group) => ({
      key: group.key,
      selection_mode: group.selectionMode,
      items: group.items
    }))
  };
}

function configDigest(publicConfig: Record<string, unknown>, typeConfig: unknown): string {
  const payload = { public_config: publicConfig, type_config: typeConfig };
  return `sha256:${createHash("sha256").update(canonicalJson(payload)).digest("hex")}`;
}

export function postgresActivityConfigDigest(command: Record<string, unknown>): string {
  return configDigest(persistedPublicConfig(command), command.typeConfig);
}

function iso(value: Date | string): string {
  return new Date(value).toISOString();
}

function etag(activityId: string, revision: string | number): string {
  return `W/"activity-${activityId}-${revision}"`;
}

function expectedMatches(activityId: string, revision: string | number, value: unknown): boolean {
  if (value === undefined || value === null || value === "") return false;
  return String(value) === etag(activityId, revision) || String(value) === String(revision);
}

function databaseError(error: any): ActivityControlError {
  if (error instanceof ActivityControlError) return error;
  if (error?.code === "23505") {
    const constraint = String(error.constraint || "");
    if (constraint === "activity_pkey" || constraint.includes("activity_key")) {
      return new ActivityControlError("ACTIVITY_KEY_CONFLICT", "activity identity already exists");
    }
    return new ActivityControlError("ACTIVITY_VERSION_CONFLICT", "activity version already exists");
  }
  return new ActivityControlError("ACTIVITY_CONTROL_UNAVAILABLE", "activity database is unavailable");
}

function jsonObject(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : {};
}

function positiveInteger(value: unknown, fallback = 1): number {
  const parsed = Number(value);
  return Number.isInteger(parsed) && parsed > 0 ? parsed : fallback;
}

function requestReference(value: unknown): string | undefined {
  if (typeof value !== "string" || !value) return undefined;
  return `sha256:${createHash("sha256").update(value).digest("hex")}`;
}

function recordReference(recordType: unknown, value: unknown): string {
  const raw = String(value);
  if (recordType !== "reward_mail") return raw;
  return `mail:sha256:${createHash("sha256").update(raw).digest("hex")}`;
}

function auditSummary(value: unknown): Record<string, unknown> {
  const source = jsonObject(value);
  const summary: Record<string, unknown> = {};
  for (const key of ["activityId", "version", "status", "total", "valid"]) {
    const item = source[key];
    if (typeof item === "string") summary[key] = item.slice(0, 128);
    else if (typeof item === "number" && Number.isFinite(item)) summary[key] = item;
    else if (typeof item === "boolean") summary[key] = item;
  }
  const notification = jsonObject(source.notification);
  if (typeof notification.status === "string") {
    summary.notification = { status: notification.status.slice(0, 32) };
  }
  return summary;
}

export class RedisActivityRefreshNotifier implements ActivityRefreshNotifier {
  constructor(private readonly redis: any, private readonly keyPrefix = "") {}

  async notify(event: ActivityRefreshEvent): Promise<void> {
    const payload = JSON.stringify({
      activity_id: event.activityId,
      version_no: event.version,
      published_at: new Date().toISOString()
    });
    await this.redis.publish(`${this.keyPrefix}activity:refresh`, payload);
  }
}

export class PostgresActivityControlRepository implements ActivityControlRepository, ActivityAuditSink {
  constructor(private readonly pool: PoolLike) {}

  async list(query: Record<string, unknown>): Promise<unknown> {
    const filterValues: unknown[] = [];
    const where: string[] = [];
    const filter = (column: string, value: unknown) => {
      if (typeof value === "string" && value.trim()) {
        filterValues.push(value.trim());
        where.push(`${column} = $${filterValues.length}`);
      }
    };
    filter("a.status", query.status);
    filter("a.activity_type", query.activityType);
    filter("a.activity_key", query.key);
    const limit = positiveInteger(query.limit, 50);
    const offset = Math.max(0, Number(query.offset) || 0);
    const whereClause = where.length ? `WHERE ${where.join(" AND ")}` : "";
    const pageValues = [...filterValues, limit, offset];
    try {
      const [countResult, pageResult] = await Promise.all([
        this.pool.query<{ total_count: string }>(
          `SELECT count(*)::text AS total_count FROM activity a ${whereClause}`,
          filterValues
        ),
        this.pool.query<ActivityRow>(
          `SELECT q.* FROM (
          ${ACTIVITY_SELECT}
          ${whereClause}
        ) q
        ORDER BY q.activity_id
        LIMIT $${pageValues.length - 1} OFFSET $${pageValues.length}`,
          pageValues
        )
      ]);
      return {
        items: pageResult.rows.map((row) => this.summary(row)),
        total: Number(countResult.rows[0]?.total_count ?? 0),
        limit,
        offset
      };
    } catch (error) {
      throw databaseError(error);
    }
  }

  async detail(activityId: string): Promise<unknown> {
    try {
      return await this.loadDetail(this.pool, activityId);
    } catch (error) {
      throw databaseError(error);
    }
  }

  async createDraft(command: Record<string, unknown>): Promise<unknown> {
    const activityId = typeof command.activityId === "string" && command.activityId.trim()
      ? command.activityId.trim()
      : randomUUID();
    await this.transaction(async (client) => {
      await client.query(
        `INSERT INTO activity (
          activity_id, activity_key, activity_type, scope, status,
          start_at, end_at, claim_deadline, timezone, show_before_start,
          claim_mode, current_version, created_by
        ) VALUES ($1, $2, $3, 'character', 'draft', $4, $5, $6, $7, $8, $9, NULL, $10)`,
        [
          activityId,
          command.key,
          command.activityType,
          command.startAt,
          command.endAt,
          command.claimDeadline,
          command.timezone,
          Boolean(jsonObject(command.publicConfig).show_before_start),
          this.claimMode(command),
          String(command.actorId || "admin-api")
        ]
      );
      await this.insertVersion(client, activityId, 1, command);
    });
    return this.detail(activityId);
  }

  async updateDraft(activityId: string, command: Record<string, unknown>): Promise<unknown> {
    await this.transaction(async (client) => {
      const row = await this.lockActivity(client, activityId);
      if (row.status === "published") {
        throw new ActivityControlError("ACTIVITY_PUBLISHED_IMMUTABLE", "published activity versions are immutable; create a new draft version");
      }
      if (row.status !== "draft" || row.draft_version === null) {
        throw new ActivityControlError("ACTIVITY_INVALID_STATE", "only draft activities can be edited");
      }
      this.requireMatch(row, command.ifMatch);
      const version = Number(row.draft_version);
      const publicConfig = persistedPublicConfig(command);
      await client.query(
        `UPDATE activity SET activity_key = $2, activity_type = $3, start_at = $4,
          end_at = $5, claim_deadline = $6, timezone = $7, show_before_start = $8,
          claim_mode = $9, updated_at = current_timestamp WHERE activity_id = $1`,
        [activityId, command.key, command.activityType, command.startAt, command.endAt,
          command.claimDeadline, command.timezone,
          Boolean(jsonObject(command.publicConfig).show_before_start), this.claimMode(command)]
      );
      const result = await client.query(
        `UPDATE activity_version SET public_config_json = $3, type_config_json = $4,
          config_digest = $5, start_at = $6, end_at = $7, claim_deadline = $8,
          timezone = $9, change_reason = $10
        WHERE activity_id = $1 AND version_no = $2 AND published_at IS NULL`,
        [activityId, version, publicConfig, command.typeConfig,
          configDigest(publicConfig, command.typeConfig), command.startAt, command.endAt,
          command.claimDeadline, command.timezone, command.reason]
      );
      if ((result.rowCount ?? 0) !== 1) {
        throw new ActivityControlError("ACTIVITY_VERSION_CONFLICT", "published activity version is immutable");
      }
      await this.replaceChildren(client, activityId, version, command);
    });
    return this.detail(activityId);
  }

  async createDraftFromPublished(activityId: string, command: Record<string, unknown>): Promise<unknown> {
    await this.transaction(async (client) => {
      const row = await this.lockActivity(client, activityId);
      if (!new Set(["published", "offline"]).has(row.status)) {
        throw new ActivityControlError("ACTIVITY_INVALID_STATE", "only published or offline activities can create a new draft");
      }
      const sourceVersion = Number(command.sourceVersion);
      if (sourceVersion !== Number(row.current_version)) {
        throw new ActivityControlError("ACTIVITY_VERSION_CONFLICT", "published source version is stale");
      }
      this.requireMatch(row, command.ifMatch);
      const detail = await this.loadDetail(client, activityId);
      const source = jsonObject((detail as any).snapshot);
      const overrides = jsonObject(command.overrides);
      if (["activityId", "key", "activityType"].some((key) => key in overrides)) {
        throw new ActivityControlError("ACTIVITY_INVALID_CONFIG", "activity identity cannot be overridden");
      }
      const candidate: Record<string, unknown> = { ...source, ...overrides, reason: command.reason };
      const nextVersion = sourceVersion + 1;
      await this.insertVersion(client, activityId, nextVersion, candidate);
      await client.query(
        `UPDATE activity SET status = 'draft', start_at = $2, end_at = $3,
          claim_deadline = $4, timezone = $5, show_before_start = $6,
          claim_mode = $7, updated_at = current_timestamp WHERE activity_id = $1`,
        [activityId, candidate.startAt, candidate.endAt, candidate.claimDeadline,
          candidate.timezone, Boolean(jsonObject(candidate.publicConfig).show_before_start),
          this.claimMode(candidate)]
      );
    });
    return this.detail(activityId);
  }

  async publish(activityId: string, command: Record<string, unknown>): Promise<unknown> {
    await this.transaction(async (client) => {
      const row = await this.lockActivity(client, activityId);
      if (row.status === "published") {
        throw new ActivityControlError("ACTIVITY_ALREADY_PUBLISHED", "activity is already published");
      }
      if (row.status !== "draft" || row.draft_version === null) {
        throw new ActivityControlError("ACTIVITY_INVALID_STATE", "only a draft version can be published");
      }
      const version = Number(command.version);
      if (version !== Number(row.draft_version)) {
        throw new ActivityControlError("ACTIVITY_VERSION_CONFLICT", "draft version is stale");
      }
      this.requireMatch(row, command.ifMatch);
      const published = await client.query(
        `UPDATE activity_version SET published_at = current_timestamp, published_by = $3
        WHERE activity_id = $1 AND version_no = $2 AND published_at IS NULL`,
        [activityId, version, String(command.actorId || "admin-api")]
      );
      if ((published.rowCount ?? 0) !== 1) {
        throw new ActivityControlError("ACTIVITY_VERSION_CONFLICT", "published version is immutable");
      }
      await client.query(
        `UPDATE activity SET status = 'published', current_version = $2,
          published_at = current_timestamp, offlined_at = NULL, offline_reason = NULL,
          updated_at = current_timestamp WHERE activity_id = $1`,
        [activityId, version]
      );
    });
    return this.detail(activityId);
  }

  async offline(activityId: string, command: Record<string, unknown>): Promise<unknown> {
    await this.transaction(async (client) => {
      const row = await this.lockActivity(client, activityId);
      if (row.status === "offline") {
        throw new ActivityControlError("ACTIVITY_ALREADY_OFFLINE", "activity is already offline");
      }
      if (!new Set(["published", "running", "ended"]).has(row.status)) {
        throw new ActivityControlError("ACTIVITY_INVALID_STATE", "only published activities can be taken offline");
      }
      if (Number(command.version) !== Number(row.current_version)) {
        throw new ActivityControlError("ACTIVITY_VERSION_CONFLICT", "activity version is stale");
      }
      this.requireMatch(row, command.ifMatch);
      const result = await client.query(
        `UPDATE activity SET status = 'offline', offlined_at = current_timestamp,
          offline_reason = $3, updated_at = current_timestamp
        WHERE activity_id = $1 AND current_version = $2 AND status IN ('published', 'running', 'ended')`,
        [activityId, command.version, command.reason]
      );
      if ((result.rowCount ?? 0) !== 1) {
        throw new ActivityControlError("ACTIVITY_VERSION_CONFLICT", "activity version or lifecycle changed");
      }
    });
    return this.detail(activityId);
  }

  async records(activityId: string, query: ActivityRecordQuery): Promise<unknown> {
    try {
      const exists = await this.pool.query("SELECT 1 FROM activity WHERE activity_id = $1", [activityId]);
      if (!exists.rows.length) throw new ActivityControlError("ACTIVITY_NOT_FOUND", "activity was not found");
      const values: unknown[] = [activityId];
      const where = ["r.activity_id = $1"];
      const add = (expression: string, value: unknown) => {
        if (value !== undefined && value !== null && value !== "") {
          values.push(value);
          where.push(`${expression} $${values.length}`);
        }
      };
      add("AND r.version_no =", query.version);
      add("AND r.character_id =", query.characterId);
      add("AND r.status =", query.status);
      add("AND r.created_at >=", query.from);
      add("AND r.created_at <", query.to);
      add("AND r.raw_request_id =", query.requestId);
      const limit = positiveInteger(query.limit, 50);
      const offset = Math.max(0, Number(query.offset) || 0);
      const pageValues = [...values, limit, offset];
      const whereClause = where.join(" ");
      const [countResult, pageResult] = await Promise.all([
        this.pool.query<{ total_count: string }>(
          `${this.recordsCte()}
          SELECT count(*)::text AS total_count FROM records r WHERE ${whereClause}`,
          values
        ),
        this.pool.query<any>(
          `${this.recordsCte()}
        SELECT r.* FROM records r
        WHERE ${whereClause}
        ORDER BY r.created_at DESC, r.record_id DESC
        LIMIT $${pageValues.length - 1} OFFSET $${pageValues.length}`,
          pageValues
        )
      ]);
      return {
        items: pageResult.rows.map((row): ActivityRecord => ({
          recordId: recordReference(row.record_type, row.record_id),
          activityId: row.activity_id,
          version: Number(row.version_no),
          recordType: row.record_type,
          ...(row.character_id ? { characterId: row.character_id } : {}),
          ...(requestReference(row.raw_request_id) ? { requestId: requestReference(row.raw_request_id) } : {}),
          status: row.status,
          createdAt: iso(row.created_at),
          details: jsonObject(row.details)
        })),
        total: Number(countResult.rows[0]?.total_count ?? 0),
        limit,
        offset
      };
    } catch (error) {
      throw databaseError(error);
    }
  }

  async write(event: ActivityAuditEvent): Promise<void> {
    const activityId = event.activityId || null;
    const version = Number.isInteger(event.version) ? event.version : null;
    const details = {
      result: event.result,
      ...(event.requestId ? { requestId: String(event.requestId) } : {}),
      ...(event.errorCode ? { errorCode: event.errorCode } : {}),
      ...(event.summary ? { summary: auditSummary(event.summary) } : {})
    };
    try {
      await this.pool.query(
        `INSERT INTO activity_audit_log (
          activity_id, version_no, event_type, actor_type, actor_id, reason, details_json
        ) SELECT
          CASE WHEN EXISTS (SELECT 1 FROM activity WHERE activity_id = $1) THEN $1 ELSE NULL END,
          CASE WHEN EXISTS (
            SELECT 1 FROM activity_version WHERE activity_id = $1 AND version_no = $2
          ) THEN $2 ELSE NULL END,
          $3, 'admin', $4, $5, $6`,
        [activityId, version, event.action, event.actorId, event.reason || event.action, details]
      );
    } catch (error) {
      throw databaseError(error);
    }
  }

  private async transaction<T>(operation: (client: Queryable) => Promise<T>): Promise<T> {
    const client = this.pool.connect ? await this.pool.connect() : this.pool;
    try {
      await client.query("BEGIN");
      const result = await operation(client);
      await client.query("COMMIT");
      return result;
    } catch (error) {
      try { await client.query("ROLLBACK"); } catch { /* preserve original error */ }
      throw databaseError(error);
    } finally {
      (client as any).release?.();
    }
  }

  private async lockActivity(client: Queryable, activityId: string): Promise<ActivityRow> {
    const result = await client.query<ActivityRow>(
      `${ACTIVITY_SELECT} WHERE a.activity_id = $1 FOR UPDATE OF a`,
      [activityId]
    );
    if (!result.rows.length) throw new ActivityControlError("ACTIVITY_NOT_FOUND", "activity was not found");
    return result.rows[0];
  }

  private requireMatch(row: ActivityRow, value: unknown): void {
    if (!expectedMatches(row.activity_id, row.revision, value)) {
      throw new ActivityControlError("ACTIVITY_VERSION_CONFLICT", "activity was changed by another operator");
    }
  }

  private async loadDetail(client: Queryable, activityId: string): Promise<Record<string, unknown>> {
    const result = await client.query<ActivityRow>(`${ACTIVITY_SELECT} WHERE a.activity_id = $1`, [activityId]);
    if (!result.rows.length) throw new ActivityControlError("ACTIVITY_NOT_FOUND", "activity was not found");
    const row = result.rows[0];
    const detail = this.summary(row);
    if (row.current_version !== null && row.current_public_config && row.current_type_config) {
      const source = await this.snapshot(client, row, {
        version: Number(row.current_version),
        publicConfig: row.current_public_config,
        typeConfig: row.current_type_config,
        digest: String(row.current_digest),
        startAt: row.current_start_at!,
        endAt: row.current_end_at!,
        claimDeadline: row.current_claim_deadline!,
        timezone: String(row.current_timezone),
        reason: String(row.current_reason || "published activity configuration")
      });
      detail.snapshot = source;
      if (row.status === "draft") detail.sourceSnapshot = source;
    }
    if (row.draft_version !== null && row.draft_public_config && row.draft_type_config) {
      detail.draft = await this.snapshot(client, row, {
        version: Number(row.draft_version),
        publicConfig: row.draft_public_config,
        typeConfig: row.draft_type_config,
        digest: String(row.draft_digest),
        startAt: row.draft_start_at!,
        endAt: row.draft_end_at!,
        claimDeadline: row.draft_claim_deadline!,
        timezone: String(row.draft_timezone),
        reason: String(row.draft_reason || "activity configuration")
      });
    }
    return detail;
  }

  private summary(row: ActivityRow): Record<string, unknown> {
    const version = row.draft_version ?? row.current_version;
    const digest = row.draft_version !== null ? row.draft_digest : row.current_digest;
    const revision = String(row.revision);
    return {
      activityId: row.activity_id,
      key: row.activity_key,
      activityType: row.activity_type,
      status: row.status,
      revision: Number(revision),
      version: version === null ? undefined : Number(version),
      configDigest: digest || undefined,
      etag: etag(row.activity_id, revision),
      ...(row.offline_reason ? { offlineReason: row.offline_reason } : {})
    };
  }

  private async snapshot(client: Queryable, row: ActivityRow, columns: SnapshotColumns): Promise<Record<string, unknown>> {
    const [stages, groups, items] = await Promise.all([
      client.query<any>(
        `SELECT stage_id, stage_no, reward_group_key, qualification_json, display_json
        FROM activity_stage WHERE activity_id = $1 AND version_no = $2 ORDER BY stage_no`,
        [row.activity_id, columns.version]
      ),
      client.query<any>(
        `SELECT reward_group_key, selection_mode, config_json
        FROM activity_reward_group WHERE activity_id = $1 AND version_no = $2 ORDER BY id`,
        [row.activity_id, columns.version]
      ),
      client.query<any>(
        `SELECT reward_group_key, reward_json
        FROM activity_reward_item WHERE activity_id = $1 AND version_no = $2 ORDER BY id`,
        [row.activity_id, columns.version]
      )
    ]);
    const itemsByGroup = new Map<string, unknown[]>();
    for (const item of items.rows) {
      const list = itemsByGroup.get(item.reward_group_key) || [];
      list.push(item.reward_json);
      itemsByGroup.set(item.reward_group_key, list);
    }
    return {
      key: row.activity_key,
      activityType: row.activity_type,
      schemaVersion: Number(columns.typeConfig.schema_version),
      startAt: iso(columns.startAt),
      endAt: iso(columns.endAt),
      claimDeadline: iso(columns.claimDeadline),
      timezone: columns.timezone,
      publicConfig: columns.publicConfig,
      typeConfig: columns.typeConfig,
      stages: stages.rows.map((stage) => ({
        stageId: stage.stage_id,
        stageNo: Number(stage.stage_no),
        rewardGroupKey: stage.reward_group_key,
        qualification: jsonObject(stage.qualification_json),
        ...(Object.keys(jsonObject(stage.display_json)).length ? { display: stage.display_json } : {})
      })),
      rewardGroups: groups.rows.map((group) => ({
        key: group.reward_group_key,
        selectionMode: group.selection_mode,
        items: itemsByGroup.get(group.reward_group_key) || []
      })),
      reason: columns.reason
    };
  }

  private async insertVersion(client: Queryable, activityId: string, version: number, command: Record<string, unknown>): Promise<void> {
    const publicConfig = persistedPublicConfig(command);
    await client.query(
      `INSERT INTO activity_version (
        activity_id, version_no, public_config_json, type_config_json, config_digest,
        start_at, end_at, claim_deadline, timezone, change_reason
      ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)`,
      [activityId, version, publicConfig, command.typeConfig,
        configDigest(publicConfig, command.typeConfig), command.startAt, command.endAt,
        command.claimDeadline, command.timezone, command.reason]
    );
    await this.insertChildren(client, activityId, version, command);
  }

  private async replaceChildren(client: Queryable, activityId: string, version: number, command: Record<string, unknown>): Promise<void> {
    await client.query("DELETE FROM activity_stage WHERE activity_id = $1 AND version_no = $2", [activityId, version]);
    await client.query("DELETE FROM activity_reward_item WHERE activity_id = $1 AND version_no = $2", [activityId, version]);
    await client.query("DELETE FROM activity_reward_group WHERE activity_id = $1 AND version_no = $2", [activityId, version]);
    await this.insertChildren(client, activityId, version, command);
  }

  private async insertChildren(client: Queryable, activityId: string, version: number, command: Record<string, unknown>): Promise<void> {
    const typeConfig = jsonObject(command.typeConfig);
    for (const group of normalizedRewardGroups(command) as any[]) {
      await client.query(
        `INSERT INTO activity_reward_group (
          activity_id, version_no, reward_group_key, selection_mode, config_json
        ) VALUES ($1, $2, $3, $4, $5)`,
        [activityId, version, group.key, group.selectionMode, {}]
      );
      for (const item of Array.isArray(group.items) ? group.items : []) {
        await client.query(
          `INSERT INTO activity_reward_item (
            activity_id, version_no, reward_group_key, reward_type,
            asset_key, quantity, weight, reward_json
          ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)`,
          [activityId, version, group.key, "item", String(item.item_id),
            item.quantity, item.weight ?? null, item]
        );
      }
    }
    for (const stage of Array.isArray(command.stages) ? command.stages as any[] : []) {
      const qualification = jsonObject(stage.qualification);
      const periodStrategy = String(qualification.periodStrategy ?? typeConfig.cycle_unit ?? "once");
      const resetPolicy = String(qualification.resetPolicy ?? typeConfig.miss_policy ?? "none");
      await client.query(
        `INSERT INTO activity_stage (
          activity_id, version_no, stage_id, stage_no, qualification_json,
          period_strategy, reward_group_key, max_claims, reset_policy, display_json
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)`,
        [activityId, version, stage.stageId, stage.stageNo, qualification,
          periodStrategy, stage.rewardGroupKey, positiveInteger(qualification.maxClaims),
          resetPolicy, jsonObject(stage.display)]
      );
    }
  }

  private claimMode(command: Record<string, unknown>): string {
    return String(jsonObject(command.typeConfig).claim_mode || "manual");
  }

  private recordsCte(): string {
    return `WITH records AS (
      SELECT 'claim:' || c.id AS record_id, c.activity_id, c.version_no,
        'claim' AS record_type, c.character_id, c.client_request_id AS raw_request_id,
        c.status, c.created_at,
        jsonb_build_object(
          'actionType', c.action_type, 'stageId', c.stage_id, 'periodKey', c.period_key,
          'errorCode', c.error_code, 'notificationFailed', c.notification_failed,
          'attemptCount', c.attempt_count
        ) AS details
      FROM activity_claim_record c
      UNION ALL
      SELECT 'draw:' || d.id, d.activity_id, d.version_no, 'draw', d.character_id,
        c.client_request_id, c.status, d.created_at,
        jsonb_build_object(
          'rewardGroupKey', d.reward_group_key, 'poolDigest', d.pool_digest,
          'randomAlgorithmVersion', d.random_algorithm_version,
          'selectedItemId', d.selected_item_id::text
        )
      FROM activity_draw_result d JOIN activity_claim_record c ON c.id = d.claim_id
      UNION ALL
      SELECT 'state:' || s.id, s.activity_id, s.version_no, 'player_state', s.character_id,
        NULL, 'current', s.updated_at,
        jsonb_build_object('currentStageId', s.current_stage_id, 'stateRevision', s.state_revision::text)
      FROM player_activity_state s
      UNION ALL
      SELECT 'grant:' || g.id, g.activity_id, g.version_no, 'reward_grant', g.character_id,
        g.request_id, g.status, g.created_at,
        jsonb_build_object('deliveryMethod', g.delivery_method,
          'deliveryRef', CASE WHEN g.delivery_id IS NULL THEN NULL ELSE 'recorded' END,
          'mailRef', CASE WHEN g.mail_id IS NULL THEN NULL ELSE 'recorded' END)
      FROM reward_grant_ledger g
      UNION ALL
      SELECT 'mail:' || o.delivery_request_id, c.activity_id, c.version_no, 'reward_mail',
        o.character_id, o.reward_request_id, o.status, o.created_at,
        jsonb_build_object('deliveryPolicy', o.delivery_policy, 'attemptCount', o.attempt_count,
          'lastErrorCode', o.last_error_code)
      FROM reward_mail_outbox o
      JOIN activity_claim_record c ON c.reward_request_id = o.reward_request_id
      UNION ALL
      SELECT 'review:' || r.id, r.activity_id, r.version_no, 'manual_review', r.character_id,
        r.client_request_id, 'manual_review', r.created_at,
        jsonb_build_object('reasonCode', r.reason_code)
      FROM activity_claim_review r
    )`;
  }
}
