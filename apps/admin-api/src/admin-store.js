import crypto from "node:crypto";
import bcrypt from "bcrypt";

import { createGlobalIdGeneratorFromEnv } from "../../../packages/global-id/node/index.js";
import { redactAuditReason } from "./operations/audit-reason.js";

const SALT_ROUNDS = 10;
const MAINTENANCE_STATE_KEY = "maintenance:global";
const UNIQUE_VIOLATION = "23505";
const CHARACTER_ID_PATTERN = /^chr_[0-9a-hjkmnp-tv-z]+$/;
const ELEMENT_KEYS = ["earth", "fire", "water", "wind"];
const AFFINITY_TOTAL = 10000;
const DISCIPLINE_TIER_ORDER = [
  "novice",
  "apprentice",
  "adept",
  "expert",
  "master",
  "grandmaster"
];
const BOOTSTRAP_POLICY_SCOPE = Object.freeze({
  world_ids: ["*"],
  service_names: ["*"],
  instance_ids: ["*"],
  field_allowlist: ["*"],
  target_types: ["*"],
  target_ids: ["*"],
  max_targets: 10000
});

function maintenanceStateKey(prefix = "") {
  return `${prefix || ""}${MAINTENANCE_STATE_KEY}`;
}

function normalizeOptionalString(value) {
  if (typeof value !== "string") {
    return null;
  }

  const normalized = value.trim();
  return normalized.length > 0 ? normalized : null;
}

function normalizeMaintenanceState(state = {}) {
  return {
    enabled: state.enabled === true,
    reason: normalizeOptionalString(state.reason),
    updatedAt: normalizeOptionalString(state.updatedAt),
    updatedBy: normalizeOptionalString(state.updatedBy)
  };
}

function parseMaintenanceState(raw) {
  if (!raw) {
    return null;
  }

  try {
    return normalizeMaintenanceState(JSON.parse(raw));
  } catch {
    return null;
  }
}

function toIsoString(value) {
  if (!value) {
    return null;
  }

  if (value instanceof Date) {
    return value.toISOString();
  }

  return String(value);
}

function hashPassword(password) {
  return bcrypt.hashSync(password, SALT_ROUNDS);
}

function verifyPassword(password, hash) {
  return bcrypt.compareSync(password, hash);
}

function hashToken(token) {
  return crypto.createHash("sha256").update(token).digest("hex");
}

function toJsonb(value) {
  return value ? JSON.stringify(value) : null;
}

function toRequiredJsonb(value) {
  return JSON.stringify(value ?? {});
}

function toAdminOperation(row) {
  if (!row) {
    return null;
  }

  return {
    operationId: row.operation_id,
    requestId: row.request_id,
    actorAdminId: toNumericId(row.actor_admin_id),
    actorSubject: row.actor_subject,
    permissionKey: row.permission_key,
    riskLevel: row.risk_level,
    authorizationScope: normalizeJson(row.authorization_scope_json),
    requestedScope: normalizeJson(row.requested_scope_json),
    scopeSha256: row.scope_sha256,
    targetSummary: normalizeJson(row.target_summary_json),
    targetSha256: row.target_sha256,
    payloadSha256: row.payload_sha256,
    semanticSha256: row.semantic_sha256,
    reason: redactAuditReason(row.reason),
    traceId: row.trace_id,
    status: row.status,
    approvalStatus: row.approval_status,
    executionClaimedAt: toIsoString(row.execution_claimed_at),
    completedAt: toIsoString(row.completed_at),
    resultSummary: normalizeJson(row.result_summary_json),
    errorSummary: normalizeJson(row.error_summary_json),
    createdAt: toIsoString(row.created_at),
    updatedAt: toIsoString(row.updated_at),
    preview: row.preview_id ? {
      previewId: row.preview_id,
      summarySha256: row.summary_sha256,
      expiresAt: toIsoString(row.preview_expires_at),
      consumedAt: toIsoString(row.preview_consumed_at)
    } : null
  };
}

const OPERATION_AUDIT_SENSITIVE_KEY = /password|token|secret|private.?key|authorization|cookie|ticket|nonce|payload/i;
const OPERATION_AUDIT_UNBOUNDED_TEXT_KEY = /content|message|prompt|broadcast|body/i;

function redactOperationAuditValue(value, key = "", depth = 0) {
  if (OPERATION_AUDIT_SENSITIVE_KEY.test(key) || OPERATION_AUDIT_UNBOUNDED_TEXT_KEY.test(key)) {
    return "[REDACTED]";
  }
  if (depth > 6) {
    return "[TRUNCATED]";
  }
  if (value === null || value === undefined || typeof value === "boolean" || typeof value === "number") {
    return value ?? null;
  }
  if (typeof value === "string") {
    return Buffer.byteLength(value, "utf8") <= 1024 ? value : "[TRUNCATED]";
  }
  if (Array.isArray(value)) {
    return value.slice(0, 100).map((entry) => redactOperationAuditValue(entry, "", depth + 1));
  }
  if (typeof value !== "object") {
    return "[REDACTED]";
  }
  return Object.fromEntries(
    Object.entries(value).slice(0, 100).map(([entryKey, entryValue]) => [
      entryKey,
      redactOperationAuditValue(entryValue, entryKey, depth + 1)
    ])
  );
}

function toAdminOperationAuditEvent(row) {
  return {
    id: toNumericId(row.id),
    operationId: row.operation_id || null,
    breakglassGrantId: row.breakglass_grant_id || null,
    eventType: row.event_type,
    actorAdminId: toNumericId(row.actor_admin_id),
    actorSubject: row.actor_subject,
    requestId: row.request_id || null,
    permissionKey: row.permission_key || null,
    riskLevel: row.risk_level || null,
    traceId: row.trace_id || null,
    reason: redactAuditReason(row.reason),
    targetSummary: redactOperationAuditValue(normalizeJson(row.target_summary_json)),
    resultSummary: redactOperationAuditValue(normalizeJson(row.result_summary_json)),
    details: redactOperationAuditValue(normalizeJson(row.details_json) || {}),
    result: row.operation_status || null,
    createdAt: toIsoString(row.created_at)
  };
}

function toBreakglassGrant(row) {
  if (!row) {
    return null;
  }

  return {
    grantId: row.grant_id,
    activationRequestId: row.activation_request_id,
    actorAdminId: toNumericId(row.actor_admin_id),
    actorSubject: row.actor_subject,
    permissionKey: row.permission_key,
    scope: normalizeJson(row.scope_json),
    scopeSha256: row.scope_sha256,
    targetSummary: normalizeJson(row.target_summary_json),
    targetSha256: row.target_sha256,
    semanticSha256: row.semantic_sha256,
    reason: row.reason,
    activatedAt: toIsoString(row.activated_at),
    expiresAt: toIsoString(row.expires_at),
    revokedAt: toIsoString(row.revoked_at),
    revokedByAdminId: toNumericId(row.revoked_by_admin_id),
    revokedBySubject: row.revoked_by_subject || null,
    revocationReason: row.revocation_reason || null
  };
}

function operationIsTerminal(status) {
  return new Set(["succeeded", "failed", "execution_uncertain", "cancelled"]).has(status);
}

function operationStoreError(code, message = code, details = {}) {
  return createAdminStoreError(code, message, details);
}

function nextParam(params) {
  return `$${params.length}`;
}

function toNumericId(value) {
  if (value === null || value === undefined) {
    return value;
  }
  const numeric = Number(value);
  return Number.isSafeInteger(numeric) ? numeric : value;
}

function toAdmin(row) {
  return {
    id: toNumericId(row.id),
    username: row.username,
    displayName: row.display_name,
    role: row.role,
    status: row.status,
    passwordAlgo: row.password_algo,
    passwordSalt: row.password_salt,
    passwordHash: row.password_hash
  };
}

function toPlayer(row) {
  return {
    player_id: row.player_id,
    guest_id: row.guest_id,
    login_name: row.login_name,
    display_name: row.display_name,
    account_type: row.account_type,
    status: row.status,
    ban_expires_at: toIsoString(row.ban_expires_at),
    banExpiresAt: toIsoString(row.ban_expires_at),
    created_at: toIsoString(row.created_at),
    last_login_at: toIsoString(row.last_login_at)
  };
}

function characterSelectColumns() {
  return `character_id,
          account_player_id,
          world_id,
          name,
          status,
          appearance_json,
          scene_id,
          x,
          y,
          dir_x,
          dir_y,
          affinity_earth,
          affinity_fire,
          affinity_water,
          affinity_wind,
          mastery_earth,
          mastery_fire,
          mastery_water,
          mastery_wind,
          created_at,
          last_login_at,
          deleted_at`;
}

function toCharacter(row) {
  return {
    characterId: row.character_id,
    character_id: row.character_id,
    accountPlayerId: row.account_player_id,
    account_player_id: row.account_player_id,
    worldId: toNumericId(row.world_id),
    world_id: toNumericId(row.world_id),
    name: row.name,
    status: row.status,
    appearance: normalizeJson(row.appearance_json) || {},
    appearance_json: normalizeJson(row.appearance_json) || {},
    position: {
      sceneId: toNumericId(row.scene_id),
      scene_id: toNumericId(row.scene_id),
      x: Number(row.x),
      y: Number(row.y),
      dirX: Number(row.dir_x),
      dir_x: Number(row.dir_x),
      dirY: Number(row.dir_y),
      dir_y: Number(row.dir_y)
    },
    attributes: {
      affinity: {
        earth: Number(row.affinity_earth),
        fire: Number(row.affinity_fire),
        water: Number(row.affinity_water),
        wind: Number(row.affinity_wind)
      },
      mastery: {
        earth: Number(row.mastery_earth),
        fire: Number(row.mastery_fire),
        water: Number(row.mastery_water),
        wind: Number(row.mastery_wind)
      }
    },
    createdAt: toIsoString(row.created_at),
    created_at: toIsoString(row.created_at),
    lastLoginAt: toIsoString(row.last_login_at),
    last_login_at: toIsoString(row.last_login_at),
    deletedAt: toIsoString(row.deleted_at),
    deleted_at: toIsoString(row.deleted_at)
  };
}

function normalizeJson(value) {
  if (value === undefined || value === null) {
    return null;
  }

  if (typeof value !== "string") {
    return value;
  }

  try {
    return JSON.parse(value);
  } catch {
    return value;
  }
}

function createAdminStoreError(code, message = code, details = {}) {
  const error = new Error(message);
  error.code = code;
  Object.assign(error, details);
  return error;
}

function normalizedPosition(position = {}) {
  return {
    sceneId: position.sceneId ?? position.scene_id ?? 100,
    x: position.x ?? 0,
    y: position.y ?? 0,
    dirX: position.dirX ?? position.dir_x ?? 0,
    dirY: position.dirY ?? position.dir_y ?? 1
  };
}

function normalizedElements(elements = {}, defaults) {
  return {
    earth: elements.earth ?? defaults.earth,
    fire: elements.fire ?? defaults.fire,
    water: elements.water ?? defaults.water,
    wind: elements.wind ?? defaults.wind
  };
}

function normalizeCharacterCreateInput(input = {}) {
  return {
    accountPlayerId: input.accountPlayerId,
    worldId: input.worldId ?? input.world_id ?? 0,
    name: input.name,
    status: input.status || "active",
    appearance: input.appearance ?? input.appearance_json ?? {},
    position: normalizedPosition(input.position),
    affinity: normalizedElements(input.affinity, {
      earth: 2500,
      fire: 2500,
      water: 2500,
      wind: 2500
    }),
    mastery: normalizedElements(input.mastery, {
      earth: 0,
      fire: 0,
      water: 0,
      wind: 0
    })
  };
}

function characterElementSnapshot(character) {
  return {
    character_id: character.character_id,
    affinity: { ...character.attributes.affinity },
    mastery: { ...character.attributes.mastery }
  };
}

function elementsDelta(before, after) {
  return ELEMENT_KEYS.reduce((delta, key) => {
    delta[key] = Number(after[key]) - Number(before[key]);
    return delta;
  }, {});
}

function isZeroElementsDelta(delta) {
  return ELEMENT_KEYS.every((key) => Number(delta[key]) === 0);
}

function titleSnapshot(rowOrTitle) {
  if (!rowOrTitle) {
    return null;
  }

  return {
    character_id: rowOrTitle.character_id,
    title_id: rowOrTitle.title_id,
    source_type: rowOrTitle.source_type,
    source_id: rowOrTitle.source_id || null,
    is_equipped: rowOrTitle.is_equipped === true,
    unlocked_at: toIsoString(rowOrTitle.unlocked_at),
    expires_at: toIsoString(rowOrTitle.expires_at),
    expired: rowOrTitle.expired === true
  };
}

function disciplineSnapshot(rowOrDiscipline) {
  if (!rowOrDiscipline) {
    return null;
  }

  return {
    character_id: rowOrDiscipline.character_id,
    discipline_id: rowOrDiscipline.discipline_id,
    points: toNumericId(rowOrDiscipline.points),
    tier: rowOrDiscipline.tier,
    active: rowOrDiscipline.active === true,
    learned_at: toIsoString(rowOrDiscipline.learned_at),
    updated_at: toIsoString(rowOrDiscipline.updated_at)
  };
}

function titleGrantStatus(existing) {
  if (!existing) {
    return "granted";
  }
  return existing.expired ? "renewed" : "already_owned";
}

function disciplineActionForUpsert(before, input) {
  if (!before) {
    return "learn";
  }

  const beforeTierIndex = DISCIPLINE_TIER_ORDER.indexOf(before.tier);
  const nextTierIndex = DISCIPLINE_TIER_ORDER.indexOf(input.tier);
  if (nextTierIndex > beforeTierIndex) {
    return "upgrade";
  }
  if (nextTierIndex < beforeTierIndex) {
    return "downgrade";
  }
  if (Number(before.points) !== Number(input.points) || before.active !== input.active) {
    return "update";
  }
  return "grant";
}

function rowsEqualDiscipline(before, input) {
  return before &&
    before.discipline_id === input.disciplineId &&
    Number(before.points) === Number(input.points) &&
    before.tier === input.tier &&
    before.active === input.active;
}

