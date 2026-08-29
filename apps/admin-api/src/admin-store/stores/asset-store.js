import { readTotal, assetLedgerFilters, assetLedgerQuery, toAssetLedgerEntry } from "../mappers/assets.js";
import { nextParam } from "../formatters.js";

export class AssetStore {
  constructor(gamePool) {
    this.gamePool = gamePool;
  }

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
}
