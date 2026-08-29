import { ELEMENT_KEYS, DISCIPLINE_TIER_ORDER } from "../constants.js";
import { toIsoString, toNumericId, normalizeJson } from "../formatters.js";

export function characterSelectColumns() {
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

export function toCharacter(row) {
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

export function characterElementSnapshot(character) {
  return {
    character_id: character.character_id,
    affinity: { ...character.attributes.affinity },
    mastery: { ...character.attributes.mastery }
  };
}

export function elementsDelta(before, after) {
  return ELEMENT_KEYS.reduce((delta, key) => {
    delta[key] = Number(after[key]) - Number(before[key]);
    return delta;
  }, {});
}

export function isZeroElementsDelta(delta) {
  return ELEMENT_KEYS.every((key) => Number(delta[key]) === 0);
}

export function titleSnapshot(rowOrTitle) {
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

export function disciplineSnapshot(rowOrDiscipline) {
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

export function titleGrantStatus(existing) {
  if (!existing) {
    return "granted";
  }
  return existing.expired ? "renewed" : "already_owned";
}

export function disciplineActionForUpsert(before, input) {
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

export function rowsEqualDiscipline(before, input) {
  return before &&
    before.discipline_id === input.disciplineId &&
    Number(before.points) === Number(input.points) &&
    before.tier === input.tier &&
    before.active === input.active;
}

export function tierAtLeast(current, required) {
  const currentIndex = DISCIPLINE_TIER_ORDER.indexOf(current);
  const requiredIndex = DISCIPLINE_TIER_ORDER.indexOf(required);
  return currentIndex >= 0 && requiredIndex >= 0 && currentIndex >= requiredIndex;
}

export function evaluateTitleUnlockRule(rule, context) {
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

export function toCharacterTitle(row) {
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

export function toCharacterDiscipline(row) {
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

export function toCharacterElementLog(row) {
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

export function toCharacterTitleLog(row) {
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

export function toCharacterDisciplineLog(row) {
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
