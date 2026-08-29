import { createAdminStoreError } from "../errors.js";
import { toRequiredJsonb, toIsoString } from "../formatters.js";
import { toBreakglassGrant } from "../mappers/admin.js";

export class PolicyStore {
  constructor(pool, operationStore = null) {
    this.pool = pool;
    this.operationStore = operationStore;
  }

  async findAdminPolicyPermission(permissionKey) {
    const { rows } = await this.pool.query(
      `SELECT permission_key, resource, action, risk_level, scope_dimensions, active
       FROM admin_permissions
       WHERE permission_key = $1
       LIMIT 1`,
      [permissionKey]
    );
    return rows[0] || null;
  }

  async listEffectiveAdminPolicyGrants(adminId, permissionKey = null, at = new Date()) {
    const params = [adminId, at];
    const permissionFilter = permissionKey
      ? ` AND p.permission_key = $${params.push(permissionKey)}`
      : "";
    const { rows } = await this.pool.query(
      `SELECT p.permission_key, p.resource, p.action, p.risk_level, p.scope_dimensions,
              ar.scope_json, 'role'::text AS grant_source, ar.id AS source_id
       FROM admin_account_roles ar
       JOIN admin_roles r ON r.role_key = ar.role_key AND r.active = true
       JOIN admin_role_permissions rp ON rp.role_key = ar.role_key
       JOIN admin_permissions p ON p.permission_key = rp.permission_key AND p.active = true
       WHERE ar.admin_id = $1
         AND ar.effective_at <= $2
         AND (ar.expires_at IS NULL OR ar.expires_at > $2)
         AND ar.revoked_at IS NULL${permissionFilter}
       UNION ALL
       SELECT p.permission_key, p.resource, p.action, p.risk_level, p.scope_dimensions,
              pg.scope_json, 'direct'::text AS grant_source, pg.id AS source_id
       FROM admin_permission_grants pg
       JOIN admin_permissions p ON p.permission_key = pg.permission_key AND p.active = true
       WHERE pg.admin_id = $1
         AND pg.effective_at <= $2
         AND (pg.expires_at IS NULL OR pg.expires_at > $2)
         AND pg.revoked_at IS NULL${permissionFilter}
       ORDER BY permission_key, grant_source, source_id`,
      params
    );
    return rows;
  }

  async grantAdminPermission({
    adminId,
    permissionKey,
    scope,
    grantedByAdminId = null,
    grantedBySubject,
    reason,
    effectiveAt = null,
    expiresAt = null
  }) {
    const client = await this.pool.connect();
    try {
      await client.query("BEGIN");
      const permission = await client.query(
        `SELECT permission_key FROM admin_permissions WHERE permission_key = $1 AND active = true LIMIT 1`,
        [permissionKey]
      );
      if (permission.rows.length === 0) {
        throw createAdminStoreError("UNKNOWN_PERMISSION", "Permission is not available", { permissionKey });
      }
      const { rows } = await client.query(
        `INSERT INTO admin_permission_grants (
           admin_id, permission_key, scope_json, granted_by_admin_id, granted_by_subject, reason, effective_at, expires_at
         ) VALUES ($1, $2, $3::jsonb, $4, $5, $6, COALESCE($7::timestamptz, current_timestamp), $8::timestamptz)
         RETURNING id, effective_at, expires_at`,
        [adminId, permissionKey, toRequiredJsonb(scope), grantedByAdminId, grantedBySubject, reason, effectiveAt, expiresAt]
      );
      await client.query(
        `INSERT INTO admin_authorization_audit_events (
           event_type, actor_admin_id, actor_subject, subject_admin_id, permission_key, grant_id, reason, scope_json, details_json
         ) VALUES ('permission_granted', $1, $2, $3, $4, $5, $6, $7::jsonb, $8::jsonb)`,
        [
          grantedByAdminId,
          grantedBySubject,
          adminId,
          permissionKey,
          rows[0].id,
          reason,
          toRequiredJsonb(scope),
          toRequiredJsonb({ effectiveAt: toIsoString(rows[0].effective_at), expiresAt: toIsoString(rows[0].expires_at) })
        ]
      );
      await client.query("COMMIT");
      return rows[0];
    } catch (error) {
      await client.query("ROLLBACK").catch(() => undefined);
      throw error;
    } finally {
      client.release();
    }
  }

