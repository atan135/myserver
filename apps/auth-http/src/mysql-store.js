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

      await this.touchPlayerLastLogin(existing.player_id);

      return {
        playerId: existing.player_id,
        guestId: existing.guest_id
      };
    }

    const playerId = `player-${crypto.randomUUID()}`;

    try {
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
    } catch (err) {
      if (err.code === "ER_DUP_ENTRY" || err.code === "ER_NO_REFERENCED_ROW_2") {
        // Concurrent insert: another request already created this guest account
        const [rows] = await this.pool.execute(
          `SELECT player_id, guest_id FROM player_accounts WHERE guest_id = ? LIMIT 1`,
          [guestId]
        );
        if (rows.length > 0) {
          await this.touchPlayerLastLogin(rows[0].player_id);
          return { playerId: rows[0].player_id, guestId: rows[0].guest_id };
        }
      }
      throw err;
    }

    return {
      playerId,
      guestId
    };
  }

  async findPasswordAccountByLoginName(loginName) {
    if (!this.enabled) {
      return null;
    }

    const [rows] = await this.pool.execute(
      `SELECT player_id,
              login_name,
              display_name,
              account_type,
              status,
              password_algo,
              password_salt,
              password_hash
       FROM player_accounts
       WHERE login_name = ?
         AND account_type = 'password'
       LIMIT 1`,
      [loginName]
    );

    if (rows.length === 0) {
      return null;
    }

    const account = rows[0];
    return {
      playerId: account.player_id,
      loginName: account.login_name,
      displayName: account.display_name,
      accountType: account.account_type,
      status: account.status,
      passwordAlgo: account.password_algo,
      passwordSalt: account.password_salt,
      passwordHash: account.password_hash
    };
  }

  async touchPlayerLastLogin(playerId) {
    if (!this.enabled) {
      return;
    }

    await this.pool.execute(
      `UPDATE player_accounts
       SET last_login_at = CURRENT_TIMESTAMP(3)
       WHERE player_id = ?`,
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
      throw new Error("MySQL auth store is disabled");
    }

    const [rows] = await this.pool.execute(
      `SELECT player_id
       FROM player_accounts
       WHERE login_name = ?
       LIMIT 1`,
      [loginName]
    );

    if (rows.length > 0) {
      const existing = rows[0];

      await this.pool.execute(
        `UPDATE player_accounts
         SET display_name = ?,
             account_type = 'password',
             status = ?,
             password_algo = ?,
             password_salt = ?,
             password_hash = ?
         WHERE player_id = ?`,
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

    const playerId = `player-${crypto.randomUUID()}`;

    await this.pool.execute(
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
       ) VALUES (?, ?, ?, 'password', ?, ?, ?, ?, CURRENT_TIMESTAMP(3), CURRENT_TIMESTAMP(3))`,
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

    await this.pool.execute(
      `INSERT INTO security_audit_logs (
         event_type,
         target_type,
         target_value,
         client_ip,
         severity,
         details_json,
         created_at
       ) VALUES (?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP(3))`,
      [
        eventType,
        targetType,
        targetValue,
        clientIp,
        severity,
        details ? JSON.stringify(details) : null
      ]
    );
  }
}
