import crypto from "node:crypto";
import bcrypt from "bcrypt";

const SALT_ROUNDS = 10;

function hashPassword(password) {
  return bcrypt.hashSync(password, SALT_ROUNDS);
}

function verifyPassword(password, hash) {
  return bcrypt.compareSync(password, hash);
}

function hashToken(token) {
  return crypto.createHash("sha256").update(token).digest("hex");
}

export class AdminStore {
  constructor(pool) {
    this.pool = pool;
  }

  async findAdminByUsername(username) {
    const [rows] = await this.pool.execute(
      `SELECT id, username, display_name, password_algo, password_salt, password_hash, role, status
       FROM admin_accounts
       WHERE username = ?
       LIMIT 1`,
      [username]
    );

    if (rows.length === 0) return null;

    const row = rows[0];
    return {
      id: row.id,
      username: row.username,
      displayName: row.display_name,
      role: row.role,
      status: row.status,
      passwordHash: row.password_hash
    };
  }

  async verifyPassword(password, hash) {
    return verifyPassword(password, hash);
  }

  async createAdmin({ username, displayName, password, role = "viewer" }) {
    const passwordSalt = crypto.randomBytes(16).toString("hex");
    const passwordHash = hashPassword(password);

    try {
      const [result] = await this.pool.execute(
        `INSERT INTO admin_accounts (username, display_name, password_algo, password_salt, password_hash, role, status)
         VALUES (?, ?, 'bcrypt', ?, ?, ?, 'active')`,
        [username, displayName || username, passwordSalt, passwordHash, role]
      );

      return {
        id: result.insertId,
        username,
        displayName: displayName || username,
        role
      };
    } catch (error) {
      if (error.code === "ER_DUP_ENTRY") {
        throw new Error("ADMIN_ALREADY_EXISTS");
      }
      throw error;
    }
  }

  async ensureInitialAdmin(config) {
    const existing = await this.findAdminByUsername(config.initialAdminUsername);
    if (existing) {
      return existing;
    }

    return this.createAdmin({
      username: config.initialAdminUsername,
      displayName: config.initialAdminDisplayName,
      password: config.initialAdminPassword,
      role: "admin"
    });
  }

  async updateLastLogin(adminId) {
    await this.pool.execute(
      `UPDATE admin_accounts SET last_login_at = CURRENT_TIMESTAMP(3) WHERE id = ?`,
      [adminId]
    );
  }

  async appendAuditLog({ adminId, adminUsername, action, targetType, targetValue, details, ip }) {
    await this.pool.execute(
      `INSERT INTO admin_audit_logs (admin_id, admin_username, action, target_type, target_value, details_json, ip)
       VALUES (?, ?, ?, ?, ?, ?, ?)`,
      [
        adminId,
        adminUsername,
        action,
        targetType || null,
        targetValue || null,
        details ? JSON.stringify(details) : null,
        ip || null
      ]
    );
  }

  async getSecurityLogs({ limit = 50, offset = 0, eventType, targetType, severity, clientIp } = {}) {
    let query = `SELECT * FROM security_audit_logs WHERE 1=1`;
    const params = [];

    if (eventType) {
      query += ` AND event_type = ?`;
      params.push(eventType);
    }

    if (targetType) {
      query += ` AND target_type = ?`;
      params.push(targetType);
    }

    if (severity) {
      query += ` AND severity = ?`;
      params.push(severity);
    }

    if (clientIp) {
      query += ` AND client_ip = ?`;
      params.push(clientIp);
    }

    query += ` ORDER BY created_at DESC LIMIT ? OFFSET ?`;
    params.push(limit, offset);

    const [rows] = await this.pool.execute(query, params);
    return rows;
  }

  async countSecurityLogs({ eventType, targetType, severity, clientIp } = {}) {
    let query = `SELECT COUNT(*) as total FROM security_audit_logs WHERE 1=1`;
    const params = [];

    if (eventType) {
      query += ` AND event_type = ?`;
      params.push(eventType);
    }

    if (targetType) {
      query += ` AND target_type = ?`;
      params.push(targetType);
    }

    if (severity) {
      query += ` AND severity = ?`;
      params.push(severity);
    }

    if (clientIp) {
      query += ` AND client_ip = ?`;
      params.push(clientIp);
    }

    const [rows] = await this.pool.execute(query, params);
    return rows[0].total;
  }

