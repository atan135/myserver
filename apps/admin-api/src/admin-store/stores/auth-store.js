import { BOOTSTRAP_POLICY_SCOPE, UNIQUE_VIOLATION } from "../constants.js";
import { generatePasswordSalt, hashPassword, verifyPassword as bcryptVerify } from "../crypto.js";
import { toIsoString, toRequiredJsonb } from "../formatters.js";
import { toAdmin } from "../mappers/admin.js";
import { createAdminStoreError } from "../errors.js";

export class AuthStore {
  constructor(pool) {
    this.pool = pool;
  }

  async findAdminByUsername(username) {
    const { rows } = await this.pool.query(
      `SELECT id, username, display_name, password_algo, password_salt, password_hash, role, status
       FROM admin_accounts
       WHERE username = $1
       LIMIT 1`,
      [username]
    );

    if (rows.length === 0) return null;

    return toAdmin(rows[0]);
  }

  async findAdminById(adminId) {
    const { rows } = await this.pool.query(
      `SELECT id, username, display_name, password_algo, password_salt, password_hash, role, status
       FROM admin_accounts
       WHERE id = $1
       LIMIT 1`,
      [adminId]
    );

    return rows.length > 0 ? toAdmin(rows[0]) : null;
  }

  async verifyPassword(password, hash) {
    return bcryptVerify(password, hash);
  }

  async createAdmin({ username, displayName, password, role = "viewer" }) {
    try {
      return await this.createAdminWithClient(this.pool, { username, displayName, password, role });
    } catch (error) {
      if (error.code === UNIQUE_VIOLATION) {
        throw new Error("ADMIN_ALREADY_EXISTS");
      }
      throw error;
    }
  }

  async createAdminWithClient(client, { username, displayName, password, role = "viewer" }) {
    const passwordSalt = generatePasswordSalt();
    const passwordHash = hashPassword(password);
    const { rows } = await client.query(
      `INSERT INTO admin_accounts (username, display_name, password_algo, password_salt, password_hash, role, status)
       VALUES ($1, $2, 'bcrypt', $3, $4, $5, 'active')
       RETURNING id`,
      [username, displayName || username, passwordSalt, passwordHash, role]
    );

    return {
      id: rows[0].id,
      username,
      displayName: displayName || username,
      role
    };
  }

  async ensureInitialAdmin(config) {
    const existing = await this.findAdminByUsername(config.initialAdminUsername);
    if (existing) {
      return existing;
    }

    const client = await this.pool.connect();
    try {
      await client.query("BEGIN");
      const admin = await this.createAdminWithClient(client, {
        username: config.initialAdminUsername,
        displayName: config.initialAdminDisplayName,
        password: config.initialAdminPassword,
        role: "admin"
      });
      await this.grantBootstrapAdminRoleInTransaction(client, admin, config);
      await client.query("COMMIT");
      return admin;
    } catch (error) {
      await client.query("ROLLBACK").catch(() => undefined);
      if (error.code === UNIQUE_VIOLATION) {
        const concurrentAdmin = await this.findAdminByUsername(config.initialAdminUsername);
        if (concurrentAdmin) {
          return concurrentAdmin;
        }
      }
      throw error;
    } finally {
      client.release();
    }
  }

  async grantBootstrapAdminRole(admin, config = {}) {
    const client = await this.pool.connect();
    try {
      await client.query("BEGIN");
      await this.grantBootstrapAdminRoleInTransaction(client, admin, config);
      await client.query("COMMIT");
    } catch (error) {
      await client.query("ROLLBACK").catch(() => undefined);
      throw error;
    } finally {
      client.release();
    }
  }

  async grantBootstrapAdminRoleInTransaction(client, admin, config = {}) {
    const roleKey = String(config.bootstrapAdminRole || "super_admin").trim();
    if (!roleKey) {
      throw createAdminStoreError("BOOTSTRAP_ADMIN_ROLE_REQUIRED", "Bootstrap admin role is required");
    }

    const scope = config.bootstrapAdminScope || BOOTSTRAP_POLICY_SCOPE;
    const subject = `bootstrap:${String(config.env || "development").trim() || "development"}:${admin.username}`;
    const reason = "initial admin bootstrap";
    const role = await client.query(
      `SELECT role_key FROM admin_roles WHERE role_key = $1 AND active = true LIMIT 1`,
      [roleKey]
    );
    if (role.rows.length === 0) {
      throw createAdminStoreError("BOOTSTRAP_ADMIN_ROLE_UNKNOWN", "Bootstrap admin role is not available", { roleKey });
    }

    const assignment = await client.query(
      `INSERT INTO admin_account_roles (
         admin_id, role_key, scope_json, granted_by_subject, reason, effective_at
       ) VALUES ($1, $2, $3::jsonb, $4, $5, current_timestamp)
       RETURNING id, effective_at`,
      [admin.id, roleKey, toRequiredJsonb(scope), subject, reason]
    );
    await client.query(
      `INSERT INTO admin_authorization_audit_events (
         event_type, actor_subject, subject_admin_id, role_key, assignment_id, reason, scope_json, details_json
       ) VALUES ('account_role_granted', $1, $2, $3, $4, $5, $6::jsonb, $7::jsonb)`,
      [
        subject,
        admin.id,
        roleKey,
        assignment.rows[0].id,
        reason,
        toRequiredJsonb(scope),
        toRequiredJsonb({ bootstrap: true, effectiveAt: toIsoString(assignment.rows[0].effective_at) })
      ]
    );
  }

  async updateLastLogin(adminId) {
    await this.pool.query(
      `UPDATE admin_accounts SET last_login_at = current_timestamp WHERE id = $1`,
      [adminId]
    );
  }

  async updateAdminPassword(adminId, password) {
    const passwordSalt = generatePasswordSalt();
    const passwordHash = hashPassword(password);
    const result = await this.pool.query(
      `UPDATE admin_accounts
       SET password_algo = 'bcrypt',
           password_salt = $1,
           password_hash = $2
       WHERE id = $3`,
      [passwordSalt, passwordHash, adminId]
    );

    return result.rowCount > 0;
  }
}

