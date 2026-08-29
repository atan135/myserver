import { toIdOrigin, toWorld, toWorldMembership, toWorldMergeEvent } from "../mappers/world.js";
import { readTotal } from "../mappers/assets.js";
import { nextParam } from "../formatters.js";

export class WorldStore {
  constructor(pool) {
    this.pool = pool;
  }

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
}