function tierAtLeast(current, required) {
  const currentIndex = DISCIPLINE_TIER_ORDER.indexOf(current);
  const requiredIndex = DISCIPLINE_TIER_ORDER.indexOf(required);
  return currentIndex >= 0 && requiredIndex >= 0 && currentIndex >= requiredIndex;
}

function evaluateTitleUnlockRule(rule, context) {
  if (!rule || typeof rule !== "object") {
    return {
      eligible: false,
      supported: false,
      reason: "missing_rule"
    };
  }

  if (Array.isArray(rule.all_of)) {
    const results = rule.all_of.map((childRule) => evaluateTitleUnlockRule(childRule, context));
    const unsupported = results.find((result) => !result.supported);
    if (unsupported) {
      return unsupported;
    }
    const failed = results.find((result) => !result.eligible);
    return failed || { eligible: true, supported: true, reason: "all_of" };
  }

  if (Array.isArray(rule.any_of)) {
    const results = rule.any_of.map((childRule) => evaluateTitleUnlockRule(childRule, context));
    if (results.some((result) => result.supported && result.eligible)) {
      return { eligible: true, supported: true, reason: "any_of" };
    }
    const supported = results.find((result) => result.supported);
    return supported || {
      eligible: false,
      supported: false,
      reason: "any_of_unsupported"
    };
  }

  if (rule.discipline || rule.type === "discipline_tier") {
    const disciplineId = String(rule.discipline || rule.discipline_id || "").trim();
    const requiredTier = String(rule.tier || rule.min_tier || "").trim();
    const discipline = context.disciplineById.get(disciplineId);
    return {
      eligible: !!discipline && tierAtLeast(discipline.tier, requiredTier),
      supported: disciplineId.length > 0 && requiredTier.length > 0,
      reason: "discipline_tier"
    };
  }

  if (rule.type === "element_mastery" || rule.type === "mastery") {
    const element = String(rule.element || "").trim();
    const min = Number(rule.min);
    return {
      eligible: ELEMENT_KEYS.includes(element) && Number.isFinite(min) &&
        Number(context.character.attributes.mastery[element]) >= min,
      supported: ELEMENT_KEYS.includes(element) && Number.isFinite(min),
      reason: "element_mastery"
    };
  }

  if (rule.type === "element_affinity" || rule.type === "affinity") {
    const element = String(rule.element || "").trim();
    const min = Number(rule.min);
    return {
      eligible: ELEMENT_KEYS.includes(element) && Number.isFinite(min) &&
        Number(context.character.attributes.affinity[element]) >= min,
      supported: ELEMENT_KEYS.includes(element) && Number.isFinite(min),
      reason: "element_affinity"
    };
  }

  if (rule.event === "character_created") {
    return {
      eligible: true,
      supported: true,
      reason: "character_created"
    };
  }

  if (rule.grant) {
    return {
      eligible: false,
      supported: false,
      reason: "explicit_grant_required"
    };
  }

  return {
    eligible: false,
    supported: false,
    reason: rule.type || rule.event || rule.grant || "unsupported_rule"
  };
}

function toCharacterTitle(row) {
  const operator = row.latest_operator_type || row.latest_operator_id
    ? {
        type: row.latest_operator_type || null,
        id: row.latest_operator_id || null
      }
    : null;

  return {
    character_id: row.character_id,
    title_id: row.title_id,
    source_type: row.source_type,
    source_id: row.source_id,
    is_equipped: row.is_equipped === true,
    unlocked_at: toIsoString(row.unlocked_at),
    expires_at: toIsoString(row.expires_at),
    expired: row.expired === true,
    created_at: toIsoString(row.created_at),
    updated_at: toIsoString(row.updated_at),
    operator_type: row.latest_operator_type || null,
    operator_id: row.latest_operator_id || null,
    operator,
    latest_log: row.latest_action ? {
      action: row.latest_action,
      operator_type: row.latest_operator_type || null,
      operator_id: row.latest_operator_id || null,
      operator,
      reason: row.latest_reason || null,
      created_at: toIsoString(row.latest_created_at)
    } : null
  };
}

function toCharacterDiscipline(row) {
  return {
    character_id: row.character_id,
    discipline_id: row.discipline_id,
    points: toNumericId(row.points),
    tier: row.tier,
    active: row.active === true,
    learned_at: toIsoString(row.learned_at),
    updated_at: toIsoString(row.updated_at)
  };
}

function toCharacterElementLog(row) {
  const operator = row.operator_type || row.operator_id
    ? {
        type: row.operator_type || null,
        id: row.operator_id || null
      }
    : null;

  return {
    id: toNumericId(row.id),
    character_id: row.character_id,
    source_type: row.source_type || null,
    source_id: row.source_id || null,
    operator_type: row.operator_type || null,
    operator_id: row.operator_id || null,
    operator,
    affinity_delta: {
      earth: Number(row.affinity_earth_delta),
      fire: Number(row.affinity_fire_delta),
      water: Number(row.affinity_water_delta),
      wind: Number(row.affinity_wind_delta)
    },
    mastery_delta: {
      earth: Number(row.mastery_earth_delta),
      fire: Number(row.mastery_fire_delta),
      water: Number(row.mastery_water_delta),
      wind: Number(row.mastery_wind_delta)
    },
    before_json: normalizeJson(row.before_json),
    after_json: normalizeJson(row.after_json),
    reason: row.reason || null,
    created_at: toIsoString(row.created_at)
  };
}

function toCharacterTitleLog(row) {
  const operator = row.operator_type || row.operator_id
    ? {
        type: row.operator_type || null,
        id: row.operator_id || null
      }
    : null;

  return {
    id: toNumericId(row.id),
    character_id: row.character_id,
    title_id: row.title_id,
    action: row.action,
    source_type: row.source_type || null,
    source_id: row.source_id || null,
    operator_type: row.operator_type || null,
    operator_id: row.operator_id || null,
    operator,
    before_json: normalizeJson(row.before_json),
    after_json: normalizeJson(row.after_json),
    reason: row.reason || null,
    created_at: toIsoString(row.created_at)
  };
}

function toCharacterDisciplineLog(row) {
  const operator = row.operator_type || row.operator_id
    ? {
        type: row.operator_type || null,
        id: row.operator_id || null
      }
    : null;

  return {
    id: toNumericId(row.id),
    character_id: row.character_id,
    discipline_id: row.discipline_id,
    action: row.action,
    source_type: row.source_type || null,
    source_id: row.source_id || null,
    operator_type: row.operator_type || null,
    operator_id: row.operator_id || null,
    operator,
    before_json: normalizeJson(row.before_json),
    after_json: normalizeJson(row.after_json),
    reason: row.reason || null,
    created_at: toIsoString(row.created_at)
  };
}

function toIdOrigin(row) {
  return {
    origin_id: toNumericId(row.origin_id),
    origin_key: row.origin_key,
    created_at: toIsoString(row.created_at),
    retired_at: toIsoString(row.retired_at)
  };
}

function toWorld(row) {
  return {
    world_id: toNumericId(row.world_id),
    world_key: row.world_key,
    active_origin_id: toNumericId(row.active_origin_id),
    active_origin_key: row.active_origin_key || null,
    origins: Array.isArray(row.origins) ? row.origins.map((origin) => ({
      origin_id: toNumericId(origin.origin_id),
      origin_key: origin.origin_key || null
    })) : [],
    created_at: toIsoString(row.created_at),
    retired_at: toIsoString(row.retired_at)
  };
}

function toWorldMembership(row) {
  return {
    world_id: toNumericId(row.world_id),
    world_key: row.world_key || null,
    origin_id: toNumericId(row.origin_id),
    origin_key: row.origin_key || null,
    active_origin_id: toNumericId(row.active_origin_id),
    active_origin_key: row.active_origin_key || null,
    joined_at: toIsoString(row.joined_at),
    left_at: toIsoString(row.left_at)
  };
}

function toWorldMergeEvent(row) {
  return {
    merge_id: toNumericId(row.merge_id),
    target_world_id: toNumericId(row.target_world_id),
    target_world_key: row.target_world_key || null,
    active_origin_id: toNumericId(row.active_origin_id),
    active_origin_key: row.active_origin_key || null,
    source_world_ids: Array.isArray(row.source_world_ids) ? row.source_world_ids.map(toNumericId) : [],
    source_world_keys: Array.isArray(row.source_world_keys) ? row.source_world_keys : [],
    source_origin_ids: Array.isArray(row.source_origin_ids) ? row.source_origin_ids.map(toNumericId) : [],
    source_origin_keys: Array.isArray(row.source_origin_keys) ? row.source_origin_keys : [],
    merged_at: toIsoString(row.merged_at),
    operator: row.operator || null,
    details_json: row.details_json || null
  };
}

function readTotal(rows) {
  return Number.parseInt(String(rows[0]?.total ?? "0"), 10);
}

function assetLedgerFilters({ characterId, requestId, originType, originId, deliveryId, from, to } = {}) {
  let where = " WHERE 1=1";
  const params = [];
  const add = (column, value) => {
    if (!value) return;
    params.push(value);
    where += ` AND ${column} = ${nextParam(params)}`;
  };

  add("character_id", characterId);
  add("request_id", requestId);
  add("origin_type", originType);
  add("origin_id", originId);
  add("delivery_id", deliveryId);
  if (from) {
    params.push(from);
    where += ` AND created_at >= ${nextParam(params)}::timestamptz`;
  }
  if (to) {
    params.push(to);
    where += ` AND created_at <= ${nextParam(params)}::timestamptz`;
  }
  return { where, params };
}

function assetLedgerQuery(filters) {
  const { where, params } = assetLedgerFilters(filters);
  return {
    query: `SELECT
              id,
              character_id,
              request_id,
              asset_type,
              item_id,
              COALESCE((binding_json ->> 'binded')::boolean, false) AS is_bound,
              quantity_before,
              quantity_after,
              quantity_delta,
              container,
              source,
              origin_type,
              origin_id,
              delivery_method,
              delivery_id,
              mail_id,
              fallback_reason,
              rule_version,
              snapshot_revision,
              created_at
            FROM character_asset_ledger${where}`,
    params
  };
}

function toAssetLedgerEntry(row) {
  return {
    id: toNumericId(row.id),
    characterId: row.character_id,
    requestId: row.request_id,
    assetType: row.asset_type,
    itemId: Number(row.item_id),
    isBound: row.is_bound === true,
    quantityBefore: Number(row.quantity_before),
    quantityAfter: Number(row.quantity_after),
    quantityDelta: Number(row.quantity_delta),
    container: row.container,
    source: row.source,
    originType: row.origin_type,
    originId: row.origin_id,
    deliveryMethod: row.delivery_method,
    deliveryId: row.delivery_id || null,
    mailId: row.mail_id || null,
    fallbackReason: row.fallback_reason || null,
    ruleVersion: row.rule_version,
    snapshotRevision: Number(row.snapshot_revision),
    createdAt: toIsoString(row.created_at)
  };
}

export class AdminStore {
  constructor(pool, redis = null, config = {}, gamePool = null) {
    this.pool = pool;
    this.gamePool = gamePool || pool;
    this.redis = redis;
    this.redisKeyPrefix = config.redisKeyPrefix || "";
    this.characterIdGenerator = config.characterIdGenerator || createGlobalIdGeneratorFromEnv({ prefix: "chr" });
  }

  prefixedKey(key) {
    return `${this.redisKeyPrefix}${key}`;
  }

  maintenanceStateKey() {
    return maintenanceStateKey(this.redisKeyPrefix);
  }

  async findAdminByUsername(username) {
    const { rows } = await this.pool.query(
      `SELECT id, username, display_name, password_algo, password_salt, password_hash, role, status
       FROM admin_accounts
       WHERE username = $1
       LIMIT 1`,
      [username]
    );

    if (rows.length === 0) return null;

    return toAdmin(rows[0]);
  }

  async findAdminById(adminId) {
    const { rows } = await this.pool.query(
      `SELECT id, username, display_name, password_algo, password_salt, password_hash, role, status
       FROM admin_accounts
       WHERE id = $1
       LIMIT 1`,
      [adminId]
    );

    return rows.length > 0 ? toAdmin(rows[0]) : null;
  }

  async verifyPassword(password, hash) {
    return verifyPassword(password, hash);
  }

  async createAdmin({ username, displayName, password, role = "viewer" }) {
    try {
      return await this.createAdminWithClient(this.pool, { username, displayName, password, role });
    } catch (error) {
      if (error.code === UNIQUE_VIOLATION) {
        throw new Error("ADMIN_ALREADY_EXISTS");
      }
      throw error;
    }
  }

  async createAdminWithClient(client, { username, displayName, password, role = "viewer" }) {
    const passwordSalt = crypto.randomBytes(16).toString("hex");
    const passwordHash = hashPassword(password);
    const { rows } = await client.query(
      `INSERT INTO admin_accounts (username, display_name, password_algo, password_salt, password_hash, role, status)
       VALUES ($1, $2, 'bcrypt', $3, $4, $5, 'active')
       RETURNING id`,
      [username, displayName || username, passwordSalt, passwordHash, role]
    );

    return {
      id: toNumericId(rows[0].id),
      username,
      displayName: displayName || username,
      role
    };
  }

  async ensureInitialAdmin(config) {
    const existing = await this.findAdminByUsername(config.initialAdminUsername);
    if (existing) {
      return existing;
    }

    const client = await this.pool.connect();
    try {
      await client.query("BEGIN");
      const admin = await this.createAdminWithClient(client, {
        username: config.initialAdminUsername,
        displayName: config.initialAdminDisplayName,
        password: config.initialAdminPassword,
        role: "admin"
      });
      await this.grantBootstrapAdminRoleInTransaction(client, admin, config);
      await client.query("COMMIT");
      return admin;
    } catch (error) {
      await client.query("ROLLBACK").catch(() => undefined);
      if (error.code === UNIQUE_VIOLATION) {
        const concurrentAdmin = await this.findAdminByUsername(config.initialAdminUsername);
        if (concurrentAdmin) {
          return concurrentAdmin;
        }
      }
      throw error;
    } finally {
      client.release();
    }
  }