  async grantAdminRole({
    adminId,
    roleKey,
    scope,
    grantedByAdminId = null,
    grantedBySubject,
    reason,
    effectiveAt = null,
    expiresAt = null
  }) {
    const client = await this.pool.connect();
    try {
      await client.query("BEGIN");
      const role = await client.query(
        `SELECT role_key FROM admin_roles WHERE role_key = $1 AND active = true LIMIT 1`,
        [roleKey]
      );
      if (role.rows.length === 0) {
        throw createAdminStoreError("UNKNOWN_ADMIN_ROLE", "Admin role is not available", { roleKey });
      }
      const { rows } = await client.query(
        `INSERT INTO admin_account_roles (
           admin_id, role_key, scope_json, granted_by_admin_id, granted_by_subject, reason, effective_at, expires_at
         ) VALUES ($1, $2, $3::jsonb, $4, $5, $6, COALESCE($7::timestamptz, current_timestamp), $8::timestamptz)
         RETURNING id, effective_at, expires_at`,
        [adminId, roleKey, toRequiredJsonb(scope), grantedByAdminId, grantedBySubject, reason, effectiveAt, expiresAt]
      );
      await client.query(
        `INSERT INTO admin_authorization_audit_events (
           event_type, actor_admin_id, actor_subject, subject_admin_id, role_key, assignment_id, reason, scope_json, details_json
         ) VALUES ('account_role_granted', $1, $2, $3, $4, $5, $6, $7::jsonb, $8::jsonb)`,
        [
          grantedByAdminId,
          grantedBySubject,
          adminId,
          roleKey,
          rows[0].id,
          reason,
          toRequiredJsonb(scope),
          toRequiredJsonb({ effectiveAt: toIsoString(rows[0].effective_at), expiresAt: toIsoString(rows[0].expires_at) })
        ]
      );
      await client.query("COMMIT");
      return rows[0];
    } catch (error) {
      await client.query("ROLLBACK").catch(() => undefined);
      throw error;
    } finally {
      client.release();
    }
  }

  async revokeAdminPermission({ grantId, revokedByAdminId = null, revokedBySubject, reason }) {
    const client = await this.pool.connect();
    try {
      await client.query("BEGIN");
      const { rows } = await client.query(
        `UPDATE admin_permission_grants
         SET revoked_at = current_timestamp,
             revoked_by_admin_id = $2,
             revoked_by_subject = $3,
             revocation_reason = $4
         WHERE id = $1 AND revoked_at IS NULL
         RETURNING id, admin_id, permission_key, scope_json, revoked_at`,
        [grantId, revokedByAdminId, revokedBySubject, reason]
      );
      if (rows.length === 0) {
        throw createAdminStoreError("ADMIN_PERMISSION_GRANT_NOT_ACTIVE", "Permission grant is not active", { grantId });
      }
      const grant = rows[0];
      await client.query(
        `INSERT INTO admin_authorization_audit_events (
           event_type, actor_admin_id, actor_subject, subject_admin_id, permission_key, grant_id, reason, scope_json, details_json
         ) VALUES ('permission_revoked', $1, $2, $3, $4, $5, $6, $7::jsonb, $8::jsonb)`,
        [
          revokedByAdminId,
          revokedBySubject,
          grant.admin_id,
          grant.permission_key,
          grant.id,
          reason,
          toRequiredJsonb(grant.scope_json),
          toRequiredJsonb({ revokedAt: toIsoString(grant.revoked_at) })
        ]
      );
      await client.query("COMMIT");
      return grant;
    } catch (error) {
      await client.query("ROLLBACK").catch(() => undefined);
      throw error;
    } finally {
      client.release();
    }
  }

