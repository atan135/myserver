import { toPlayer } from "../mappers/admin.js";
import { readTotal } from "../mappers/assets.js";
import { nextParam } from "../formatters.js";

export class PlayerStore {
  constructor(pool) {
    this.pool = pool;
  }

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
}