  async getAuditLogs({ limit = 50, offset = 0, adminId, action, targetType } = {}) {
    let query = `SELECT * FROM admin_audit_logs WHERE 1=1`;
    const params = [];

    if (adminId) {
      query += ` AND admin_id = ?`;
      params.push(adminId);
    }

    if (action) {
      query += ` AND action = ?`;
      params.push(action);
    }

    if (targetType) {
      query += ` AND target_type = ?`;
      params.push(targetType);
    }

    query += ` ORDER BY created_at DESC LIMIT ? OFFSET ?`;
    params.push(limit, offset);

    const [rows] = await this.pool.execute(query, params);
    return rows;
  }

  async countAuditLogs({ adminId, action, targetType } = {}) {
    let query = `SELECT COUNT(*) as total FROM admin_audit_logs WHERE 1=1`;
    const params = [];

    if (adminId) {
      query += ` AND admin_id = ?`;
      params.push(adminId);
    }

    if (action) {
      query += ` AND action = ?`;
      params.push(action);
    }

    if (targetType) {
      query += ` AND target_type = ?`;
      params.push(targetType);
    }

    const [rows] = await this.pool.execute(query, params);
    return rows[0].total;
  }

  // ============================================================
  // Player Management (read from player_accounts)
  // ============================================================

  async findPlayerById(playerId) {
    const [rows] = await this.pool.execute(
      `SELECT player_id, guest_id, login_name, display_name, account_type, status, created_at, last_login_at
       FROM player_accounts
       WHERE player_id = ?
       LIMIT 1`,
      [playerId]
    );
    return rows.length > 0 ? rows[0] : null;
  }

  async findPlayers({ loginName, guestId, status, limit = 50, offset = 0 } = {}) {
    let query = `SELECT player_id, guest_id, login_name, display_name, account_type, status, created_at, last_login_at
       FROM player_accounts
       WHERE 1=1`;
    const params = [];

    if (loginName) {
      query += ` AND login_name LIKE ?`;
      params.push(`%${loginName}%`);
    }

    if (guestId) {
      query += ` AND guest_id LIKE ?`;
      params.push(`%${guestId}%`);
    }

    if (status) {
      query += ` AND status = ?`;
      params.push(status);
    }

    query += ` ORDER BY last_login_at DESC LIMIT ? OFFSET ?`;
    params.push(limit, offset);

    const [rows] = await this.pool.execute(query, params);
    return rows;
  }

  async countPlayers({ loginName, guestId, status } = {}) {
    let query = `SELECT COUNT(*) as total FROM player_accounts WHERE 1=1`;
    const params = [];

    if (loginName) {
      query += ` AND login_name LIKE ?`;
      params.push(`%${loginName}%`);
    }

    if (guestId) {
      query += ` AND guest_id LIKE ?`;
      params.push(`%${guestId}%`);
    }

    if (status) {
      query += ` AND status = ?`;
      params.push(status);
    }

    const [rows] = await this.pool.execute(query, params);
    return rows[0].total;
  }

  async updatePlayerStatus(playerId, status) {
    const [result] = await this.pool.execute(
      `UPDATE player_accounts SET status = ? WHERE player_id = ?`,
      [status, playerId]
    );
    return result.affectedRows > 0;
  }

  // ============================================================
  // Maintenance Mode
  // ============================================================

  async setMaintenanceMode(enabled, reason = "") {
    await this.pool.execute(
      `INSERT INTO admin_audit_logs (admin_id, admin_username, action, target_type, target_value, details_json, ip)
       VALUES (NULL, 'system', ?, 'system', 'maintenance', ?, NULL)`,
      [enabled ? "maintenance_enabled" : "maintenance_disabled", JSON.stringify({ reason })]
    );
    return { ok: true };
  }

  async getMaintenanceStatus() {
    const [rows] = await this.pool.execute(
      `SELECT action, details_json, created_at
       FROM admin_audit_logs
       WHERE action IN ('maintenance_enabled', 'maintenance_disabled')
       ORDER BY created_at DESC
       LIMIT 1`
    );
    if (rows.length === 0) {
      return { enabled: false, reason: null, updatedAt: null };
    }
    const latest = rows[0];
    const details = latest.details_json ? JSON.parse(latest.details_json) : {};
    return {
      enabled: latest.action === "maintenance_enabled",
      reason: details.reason || null,
      updatedAt: latest.created_at
    };
  }
}

export { hashPassword, verifyPassword, hashToken };
