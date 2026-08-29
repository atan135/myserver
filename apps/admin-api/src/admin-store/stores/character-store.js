import { createAdminStoreError } from "../errors.js";
import { toRequiredJsonb } from "../formatters.js";
import { readTotal } from "../mappers/assets.js";
import {
  characterSelectColumns,
  toCharacter,
  characterElementSnapshot,
  elementsDelta,
  isZeroElementsDelta,
  titleSnapshot,
  disciplineSnapshot,
  titleGrantStatus,
  disciplineActionForUpsert,
  rowsEqualDiscipline,
  evaluateTitleUnlockRule,
  toCharacterTitle,
  toCharacterDiscipline,
  toCharacterElementLog,
  toCharacterTitleLog,
  toCharacterDisciplineLog
} from "../mappers/character.js";

export class CharacterStore {
  constructor(pool, gamePool = null) {
    this.pool = pool;
    this.gamePool = gamePool || pool;
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
}