  async grantBootstrapAdminRole(admin, config = {}) {
    const client = await this.pool.connect();
    try {
      await client.query("BEGIN");
      await this.grantBootstrapAdminRoleInTransaction(client, admin, config);
      await client.query("COMMIT");
    } catch (error) {
      await client.query("ROLLBACK").catch(() => undefined);
      throw error;
    } finally {
      client.release();
    }
  }

  async grantBootstrapAdminRoleInTransaction(client, admin, config = {}) {
    const roleKey = String(config.bootstrapAdminRole || "super_admin").trim();
    if (!roleKey) {
      throw createAdminStoreError("BOOTSTRAP_ADMIN_ROLE_REQUIRED", "Bootstrap admin role is required");
    }

    const scope = config.bootstrapAdminScope || BOOTSTRAP_POLICY_SCOPE;
    const subject = `bootstrap:${String(config.env || "development").trim() || "development"}:${admin.username}`;
    const reason = "initial admin bootstrap";
    const role = await client.query(
      `SELECT role_key FROM admin_roles WHERE role_key = $1 AND active = true LIMIT 1`,
      [roleKey]
    );
    if (role.rows.length === 0) {
      throw createAdminStoreError("BOOTSTRAP_ADMIN_ROLE_UNKNOWN", "Bootstrap admin role is not available", { roleKey });
    }

    const assignment = await client.query(
      `INSERT INTO admin_account_roles (
         admin_id, role_key, scope_json, granted_by_subject, reason, effective_at
       ) VALUES ($1, $2, $3::jsonb, $4, $5, current_timestamp)
       RETURNING id, effective_at`,
      [admin.id, roleKey, toRequiredJsonb(scope), subject, reason]
    );
    await client.query(
      `INSERT INTO admin_authorization_audit_events (
         event_type, actor_subject, subject_admin_id, role_key, assignment_id, reason, scope_json, details_json
       ) VALUES ('account_role_granted', $1, $2, $3, $4, $5, $6::jsonb, $7::jsonb)`,
      [
        subject,
        admin.id,
        roleKey,
        assignment.rows[0].id,
        reason,
        toRequiredJsonb(scope),
        toRequiredJsonb({ bootstrap: true, effectiveAt: toIsoString(assignment.rows[0].effective_at) })
      ]
    );
  }

  async findAdminPolicyPermission(permissionKey) {
    const { rows } = await this.pool.query(
      `SELECT permission_key, resource, action, risk_level, scope_dimensions, active
       FROM admin_permissions
       WHERE permission_key = $1
       LIMIT 1`,
      [permissionKey]
    );
    return rows[0] || null;
  }

  async listEffectiveAdminPolicyGrants(adminId, permissionKey = null, at = new Date()) {
    const params = [adminId, at];
    const permissionFilter = permissionKey
      ? ` AND p.permission_key = $${params.push(permissionKey)}`
      : "";
    const { rows } = await this.pool.query(
      `SELECT p.permission_key, p.resource, p.action, p.risk_level, p.scope_dimensions,
              ar.scope_json, 'role'::text AS grant_source, ar.id AS source_id
       FROM admin_account_roles ar
       JOIN admin_roles r ON r.role_key = ar.role_key AND r.active = true
       JOIN admin_role_permissions rp ON rp.role_key = ar.role_key
       JOIN admin_permissions p ON p.permission_key = rp.permission_key AND p.active = true
       WHERE ar.admin_id = $1
         AND ar.effective_at <= $2
         AND (ar.expires_at IS NULL OR ar.expires_at > $2)
         AND ar.revoked_at IS NULL${permissionFilter}
       UNION ALL
       SELECT p.permission_key, p.resource, p.action, p.risk_level, p.scope_dimensions,
              pg.scope_json, 'direct'::text AS grant_source, pg.id AS source_id
       FROM admin_permission_grants pg
       JOIN admin_permissions p ON p.permission_key = pg.permission_key AND p.active = true
       WHERE pg.admin_id = $1
         AND pg.effective_at <= $2
         AND (pg.expires_at IS NULL OR pg.expires_at > $2)
         AND pg.revoked_at IS NULL${permissionFilter}
       ORDER BY permission_key, grant_source, source_id`,
      params
    );
    return rows;
  }

  async grantAdminPermission({
    adminId,
    permissionKey,
    scope,
    grantedByAdminId = null,
    grantedBySubject,
    reason,
    effectiveAt = null,
    expiresAt = null
  }) {
    const client = await this.pool.connect();
    try {
      await client.query("BEGIN");
      const permission = await client.query(
        `SELECT permission_key FROM admin_permissions WHERE permission_key = $1 AND active = true LIMIT 1`,
        [permissionKey]
      );
      if (permission.rows.length === 0) {
        throw createAdminStoreError("UNKNOWN_PERMISSION", "Permission is not available", { permissionKey });
      }
      const { rows } = await client.query(
        `INSERT INTO admin_permission_grants (
           admin_id, permission_key, scope_json, granted_by_admin_id, granted_by_subject, reason, effective_at, expires_at
         ) VALUES ($1, $2, $3::jsonb, $4, $5, $6, COALESCE($7::timestamptz, current_timestamp), $8::timestamptz)
         RETURNING id, effective_at, expires_at`,
        [adminId, permissionKey, toRequiredJsonb(scope), grantedByAdminId, grantedBySubject, reason, effectiveAt, expiresAt]
      );
      await client.query(
        `INSERT INTO admin_authorization_audit_events (
           event_type, actor_admin_id, actor_subject, subject_admin_id, permission_key, grant_id, reason, scope_json, details_json
         ) VALUES ('permission_granted', $1, $2, $3, $4, $5, $6, $7::jsonb, $8::jsonb)`,
        [
          grantedByAdminId,
          grantedBySubject,
          adminId,
          permissionKey,
          rows[0].id,
          reason,
          toRequiredJsonb(scope),
          toRequiredJsonb({ effectiveAt: toIsoString(rows[0].effective_at), expiresAt: toIsoString(rows[0].expires_at) })
        ]
      );
      await client.query("COMMIT");
      return rows[0];
    } catch (error) {
      await client.query("ROLLBACK").catch(() => undefined);
      throw error;
    } finally {
      client.release();
    }
  }

  async grantAdminRole({
    adminId,
    roleKey,
    scope,
    grantedByAdminId = null,
    grantedBySubject,
    reason,
    effectiveAt = null,
    expiresAt = null
  }) {
    const client = await this.pool.connect();
    try {
      await client.query("BEGIN");
      const role = await client.query(
        `SELECT role_key FROM admin_roles WHERE role_key = $1 AND active = true LIMIT 1`,
        [roleKey]
      );
      if (role.rows.length === 0) {
        throw createAdminStoreError("UNKNOWN_ADMIN_ROLE", "Admin role is not available", { roleKey });
      }
      const { rows } = await client.query(
        `INSERT INTO admin_account_roles (
           admin_id, role_key, scope_json, granted_by_admin_id, granted_by_subject, reason, effective_at, expires_at
         ) VALUES ($1, $2, $3::jsonb, $4, $5, $6, COALESCE($7::timestamptz, current_timestamp), $8::timestamptz)
         RETURNING id, effective_at, expires_at`,
        [adminId, roleKey, toRequiredJsonb(scope), grantedByAdminId, grantedBySubject, reason, effectiveAt, expiresAt]
      );
      await client.query(
        `INSERT INTO admin_authorization_audit_events (
           event_type, actor_admin_id, actor_subject, subject_admin_id, role_key, assignment_id, reason, scope_json, details_json
         ) VALUES ('account_role_granted', $1, $2, $3, $4, $5, $6, $7::jsonb, $8::jsonb)`,
        [
          grantedByAdminId,
          grantedBySubject,
          adminId,
          roleKey,
          rows[0].id,
          reason,
          toRequiredJsonb(scope),
          toRequiredJsonb({ effectiveAt: toIsoString(rows[0].effective_at), expiresAt: toIsoString(rows[0].expires_at) })
        ]
      );
      await client.query("COMMIT");
      return rows[0];
    } catch (error) {
      await client.query("ROLLBACK").catch(() => undefined);
      throw error;
    } finally {
      client.release();
    }
  }

  async revokeAdminPermission({ grantId, revokedByAdminId = null, revokedBySubject, reason }) {
    const client = await this.pool.connect();
    try {
      await client.query("BEGIN");
      const { rows } = await client.query(
        `UPDATE admin_permission_grants
         SET revoked_at = current_timestamp,
             revoked_by_admin_id = $2,
             revoked_by_subject = $3,
             revocation_reason = $4
         WHERE id = $1 AND revoked_at IS NULL
         RETURNING id, admin_id, permission_key, scope_json, revoked_at`,
        [grantId, revokedByAdminId, revokedBySubject, reason]
      );
      if (rows.length === 0) {
        throw createAdminStoreError("ADMIN_PERMISSION_GRANT_NOT_ACTIVE", "Permission grant is not active", { grantId });
      }
      const grant = rows[0];
      await client.query(
        `INSERT INTO admin_authorization_audit_events (
           event_type, actor_admin_id, actor_subject, subject_admin_id, permission_key, grant_id, reason, scope_json, details_json
         ) VALUES ('permission_revoked', $1, $2, $3, $4, $5, $6, $7::jsonb, $8::jsonb)`,
        [
          revokedByAdminId,
          revokedBySubject,
          grant.admin_id,
          grant.permission_key,
          grant.id,
          reason,
          toRequiredJsonb(grant.scope_json),
          toRequiredJsonb({ revokedAt: toIsoString(grant.revoked_at) })
        ]
      );
      await client.query("COMMIT");
      return grant;
    } catch (error) {
      await client.query("ROLLBACK").catch(() => undefined);
      throw error;
    } finally {
      client.release();
    }
  }

  async revokeAdminRole({ assignmentId, revokedByAdminId = null, revokedBySubject, reason }) {
    const client = await this.pool.connect();
    try {
      await client.query("BEGIN");
      const { rows } = await client.query(
        `UPDATE admin_account_roles
         SET revoked_at = current_timestamp,
             revoked_by_admin_id = $2,
             revoked_by_subject = $3,
             revocation_reason = $4
         WHERE id = $1 AND revoked_at IS NULL
         RETURNING id, admin_id, role_key, scope_json, revoked_at`,
        [assignmentId, revokedByAdminId, revokedBySubject, reason]
      );
      if (rows.length === 0) {
        throw createAdminStoreError("ADMIN_ROLE_ASSIGNMENT_NOT_ACTIVE", "Admin role assignment is not active", { assignmentId });
      }
      const assignment = rows[0];
      await client.query(
        `INSERT INTO admin_authorization_audit_events (
           event_type, actor_admin_id, actor_subject, subject_admin_id, role_key, assignment_id, reason, scope_json, details_json
         ) VALUES ('account_role_revoked', $1, $2, $3, $4, $5, $6, $7::jsonb, $8::jsonb)`,
        [
          revokedByAdminId,
          revokedBySubject,
          assignment.admin_id,
          assignment.role_key,
          assignment.id,
          reason,
          toRequiredJsonb(assignment.scope_json),
          toRequiredJsonb({ revokedAt: toIsoString(assignment.revoked_at) })
        ]
      );
      await client.query("COMMIT");
      return assignment;
    } catch (error) {
      await client.query("ROLLBACK").catch(() => undefined);
      throw error;
    } finally {
      client.release();
    }
  }

  async insertAdminOperationAuditEvent(client, {
    operation = null,
    breakglassGrantId = null,
    eventType,
    actorAdminId = null,
    actorSubject,
    requestId = null,
    permissionKey = null,
    riskLevel = null,
    traceId = null,
    reason,
    targetSummary = null,
    resultSummary = null,
    details = {}
  }) {
    await client.query(
      `INSERT INTO admin_operation_audit_events (
         operation_id, breakglass_grant_id, event_type, actor_admin_id, actor_subject,
         request_id, permission_key, risk_level, trace_id, reason,
         target_summary_json, result_summary_json, details_json
       ) VALUES (
         $1, $2, $3, $4, $5,
         $6, $7, $8, $9, $10,
         $11::jsonb, $12::jsonb, $13::jsonb
       )`,
      [
        operation?.operationId || operation?.operation_id || null,
        breakglassGrantId,
        eventType,
        actorAdminId,
        actorSubject,
        requestId || operation?.requestId || operation?.request_id || null,
        permissionKey || operation?.permissionKey || operation?.permission_key || null,
        riskLevel || operation?.riskLevel || operation?.risk_level || null,
        traceId || operation?.traceId || operation?.trace_id || null,
        reason,
        targetSummary === null ? null : toRequiredJsonb(targetSummary),
        resultSummary === null ? null : toRequiredJsonb(resultSummary),
        toRequiredJsonb(details)
      ]
    );
  }

