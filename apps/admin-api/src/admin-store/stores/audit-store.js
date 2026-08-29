import { toJsonb, nextParam } from "../formatters.js";
import { readTotal } from "../mappers/assets.js";
import { toAdminOperationAuditEvent } from "../mappers/admin.js";

export class AuditStore {
  constructor(pool) {
    this.pool = pool;
  }

  async appendAuditLog({ adminId, adminUsername, action, targetType, targetValue, details, ip }) {
    await this.pool.query(
      `INSERT INTO admin_audit_logs (admin_id, admin_username, action, target_type, target_value, details_json, ip)
       VALUES ($1, $2, $3, $4, $5, $6::jsonb, $7)`,
      [
        adminId,
        adminUsername,
        action,
        targetType || null,
        targetValue || null,
        toJsonb(details),
        ip || null
      ]
    );
  }

  async appendSecurityAuditLog({
    eventType,
    targetType,
    targetValue,
    severity = "warning",
    clientIp,
    details
  }) {
    await this.pool.query(
      `INSERT INTO security_audit_logs (event_type, target_type, target_value, severity, client_ip, details_json)
       VALUES ($1, $2, $3, $4, $5, $6::jsonb)`,
      [
        eventType,
        targetType || null,
        targetValue || null,
        severity,
        clientIp || null,
        toJsonb(details)
      ]
    );
  }

  async getSecurityLogs({ limit = 50, offset = 0, eventType, targetType, severity, clientIp } = {}) {
    let query = `SELECT * FROM security_audit_logs WHERE 1=1`;
    const params = [];

    if (eventType) {
      params.push(eventType);
      query += ` AND event_type = ${nextParam(params)}`;
    }

    if (targetType) {
      params.push(targetType);
      query += ` AND target_type = ${nextParam(params)}`;
    }

    if (severity) {
      params.push(severity);
      query += ` AND severity = ${nextParam(params)}`;
    }

    if (clientIp) {
      params.push(clientIp);
      query += ` AND client_ip = ${nextParam(params)}`;
    }

    params.push(limit);
    query += ` ORDER BY created_at DESC LIMIT ${nextParam(params)}`;
    params.push(offset);
    query += ` OFFSET ${nextParam(params)}`;

    const { rows } = await this.pool.query(query, params);
    return rows;
  }

  async countSecurityLogs({ eventType, targetType, severity, clientIp } = {}) {
    let query = `SELECT COUNT(*) as total FROM security_audit_logs WHERE 1=1`;
    const params = [];

    if (eventType) {
      params.push(eventType);
      query += ` AND event_type = ${nextParam(params)}`;
    }

    if (targetType) {
      params.push(targetType);
      query += ` AND target_type = ${nextParam(params)}`;
    }

    if (severity) {
      params.push(severity);
      query += ` AND severity = ${nextParam(params)}`;
    }

    if (clientIp) {
      params.push(clientIp);
      query += ` AND client_ip = ${nextParam(params)}`;
    }

    const { rows } = await this.pool.query(query, params);
    return readTotal(rows);
  }

  async getAuditLogs({ limit = 50, offset = 0, adminId, action, targetType } = {}) {
    let query = `SELECT * FROM admin_audit_logs WHERE 1=1`;
    const params = [];

    if (adminId) {
      params.push(adminId);
      query += ` AND admin_id = ${nextParam(params)}`;
    }

    if (action) {
      params.push(action);
      query += ` AND action = ${nextParam(params)}`;
    }

    if (targetType) {
      params.push(targetType);
      query += ` AND target_type = ${nextParam(params)}`;
    }

    params.push(limit);
    query += ` ORDER BY created_at DESC LIMIT ${nextParam(params)}`;
    params.push(offset);
    query += ` OFFSET ${nextParam(params)}`;

    const { rows } = await this.pool.query(query, params);
    return rows;
  }

  async countAuditLogs({ adminId, action, targetType } = {}) {
    let query = `SELECT COUNT(*) as total FROM admin_audit_logs WHERE 1=1`;
    const params = [];

    if (adminId) {
      params.push(adminId);
      query += ` AND admin_id = ${nextParam(params)}`;
    }

    if (action) {
      params.push(action);
      query += ` AND action = ${nextParam(params)}`;
    }

    if (targetType) {
      params.push(targetType);
      query += ` AND target_type = ${nextParam(params)}`;
    }

    const { rows } = await this.pool.query(query, params);
    return readTotal(rows);
  }

  async countRecentAdminAuditActions({ adminId, action, since }) {
    const { rows } = await this.pool.query(
      `SELECT COUNT(*) AS total
       FROM admin_audit_logs
       WHERE admin_id = $1 AND action = $2 AND created_at >= $3::timestamptz`,
      [adminId, action, since]
    );
    return readTotal(rows);
  }
}
