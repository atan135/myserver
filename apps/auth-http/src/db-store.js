import crypto from "node:crypto";

import { generatePlayerId } from "./global-id.js";

const UNIQUE_VIOLATION = "23505";

function sha256Hex(value) {
  return crypto.createHash("sha256").update(value).digest("hex");
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

function toJsonb(value) {
  return value ? JSON.stringify(value) : null;
}

function toAuthAccount(row) {
  return {
    playerId: row.player_id,
    guestId: row.guest_id || null,
    loginName: row.login_name || null,
    displayName: row.display_name || null,
    accountType: row.account_type,
    status: row.status,
    banExpiresAt: toIsoString(row.ban_expires_at),
    passwordAlgo: row.password_algo,
    passwordSalt: row.password_salt,
    passwordHash: row.password_hash
  };
}

export class DbAuthStore {
  constructor(pool) {
    this.pool = pool;
  }

  get enabled() {
    return Boolean(this.pool);
  }

  async findOrCreateGuestPlayer(guestId) {
    if (!this.enabled) {
      return {
        playerId: generatePlayerId(),
        guestId
      };
    }

    const { rows } = await this.pool.query(
      `SELECT player_id,
              guest_id,
              login_name,
              display_name,
              account_type,
              status,
              ban_expires_at,
              password_algo,
              password_salt,
              password_hash
       FROM player_accounts
       WHERE guest_id = $1
       LIMIT 1`,
      [guestId]
    );

    if (rows.length > 0) {
      return toAuthAccount(rows[0]);
    }

    const playerId = generatePlayerId();

    try {
      await this.pool.query(
        `INSERT INTO player_accounts (
           player_id,
           guest_id,
           account_type,
           status,
           created_at,
           last_login_at
         ) VALUES ($1, $2, 'guest', 'active', current_timestamp, current_timestamp)`,
        [playerId, guestId]
      );
    } catch (err) {
      if (err.code === UNIQUE_VIOLATION) {
        const { rows: existingRows } = await this.pool.query(
          `SELECT player_id,
                  guest_id,
                  login_name,
                  display_name,
                  account_type,
                  status,
                  ban_expires_at,
                  password_algo,
                  password_salt,
                  password_hash
           FROM player_accounts
           WHERE guest_id = $1
           LIMIT 1`,
          [guestId]
        );
        if (existingRows.length > 0) {
          return toAuthAccount(existingRows[0]);
        }
      }
      throw err;
    }

    return {
      playerId,
      guestId,
      status: "active",
      banExpiresAt: null
    };
  }

  async findPasswordAccountByLoginName(loginName) {
    if (!this.enabled) {
      return null;
    }

    const { rows } = await this.pool.query(
      `SELECT player_id,
              login_name,
              display_name,
              account_type,
              status,
              ban_expires_at,
              password_algo,
              password_salt,
              password_hash
       FROM player_accounts
       WHERE login_name = $1
         AND account_type = 'password'
       LIMIT 1`,
      [loginName]
    );

    if (rows.length === 0) {
      return null;
    }

    return toAuthAccount(rows[0]);
  }

  async findPlayerAuthStateByPlayerId(playerId) {
    if (!this.enabled) {
      return null;
    }

    const { rows } = await this.pool.query(
      `SELECT player_id,
              guest_id,
              login_name,
              display_name,
              account_type,
              status,
              ban_expires_at,
              password_algo,
              password_salt,
              password_hash
       FROM player_accounts
       WHERE player_id = $1
       LIMIT 1`,
      [playerId]
    );

    return rows.length > 0 ? toAuthAccount(rows[0]) : null;
  }

  async restoreExpiredBan(playerId) {
    if (!this.enabled) {
      return false;
    }

    const result = await this.pool.query(
      `UPDATE player_accounts
       SET status = 'active',
           ban_expires_at = NULL
       WHERE player_id = $1
         AND status = 'banned'
         AND ban_expires_at IS NOT NULL
         AND ban_expires_at <= current_timestamp`,
      [playerId]
    );
    return result.rowCount > 0;
  }

  async touchPlayerLastLogin(playerId) {
    if (!this.enabled) {
      return;
    }

    await this.pool.query(
      `UPDATE player_accounts
       SET last_login_at = current_timestamp
       WHERE player_id = $1`,
      [playerId]
    );
  }

  async upsertPasswordAccount({
    loginName,
    displayName = null,
    status = "active",
    passwordAlgo = "scrypt",
    passwordSalt,
    passwordHash
  }) {
    if (!this.enabled) {
      throw new Error("database auth store is disabled");
    }

    const { rows } = await this.pool.query(
      `SELECT player_id
       FROM player_accounts
       WHERE login_name = $1
       LIMIT 1`,
      [loginName]
    );

    if (rows.length > 0) {
      const existing = rows[0];

      await this.pool.query(
        `UPDATE player_accounts
         SET display_name = $1,
             account_type = 'password',
             status = $2,
             password_algo = $3,
             password_salt = $4,
             password_hash = $5
         WHERE player_id = $6`,
        [
          displayName,
          status,
          passwordAlgo,
          passwordSalt,
          passwordHash,
          existing.player_id
        ]
      );

      return {
        created: false,
        playerId: existing.player_id,
        loginName,
        displayName
      };
    }

    const playerId = generatePlayerId();

    await this.pool.query(
      `INSERT INTO player_accounts (
         player_id,
         login_name,
         display_name,
         account_type,
         status,
         password_algo,
         password_salt,
         password_hash,
         created_at,
         last_login_at
       ) VALUES ($1, $2, $3, 'password', $4, $5, $6, $7, current_timestamp, current_timestamp)`,
      [
        playerId,
        loginName,
        displayName,
        status,
        passwordAlgo,
        passwordSalt,
        passwordHash
      ]
    );

    return {
      created: true,
      playerId,
      loginName,
      displayName
    };
  }

  async updatePassword(playerId, { passwordSalt, passwordHash, passwordAlgo = "scrypt" }) {
    if (!this.enabled) {
      throw new Error("database auth store is disabled");
    }

    await this.pool.query(
      `UPDATE player_accounts
       SET password_algo = $1,
           password_salt = $2,
           password_hash = $3
       WHERE player_id = $4
         AND account_type = 'password'`,
      [passwordAlgo, passwordSalt, passwordHash, playerId]
    );
  }

  async findPasswordAccountByPlayerId(playerId) {
    if (!this.enabled) {
      return null;
    }

    const { rows } = await this.pool.query(
      `SELECT player_id,
              login_name,
              display_name,
              account_type,
              status,
              ban_expires_at,
              password_algo,
              password_salt,
              password_hash
       FROM player_accounts
       WHERE player_id = $1
         AND account_type = 'password'
       LIMIT 1`,
      [playerId]
    );

    if (rows.length === 0) {
      return null;
    }

    return toAuthAccount(rows[0]);
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

    await this.pool.query(
      `INSERT INTO auth_audit_logs (
         player_id,
         guest_id,
         event_type,
         access_token_hash,
         ticket_hash,
         client_ip,
         details_json,
         created_at
       ) VALUES ($1, $2, $3, $4, $5, $6, $7::jsonb, current_timestamp)`,
      [
        playerId,
        guestId,
        eventType,
        accessToken ? sha256Hex(accessToken) : null,
        ticket ? sha256Hex(ticket) : null,
        clientIp,
        toJsonb(details)
      ]
    );
  }

  async appendSecurityAudit({
    eventType,
    targetType = null,
    targetValue = null,
    clientIp = null,
    severity = "warning",
    details = null
  }) {
    if (!this.enabled) {
      return;
    }

    await this.pool.query(
      `INSERT INTO security_audit_logs (
         event_type,
         target_type,
         target_value,
         client_ip,
         severity,
         details_json,
         created_at
       ) VALUES ($1, $2, $3, $4, $5, $6::jsonb, current_timestamp)`,
      [
        eventType,
        targetType,
        targetValue,
        clientIp,
        severity,
        toJsonb(details)
      ]
    );
  }
}