  async reserveAdminOperationPreflight({
    operationId,
    requestId,
    actorAdminId,
    actorSubject,
    permissionKey,
    riskLevel,
    authorizationScope,
    requestedScope,
    scopeSha256,
    targetSummary,
    targetSha256,
    payloadSha256,
    semanticSha256,
    reason,
    traceId,
    approvalStatus,
    preview
  }) {
    const client = await this.pool.connect();
    try {
      await client.query("BEGIN");
      const existing = await client.query(
        `SELECT r.*,
                p.preview_id, p.summary_sha256, p.expires_at AS preview_expires_at,
                p.consumed_at AS preview_consumed_at
         FROM admin_operation_requests r
         LEFT JOIN admin_operation_previews p ON p.operation_id = r.operation_id
         WHERE r.request_id = $1
         FOR UPDATE OF r`,
        [requestId]
      );
      if (existing.rows.length > 0) {
        const operation = toAdminOperation(existing.rows[0]);
        await client.query("COMMIT");
        return {
          kind: operation.semanticSha256 === semanticSha256 ? "existing" : "conflict",
          operation
        };
      }

      const inserted = await client.query(
        `INSERT INTO admin_operation_requests (
           operation_id, request_id, actor_admin_id, actor_subject, permission_key, risk_level,
           authorization_scope_json, requested_scope_json, scope_sha256,
           target_summary_json, target_sha256, payload_sha256, semantic_sha256,
           reason, trace_id, status, approval_status
         ) VALUES (
           $1::uuid, $2, $3, $4, $5, $6,
           $7::jsonb, $8::jsonb, $9,
           $10::jsonb, $11, $12, $13,
           $14, $15, 'preflighted', $16
         )
         RETURNING *`,
        [
          operationId,
          requestId,
          actorAdminId,
          actorSubject,
          permissionKey,
          riskLevel,
          toRequiredJsonb(authorizationScope),
          toRequiredJsonb(requestedScope),
          scopeSha256,
          toRequiredJsonb(targetSummary),
          targetSha256,
          payloadSha256,
          semanticSha256,
          reason,
          traceId,
          approvalStatus
        ]
      );
      const operation = toAdminOperation(inserted.rows[0]);
      await client.query(
        `INSERT INTO admin_operation_previews (
           preview_id, operation_id, nonce_sha256, impact_summary_json, summary_sha256,
           target_sha256, payload_sha256, expires_at
         ) VALUES ($1::uuid, $2::uuid, $3, $4::jsonb, $5, $6, $7, $8::timestamptz)`,
        [
          preview.previewId,
          operationId,
          preview.nonceSha256,
          toRequiredJsonb(preview.impactSummary),
          preview.summarySha256,
          targetSha256,
          payloadSha256,
          preview.expiresAt
        ]
      );
      await client.query(
        `INSERT INTO admin_operation_approvals (operation_id, status, evidence_summary_json)
         VALUES ($1::uuid, $2, $3::jsonb)`,
        [operationId, approvalStatus, toRequiredJsonb({ requirement: approvalStatus })]
      );
      await this.insertAdminOperationAuditEvent(client, {
        operation,
        eventType: "preflight_created",
        actorAdminId,
        actorSubject,
        reason,
        targetSummary,
        details: {
          previewId: preview.previewId,
          previewExpiresAt: preview.expiresAt,
          approvalStatus,
          payloadSha256
        }
      });
      await client.query("COMMIT");
      return {
        kind: "created",
        operation: {
          ...operation,
          preview: {
            previewId: preview.previewId,
            summarySha256: preview.summarySha256,
            expiresAt: preview.expiresAt,
            consumedAt: null
          }
        }
      };
    } catch (error) {
      await client.query("ROLLBACK").catch(() => undefined);
      if (error.code === UNIQUE_VIOLATION) {
        const { rows } = await this.pool.query(
          `SELECT * FROM admin_operation_requests WHERE request_id = $1 LIMIT 1`,
          [requestId]
        );
        if (rows.length > 0) {
          const operation = toAdminOperation(rows[0]);
          return { kind: operation.semanticSha256 === semanticSha256 ? "existing" : "conflict", operation };
        }
      }
      throw error;
    } finally {
      client.release();
    }
  }

  async getAdminOperationByRequestId(requestId) {
    const { rows } = await this.pool.query(
      `SELECT r.*,
              p.preview_id, p.summary_sha256, p.expires_at AS preview_expires_at,
              p.consumed_at AS preview_consumed_at
       FROM admin_operation_requests r
       LEFT JOIN admin_operation_previews p ON p.operation_id = r.operation_id
       WHERE r.request_id = $1
       LIMIT 1`,
      [requestId]
    );
    return rows.length > 0 ? toAdminOperation(rows[0]) : null;
  }

  async claimAdminOperationExecution({ requestId, semanticSha256, nonceSha256, summarySha256, now = new Date() }) {
    const client = await this.pool.connect();
    try {
      await client.query("BEGIN");
      const selected = await client.query(
        `SELECT r.*,
                p.preview_id, p.nonce_sha256, p.summary_sha256, p.expires_at AS preview_expires_at,
                p.consumed_at AS preview_consumed_at,
                a.status AS approval_record_status
         FROM admin_operation_requests r
         JOIN admin_operation_previews p ON p.operation_id = r.operation_id
         JOIN admin_operation_approvals a ON a.operation_id = r.operation_id
         WHERE r.request_id = $1
         FOR UPDATE OF r, p, a`,
        [requestId]
      );
      if (selected.rows.length === 0) {
        await client.query("COMMIT");
        return { kind: "not_found" };
      }

      const row = selected.rows[0];
      const operation = toAdminOperation(row);
      if (operation.semanticSha256 !== semanticSha256) {
        await client.query("COMMIT");
        return { kind: "conflict", operation };
      }
      if (operationIsTerminal(operation.status)) {
        await client.query("COMMIT");
        return { kind: "terminal", operation };
      }
      if (operation.status === "executing") {
        await client.query("COMMIT");
        return { kind: "in_progress", operation };
      }
      if (!(["preflighted", "approved"].includes(operation.status)) ||
          operation.approvalStatus !== row.approval_record_status) {
        await client.query("COMMIT");
        return { kind: "state_conflict", operation };
      }
      if (operation.approvalStatus === "pending") {
        await client.query("COMMIT");
        return { kind: "approval_pending", operation };
      }
      if (operation.approvalStatus === "rejected") {
        await client.query("COMMIT");
        return { kind: "approval_rejected", operation };
      }
      if (new Date(row.preview_expires_at).getTime() <= new Date(now).getTime()) {
        await client.query("COMMIT");
        return { kind: "preview_expired", operation };
      }
      if (row.preview_consumed_at) {
        await client.query("COMMIT");
        return { kind: "nonce_replayed", operation };
      }
      if (row.nonce_sha256 !== nonceSha256 || row.summary_sha256 !== summarySha256) {
        await client.query("COMMIT");
        return { kind: "preview_mismatch", operation };
      }

      const previewUpdate = await client.query(
        `UPDATE admin_operation_previews
         SET consumed_at = $2::timestamptz
         WHERE preview_id = $1::uuid AND consumed_at IS NULL`,
        [row.preview_id, now]
      );
      if (previewUpdate.rowCount !== 1) {
        await client.query("COMMIT");
        return { kind: "nonce_replayed", operation };
      }
      const claimed = await client.query(
        `UPDATE admin_operation_requests
         SET status = 'executing', execution_claimed_at = $2::timestamptz, updated_at = $2::timestamptz
         WHERE operation_id = $1::uuid AND status IN ('preflighted', 'approved')
         RETURNING *`,
        [operation.operationId, now]
      );
      if (claimed.rows.length === 0) {
        await client.query("COMMIT");
        return { kind: "state_conflict", operation };
      }
      const claimedOperation = toAdminOperation({
        ...claimed.rows[0],
        preview_id: row.preview_id,
        summary_sha256: row.summary_sha256,
        preview_expires_at: row.preview_expires_at,
        preview_consumed_at: now
      });
      await this.insertAdminOperationAuditEvent(client, {
        operation: claimedOperation,
        eventType: "execution_claimed",
        actorAdminId: claimedOperation.actorAdminId,
        actorSubject: claimedOperation.actorSubject,
        reason: claimedOperation.reason,
        targetSummary: claimedOperation.targetSummary,
        details: { previewId: row.preview_id }
      });
      await client.query("COMMIT");
      return { kind: "claimed", operation: claimedOperation };
    } catch (error) {
      await client.query("ROLLBACK").catch(() => undefined);
      throw error;
    } finally {
      client.release();
    }
  }

  async completeAdminOperation({ operationId, status, resultSummary = null, errorSummary = null, details = {}, now = new Date() }) {
    const eventTypes = {
      succeeded: "execution_succeeded",
      failed: "execution_failed",
      execution_uncertain: "execution_uncertain",
      cancelled: "execution_cancelled"
    };
    if (!Object.prototype.hasOwnProperty.call(eventTypes, status)) {
      throw operationStoreError("ADMIN_OPERATION_RESULT_STATUS_INVALID", "Operation result status is invalid", { status });
    }

    const client = await this.pool.connect();
    try {
      await client.query("BEGIN");
      const existing = await client.query(
        `SELECT * FROM admin_operation_requests WHERE operation_id = $1::uuid FOR UPDATE`,
        [operationId]
      );
      if (existing.rows.length === 0) {
        throw operationStoreError("ADMIN_OPERATION_NOT_FOUND", "Operation does not exist", { operationId });
      }
      const prior = toAdminOperation(existing.rows[0]);
      if (operationIsTerminal(prior.status)) {
        await client.query("COMMIT");
        return { kind: "terminal", operation: prior };
      }
      if (prior.status !== "executing") {
        await client.query("COMMIT");
        return { kind: "state_conflict", operation: prior };
      }
      const updated = await client.query(
        `UPDATE admin_operation_requests
         SET status = $2,
             result_summary_json = $3::jsonb,
             error_summary_json = $4::jsonb,
             completed_at = $5::timestamptz,
             updated_at = $5::timestamptz
         WHERE operation_id = $1::uuid AND status = 'executing'
         RETURNING *`,
        [operationId, status, resultSummary === null ? null : toRequiredJsonb(resultSummary), errorSummary === null ? null : toRequiredJsonb(errorSummary), now]
      );
      if (updated.rows.length === 0) {
        await client.query("COMMIT");
        return { kind: "state_conflict", operation: prior };
      }
      const operation = toAdminOperation(updated.rows[0]);
      await this.insertAdminOperationAuditEvent(client, {
        operation,
        eventType: eventTypes[status],
        actorAdminId: operation.actorAdminId,
        actorSubject: operation.actorSubject,
        reason: operation.reason,
        targetSummary: operation.targetSummary,
        resultSummary: status === "succeeded" ? resultSummary : errorSummary,
        details
      });
      await client.query("COMMIT");
      return { kind: "completed", operation };
    } catch (error) {
      await client.query("ROLLBACK").catch(() => undefined);
      throw error;
    } finally {
      client.release();
    }
  }

  async markAdminOperationExecutionUncertain({ operationId, errorSummary, now = new Date() }) {
    const { rows } = await this.pool.query(
      `UPDATE admin_operation_requests
       SET status = 'execution_uncertain',
           error_summary_json = $2::jsonb,
           completed_at = $3::timestamptz,
           updated_at = $3::timestamptz
       WHERE operation_id = $1::uuid AND status = 'executing'
       RETURNING *`,
      [operationId, toRequiredJsonb(errorSummary), now]
    );
    if (rows.length > 0) {
      return { kind: "marked_uncertain", operation: toAdminOperation(rows[0]) };
    }
    const existing = await this.pool.query(
      `SELECT * FROM admin_operation_requests WHERE operation_id = $1::uuid LIMIT 1`,
      [operationId]
    );
    if (existing.rows.length === 0) {
      throw operationStoreError("ADMIN_OPERATION_NOT_FOUND", "Operation does not exist", { operationId });
    }
    return { kind: "terminal_or_conflict", operation: toAdminOperation(existing.rows[0]) };
  }

  async decideAdminOperationApproval({
    requestId,
    status,
    decidedByAdminId = null,
    decidedBySubject,
    evidenceSummary = {},
    rejectionReason = null,
    now = new Date()
  }) {
    if (!["approved", "rejected"].includes(status)) {
      throw operationStoreError("ADMIN_OPERATION_APPROVAL_STATUS_INVALID", "Approval status is invalid", { status });
    }

    const client = await this.pool.connect();
    try {
      await client.query("BEGIN");
      const selected = await client.query(
        `SELECT r.*,
                a.status AS approval_record_status
         FROM admin_operation_requests r
         JOIN admin_operation_approvals a ON a.operation_id = r.operation_id
         WHERE r.request_id = $1
         FOR UPDATE OF r, a`,
        [requestId]
      );
      if (selected.rows.length === 0) {
        throw operationStoreError("ADMIN_OPERATION_NOT_FOUND", "Operation does not exist", { requestId });
      }
      const prior = toAdminOperation(selected.rows[0]);
      if (prior.approvalStatus !== "pending" || selected.rows[0].approval_record_status !== "pending" || prior.status !== "preflighted") {
        await client.query("COMMIT");
        return { kind: "state_conflict", operation: prior };
      }
      const nextOperationStatus = status === "approved" ? "approved" : "cancelled";
      const next = await client.query(
        `UPDATE admin_operation_requests
         SET approval_status = $2,
             status = $3,
             completed_at = CASE WHEN $3 = 'cancelled' THEN $4::timestamptz ELSE NULL END,
             error_summary_json = CASE WHEN $3 = 'cancelled' THEN $5::jsonb ELSE NULL END,
             updated_at = $4::timestamptz
         WHERE operation_id = $1::uuid
         RETURNING *`,
        [
          prior.operationId,
          status,
          nextOperationStatus,
          now,
          status === "rejected" ? toRequiredJsonb({ code: "ADMIN_OPERATION_APPROVAL_REJECTED", reason: rejectionReason }) : null
        ]
      );
      await client.query(
        `UPDATE admin_operation_approvals
         SET status = $2,
             decided_at = $3::timestamptz,
             decided_by_admin_id = $4,
             decided_by_subject = $5,
             evidence_summary_json = $6::jsonb,
             rejection_reason = $7,
             updated_at = $3::timestamptz
         WHERE operation_id = $1::uuid AND status = 'pending'`,
        [prior.operationId, status, now, decidedByAdminId, decidedBySubject, toRequiredJsonb(evidenceSummary), rejectionReason]
      );
      const operation = toAdminOperation(next.rows[0]);
      await this.insertAdminOperationAuditEvent(client, {
        operation,
        eventType: status === "approved" ? "approval_approved" : "approval_rejected",
        actorAdminId: decidedByAdminId,
        actorSubject: decidedBySubject,
        reason: status === "approved" ? operation.reason : rejectionReason,
        targetSummary: operation.targetSummary,
        resultSummary: status === "approved" ? evidenceSummary : { code: "ADMIN_OPERATION_APPROVAL_REJECTED" },
        details: { approvalStatus: status }
      });
      await client.query("COMMIT");
      return { kind: status, operation };
    } catch (error) {
      await client.query("ROLLBACK").catch(() => undefined);
      throw error;
    } finally {
      client.release();
    }
  }

