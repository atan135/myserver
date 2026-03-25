import crypto from "node:crypto";

function sha256Hex(value) {
  return crypto.createHash("sha256").update(value).digest("hex");
}

export class MySqlAuthStore {
  constructor(pool) {
    this.pool = pool;
  }

  get enabled() {
    return Boolean(this.pool);
  }

  async findOrCreateGuestPlayer(guestId) {
    if (!this.enabled) {
      return {
        playerId: `player-${crypto.randomUUID()}`,
        guestId
      };
    }

    const [rows] = await this.pool.execute(
      `SELECT player_id, guest_id
       FROM player_accounts
       WHERE guest_id = ?
       LIMIT 1`,
      [guestId]
    );

    if (rows.length > 0) {
      const existing = rows[0];

      await this.pool.execute(
        `UPDATE player_accounts
         SET last_login_at = CURRENT_TIMESTAMP(3)
         WHERE player_id = ?`,
        [existing.player_id]
      );

      return {
        playerId: existing.player_id,
        guestId: existing.guest_id
      };
    }

    const playerId = `player-${crypto.randomUUID()}`;

    await this.pool.execute(
      `INSERT INTO player_accounts (
         player_id,
         guest_id,
         account_type,
         status,
         created_at,
         last_login_at
       ) VALUES (?, ?, 'guest', 'active', CURRENT_TIMESTAMP(3), CURRENT_TIMESTAMP(3))`,
      [playerId, guestId]
    );

    return {
      playerId,
      guestId
    };
  }

  async appendAuthAudit({
    playerId = null,
    guestId = null,
    eventType,
    accessToken = null,
    ticket = null,
    clientIp = null,
    details = null
  }) {
    if (!this.enabled) {
      return;
    }

    await this.pool.execute(
      `INSERT INTO auth_audit_logs (
         player_id,
         guest_id,
         event_type,
         access_token_hash,
         ticket_hash,
         client_ip,
         details_json,
         created_at
       ) VALUES (?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP(3))`,
      [
        playerId,
        guestId,
        eventType,
        accessToken ? sha256Hex(accessToken) : null,
        ticket ? sha256Hex(ticket) : null,
        clientIp,
        details ? JSON.stringify(details) : null
      ]
    );
  }
}
