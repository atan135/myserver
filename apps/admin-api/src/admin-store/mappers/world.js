import { toIsoString, toNumericId } from "../formatters.js";

export function toIdOrigin(row) {
  return {
    origin_id: toNumericId(row.origin_id),
    origin_key: row.origin_key,
    created_at: toIsoString(row.created_at),
    retired_at: toIsoString(row.retired_at)
  };
}

export function toWorld(row) {
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

export function toWorldMembership(row) {
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

export function toWorldMergeEvent(row) {
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