  async createAdminBreakglassGrant({
    grantId,
    activationRequestId,
    actorAdminId,
    actorSubject,
    permissionKey,
    scope,
    scopeSha256,
    targetSummary,
    targetSha256,
    semanticSha256,
    reason,
    expiresAt
  }) {
    const client = await this.pool.connect();
    try {
      await client.query("BEGIN");
      const existing = await client.query(
        `SELECT * FROM admin_breakglass_grants WHERE activation_request_id = $1 FOR UPDATE`,
        [activationRequestId]
      );
      if (existing.rows.length > 0) {
        const grant = toBreakglassGrant(existing.rows[0]);
        const same = String(grant.actorAdminId) === String(actorAdminId) &&
          grant.permissionKey === permissionKey &&
          grant.semanticSha256 === semanticSha256;
        await client.query("COMMIT");
        return { kind: same ? "existing" : "conflict", grant };
      }
      const permission = await client.query(
        `SELECT permission_key, risk_level, active
         FROM admin_permissions
         WHERE permission_key = $1
         FOR KEY SHARE`,
        [permissionKey]
      );
      if (permission.rows.length === 0 || permission.rows[0].active !== true || permission.rows[0].risk_level !== "emergency") {
        throw operationStoreError("ADMIN_BREAKGLASS_PERMISSION_INVALID", "Break-glass requires an active emergency permission", { permissionKey });
      }
      const inserted = await client.query(
        `INSERT INTO admin_breakglass_grants (
           grant_id, activation_request_id, actor_admin_id, actor_subject, permission_key,
           scope_json, scope_sha256, target_summary_json, target_sha256, semantic_sha256, reason, expires_at
         ) VALUES (
           $1::uuid, $2, $3, $4, $5,
           $6::jsonb, $7, $8::jsonb, $9, $10, $11, $12::timestamptz
         ) RETURNING *`,
        [
          grantId,
          activationRequestId,
          actorAdminId,
          actorSubject,
          permissionKey,
          toRequiredJsonb(scope),
          scopeSha256,
          toRequiredJsonb(targetSummary),
          targetSha256,
          semanticSha256,
          reason,
          expiresAt
        ]
      );
      const grant = toBreakglassGrant(inserted.rows[0]);
      await this.insertAdminOperationAuditEvent(client, {
        breakglassGrantId: grant.grantId,
        eventType: "breakglass_activated",
        actorAdminId,
        actorSubject,
        requestId: activationRequestId,
        permissionKey,
        riskLevel: "emergency",
        reason,
        targetSummary,
        details: { expiresAt: grant.expiresAt, scopeSha256 }
      });
      await client.query("COMMIT");
      return { kind: "created", grant };
    } catch (error) {
      await client.query("ROLLBACK").catch(() => undefined);
      throw error;
    } finally {
      client.release();
    }
  }

  async revokeAdminBreakglassGrant({ grantId, revokedByAdminId = null, revokedBySubject, reason }) {
    const client = await this.pool.connect();
    try {
      await client.query("BEGIN");
      const updated = await client.query(
        `UPDATE admin_breakglass_grants
         SET revoked_at = current_timestamp,
             revoked_by_admin_id = $2,
             revoked_by_subject = $3,
             revocation_reason = $4
         WHERE grant_id = $1::uuid AND revoked_at IS NULL
         RETURNING *`,
        [grantId, revokedByAdminId, revokedBySubject, reason]
      );
      if (updated.rows.length === 0) {
        throw operationStoreError("ADMIN_BREAKGLASS_GRANT_NOT_ACTIVE", "Break-glass grant is not active", { grantId });
      }
      const grant = toBreakglassGrant(updated.rows[0]);
      await this.insertAdminOperationAuditEvent(client, {
        breakglassGrantId: grant.grantId,
        eventType: "breakglass_revoked",
        actorAdminId: revokedByAdminId,
        actorSubject: revokedBySubject,
        requestId: grant.activationRequestId,
        permissionKey: grant.permissionKey,
        riskLevel: "emergency",
        reason,
        targetSummary: grant.targetSummary,
        details: { revokedAt: grant.revokedAt }
      });
      await client.query("COMMIT");
      return grant;
    } catch (error) {
      await client.query("ROLLBACK").catch(() => undefined);
      throw error;
    } finally {
      client.release();
    }
  }

  async listActiveAdminBreakglassGrants(adminId, permissionKey = null, at = new Date()) {
    const params = [adminId, at];
    const permissionFilter = permissionKey ? ` AND permission_key = $${params.push(permissionKey)}` : "";
    const { rows } = await this.pool.query(
      `SELECT * FROM admin_breakglass_grants
       WHERE actor_admin_id = $1
         AND activated_at <= $2
         AND expires_at > $2
         AND revoked_at IS NULL${permissionFilter}
       ORDER BY expires_at ASC, grant_id ASC`,
      params
    );
    return rows.map(toBreakglassGrant);
  }

  async updateLastLogin(adminId) {
    await this.pool.query(
      `UPDATE admin_accounts SET last_login_at = current_timestamp WHERE id = $1`,
      [adminId]
    );
  }

  async updateAdminPassword(adminId, password) {
    const passwordSalt = crypto.randomBytes(16).toString("hex");
    const passwordHash = hashPassword(password);
    const result = await this.pool.query(
      `UPDATE admin_accounts
       SET password_algo = 'bcrypt',
           password_salt = $1,
           password_hash = $2
       WHERE id = $3`,
      [passwordSalt, passwordHash, adminId]
    );

    return result.rowCount > 0;
  }

  async appendAuditLog({ adminId, adminUsername, action, targetType, targetValue, details, ip }) {
    await this.pool.query(
      `INSERT INTO admin_audit_logs (admin_id, admin_username, action, target_type, target_value, details_json, ip)
       VALUES ($1, $2, $3, $4, $5, $6::jsonb, $7)`,
      [
        adminId,
        adminUsername,
        action,
        targetType || null,
        targetValue || null,
        toJsonb(details),
        ip || null
      ]
    );
  }

  async appendSecurityAuditLog({
    eventType,
    targetType,
    targetValue,
    severity = "warning",
    clientIp,
    details
  }) {
    await this.pool.query(
      `INSERT INTO security_audit_logs (event_type, target_type, target_value, severity, client_ip, details_json)
       VALUES ($1, $2, $3, $4, $5, $6::jsonb)`,
      [
        eventType,
        targetType || null,
        targetValue || null,
        severity,
        clientIp || null,
        toJsonb(details)
      ]
    );
  }

  async listAdminOperationAuditEvents({
    limit = 100,
    from,
    to,
    cursor = null,
    actorAdminId,
    permissionKey,
    eventType,
    target,
    requestId,
    traceId,
    riskLevel,
    result
  } = {}) {
    const params = [from, to];
    let query = `SELECT e.*, r.status AS operation_status
                 FROM admin_operation_audit_events e
                 LEFT JOIN admin_operation_requests r ON r.operation_id = e.operation_id
                 WHERE e.created_at >= $1::timestamptz AND e.created_at < $2::timestamptz`;
    const add = (value) => {
      params.push(value);
      return `$${params.length}`;
    };

    if (actorAdminId !== undefined && actorAdminId !== null) {
      query += ` AND e.actor_admin_id = ${add(actorAdminId)}`;
    }
    if (permissionKey) {
      query += ` AND e.permission_key = ${add(permissionKey)}`;
    }
    if (eventType) {
      query += ` AND e.event_type = ${add(eventType)}`;
    }
    if (target) {
      const targetParam = add(target);
      query += ` AND (e.target_summary_json -> 'targetIds' ? ${targetParam} OR e.target_summary_json ->> 'targetId' = ${targetParam})`;
    }
    if (requestId) {
      query += ` AND e.request_id = ${add(requestId)}`;
    }
    if (traceId) {
      query += ` AND e.trace_id = ${add(traceId)}`;
    }
    if (riskLevel) {
      query += ` AND e.risk_level = ${add(riskLevel)}`;
    }
    if (result) {
      query += ` AND r.status = ${add(result)}`;
    }
    if (cursor) {
      const createdAt = add(cursor.createdAt);
      const id = add(cursor.id);
      query += ` AND (e.created_at, e.id) < (${createdAt}::timestamptz, ${id}::bigint)`;
    }
    query += ` ORDER BY e.created_at DESC, e.id DESC LIMIT ${add(Math.max(1, Math.min(Number(limit) || 1, 5001)))}`;
    const { rows } = await this.pool.query(query, params);
    return rows.map(toAdminOperationAuditEvent);
  }

  async getSecurityLogs({ limit = 50, offset = 0, eventType, targetType, severity, clientIp } = {}) {
    let query = `SELECT * FROM security_audit_logs WHERE 1=1`;
    const params = [];

    if (eventType) {
      params.push(eventType);
      query += ` AND event_type = ${nextParam(params)}`;
    }

    if (targetType) {
      params.push(targetType);
      query += ` AND target_type = ${nextParam(params)}`;
    }

    if (severity) {
      params.push(severity);
      query += ` AND severity = ${nextParam(params)}`;
    }

    if (clientIp) {
      params.push(clientIp);
      query += ` AND client_ip = ${nextParam(params)}`;
    }

    params.push(limit);
    query += ` ORDER BY created_at DESC LIMIT ${nextParam(params)}`;
    params.push(offset);
    query += ` OFFSET ${nextParam(params)}`;

    const { rows } = await this.pool.query(query, params);
    return rows;
  }

  async countSecurityLogs({ eventType, targetType, severity, clientIp } = {}) {
    let query = `SELECT COUNT(*) as total FROM security_audit_logs WHERE 1=1`;
    const params = [];

    if (eventType) {
      params.push(eventType);
      query += ` AND event_type = ${nextParam(params)}`;
    }

    if (targetType) {
      params.push(targetType);
      query += ` AND target_type = ${nextParam(params)}`;
    }

    if (severity) {
      params.push(severity);
      query += ` AND severity = ${nextParam(params)}`;
    }

    if (clientIp) {
      params.push(clientIp);
      query += ` AND client_ip = ${nextParam(params)}`;
    }

    const { rows } = await this.pool.query(query, params);
    return readTotal(rows);
  }

  async getAuditLogs({ limit = 50, offset = 0, adminId, action, targetType } = {}) {
    let query = `SELECT * FROM admin_audit_logs WHERE 1=1`;
    const params = [];

    if (adminId) {
      params.push(adminId);
      query += ` AND admin_id = ${nextParam(params)}`;
    }

    if (action) {
      params.push(action);
      query += ` AND action = ${nextParam(params)}`;
    }

    if (targetType) {
      params.push(targetType);
      query += ` AND target_type = ${nextParam(params)}`;
    }

    params.push(limit);
    query += ` ORDER BY created_at DESC LIMIT ${nextParam(params)}`;
    params.push(offset);
    query += ` OFFSET ${nextParam(params)}`;

    const { rows } = await this.pool.query(query, params);
    return rows;
  }

  async countAuditLogs({ adminId, action, targetType } = {}) {
    let query = `SELECT COUNT(*) as total FROM admin_audit_logs WHERE 1=1`;
    const params = [];

    if (adminId) {
      params.push(adminId);
      query += ` AND admin_id = ${nextParam(params)}`;
    }

    if (action) {
      params.push(action);
      query += ` AND action = ${nextParam(params)}`;
    }

    if (targetType) {
      params.push(targetType);
      query += ` AND target_type = ${nextParam(params)}`;
    }

    const { rows } = await this.pool.query(query, params);
    return readTotal(rows);
  }

  async countRecentAdminAuditActions({ adminId, action, since }) {
    const { rows } = await this.pool.query(
      `SELECT COUNT(*) AS total
       FROM admin_audit_logs
       WHERE admin_id = $1 AND action = $2 AND created_at >= $3::timestamptz`,
      [adminId, action, since]
    );
    return readTotal(rows);
  }

  // Asset ledger is owned by the game database.  This is a deliberately read-only projection:
  // there is no admin-store method to update or delete a ledger row.
  async getAssetLedger({
    characterId,
    requestId,
    originType,
    originId,
    deliveryId,
    from,
    to,
    limit = 50,
    offset = 0
  } = {}) {
    if (!this.gamePool) {
      throw new Error("GAME_DATABASE_UNAVAILABLE");
    }

    const { query, params } = assetLedgerQuery({
      characterId,
      requestId,
      originType,
      originId,
      deliveryId,
      from,
      to
    });
    params.push(limit);
    const limitParam = nextParam(params);
    params.push(offset);
    const offsetParam = nextParam(params);
    const { rows } = await this.gamePool.query(
      `${query}
       ORDER BY created_at DESC, id DESC
       LIMIT ${limitParam} OFFSET ${offsetParam}`,
      params
    );
    return rows.map(toAssetLedgerEntry);
  }

  async countAssetLedger(filters = {}) {
    if (!this.gamePool) {
      throw new Error("GAME_DATABASE_UNAVAILABLE");
    }

    const { where, params } = assetLedgerFilters(filters);
    const { rows } = await this.gamePool.query(
      `SELECT COUNT(*) AS total FROM character_asset_ledger${where}`,
      params
    );
    return readTotal(rows);
  }

  // ============================================================
  // Player Management (read from player_accounts)
  // ============================================================

  async findPlayerById(playerId) {
    const { rows } = await this.pool.query(
      `SELECT player_id, guest_id, login_name, display_name, account_type, status, ban_expires_at, created_at, last_login_at
       FROM player_accounts
       WHERE player_id = $1
       LIMIT 1`,
      [playerId]
    );
    return rows.length > 0 ? toPlayer(rows[0]) : null;
  }