  async revokeAdminRole({ assignmentId, revokedByAdminId = null, revokedBySubject, reason }) {
    const client = await this.pool.connect();
    try {
      await client.query("BEGIN");
      const { rows } = await client.query(
        `UPDATE admin_account_roles
         SET revoked_at = current_timestamp,
             revoked_by_admin_id = $2,
             revoked_by_subject = $3,
             revocation_reason = $4
         WHERE id = $1 AND revoked_at IS NULL
         RETURNING id, admin_id, role_key, scope_json, revoked_at`,
        [assignmentId, revokedByAdminId, revokedBySubject, reason]
      );
      if (rows.length === 0) {
        throw createAdminStoreError("ADMIN_ROLE_ASSIGNMENT_NOT_ACTIVE", "Admin role assignment is not active", { assignmentId });
      }
      const assignment = rows[0];
      await client.query(
        `INSERT INTO admin_authorization_audit_events (
           event_type, actor_admin_id, actor_subject, subject_admin_id, role_key, assignment_id, reason, scope_json, details_json
         ) VALUES ('account_role_revoked', $1, $2, $3, $4, $5, $6, $7::jsonb, $8::jsonb)`,
        [
          revokedByAdminId,
          revokedBySubject,
          assignment.admin_id,
          assignment.role_key,
          assignment.id,
          reason,
          toRequiredJsonb(assignment.scope_json),
          toRequiredJsonb({ revokedAt: toIsoString(assignment.revoked_at) })
        ]
      );
      await client.query("COMMIT");
      return assignment;
    } catch (error) {
      await client.query("ROLLBACK").catch(() => undefined);
      throw error;
    } finally {
      client.release();
    }
  }
  async createAdminBreakglassGrant({
    grantId,
    activationRequestId,
    actorAdminId,
    actorSubject,
    permissionKey,
    scope,
    scopeSha256,
    targetSummary,
    targetSha256,
    semanticSha256,
    reason,
    expiresAt
  }) {
    const client = await this.pool.connect();
    try {
      await client.query("BEGIN");
      const existing = await client.query(
        `SELECT * FROM admin_breakglass_grants WHERE activation_request_id = $1 FOR UPDATE`,
        [activationRequestId]
      );
      if (existing.rows.length > 0) {
        const grant = toBreakglassGrant(existing.rows[0]);
        const same = String(grant.actorAdminId) === String(actorAdminId) &&
          grant.permissionKey === permissionKey &&
          grant.semanticSha256 === semanticSha256;
        await client.query("COMMIT");
        return { kind: same ? "existing" : "conflict", grant };
      }
      const permission = await client.query(
        `SELECT permission_key, risk_level, active
         FROM admin_permissions
         WHERE permission_key = $1
         FOR KEY SHARE`,
        [permissionKey]
      );
      if (permission.rows.length === 0 || permission.rows[0].active !== true || permission.rows[0].risk_level !== "emergency") {
        throw operationStoreError("ADMIN_BREAKGLASS_PERMISSION_INVALID", "Break-glass requires an active emergency permission", { permissionKey });
      }
      const inserted = await client.query(
        `INSERT INTO admin_breakglass_grants (
           grant_id, activation_request_id, actor_admin_id, actor_subject, permission_key,
           scope_json, scope_sha256, target_summary_json, target_sha256, semantic_sha256, reason, expires_at
         ) VALUES (
           $1::uuid, $2, $3, $4, $5,
           $6::jsonb, $7, $8::jsonb, $9, $10, $11, $12::timestamptz
         ) RETURNING *`,
        [
          grantId,
          activationRequestId,
          actorAdminId,
          actorSubject,
          permissionKey,
          toRequiredJsonb(scope),
          scopeSha256,
          toRequiredJsonb(targetSummary),
          targetSha256,
          semanticSha256,
          reason,
          expiresAt
        ]
      );
      const grant = toBreakglassGrant(inserted.rows[0]);
      await this.operationStore.insertAdminOperationAuditEvent(client, {
        breakglassGrantId: grant.grantId,
        eventType: "breakglass_activated",
        actorAdminId,
        actorSubject,
        requestId: activationRequestId,
        permissionKey,
        riskLevel: "emergency",
        reason,
        targetSummary,
        details: { expiresAt: grant.expiresAt, scopeSha256 }
      });
      // A break-glass grant is not committed unless its security alert is durable too.
      await client.query(
        `INSERT INTO security_audit_logs (event_type, target_type, target_value, severity, details_json)
         VALUES ($1, $2, $3, $4, $5::jsonb)`,
        [
          "admin_breakglass_activated",
          "breakglass_grant",
          grant.grantId,
          "critical",
          toRequiredJsonb({
            actorAdminId: String(actorAdminId),
            permission: permissionKey,
            requestId: activationRequestId,
            expiresAt: grant.expiresAt,
            scopeSha256,
            targetSha256
          })
        ]
      );
      await client.query("COMMIT");
      return { kind: "created", grant };
    } catch (error) {
      await client.query("ROLLBACK").catch(() => undefined);
      throw error;
    } finally {
      client.release();
    }
  }

  async revokeAdminBreakglassGrant({ grantId, revokedByAdminId = null, revokedBySubject, reason }) {
    const client = await this.pool.connect();
    try {
      await client.query("BEGIN");
      const updated = await client.query(
        `UPDATE admin_breakglass_grants
         SET revoked_at = current_timestamp,
             revoked_by_admin_id = $2,
             revoked_by_subject = $3,
             revocation_reason = $4
         WHERE grant_id = $1::uuid AND revoked_at IS NULL
         RETURNING *`,
        [grantId, revokedByAdminId, revokedBySubject, reason]
      );
      if (updated.rows.length === 0) {
        throw operationStoreError("ADMIN_BREAKGLASS_GRANT_NOT_ACTIVE", "Break-glass grant is not active", { grantId });
      }
      const grant = toBreakglassGrant(updated.rows[0]);
      await this.operationStore.insertAdminOperationAuditEvent(client, {
        breakglassGrantId: grant.grantId,
        eventType: "breakglass_revoked",
        actorAdminId: revokedByAdminId,
        actorSubject: revokedBySubject,
        requestId: grant.activationRequestId,
        permissionKey: grant.permissionKey,
        riskLevel: "emergency",
        reason,
        targetSummary: grant.targetSummary,
        details: { revokedAt: grant.revokedAt }
      });
      await client.query("COMMIT");
      return grant;
    } catch (error) {
      await client.query("ROLLBACK").catch(() => undefined);
      throw error;
    } finally {
      client.release();
    }
  }

  async listActiveAdminBreakglassGrants(adminId, permissionKey = null, at = new Date()) {
    const params = [adminId, at];
    const permissionFilter = permissionKey ? ` AND permission_key = $${params.push(permissionKey)}` : "";
    const { rows } = await this.pool.query(
      `SELECT * FROM admin_breakglass_grants
       WHERE actor_admin_id = $1
         AND activated_at <= $2
         AND expires_at > $2
         AND revoked_at IS NULL${permissionFilter}
       ORDER BY expires_at ASC, grant_id ASC`,
      params
    );
    return rows.map(toBreakglassGrant);
  }
}