  async findPlayers({ loginName, guestId, status, limit = 50, offset = 0 } = {}) {
    let query = `SELECT player_id, guest_id, login_name, display_name, account_type, status, ban_expires_at, created_at, last_login_at
       FROM player_accounts
       WHERE 1=1`;
    const params = [];

    if (loginName) {
      params.push(`%${loginName}%`);
      query += ` AND login_name LIKE ${nextParam(params)}`;
    }

    if (guestId) {
      params.push(`%${guestId}%`);
      query += ` AND guest_id LIKE ${nextParam(params)}`;
    }

    if (status) {
      params.push(status);
      query += ` AND status = ${nextParam(params)}`;
    }

    params.push(limit);
    query += ` ORDER BY last_login_at DESC LIMIT ${nextParam(params)}`;
    params.push(offset);
    query += ` OFFSET ${nextParam(params)}`;

    const { rows } = await this.pool.query(query, params);
    return rows.map(toPlayer);
  }

  async countPlayers({ loginName, guestId, status } = {}) {
    let query = `SELECT COUNT(*) as total FROM player_accounts WHERE 1=1`;
    const params = [];

    if (loginName) {
      params.push(`%${loginName}%`);
      query += ` AND login_name LIKE ${nextParam(params)}`;
    }

    if (guestId) {
      params.push(`%${guestId}%`);
      query += ` AND guest_id LIKE ${nextParam(params)}`;
    }

    if (status) {
      params.push(status);
      query += ` AND status = ${nextParam(params)}`;
    }

    const { rows } = await this.pool.query(query, params);
    return readTotal(rows);
  }

  async updatePlayerStatus(playerId, status, { banExpiresAt = undefined } = {}) {
    const nextBanExpiresAt = status === "banned" ? banExpiresAt ?? null : null;
    const result = await this.pool.query(
      `UPDATE player_accounts SET status = $1, ban_expires_at = $2 WHERE player_id = $3`,
      [status, nextBanExpiresAt, playerId]
    );
    return result.rowCount > 0;
  }

  async findCharacterById(characterId, { includeDeleted = true } = {}) {
    const { rows } = await this.gamePool.query(
      `SELECT ${characterSelectColumns()}
       FROM characters
       WHERE character_id = $1
         ${includeDeleted ? "" : "AND deleted_at IS NULL"}
       LIMIT 1`,
      [characterId]
    );

    return rows.length > 0 ? toCharacter(rows[0]) : null;
  }

  async findCharactersByAccountPlayerId(accountPlayerId, { includeDeleted = true, limit = 50, offset = 0 } = {}) {
    const { rows } = await this.gamePool.query(
      `SELECT ${characterSelectColumns()}
       FROM characters
       WHERE account_player_id = $1
         ${includeDeleted ? "" : "AND deleted_at IS NULL"}
       ORDER BY deleted_at NULLS FIRST, last_login_at DESC NULLS LAST, created_at DESC
       LIMIT $2 OFFSET $3`,
      [accountPlayerId, limit, offset]
    );

    return rows.map(toCharacter);
  }

  async countCharactersByAccountPlayerId(accountPlayerId, { includeDeleted = true } = {}) {
    const { rows } = await this.gamePool.query(
      `SELECT COUNT(*) as total
       FROM characters
       WHERE account_player_id = $1
         ${includeDeleted ? "" : "AND deleted_at IS NULL"}`,
      [accountPlayerId]
    );

    return readTotal(rows);
  }

  async findCharacterElementLogs({ characterId, limit = 20 } = {}) {
    const { rows } = await this.gamePool.query(
      `SELECT id,
              character_id,
              source_type,
              source_id,
              operator_type,
              operator_id,
              affinity_earth_delta,
              affinity_fire_delta,
              affinity_water_delta,
              affinity_wind_delta,
              mastery_earth_delta,
              mastery_fire_delta,
              mastery_water_delta,
              mastery_wind_delta,
              before_json,
              after_json,
              reason,
              created_at
       FROM character_element_logs
       WHERE character_id = $1
       ORDER BY created_at DESC, id DESC
       LIMIT $2`,
      [characterId, limit]
    );

    return rows.map(toCharacterElementLog);
  }

  async findCharacterDisciplineLogs({ characterId, limit = 20 } = {}) {
    const { rows } = await this.gamePool.query(
      `SELECT id,
              character_id,
              discipline_id,
              action,
              source_type,
              source_id,
              operator_type,
              operator_id,
              before_json,
              after_json,
              reason,
              created_at
       FROM character_discipline_logs
       WHERE character_id = $1
       ORDER BY created_at DESC, id DESC
       LIMIT $2`,
      [characterId, limit]
    );

    return rows.map(toCharacterDisciplineLog);
  }

  async findCharacterProfileOverview({ characterId, logLimit = 20 } = {}) {
    const character = await this.findCharacterById(characterId, { includeDeleted: true });
    if (!character) {
      return null;
    }

    const [titleOverview, elementLogs, disciplineLogs] = await Promise.all([
      this.findCharacterTitleOverview({ characterId, logLimit }),
      this.findCharacterElementLogs({ characterId, limit: logLimit }),
      this.findCharacterDisciplineLogs({ characterId, limit: logLimit })
    ]);

    return {
      character,
      titles: titleOverview.titles,
      equippedTitle: titleOverview.equippedTitle,
      disciplines: titleOverview.disciplines,
      titleLogs: titleOverview.titleLogs,
      elementLogs,
      disciplineLogs
    };
  }

  async withGameTransaction(callback) {
    const client = typeof this.gamePool.connect === "function"
      ? await this.gamePool.connect()
      : this.gamePool;
    const shouldRelease = typeof client.release === "function";

    try {
      await client.query("BEGIN");
      const result = await callback(client);
      await client.query("COMMIT");
      return result;
    } catch (error) {
      try {
        await client.query("ROLLBACK");
      } catch {
        // Preserve the original failure.
      }
      throw error;
    } finally {
      if (shouldRelease) {
        client.release();
      }
    }
  }

  async lockActiveCharacter(client, characterId) {
    const { rows } = await client.query(
      `SELECT ${characterSelectColumns()}
       FROM characters
       WHERE character_id = $1
         AND deleted_at IS NULL
       FOR UPDATE`,
      [characterId]
    );

    return rows.length > 0 ? toCharacter(rows[0]) : null;
  }

  async setCharacterElementsForAdmin({
    characterId,
    affinity,
    mastery,
    operatorType = "admin",
    operatorId,
    sourceType = "gm",
    sourceId = "admin-api-character-elements",
    reason = null
  } = {}) {
    return this.withGameTransaction(async (client) => {
      const beforeCharacter = await this.lockActiveCharacter(client, characterId);
      if (!beforeCharacter) {
        throw createAdminStoreError("CHARACTER_NOT_FOUND", "Character not found");
      }

      const beforeSnapshot = characterElementSnapshot(beforeCharacter);
      const nextAffinity = affinity || beforeSnapshot.affinity;
      const nextMastery = mastery || beforeSnapshot.mastery;
      const affinityDelta = elementsDelta(beforeSnapshot.affinity, nextAffinity);
      const masteryDelta = elementsDelta(beforeSnapshot.mastery, nextMastery);
      const changed = !isZeroElementsDelta(affinityDelta) || !isZeroElementsDelta(masteryDelta);

      let afterCharacter = beforeCharacter;
      if (changed) {
        const { rows } = await client.query(
          `UPDATE characters
           SET affinity_earth = $1,
               affinity_fire = $2,
               affinity_water = $3,
               affinity_wind = $4,
               mastery_earth = $5,
               mastery_fire = $6,
               mastery_water = $7,
               mastery_wind = $8
           WHERE character_id = $9
           RETURNING ${characterSelectColumns()}`,
          [
            nextAffinity.earth,
            nextAffinity.fire,
            nextAffinity.water,
            nextAffinity.wind,
            nextMastery.earth,
            nextMastery.fire,
            nextMastery.water,
            nextMastery.wind,
            characterId
          ]
        );
        afterCharacter = toCharacter(rows[0]);
      }

      const afterSnapshot = characterElementSnapshot(afterCharacter);
      await client.query(
        `INSERT INTO character_element_logs (
           character_id,
           source_type,
           source_id,
           operator_type,
           operator_id,
           affinity_earth_delta,
           affinity_fire_delta,
           affinity_water_delta,
           affinity_wind_delta,
           mastery_earth_delta,
           mastery_fire_delta,
           mastery_water_delta,
           mastery_wind_delta,
           before_json,
           after_json,
           reason,
           created_at
         ) VALUES (
           $1, $2, $3, $4, $5,
           $6, $7, $8, $9,
           $10, $11, $12, $13,
           $14::jsonb, $15::jsonb, $16,
           current_timestamp
         )`,
        [
          characterId,
          sourceType,
          sourceId,
          operatorType,
          operatorId || null,
          affinityDelta.earth,
          affinityDelta.fire,
          affinityDelta.water,
          affinityDelta.wind,
          masteryDelta.earth,
          masteryDelta.fire,
          masteryDelta.water,
          masteryDelta.wind,
          toRequiredJsonb(beforeSnapshot),
          toRequiredJsonb(afterSnapshot),
          reason
        ]
      );

      return {
        character: afterCharacter,
        before: beforeSnapshot,
        after: afterSnapshot,
        affinityDelta,
        masteryDelta,
        changed
      };
    });
  }

  async applyCharacterTitleForAdmin({
    characterId,
    action,
    titleId,
    expiresAt = null,
    operatorType = "admin",
    operatorId,
    sourceType = "gm",
    sourceId = "admin-api-character-titles",
    reason = null
  } = {}) {
    return this.withGameTransaction(async (client) => {
      const character = await this.lockActiveCharacter(client, characterId);
      if (!character) {
        throw createAdminStoreError("CHARACTER_NOT_FOUND", "Character not found");
      }

      if (action === "grant") {
        return this.grantCharacterTitleInTransaction(client, {
          characterId,
          titleId,
          expiresAt,
          operatorType,
          operatorId,
          sourceType,
          sourceId,
          reason
        });
      }

      if (action === "revoke") {
        return this.revokeCharacterTitleInTransaction(client, {
          characterId,
          titleId,
          operatorType,
          operatorId,
          sourceType,
          sourceId,
          reason
        });
      }

      if (action === "equip") {
        return this.equipCharacterTitleInTransaction(client, {
          characterId,
          titleId,
          operatorType,
          operatorId,
          sourceType,
          sourceId,
          reason
        });
      }

      if (action === "unequip") {
        return this.unequipCharacterTitleInTransaction(client, {
          characterId,
          titleId,
          operatorType,
          operatorId,
          sourceType,
          sourceId,
          reason
        });
      }

      throw createAdminStoreError("INVALID_GM_TITLE_ACTION", "invalid title action");
    });
  }

  async lockCharacterTitle(client, characterId, titleId) {
    const { rows } = await client.query(
      `SELECT character_id,
              title_id,
              source_type,
              source_id,
              is_equipped,
              unlocked_at,
              expires_at,
              created_at,
              updated_at,
              (expires_at IS NOT NULL AND expires_at <= current_timestamp) AS expired
       FROM character_titles
       WHERE character_id = $1 AND title_id = $2
       FOR UPDATE`,
      [characterId, titleId]
    );

    return rows.length > 0 ? rows[0] : null;
  }

  async insertCharacterTitleLog(client, {
    characterId,
    titleId,
    action,
    sourceType,
    sourceId,
    operatorType,
    operatorId,
    before,
    after,
    reason
  }) {
    await client.query(
      `INSERT INTO character_title_logs (
         character_id,
         title_id,
         action,
         source_type,
         source_id,
         operator_type,
         operator_id,
         before_json,
         after_json,
         reason,
         created_at
       ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8::jsonb, $9::jsonb, $10, current_timestamp)`,
      [
        characterId,
        titleId,
        action,
        sourceType || null,
        sourceId || null,
        operatorType || null,
        operatorId || null,
        before ? toRequiredJsonb(before) : null,
        after ? toRequiredJsonb(after) : null,
        reason || null
      ]
    );
  }

  async grantCharacterTitleInTransaction(client, input) {
    const existing = await this.lockCharacterTitle(client, input.characterId, input.titleId);
    const status = titleGrantStatus(existing);

    if (existing && !existing.expired) {
      const snapshot = titleSnapshot(existing);
      await this.insertCharacterTitleLog(client, {
        ...input,
        action: "grant",
        before: snapshot,
        after: snapshot
      });
      return {
        action: "grant",
        status,
        changed: false,
        title: toCharacterTitle(existing),
        before: snapshot,
        after: snapshot
      };
    }

    const before = titleSnapshot(existing);
    const { rows } = existing
      ? await client.query(
        `UPDATE character_titles
         SET source_type = $3,
             source_id = $4,
             is_equipped = false,
             unlocked_at = current_timestamp,
             expires_at = $5::timestamptz,
             updated_at = current_timestamp
         WHERE character_id = $1 AND title_id = $2
         RETURNING character_id,
                   title_id,
                   source_type,
                   source_id,
                   is_equipped,
                   unlocked_at,
                   expires_at,
                   created_at,
                   updated_at,
                   (expires_at IS NOT NULL AND expires_at <= current_timestamp) AS expired`,
        [input.characterId, input.titleId, input.sourceType, input.sourceId || null, input.expiresAt]
      )
      : await client.query(
        `INSERT INTO character_titles (
           character_id,
           title_id,
           source_type,
           source_id,
           is_equipped,
           unlocked_at,
           expires_at,
           created_at,
           updated_at
         ) VALUES ($1, $2, $3, $4, false, current_timestamp, $5::timestamptz, current_timestamp, current_timestamp)
         RETURNING character_id,
                   title_id,
                   source_type,
                   source_id,
                   is_equipped,
                   unlocked_at,
                   expires_at,
                   created_at,
                   updated_at,
                   (expires_at IS NOT NULL AND expires_at <= current_timestamp) AS expired`,
        [input.characterId, input.titleId, input.sourceType, input.sourceId || null, input.expiresAt]
      );

    const after = titleSnapshot(rows[0]);
    await this.insertCharacterTitleLog(client, {
      ...input,
      action: "grant",
      before,
      after
    });

    return {
      action: "grant",
      status,
      changed: true,
      title: toCharacterTitle(rows[0]),
      before,
      after
    };
  }

  async revokeCharacterTitleInTransaction(client, input) {
    const existing = await this.lockCharacterTitle(client, input.characterId, input.titleId);
    const before = titleSnapshot(existing);

    if (!existing) {
      await this.insertCharacterTitleLog(client, {
        ...input,
        action: "revoke",
        before: null,
        after: null
      });
      return {
        action: "revoke",
        status: "not_owned",
        changed: false,
        title: null,
        before: null,
        after: null
      };
    }

    await client.query(
      `DELETE FROM character_titles
       WHERE character_id = $1 AND title_id = $2`,
      [input.characterId, input.titleId]
    );
    await this.insertCharacterTitleLog(client, {
      ...input,
      action: "revoke",
      before,
      after: null
    });

    return {
      action: "revoke",
      status: "revoked",
      changed: true,
      title: null,
      before,
      after: null
    };
  }

  async equipCharacterTitleInTransaction(client, input) {
    const target = await this.lockCharacterTitle(client, input.characterId, input.titleId);
    if (!target) {
      throw createAdminStoreError("TITLE_NOT_OWNED", "title is not owned");
    }
    if (target.expired) {
      throw createAdminStoreError("TITLE_EXPIRED", "title is expired");
    }

    const before = titleSnapshot(target);
    if (target.is_equipped) {
      await this.insertCharacterTitleLog(client, {
        ...input,
        action: "equip",
        before,
        after: before
      });
      return {
        action: "equip",
        status: "already_equipped",
        changed: false,
        title: toCharacterTitle(target),
        unequipped: [],
        before,
        after: before
      };
    }

    const equippedRows = await client.query(
      `SELECT character_id,
              title_id,
              source_type,
              source_id,
              is_equipped,
              unlocked_at,
              expires_at,
              created_at,
              updated_at,
              (expires_at IS NOT NULL AND expires_at <= current_timestamp) AS expired
       FROM character_titles
       WHERE character_id = $1
         AND title_id <> $2
         AND is_equipped = true
       FOR UPDATE`,
      [input.characterId, input.titleId]
    );

    const unequipped = [];
    for (const row of equippedRows.rows) {
      const unequipBefore = titleSnapshot(row);
      const { rows } = await client.query(
        `UPDATE character_titles
         SET is_equipped = false,
             updated_at = current_timestamp
         WHERE character_id = $1 AND title_id = $2
         RETURNING character_id,
                   title_id,
                   source_type,
                   source_id,
                   is_equipped,
                   unlocked_at,
                   expires_at,
                   created_at,
                   updated_at,
                   (expires_at IS NOT NULL AND expires_at <= current_timestamp) AS expired`,
        [input.characterId, row.title_id]
      );
      const unequipAfter = titleSnapshot(rows[0]);
      await this.insertCharacterTitleLog(client, {
        ...input,
        titleId: row.title_id,
        action: "unequip",
        before: unequipBefore,
        after: unequipAfter
      });
      unequipped.push(toCharacterTitle(rows[0]));
    }

    const { rows } = await client.query(
      `UPDATE character_titles
       SET is_equipped = true,
           updated_at = current_timestamp
       WHERE character_id = $1 AND title_id = $2
       RETURNING character_id,
                 title_id,
                 source_type,
                 source_id,
                 is_equipped,
                 unlocked_at,
                 expires_at,
                 created_at,
                 updated_at,
                 (expires_at IS NOT NULL AND expires_at <= current_timestamp) AS expired`,
      [input.characterId, input.titleId]
    );
    const after = titleSnapshot(rows[0]);
    await this.insertCharacterTitleLog(client, {
      ...input,
      action: "equip",
      before,
      after
    });

    return {
      action: "equip",
      status: "equipped",
      changed: true,
      title: toCharacterTitle(rows[0]),
      unequipped,
      before,
      after
    };
  }

  async unequipCharacterTitleInTransaction(client, input) {
    const target = await this.lockCharacterTitle(client, input.characterId, input.titleId);
    const before = titleSnapshot(target);

    if (!target) {
      await this.insertCharacterTitleLog(client, {
        ...input,
        action: "unequip",
        before: null,
        after: null
      });
      return {
        action: "unequip",
        status: "not_owned",
        changed: false,
        title: null,
        before: null,
        after: null
      };
    }

    if (!target.is_equipped) {
      await this.insertCharacterTitleLog(client, {
        ...input,
        action: "unequip",
        before,
        after: before
      });
      return {
        action: "unequip",
        status: "already_unequipped",
        changed: false,
        title: toCharacterTitle(target),
        before,
        after: before
      };
    }

    const { rows } = await client.query(
      `UPDATE character_titles
       SET is_equipped = false,
           updated_at = current_timestamp
       WHERE character_id = $1 AND title_id = $2
       RETURNING character_id,
                 title_id,
                 source_type,
                 source_id,
                 is_equipped,
                 unlocked_at,
                 expires_at,
                 created_at,
                 updated_at,
                 (expires_at IS NOT NULL AND expires_at <= current_timestamp) AS expired`,
      [input.characterId, input.titleId]
    );
    const after = titleSnapshot(rows[0]);
    await this.insertCharacterTitleLog(client, {
      ...input,
      action: "unequip",
      before,
      after
    });

    return {
      action: "unequip",
      status: "unequipped",
      changed: true,
      title: toCharacterTitle(rows[0]),
      before,
      after
    };
  }

  async setCharacterDisciplineForAdmin({
    characterId,
    disciplineId,
    points,
    tier,
    active,
    operatorType = "admin",
    operatorId,
    sourceType = "gm",
    sourceId = "admin-api-character-disciplines",
    reason = null
  } = {}) {
    return this.withGameTransaction(async (client) => {
      const character = await this.lockActiveCharacter(client, characterId);
      if (!character) {
        throw createAdminStoreError("CHARACTER_NOT_FOUND", "Character not found");
      }

      const existingResult = await client.query(
        `SELECT character_id,
                discipline_id,
                points,
                tier,
                active,
                learned_at,
                updated_at
         FROM character_disciplines
         WHERE character_id = $1 AND discipline_id = $2
         FOR UPDATE`,
        [characterId, disciplineId]
      );
      const beforeRow = existingResult.rows[0] || null;
      const before = disciplineSnapshot(beforeRow);
      const input = { disciplineId, points, tier, active };
      const action = disciplineActionForUpsert(beforeRow, input);

      let afterRow = beforeRow;
      const changed = !rowsEqualDiscipline(beforeRow, input);
      if (changed) {
        const { rows } = await client.query(
          `INSERT INTO character_disciplines (
             character_id,
             discipline_id,
             points,
             tier,
             active,
             learned_at,
             updated_at
           ) VALUES ($1, $2, $3, $4, $5, current_timestamp, current_timestamp)
           ON CONFLICT (character_id, discipline_id)
           DO UPDATE SET
             points = EXCLUDED.points,
             tier = EXCLUDED.tier,
             active = EXCLUDED.active,
             updated_at = current_timestamp
           RETURNING character_id,
                     discipline_id,
                     points,
                     tier,
                     active,
                     learned_at,
                     updated_at`,
          [characterId, disciplineId, points, tier, active]
        );
        afterRow = rows[0];
      }

      const after = disciplineSnapshot(afterRow);
      await client.query(
        `INSERT INTO character_discipline_logs (
           character_id,
           discipline_id,
           action,
           source_type,
           source_id,
           operator_type,
           operator_id,
           before_json,
           after_json,
           reason,
           created_at
         ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8::jsonb, $9::jsonb, $10, current_timestamp)`,
        [
          characterId,
          disciplineId,
          action,
          sourceType,
          sourceId || null,
          operatorType,
          operatorId || null,
          before ? toRequiredJsonb(before) : null,
          after ? toRequiredJsonb(after) : null,
          reason
        ]
      );

      return {
        action,
        status: changed ? "updated" : "unchanged",
        changed,
        discipline: toCharacterDiscipline(afterRow),
        before,
        after
      };
    });
  }

  async runCharacterUnlockCheckForAdmin({
    characterId,
    titleDefinitions = {},
    operatorType = "admin",
    operatorId,
    sourceType = "gm",
    sourceId = "admin-api-unlock-check",
    reason = null
  } = {}) {
    const character = await this.findCharacterById(characterId, { includeDeleted: false });
    if (!character) {
      throw createAdminStoreError("CHARACTER_NOT_FOUND", "Character not found");
    }

    const overview = await this.findCharacterTitleOverview({ characterId, logLimit: 1 });
    const ownedTitleIds = new Set(
      overview.titles
        .filter((title) => !title.expired)
        .map((title) => String(title.title_id))
    );
    const disciplineById = new Map(
      overview.disciplines.map((discipline) => [String(discipline.discipline_id), discipline])
    );
    const context = { character, disciplineById };
    const candidates = Object.values(titleDefinitions)
      .filter((definition) => definition && typeof definition === "object")
      .sort((left, right) => Number(left.sort_order ?? 0) - Number(right.sort_order ?? 0));
    const results = [];

    for (const definition of candidates) {
      const titleId = String(definition.title_id || "").trim();
      if (!titleId) {
        continue;
      }

      if (definition.limited === true) {
        results.push({
          title_id: titleId,
          status: "skipped",
          reason: "limited_title_requires_explicit_grant"
        });
        continue;
      }

      if (ownedTitleIds.has(titleId)) {
        results.push({
          title_id: titleId,
          status: "already_owned"
        });
        continue;
      }

      const evaluation = evaluateTitleUnlockRule(definition.unlock_rules, context);
      if (!evaluation.supported) {
        results.push({
          title_id: titleId,
          status: "unsupported",
          reason: evaluation.reason
        });
        continue;
      }

      if (!evaluation.eligible) {
        results.push({
          title_id: titleId,
          status: "not_eligible",
          reason: evaluation.reason
        });
        continue;
      }

      const grant = await this.applyCharacterTitleForAdmin({
        characterId,
        action: "grant",
        titleId,
        operatorType,
        operatorId,
        sourceType,
        sourceId: definition.title_type === "discipline"
          ? `discipline/${definition.source_domain_id || "unknown"}`
          : sourceId,
        reason: reason || `unlock_check:${evaluation.reason}`
      });
      ownedTitleIds.add(titleId);
      results.push({
        title_id: titleId,
        status: grant.status,
        changed: grant.changed,
        reason: evaluation.reason,
        title: grant.title
      });
    }

    return {
      characterId,
      checked: results.length,
      granted: results.filter((result) => result.changed === true).length,
      results
    };
  }

  async createCharacterForAdmin(input) {
    const normalized = normalizeCharacterCreateInput(input);
    const characterId = this.generateCharacterId();

    if (!CHARACTER_ID_PATTERN.test(characterId)) {
      throw createAdminStoreError("CHARACTER_ID_GENERATION_FAILED", "generated characterId has invalid format", {
        generatedCharacterId: characterId
      });
    }

    try {
      const { rows } = await this.gamePool.query(
        `INSERT INTO characters (
           character_id,
           account_player_id,
           world_id,
           name,
           status,
           appearance_json,
           scene_id,
           x,
           y,
           dir_x,
           dir_y,
           affinity_earth,
           affinity_fire,
           affinity_water,
           affinity_wind,
           mastery_earth,
           mastery_fire,
           mastery_water,
           mastery_wind
         ) VALUES (
           $1, $2, $3, $4, $5, $6::jsonb, $7, $8, $9, $10,
           $11, $12, $13, $14, $15, $16, $17, $18, $19
         )
         RETURNING ${characterSelectColumns()}`,
        [
          characterId,
          normalized.accountPlayerId,
          normalized.worldId,
          normalized.name,
          normalized.status,
          toRequiredJsonb(normalized.appearance),
          normalized.position.sceneId,
          normalized.position.x,
          normalized.position.y,
          normalized.position.dirX,
          normalized.position.dirY,
          normalized.affinity.earth,
          normalized.affinity.fire,
          normalized.affinity.water,
          normalized.affinity.wind,
          normalized.mastery.earth,
          normalized.mastery.fire,
          normalized.mastery.water,
          normalized.mastery.wind
        ]
      );

      return toCharacter(rows[0]);
    } catch (error) {
      if (error?.code === UNIQUE_VIOLATION) {
        throw createAdminStoreError("CHARACTER_ID_EXISTS", "characterId already exists");
      }
      throw error;
    }
  }

  async restoreCharacterForAdmin(characterId) {
    const { rows } = await this.gamePool.query(
      `UPDATE characters
       SET status = 'active',
           deleted_at = NULL
       WHERE character_id = $1
         AND deleted_at IS NOT NULL
         AND status = 'deleted'
       RETURNING ${characterSelectColumns()}`,
      [characterId]
    );

    return rows.length > 0 ? toCharacter(rows[0]) : null;
  }

  generateCharacterId() {
    if (typeof this.characterIdGenerator === "function") {
      return this.characterIdGenerator();
    }

    return this.characterIdGenerator.generateString("chr");
  }

  async findCharacterTitleOverview({ characterId, logLimit = 20 } = {}) {
    const [titleResult, disciplineResult, logResult] = await Promise.all([
      this.gamePool.query(
        `SELECT
           ct.character_id,
           ct.title_id,
           ct.source_type,
           ct.source_id,
           ct.is_equipped,
           ct.unlocked_at,
           ct.expires_at,
           ct.created_at,
           ct.updated_at,
           (ct.expires_at IS NOT NULL AND ct.expires_at <= current_timestamp) AS expired,
           latest_log.action AS latest_action,
           latest_log.operator_type AS latest_operator_type,
           latest_log.operator_id AS latest_operator_id,
           latest_log.reason AS latest_reason,
           latest_log.created_at AS latest_created_at
         FROM character_titles ct
         LEFT JOIN LATERAL (
           SELECT action, operator_type, operator_id, reason, created_at
           FROM character_title_logs ctl
           WHERE ctl.character_id = ct.character_id
             AND ctl.title_id = ct.title_id
           ORDER BY ctl.created_at DESC, ctl.id DESC
           LIMIT 1
         ) latest_log ON true
         WHERE ct.character_id = $1
         ORDER BY ct.is_equipped DESC, expired ASC, ct.unlocked_at DESC, ct.title_id ASC`,
        [characterId]
      ),
      this.gamePool.query(
        `SELECT discipline_id, points, tier, active, learned_at, updated_at
         FROM character_disciplines
         WHERE character_id = $1
         ORDER BY active DESC, updated_at DESC, discipline_id ASC`,
        [characterId]
      ),
      this.gamePool.query(
        `SELECT id, character_id, title_id, action, source_type, source_id, operator_type, operator_id,
                before_json, after_json, reason, created_at
         FROM character_title_logs
         WHERE character_id = $1
         ORDER BY created_at DESC, id DESC
         LIMIT $2`,
        [characterId, logLimit]
      )
    ]);

    const titles = titleResult.rows.map(toCharacterTitle);
    return {
      titles,
      equippedTitle: titles.find((title) => title.is_equipped && !title.expired) || null,
      disciplines: disciplineResult.rows.map(toCharacterDiscipline),
      titleLogs: logResult.rows.map(toCharacterTitleLog)
    };
  }

  // ============================================================
  // Global ID metadata queries
  // ============================================================

  async findIdOrigin(originId) {
    const { rows } = await this.pool.query(
      `SELECT origin_id, origin_key, created_at, retired_at
       FROM id_origins
       WHERE origin_id = $1
       LIMIT 1`,
      [originId]
    );
    return rows.length > 0 ? toIdOrigin(rows[0]) : null;
  }

  async findWorldMembershipAt({ originId, createdAt }) {
    const { rows } = await this.pool.query(
      `SELECT
         wom.world_id,
         w.world_key,
         wom.origin_id,
         io.origin_key,
         w.active_origin_id,
         active_origin.origin_key AS active_origin_key,
         wom.joined_at,
         wom.left_at
       FROM world_origin_memberships wom
       LEFT JOIN worlds w ON w.world_id = wom.world_id
       LEFT JOIN id_origins io ON io.origin_id = wom.origin_id
       LEFT JOIN id_origins active_origin ON active_origin.origin_id = w.active_origin_id
       WHERE wom.origin_id = $1
         AND wom.joined_at <= $2
         AND (wom.left_at IS NULL OR wom.left_at > $2)
       ORDER BY wom.joined_at DESC
       LIMIT 1`,
      [originId, createdAt]
    );
    return rows.length > 0 ? toWorldMembership(rows[0]) : null;
  }

  async findCurrentWorldMembership(originId) {
    const { rows } = await this.pool.query(
      `SELECT
         wom.world_id,
         w.world_key,
         wom.origin_id,
         io.origin_key,
         w.active_origin_id,
         active_origin.origin_key AS active_origin_key,
         wom.joined_at,
         wom.left_at
       FROM world_origin_memberships wom
       LEFT JOIN worlds w ON w.world_id = wom.world_id
       LEFT JOIN id_origins io ON io.origin_id = wom.origin_id
       LEFT JOIN id_origins active_origin ON active_origin.origin_id = w.active_origin_id
       WHERE wom.origin_id = $1
         AND wom.left_at IS NULL
       ORDER BY wom.joined_at DESC
       LIMIT 1`,
      [originId]
    );
    return rows.length > 0 ? toWorldMembership(rows[0]) : null;
  }

  async findMergeContext({ originId, createdAt, worldId = null }) {
    const params = [originId, createdAt];
    let query = `SELECT
         wme.merge_id,
         wme.target_world_id,
         target_world.world_key AS target_world_key,
         wme.active_origin_id,
         active_origin.origin_key AS active_origin_key,
         wme.source_world_ids,
         (
           SELECT array_agg(source_world.world_key ORDER BY source_world_ref.ordinality)
           FROM unnest(wme.source_world_ids) WITH ORDINALITY AS source_world_ref(world_id, ordinality)
           LEFT JOIN worlds source_world ON source_world.world_id = source_world_ref.world_id
         ) AS source_world_keys,
         wme.source_origin_ids,
         (
           SELECT array_agg(source_origin.origin_key ORDER BY source_origin_ref.ordinality)
           FROM unnest(wme.source_origin_ids) WITH ORDINALITY AS source_origin_ref(origin_id, ordinality)
           LEFT JOIN id_origins source_origin ON source_origin.origin_id = source_origin_ref.origin_id
         ) AS source_origin_keys,
         wme.merged_at,
         wme.operator,
         wme.details_json
       FROM world_merge_events wme
       LEFT JOIN worlds target_world ON target_world.world_id = wme.target_world_id
       LEFT JOIN id_origins active_origin ON active_origin.origin_id = wme.active_origin_id
       WHERE $1 = ANY(wme.source_origin_ids)
         AND wme.merged_at >= $2`;

    if (worldId !== null && worldId !== undefined) {
      params.push(worldId);
      const placeholder = nextParam(params);
      query += ` AND (wme.target_world_id = ${placeholder} OR ${placeholder} = ANY(wme.source_world_ids))`;
    }

    query += ` ORDER BY wme.merged_at ASC LIMIT 1`;

    const { rows } = await this.pool.query(query, params);
    return rows.length > 0 ? toWorldMergeEvent(rows[0]) : null;
  }

  async findIdOrigins({ originId, originKey, limit = 50, offset = 0 } = {}) {
    let query = `SELECT origin_id, origin_key, created_at, retired_at
       FROM id_origins
       WHERE 1=1`;
    const params = [];

    if (originId !== undefined && originId !== null) {
      params.push(originId);
      query += ` AND origin_id = ${nextParam(params)}`;
    }

    if (originKey) {
      params.push(`%${originKey}%`);
      query += ` AND origin_key LIKE ${nextParam(params)}`;
    }

    params.push(limit);
    query += ` ORDER BY origin_id ASC LIMIT ${nextParam(params)}`;
    params.push(offset);
    query += ` OFFSET ${nextParam(params)}`;

    const { rows } = await this.pool.query(query, params);
    return rows.map(toIdOrigin);
  }

  async countIdOrigins({ originId, originKey } = {}) {
    let query = `SELECT COUNT(*) as total FROM id_origins WHERE 1=1`;
    const params = [];

    if (originId !== undefined && originId !== null) {
      params.push(originId);
      query += ` AND origin_id = ${nextParam(params)}`;
    }

    if (originKey) {
      params.push(`%${originKey}%`);
      query += ` AND origin_key LIKE ${nextParam(params)}`;
    }

    const { rows } = await this.pool.query(query, params);
    return readTotal(rows);
  }

  async findWorlds({ worldId, worldKey, originId, limit = 50, offset = 0 } = {}) {
    let query = `SELECT
         w.world_id,
         w.world_key,
         w.active_origin_id,
         active_origin.origin_key AS active_origin_key,
         COALESCE(
           jsonb_agg(
             DISTINCT jsonb_build_object(
               'origin_id', wom.origin_id,
               'origin_key', member_origin.origin_key
             )
           ) FILTER (WHERE wom.origin_id IS NOT NULL),
           '[]'::jsonb
         ) AS origins,
         w.created_at,
         w.retired_at
       FROM worlds w
       LEFT JOIN id_origins active_origin ON active_origin.origin_id = w.active_origin_id
       LEFT JOIN world_origin_memberships wom ON wom.world_id = w.world_id
       LEFT JOIN id_origins member_origin ON member_origin.origin_id = wom.origin_id
       WHERE 1=1`;
    const params = [];

    if (worldId !== undefined && worldId !== null) {
      params.push(worldId);
      query += ` AND w.world_id = ${nextParam(params)}`;
    }

    if (worldKey) {
      params.push(`%${worldKey}%`);
      query += ` AND w.world_key LIKE ${nextParam(params)}`;
    }

    if (originId !== undefined && originId !== null) {
      params.push(originId);
      const placeholder = nextParam(params);
      query += ` AND (w.active_origin_id = ${placeholder} OR EXISTS (
        SELECT 1 FROM world_origin_memberships filter_wom
        WHERE filter_wom.world_id = w.world_id AND filter_wom.origin_id = ${placeholder}
      ))`;
    }

    query += ` GROUP BY w.world_id, w.world_key, w.active_origin_id, active_origin.origin_key, w.created_at, w.retired_at`;
    params.push(limit);
    query += ` ORDER BY w.world_id ASC LIMIT ${nextParam(params)}`;
    params.push(offset);
    query += ` OFFSET ${nextParam(params)}`;

    const { rows } = await this.pool.query(query, params);
    return rows.map(toWorld);
  }

  async countWorlds({ worldId, worldKey, originId } = {}) {
    let query = `SELECT COUNT(*) as total FROM worlds w WHERE 1=1`;
    const params = [];

    if (worldId !== undefined && worldId !== null) {
      params.push(worldId);
      query += ` AND w.world_id = ${nextParam(params)}`;
    }

    if (worldKey) {
      params.push(`%${worldKey}%`);
      query += ` AND w.world_key LIKE ${nextParam(params)}`;
    }

    if (originId !== undefined && originId !== null) {
      params.push(originId);
      const placeholder = nextParam(params);
      query += ` AND (w.active_origin_id = ${placeholder} OR EXISTS (
        SELECT 1 FROM world_origin_memberships filter_wom
        WHERE filter_wom.world_id = w.world_id AND filter_wom.origin_id = ${placeholder}
      ))`;
    }

    const { rows } = await this.pool.query(query, params);
    return readTotal(rows);
  }

  async findWorldMergeEvents({ worldId, originId, limit = 50, offset = 0 } = {}) {
    let query = `SELECT
         wme.merge_id,
         wme.target_world_id,
         target_world.world_key AS target_world_key,
         wme.active_origin_id,
         active_origin.origin_key AS active_origin_key,
         wme.source_world_ids,
         (
           SELECT array_agg(source_world.world_key ORDER BY source_world_ref.ordinality)
           FROM unnest(wme.source_world_ids) WITH ORDINALITY AS source_world_ref(world_id, ordinality)
           LEFT JOIN worlds source_world ON source_world.world_id = source_world_ref.world_id
         ) AS source_world_keys,
         wme.source_origin_ids,
         (
           SELECT array_agg(source_origin.origin_key ORDER BY source_origin_ref.ordinality)
           FROM unnest(wme.source_origin_ids) WITH ORDINALITY AS source_origin_ref(origin_id, ordinality)
           LEFT JOIN id_origins source_origin ON source_origin.origin_id = source_origin_ref.origin_id
         ) AS source_origin_keys,
         wme.merged_at,
         wme.operator,
         wme.details_json
       FROM world_merge_events wme
       LEFT JOIN worlds target_world ON target_world.world_id = wme.target_world_id
       LEFT JOIN id_origins active_origin ON active_origin.origin_id = wme.active_origin_id
       WHERE 1=1`;
    const params = [];

    if (worldId !== undefined && worldId !== null) {
      params.push(worldId);
      const placeholder = nextParam(params);
      query += ` AND (wme.target_world_id = ${placeholder} OR ${placeholder} = ANY(wme.source_world_ids))`;
    }

    if (originId !== undefined && originId !== null) {
      params.push(originId);
      const placeholder = nextParam(params);
      query += ` AND (wme.active_origin_id = ${placeholder} OR ${placeholder} = ANY(wme.source_origin_ids))`;
    }

    params.push(limit);
    query += ` ORDER BY wme.merged_at DESC LIMIT ${nextParam(params)}`;
    params.push(offset);
    query += ` OFFSET ${nextParam(params)}`;

    const { rows } = await this.pool.query(query, params);
    return rows.map(toWorldMergeEvent);
  }

  async countWorldMergeEvents({ worldId, originId } = {}) {
    let query = `SELECT COUNT(*) as total FROM world_merge_events wme WHERE 1=1`;
    const params = [];

    if (worldId !== undefined && worldId !== null) {
      params.push(worldId);
      const placeholder = nextParam(params);
      query += ` AND (wme.target_world_id = ${placeholder} OR ${placeholder} = ANY(wme.source_world_ids))`;
    }

    if (originId !== undefined && originId !== null) {
      params.push(originId);
      const placeholder = nextParam(params);
      query += ` AND (wme.active_origin_id = ${placeholder} OR ${placeholder} = ANY(wme.source_origin_ids))`;
    }

    const { rows } = await this.pool.query(query, params);
    return readTotal(rows);
  }

  // ============================================================
  // Maintenance Mode
  // ============================================================

  async setMaintenanceMode(enabled, { reason = null, updatedAt = null, updatedBy = null } = {}) {
    if (!this.redis) {
      throw new Error("MAINTENANCE_REDIS_UNAVAILABLE");
    }

    const state = normalizeMaintenanceState({
      enabled,
      reason,
      updatedAt: updatedAt || new Date().toISOString(),
      updatedBy
    });
    await this.redis.set(this.maintenanceStateKey(), JSON.stringify(state));
    return state;
  }

  async getMaintenanceStatus() {
    if (this.redis) {
      const raw = await this.redis.get(this.maintenanceStateKey());
      const state = parseMaintenanceState(raw);
      if (state) {
        return state;
      }
    }

    const { rows } = await this.pool.query(
      `SELECT action, admin_username, details_json, created_at
       FROM admin_audit_logs
       WHERE action IN ('maintenance_enabled', 'maintenance_disabled')
       ORDER BY created_at DESC
       LIMIT 1`
    );
    if (rows.length === 0) {
      return normalizeMaintenanceState();
    }
    const latest = rows[0];
    let details = {};
    try {
      details = typeof latest.details_json === "string"
        ? JSON.parse(latest.details_json)
        : latest.details_json || {};
    } catch {
      details = {};
    }

    return normalizeMaintenanceState({
      enabled: latest.action === "maintenance_enabled",
      reason: details.reason || null,
      updatedAt: toIsoString(latest.created_at),
      updatedBy: latest.admin_username || null
    });
  }
}

export {
  MAINTENANCE_STATE_KEY,
  hashPassword,
  maintenanceStateKey,
  normalizeMaintenanceState,
  parseMaintenanceState,
  verifyPassword,
  hashToken
};
