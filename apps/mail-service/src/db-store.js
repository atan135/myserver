import { randomUUID } from "node:crypto";

import { buildMailNotificationEvent } from "./notification-outbox.js";

function toDateOrNull(value) {
  if (!value) {
    return null;
  }

  const date = value instanceof Date ? value : new Date(value);
  return Number.isNaN(date.getTime()) ? null : date;
}

function parseAttachments(value) {
  if (!value) {
    return null;
  }

  if (typeof value === "string") {
    return JSON.parse(value);
  }

  return value;
}

function cloneAttachments(value) {
  if (!value) {
    return null;
  }

  return JSON.parse(JSON.stringify(value));
}

function parseJson(value) {
  if (!value) {
    return null;
  }

  if (typeof value === "string") {
    return JSON.parse(value);
  }

  return value;
}

function isValidLeaseToken(value) {
  return typeof value === "string" && value.trim().length > 0;
}

async function rollbackQuietly(client) {
  try {
    await client.query("ROLLBACK");
  } catch {
    // Keep the original transaction error visible to callers.
  }
}

export class DbMailStore {
  constructor(pool, options = {}) {
    this.pool = pool;
    this.outboxMaxAttempts = options.outboxMaxAttempts || 8;
    this.outboxLeaseMs = options.outboxLeaseMs || 30_000;
    this.outboxLeaseOwner = options.outboxLeaseOwner || "mail-service";
    this.memory = new Map();
    this.memoryNextId = 1;
    this.memoryClaimWorkflows = new Map();
    this.memoryClaimWorkflowRequestIds = new Map();
    this.memoryClaimWorkflowNextId = 1;
    this.memoryOutbox = new Map();
    this.memoryOutboxNextId = 1;
  }

  async createMail(mail) {
    return this.createMailWithNotificationOutbox(mail).then((result) => result.mailId);
  }

  async createMailWithNotificationOutbox(mail) {
    const event = buildMailNotificationEvent(mail);
    if (!this.pool) {
      const id = this.memoryNextId++;
      const createdAt = toDateOrNull(mail.created_at) || new Date();
      const normalizedSenderId = mail.sender_id && mail.sender_id.toLowerCase() === "system"
        ? "system"
        : (mail.sender_id || mail.from_player_id);
      const isSystemSender = normalizedSenderId === "system";

      this.memory.set(mail.mail_id, {
        id,
        mail_id: mail.mail_id,
        sender_type: mail.sender_type || (isSystemSender ? "system" : "player"),
        sender_id: normalizedSenderId,
        sender_name: mail.sender_name || (isSystemSender ? "系统" : normalizedSenderId),
        from_player_id: mail.from_player_id,
        to_player_id: mail.to_player_id,
        title: mail.title,
        content: mail.content || null,
        attachments: cloneAttachments(mail.attachments),
        mail_type: mail.mail_type || "system",
        created_by_type: mail.created_by_type || (isSystemSender ? "system" : "player"),
        created_by_id: mail.created_by_id || normalizedSenderId,
        created_by_name: mail.created_by_name || mail.sender_name || (isSystemSender ? "系统" : normalizedSenderId),
        status: mail.status || "unread",
        created_at: createdAt,
        read_at: toDateOrNull(mail.read_at),
        claimed_at: toDateOrNull(mail.claimed_at),
        expires_at: toDateOrNull(mail.expires_at)
      });

      const outbox = this.enqueueMailNotificationOutboxMemory({
        mail_id: mail.mail_id,
        to_player_id: mail.to_player_id,
        event_id: event.event_id,
        event_version: event.version,
        trace_id: event.trace_id,
        occurred_at: event.occurred_at,
        max_attempts: this.outboxMaxAttempts,
        payload: event,
        created_at: createdAt
      });

      return {
        mailId: id,
        outboxId: outbox.id
      };
    }

    const client = await this.pool.connect();
    try {
      await client.query("BEGIN");
      const mailId = await this.insertMail(client, mail);
      const outboxResult = await client.query(
        `INSERT INTO mail_notification_outbox
          (mail_id, to_player_id, event_id, event_version, trace_id, occurred_at,
           payload, status, max_attempts, next_attempt_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7::jsonb, 'pending', $8, current_timestamp)
         RETURNING id`,
        [
          mail.mail_id,
          mail.to_player_id,
          event.event_id,
          event.version,
          event.trace_id,
          event.occurred_at,
          JSON.stringify(event),
          this.outboxMaxAttempts
        ]
      );
      await client.query("COMMIT");

      return {
        mailId,
        outboxId: outboxResult.rows[0].id
      };
    } catch (error) {
      await rollbackQuietly(client);
      throw error;
    } finally {
      client.release();
    }
  }

  async insertMail(executor, mail) {
    const sql = `INSERT INTO mails
      (
        mail_id,
        sender_type,
        sender_id,
        sender_name,
        from_player_id,
        to_player_id,
        title,
        content,
        attachments,
        mail_type,
        created_by_type,
        created_by_id,
        created_by_name,
        expires_at
      )
      VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::jsonb, $10, $11, $12, $13, $14)
      RETURNING id`;

    const attachments = mail.attachments
      ? JSON.stringify(mail.attachments)
      : null;

    const result = await executor.query(sql, [
      mail.mail_id,
      mail.sender_type,
      mail.sender_id,
      mail.sender_name || null,
      mail.from_player_id,
      mail.to_player_id,
      mail.title,
      mail.content || null,
      attachments,
      mail.mail_type || "system",
      mail.created_by_type,
      mail.created_by_id || null,
      mail.created_by_name || null,
      mail.expires_at || null
    ]);

    return result.rows[0].id;
  }

  enqueueMailNotificationOutboxMemory(entry) {
    const id = this.memoryOutboxNextId++;
    const now = toDateOrNull(entry.created_at) || new Date();
    const row = {
      id,
      mail_id: entry.mail_id,
      to_player_id: entry.to_player_id,
      event_id: entry.event_id,
      event_version: entry.event_version,
      trace_id: entry.trace_id,
      occurred_at: entry.occurred_at,
      payload: JSON.parse(JSON.stringify(entry.payload)),
      status: "pending",
      attempts: 0,
      max_attempts: entry.max_attempts || this.outboxMaxAttempts,
      next_attempt_at: now,
      locked_until: null,
      lease_owner: null,
      lease_token: null,
      last_error: null,
      created_at: now,
      sent_at: null,
      terminal_at: null
    };
    this.memoryOutbox.set(id, row);
    return this.parseOutboxRow(row);
  }

  async reservePendingMailNotificationOutbox(limit = 20, options = {}) {
    const leaseMs = options.leaseMs || this.outboxLeaseMs;
    const leaseOwner = options.leaseOwner || this.outboxLeaseOwner;
    const leaseToken = options.leaseToken || randomUUID();
    if (!this.pool) {
      const now = Date.now();
      const reserved = Array.from(this.memoryOutbox.values())
        .filter((row) => row.status !== "sent" && row.status !== "terminal")
        .filter((row) => !row.next_attempt_at || new Date(row.next_attempt_at).getTime() <= now)
        .filter((row) => row.status !== "sending" || !row.locked_until || new Date(row.locked_until).getTime() <= now)
        .sort((a, b) => a.id - b.id)
        .slice(0, limit);

      for (const row of reserved) {
        row.lease_taken_over = row.status === "sending";
        row.attempts_exhausted = row.attempts >= row.max_attempts;
        row.status = "sending";
        if (!row.attempts_exhausted) {
          row.attempts += 1;
        }
        row.locked_until = new Date(now + leaseMs);
        row.lease_owner = leaseOwner;
        row.lease_token = leaseToken;
        this.memoryOutbox.set(row.id, row);
      }

      return reserved.map((row) => this.parseOutboxRow(row));
    }

    const client = await this.pool.connect();
    try {
      await client.query("BEGIN");
      const { rows } = await client.query(
        `SELECT *
          FROM mail_notification_outbox
          WHERE status NOT IN ('sent', 'terminal')
            AND (next_attempt_at IS NULL OR next_attempt_at <= current_timestamp)
            AND (status <> 'sending' OR locked_until IS NULL OR locked_until <= current_timestamp)
          ORDER BY id ASC
          LIMIT $1
          FOR UPDATE SKIP LOCKED`,
        [limit]
      );

      if (rows.length > 0) {
        const ids = rows.map((row) => row.id);
        await client.query(
          `UPDATE mail_notification_outbox
              SET status = 'sending',
                  attempts = attempts + CASE WHEN attempts < max_attempts THEN 1 ELSE 0 END,
                  locked_until = current_timestamp + ($2 * interval '1 millisecond'),
                  lease_owner = $3,
                  lease_token = $4
            WHERE id = ANY($1::bigint[])`,
          [ids, leaseMs, leaseOwner, leaseToken]
        );
      }

      await client.query("COMMIT");
      return rows.map((row) => this.parseOutboxRow({
        ...row,
        status: "sending",
        attempts: row.attempts < row.max_attempts ? row.attempts + 1 : row.attempts,
        locked_until: new Date(Date.now() + leaseMs),
        lease_owner: leaseOwner,
        lease_token: leaseToken,
        lease_taken_over: row.status === "sending",
        attempts_exhausted: row.attempts >= row.max_attempts
      }));
    } catch (error) {
      await rollbackQuietly(client);
      throw error;
    } finally {
      client.release();
    }
  }

  async markMailNotificationOutboxSent(outboxId, leaseToken) {
    if (!isValidLeaseToken(leaseToken)) {
      return false;
    }
    if (!this.pool) {
      const row = this.memoryOutbox.get(outboxId);
      if (!row || row.status !== "sending" || row.lease_token !== leaseToken) {
        return false;
      }

      row.status = "sent";
      row.sent_at = new Date();
      row.locked_until = null;
      row.lease_owner = null;
      row.lease_token = null;
      this.memoryOutbox.set(outboxId, row);
      return true;
    }

    const result = await this.pool.query(
      `UPDATE mail_notification_outbox
          SET status = 'sent',
              sent_at = current_timestamp,
              locked_until = NULL,
              lease_owner = NULL,
              lease_token = NULL
        WHERE id = $1
          AND status = 'sending'
          AND lease_token = $2`,
      [outboxId, leaseToken]
    );
    return result.rowCount > 0;
  }

  async markMailNotificationOutboxFailed(outboxId, errorMessage, options = {}) {
    const leaseToken = options.leaseToken;
    const delayMs = Math.max(0, options.delayMs ?? 1000);
    if (!isValidLeaseToken(leaseToken)) {
      return false;
    }
    if (!this.pool) {
      const row = this.memoryOutbox.get(outboxId);
      if (!row || row.status !== "sending" || row.lease_token !== leaseToken) {
        return false;
      }

      row.status = "failed";
      row.last_error = String(errorMessage || "").slice(0, 512);
      row.next_attempt_at = new Date(Date.now() + delayMs);
      row.locked_until = null;
      row.lease_owner = null;
      row.lease_token = null;
      this.memoryOutbox.set(outboxId, row);
      return true;
    }

    const result = await this.pool.query(
      `UPDATE mail_notification_outbox
          SET status = 'failed',
              last_error = $1,
              next_attempt_at = current_timestamp + ($2 * interval '1 millisecond'),
              locked_until = NULL,
              lease_owner = NULL,
              lease_token = NULL
        WHERE id = $3
          AND status = 'sending'
          AND lease_token = $4`,
      [String(errorMessage || "").slice(0, 512), delayMs, outboxId, leaseToken]
    );
    return result.rowCount > 0;
  }

  async markMailNotificationOutboxTerminal(outboxId, errorMessage, options = {}) {
    const leaseToken = options.leaseToken;
    if (!isValidLeaseToken(leaseToken)) {
      return false;
    }
    if (!this.pool) {
      const row = this.memoryOutbox.get(outboxId);
      if (!row || row.status !== "sending" || row.lease_token !== leaseToken) {
        return false;
      }
      row.status = "terminal";
      row.last_error = String(errorMessage || "").slice(0, 512);
      row.next_attempt_at = null;
      row.locked_until = null;
      row.lease_owner = null;
      row.lease_token = null;
      row.terminal_at = new Date();
      this.memoryOutbox.set(outboxId, row);
      return true;
    }

    const result = await this.pool.query(
      `UPDATE mail_notification_outbox
          SET status = 'terminal',
              last_error = $1,
              next_attempt_at = NULL,
              locked_until = NULL,
              lease_owner = NULL,
              lease_token = NULL,
              terminal_at = current_timestamp
        WHERE id = $2
          AND status = 'sending'
          AND lease_token = $3`,
      [String(errorMessage || "").slice(0, 512), outboxId, leaseToken]
    );
    return result.rowCount > 0;
  }

  async getMailNotificationOutboxStats(now = new Date()) {
    if (!this.pool) {
      const active = Array.from(this.memoryOutbox.values())
        .filter((row) => row.status !== "sent" && row.status !== "terminal");
      const oldestCreatedAt = active.reduce((oldest, row) => {
        const timestamp = new Date(row.created_at).getTime();
        return oldest === null || timestamp < oldest ? timestamp : oldest;
      }, null);
      return {
        backlog: active.length,
        oldestAgeMs: oldestCreatedAt === null ? 0 : Math.max(0, now.getTime() - oldestCreatedAt)
      };
    }

    const { rows } = await this.pool.query(
      `SELECT COUNT(*) FILTER (WHERE status NOT IN ('sent', 'terminal'))::bigint AS backlog,
              EXTRACT(EPOCH FROM ($1::timestamptz - MIN(created_at)
                FILTER (WHERE status NOT IN ('sent', 'terminal')))) * 1000 AS oldest_age_ms
         FROM mail_notification_outbox`,
      [now]
    );
    return {
      backlog: Number(rows[0]?.backlog || 0),
      oldestAgeMs: Math.max(0, Number(rows[0]?.oldest_age_ms || 0))
    };
  }

  async cleanupMailNotificationOutbox(options = {}) {
    const now = options.now || new Date();
    const sentRetentionMs = options.sentRetentionMs;
    const terminalRetentionMs = options.terminalRetentionMs;
    const limit = options.limit || 500;
    const sentBefore = new Date(now.getTime() - sentRetentionMs);
    const terminalBefore = new Date(now.getTime() - terminalRetentionMs);

    if (!this.pool) {
      let deleted = 0;
      for (const [id, row] of Array.from(this.memoryOutbox.entries()).sort(([a], [b]) => a - b)) {
        const shouldDelete = (row.status === "sent" && row.sent_at && new Date(row.sent_at) <= sentBefore)
          || (row.status === "terminal" && row.terminal_at && new Date(row.terminal_at) <= terminalBefore);
        if (shouldDelete && deleted < limit) {
          this.memoryOutbox.delete(id);
          deleted += 1;
        }
      }
      return deleted;
    }

    const result = await this.pool.query(
      `WITH expired AS (
         SELECT id
           FROM mail_notification_outbox
          WHERE (status = 'sent' AND sent_at < $1)
             OR (status = 'terminal' AND terminal_at < $2)
          ORDER BY id ASC
          LIMIT $3
       )
       DELETE FROM mail_notification_outbox outbox
        USING expired
        WHERE outbox.id = expired.id`,
      [sentBefore, terminalBefore, limit]
    );
    return result.rowCount;
  }

  async getMailNotificationOutboxByMailId(mailId) {
    if (!this.pool) {
      const row = Array.from(this.memoryOutbox.values()).find((entry) => entry.mail_id === mailId);
      return this.parseOutboxRow(row);
    }

    const { rows } = await this.pool.query(
      `SELECT * FROM mail_notification_outbox WHERE mail_id = $1 ORDER BY id ASC LIMIT 1`,
      [mailId]
    );
    return this.parseOutboxRow(rows[0]);
  }

  async getMailById(mailId) {
    if (!this.pool) {
      return this.parseMailRow(this.memory.get(mailId));
    }

    const sql = `SELECT * FROM mails WHERE mail_id = $1`;
    const { rows } = await this.pool.query(sql, [mailId]);

    if (rows.length === 0) {
      return null;
    }

    return this.parseMailRow(rows[0]);
  }

  async getMailsByPlayerId(playerId, options = {}) {
    const { status, limit = 50, offset = 0 } = options;

    if (!this.pool) {
      return Array.from(this.memory.values())
        .filter((mail) => mail.to_player_id === playerId)
        .filter((mail) => !status || mail.status === status)
        .sort((a, b) => new Date(b.created_at).getTime() - new Date(a.created_at).getTime())
        .slice(offset, offset + limit)
        .map((mail) => this.parseMailRow(mail));
    }

    let sql = `SELECT * FROM mails WHERE to_player_id = $1`;
    const params = [playerId];
    const addParam = (value) => {
      params.push(value);
      return `$${params.length}`;
    };

    if (status) {
      sql += ` AND status = ${addParam(status)}`;
    }

    sql += ` ORDER BY created_at DESC LIMIT ${addParam(limit)} OFFSET ${addParam(offset)}`;

    const { rows } = await this.pool.query(sql, params);
    return rows.map((row) => this.parseMailRow(row));
  }

  async markAsRead(mailId) {
    if (!this.pool) {
      const mail = this.memory.get(mailId);
      if (!mail || mail.status !== "unread") {
        return false;
      }

      mail.status = "read";
      mail.read_at = new Date();
      this.memory.set(mailId, mail);
      return true;
    }

    const sql = `UPDATE mails SET status = 'read', read_at = current_timestamp WHERE mail_id = $1 AND status = 'unread'`;
    const result = await this.pool.query(sql, [mailId]);
    return result.rowCount > 0;
  }

  async getMailClaimWorkflow(mailId) {
    if (!this.pool) {
      return this.parseClaimWorkflowRow(this.memoryClaimWorkflows.get(mailId));
    }

    const { rows } = await this.pool.query(
      `SELECT * FROM mail_claim_workflows WHERE mail_id = $1`,
      [mailId]
    );
    return this.parseClaimWorkflowRow(rows[0]);
  }

  async reserveMailClaimWorkflow(input, options = {}) {
    const leaseMs = options.leaseMs || 30_000;
    const leaseOwner = options.leaseOwner || "mail-service";
    const leaseToken = options.leaseToken || randomUUID();
    const now = new Date();
    const leaseExpiresAt = new Date(now.getTime() + leaseMs);

    if (!this.pool) {
      const existing = this.memoryClaimWorkflows.get(input.mailId);
      if (existing) {
        return this.reserveExistingClaimWorkflowMemory(existing, input, {
          leaseOwner,
          leaseToken,
          leaseExpiresAt,
          now
        });
      }

      const mail = this.memory.get(input.mailId);
      const precondition = this.validateNewClaimWorkflowMail(mail, input, now);
      if (precondition) {
        return precondition;
      }

      const workflow = {
        id: this.memoryClaimWorkflowNextId++,
        mail_id: input.mailId,
        player_id: input.playerId,
        claim_request_id: input.requestId,
        character_id: input.characterId,
        attachments_snapshot: cloneAttachments(input.attachmentsSnapshot),
        attachments_fingerprint: input.attachmentsFingerprint,
        status: "processing",
        attempts: 1,
        lease_owner: leaseOwner,
        lease_token: leaseToken,
        lease_expires_at: leaseExpiresAt,
        last_trace_id: input.traceId,
        last_error_code: null,
        last_error_category: null,
        last_result_state: null,
        last_error_retryable: null,
        last_error_message: null,
        result_summary: null,
        game_instance_id: null,
        recovery_attempts: 0,
        next_recovery_at: now,
        recovery_mode: null,
        recovery_lease_owner: null,
        recovery_lease_token: null,
        recovery_lease_expires_at: null,
        recovery_started_at: null,
        last_recovery_at: null,
        last_query_status: null,
        last_query_fingerprint: null,
        last_query_error_code: null,
        last_query_result_state: null,
        last_query_instance_ids: null,
        manual_review_at: null,
        created_at: now,
        updated_at: now,
        completed_at: null
      };
      const requestOwner = this.memoryClaimWorkflowRequestIds.get(input.requestId);
      if (requestOwner && requestOwner !== input.mailId) {
        throw new Error(`mail claim request id already belongs to ${requestOwner}`);
      }
      mail.status = "claiming";
      this.memory.set(input.mailId, mail);
      this.memoryClaimWorkflows.set(input.mailId, workflow);
      this.memoryClaimWorkflowRequestIds.set(input.requestId, input.mailId);
      return this.claimWorkflowReservation(workflow, mail, { acquired: true });
    }

    const client = await this.pool.connect();
    try {
      await client.query("BEGIN");
      const workflowResult = await client.query(
        `SELECT * FROM mail_claim_workflows WHERE mail_id = $1 FOR UPDATE`,
        [input.mailId]
      );
      const existing = workflowResult.rows[0];
      if (existing) {
        const existingResult = await this.reserveExistingClaimWorkflowPostgres(
          client,
          existing,
          input,
          { leaseMs, leaseOwner, leaseToken }
        );
        await client.query("COMMIT");
        return existingResult;
      }

      const mailResult = await client.query(
        `SELECT * FROM mails WHERE mail_id = $1 FOR UPDATE`,
        [input.mailId]
      );
      const mail = mailResult.rows[0];
      // A missing-row SELECT does not take a PostgreSQL gap lock. Recheck after
      // locking the mail so a concurrent first claimant cannot cause a unique-key 500.
      const concurrentWorkflowResult = await client.query(
        `SELECT * FROM mail_claim_workflows WHERE mail_id = $1 FOR UPDATE`,
        [input.mailId]
      );
      if (concurrentWorkflowResult.rows[0]) {
        const concurrentResult = await this.reserveExistingClaimWorkflowPostgres(
          client,
          concurrentWorkflowResult.rows[0],
          input,
          { leaseMs, leaseOwner, leaseToken }
        );
        await client.query("COMMIT");
        return concurrentResult;
      }
      const precondition = this.validateNewClaimWorkflowMail(mail, input, now);
      if (precondition) {
        await client.query("COMMIT");
        return precondition;
      }

      const insertResult = await client.query(
        `INSERT INTO mail_claim_workflows
          (mail_id, player_id, claim_request_id, character_id, attachments_snapshot,
           attachments_fingerprint, status, attempts, lease_owner, lease_token,
           lease_expires_at, last_trace_id)
         VALUES ($1, $2, $3, $4, $5::jsonb, $6, 'processing', 1, $7, $8,
                 current_timestamp + ($9 * interval '1 millisecond'), $10)
         RETURNING *`,
        [
          input.mailId,
          input.playerId,
          input.requestId,
          input.characterId,
          JSON.stringify(input.attachmentsSnapshot),
          input.attachmentsFingerprint,
          leaseOwner,
          leaseToken,
          leaseMs,
          input.traceId
        ]
      );
      await client.query(
        `UPDATE mails SET status = 'claiming'
          WHERE mail_id = $1 AND status <> 'claimed' AND claimed_at IS NULL`,
        [input.mailId]
      );
      await client.query("COMMIT");
      return this.claimWorkflowReservation(insertResult.rows[0], mail, { acquired: true });
    } catch (error) {
      await rollbackQuietly(client);
      throw error;
    } finally {
      client.release();
    }
  }

  validateNewClaimWorkflowMail(mail, input, now) {
    if (!mail) {
      return this.claimWorkflowReservation(null, null, { notFound: true });
    }
    if (mail.to_player_id !== input.playerId) {
      return this.claimWorkflowReservation(null, mail, { ownerMismatch: true });
    }
    if (mail.status === "claimed" || mail.claimed_at) {
      return this.claimWorkflowReservation(null, mail, { alreadyClaimed: true });
    }
    const expiresAt = toDateOrNull(mail.expires_at);
    if (expiresAt && expiresAt.getTime() <= now.getTime()) {
      return this.claimWorkflowReservation(null, mail, { expired: true });
    }
    if (JSON.stringify(parseAttachments(mail.attachments)) !== JSON.stringify(input.expectedAttachments)) {
      return this.claimWorkflowReservation(null, mail, { attachmentChanged: true });
    }
    return null;
  }

  reserveExistingClaimWorkflowMemory(workflow, input, lease) {
    const mail = this.memory.get(input.mailId);
    const precondition = this.validateExistingClaimWorkflow(workflow, input, lease.now);
    if (precondition) {
      return this.claimWorkflowReservation(workflow, mail, precondition);
    }

    const leaseTakenOver = workflow.status === "processing";
    workflow.status = "processing";
    workflow.attempts += 1;
    workflow.lease_owner = lease.leaseOwner;
    workflow.lease_token = lease.leaseToken;
    workflow.lease_expires_at = lease.leaseExpiresAt;
    workflow.last_trace_id = input.traceId;
    workflow.recovery_lease_owner = null;
    workflow.recovery_lease_token = null;
    workflow.recovery_lease_expires_at = null;
    workflow.recovery_mode = null;
    workflow.updated_at = lease.now;
    if (mail && mail.status !== "claimed" && !mail.claimed_at) {
      mail.status = "claiming";
      this.memory.set(input.mailId, mail);
    }
    this.memoryClaimWorkflows.set(input.mailId, workflow);
    return this.claimWorkflowReservation(workflow, mail, { acquired: true, leaseTakenOver });
  }

  async reserveExistingClaimWorkflowPostgres(client, workflow, input, lease) {
    const now = new Date();
    const precondition = this.validateExistingClaimWorkflow(workflow, input, now);
    if (precondition) {
      return this.claimWorkflowReservation(workflow, null, precondition);
    }

    const leaseTakenOver = workflow.status === "processing";
    const { rows } = await client.query(
      `UPDATE mail_claim_workflows
          SET status = 'processing',
              attempts = attempts + 1,
              lease_owner = $2,
              lease_token = $3,
              lease_expires_at = current_timestamp + ($4 * interval '1 millisecond'),
              last_trace_id = $5,
              recovery_lease_owner = NULL,
              recovery_lease_token = NULL,
              recovery_lease_expires_at = NULL,
              recovery_mode = NULL,
              updated_at = current_timestamp
        WHERE id = $1
        RETURNING *`,
      [workflow.id, lease.leaseOwner, lease.leaseToken, lease.leaseMs, input.traceId]
    );
    await client.query(
      `UPDATE mails SET status = 'claiming'
        WHERE mail_id = $1 AND status <> 'claimed' AND claimed_at IS NULL`,
      [input.mailId]
    );
    return this.claimWorkflowReservation(rows[0], null, { acquired: true, leaseTakenOver });
  }

  validateExistingClaimWorkflow(workflow, input, now) {
    if (workflow.player_id !== input.playerId) {
      return { ownerMismatch: true };
    }
    if (workflow.status === "claimed") {
      return { alreadyClaimed: true };
    }
    if (workflow.status === "reconciliation_pending") {
      return { reconciliationPending: true };
    }
    if (workflow.status === "manual_review") {
      return { manualReview: true };
    }
    if (toDateOrNull(workflow.recovery_lease_expires_at)?.getTime() > now.getTime()) {
      return { inProgress: true };
    }
    if (
      workflow.status === "processing" &&
      toDateOrNull(workflow.lease_expires_at)?.getTime() > now.getTime()
    ) {
      return { inProgress: true };
    }
    if (workflow.character_id !== input.characterId) {
      return { characterMismatch: true };
    }
    return null;
  }

  async recordMailClaimWorkflowFailure(mailId, leaseToken, failure) {
    if (!isValidLeaseToken(leaseToken)) {
      return { updated: false, workflow: await this.getMailClaimWorkflow(mailId) };
    }
    const status = failure.status;
    if (!new Set(["retryable_failure", "permanent_failure", "reconciliation_pending"]).has(status)) {
      throw new Error(`invalid mail claim failure status: ${status}`);
    }

    if (!this.pool) {
      const workflow = this.memoryClaimWorkflows.get(mailId);
      if (!workflow || workflow.status !== "processing" || workflow.lease_token !== leaseToken) {
        return { updated: false, workflow: this.parseClaimWorkflowRow(workflow) };
      }
      workflow.status = status;
      workflow.lease_owner = null;
      workflow.lease_token = null;
      workflow.lease_expires_at = null;
      workflow.last_trace_id = failure.traceId || workflow.last_trace_id;
      workflow.last_error_code = failure.errorCode || null;
      workflow.last_error_category = failure.errorCategory || null;
      workflow.last_result_state = failure.resultState || null;
      workflow.last_error_retryable = failure.retryable ?? null;
      workflow.last_error_message = String(failure.message || "").slice(0, 512) || null;
      workflow.game_instance_id = failure.instanceId || null;
      workflow.next_recovery_at = new Date();
      workflow.updated_at = new Date();
      this.memoryClaimWorkflows.set(mailId, workflow);
      return { updated: true, workflow: this.parseClaimWorkflowRow(workflow) };
    }

    const { rows } = await this.pool.query(
      `UPDATE mail_claim_workflows
          SET status = $3,
              lease_owner = NULL,
              lease_token = NULL,
              lease_expires_at = NULL,
              last_trace_id = COALESCE($4, last_trace_id),
              last_error_code = $5,
              last_error_category = $6,
              last_result_state = $7,
              last_error_retryable = $8,
              last_error_message = $9,
              game_instance_id = $10,
              next_recovery_at = current_timestamp,
              updated_at = current_timestamp
        WHERE mail_id = $1 AND status = 'processing' AND lease_token = $2
        RETURNING *`,
      [
        mailId,
        leaseToken,
        status,
        failure.traceId || null,
        failure.errorCode || null,
        failure.errorCategory || null,
        failure.resultState || null,
        failure.retryable ?? null,
        String(failure.message || "").slice(0, 512) || null,
        failure.instanceId || null
      ]
    );
    return {
      updated: rows.length > 0,
      workflow: rows.length > 0 ? this.parseClaimWorkflowRow(rows[0]) : await this.getMailClaimWorkflow(mailId)
    };
  }

  async completeMailClaimWorkflow(mailId, leaseToken, outcome = {}) {
    if (!isValidLeaseToken(leaseToken)) {
      return { claimed: false, workflow: await this.getMailClaimWorkflow(mailId), mail: null };
    }
    if (!this.pool) {
      const workflow = this.memoryClaimWorkflows.get(mailId);
      if (!workflow || workflow.status !== "processing" || workflow.lease_token !== leaseToken) {
        return {
          claimed: false,
          workflow: this.parseClaimWorkflowRow(workflow),
          mail: this.parseMailRow(this.memory.get(mailId))
        };
      }
      const now = new Date();
      workflow.status = "claimed";
      workflow.lease_owner = null;
      workflow.lease_token = null;
      workflow.lease_expires_at = null;
      workflow.last_trace_id = outcome.traceId || workflow.last_trace_id;
      workflow.last_error_code = null;
      workflow.last_error_category = null;
      workflow.last_result_state = "applied";
      workflow.last_error_retryable = false;
      workflow.last_error_message = null;
      workflow.result_summary = cloneAttachments(outcome.resultSummary);
      workflow.game_instance_id = outcome.instanceId || null;
      workflow.next_recovery_at = null;
      workflow.recovery_mode = null;
      workflow.recovery_lease_owner = null;
      workflow.recovery_lease_token = null;
      workflow.recovery_lease_expires_at = null;
      workflow.last_recovery_at ||= now;
      workflow.updated_at = now;
      workflow.completed_at = now;
      this.memoryClaimWorkflows.set(mailId, workflow);
      const mail = this.memory.get(mailId);
      if (mail) {
        mail.status = "claimed";
        mail.read_at ||= now;
        mail.claimed_at ||= now;
        this.memory.set(mailId, mail);
      }
      return {
        claimed: true,
        workflow: this.parseClaimWorkflowRow(workflow),
        mail: this.parseMailRow(mail)
      };
    }

    const client = await this.pool.connect();
    try {
      await client.query("BEGIN");
      const workflowResult = await client.query(
        `UPDATE mail_claim_workflows
            SET status = 'claimed',
                lease_owner = NULL,
                lease_token = NULL,
                lease_expires_at = NULL,
                last_trace_id = COALESCE($3, last_trace_id),
                last_error_code = NULL,
                last_error_category = NULL,
                last_result_state = 'applied',
                last_error_retryable = false,
                last_error_message = NULL,
                result_summary = $4::jsonb,
                game_instance_id = $5,
                next_recovery_at = NULL,
                recovery_mode = NULL,
                recovery_lease_owner = NULL,
                recovery_lease_token = NULL,
                recovery_lease_expires_at = NULL,
                last_recovery_at = COALESCE(last_recovery_at, current_timestamp),
                updated_at = current_timestamp,
                completed_at = current_timestamp
          WHERE mail_id = $1 AND status = 'processing' AND lease_token = $2
          RETURNING *`,
        [
          mailId,
          leaseToken,
          outcome.traceId || null,
          outcome.resultSummary ? JSON.stringify(outcome.resultSummary) : null,
          outcome.instanceId || null
        ]
      );
      if (workflowResult.rows.length === 0) {
        await client.query("COMMIT");
        return { claimed: false, workflow: await this.getMailClaimWorkflow(mailId), mail: null };
      }
      const mailResult = await client.query(
        `UPDATE mails
            SET status = 'claimed',
                read_at = COALESCE(read_at, current_timestamp),
                claimed_at = COALESCE(claimed_at, current_timestamp)
          WHERE mail_id = $1 AND status <> 'claimed' AND claimed_at IS NULL
          RETURNING *`,
        [mailId]
      );
      await client.query("COMMIT");
      return {
        claimed: true,
        workflow: this.parseClaimWorkflowRow(workflowResult.rows[0]),
        mail: this.parseMailRow(mailResult.rows[0])
      };
    } catch (error) {
      await rollbackQuietly(client);
      throw error;
    } finally {
      client.release();
    }
  }

  async reserveMailClaimRecoveries(limit = 20, options = {}) {
    const leaseMs = options.leaseMs || 60_000;
    const leaseOwner = options.leaseOwner || "mail-service";
    const maxAttempts = options.maxAttempts || 12;
    const now = toDateOrNull(options.now) || new Date();
    const leaseExpiresAt = new Date(now.getTime() + leaseMs);

    if (!this.pool) {
      const candidates = Array.from(this.memoryClaimWorkflows.values())
        .filter((workflow) => isClaimRecoveryDue(workflow, now))
        .sort(compareClaimRecoveryRows)
        .slice(0, limit);
      const workflows = [];
      let manualReviewCount = 0;
      for (const workflow of candidates) {
        if ((Number(workflow.recovery_attempts) || 0) >= maxAttempts) {
          moveClaimWorkflowToManualReview(workflow, now, {
            preserveLastError: true,
            errorCode: "MAIL_CLAIM_RECOVERY_ATTEMPTS_EXHAUSTED",
            errorCategory: "RESULT_UNKNOWN",
            resultState: workflow.last_result_state || "unknown",
            message: "automatic mail claim recovery attempts exhausted"
          });
          this.memoryClaimWorkflows.set(workflow.mail_id, workflow);
          manualReviewCount += 1;
          continue;
        }

        const leaseTakenOver = Boolean(
          workflow.recovery_lease_token &&
          toDateOrNull(workflow.recovery_lease_expires_at)?.getTime() <= now.getTime()
        );
        if (workflow.status === "processing") {
          workflow.status = "reconciliation_pending";
          workflow.lease_owner = null;
          workflow.lease_token = null;
          workflow.lease_expires_at = null;
        }
        workflow.recovery_attempts = (Number(workflow.recovery_attempts) || 0) + 1;
        workflow.recovery_mode = workflow.status === "retryable_failure" ? "grant" : "query";
        workflow.recovery_lease_owner = leaseOwner;
        workflow.recovery_lease_token = randomUUID();
        workflow.recovery_lease_expires_at = leaseExpiresAt;
        workflow.recovery_started_at ||= toDateOrNull(workflow.updated_at) || now;
        workflow.last_recovery_at = now;
        workflow.updated_at = now;
        this.memoryClaimWorkflows.set(workflow.mail_id, workflow);
        workflows.push({
          ...this.parseClaimWorkflowRow(workflow),
          recovery_lease_taken_over: leaseTakenOver
        });
      }
      return { workflows, manualReviewCount };
    }

    const client = await this.pool.connect();
    try {
      await client.query("BEGIN");
      const { rows } = await client.query(
        `SELECT *
           FROM mail_claim_workflows
          WHERE (
                  status IN ('retryable_failure', 'reconciliation_pending')
                  OR (status = 'processing' AND lease_expires_at <= current_timestamp)
                )
            AND (next_recovery_at IS NULL OR next_recovery_at <= current_timestamp)
            AND (recovery_lease_expires_at IS NULL OR recovery_lease_expires_at <= current_timestamp)
          ORDER BY next_recovery_at ASC NULLS FIRST, updated_at ASC, id ASC
          FOR UPDATE SKIP LOCKED
          LIMIT $1`,
        [limit]
      );

      const workflows = [];
      let manualReviewCount = 0;
      for (const row of rows) {
        if ((Number(row.recovery_attempts) || 0) >= maxAttempts) {
          await client.query(
            `UPDATE mail_claim_workflows
                SET status = 'manual_review',
                    lease_owner = NULL,
                    lease_token = NULL,
                    lease_expires_at = NULL,
                    recovery_mode = NULL,
                    recovery_lease_owner = NULL,
                    recovery_lease_token = NULL,
                    recovery_lease_expires_at = NULL,
                    next_recovery_at = NULL,
                    manual_review_at = current_timestamp,
                    last_recovery_at = current_timestamp,
                    last_error_code = COALESCE(last_error_code, 'MAIL_CLAIM_RECOVERY_ATTEMPTS_EXHAUSTED'),
                    last_error_category = COALESCE(last_error_category, 'RESULT_UNKNOWN'),
                    last_result_state = COALESCE(last_result_state, 'unknown'),
                    last_error_retryable = false,
                    last_error_message = COALESCE(last_error_message, 'automatic mail claim recovery attempts exhausted'),
                    updated_at = current_timestamp
              WHERE id = $1`,
            [row.id]
          );
          manualReviewCount += 1;
          continue;
        }

        const leaseToken = randomUUID();
        const leaseTakenOver = Boolean(row.recovery_lease_token);
        const mode = row.status === "retryable_failure" ? "grant" : "query";
        const updated = await client.query(
          `UPDATE mail_claim_workflows
              SET status = CASE WHEN status = 'processing' THEN 'reconciliation_pending' ELSE status END,
                  lease_owner = CASE WHEN status = 'processing' THEN NULL ELSE lease_owner END,
                  lease_token = CASE WHEN status = 'processing' THEN NULL ELSE lease_token END,
                  lease_expires_at = CASE WHEN status = 'processing' THEN NULL ELSE lease_expires_at END,
                  recovery_attempts = recovery_attempts + 1,
                  recovery_mode = $2,
                  recovery_lease_owner = $3,
                  recovery_lease_token = $4,
                  recovery_lease_expires_at = current_timestamp + ($5 * interval '1 millisecond'),
                  recovery_started_at = COALESCE(recovery_started_at, updated_at, current_timestamp),
                  last_recovery_at = current_timestamp,
                  updated_at = current_timestamp
            WHERE id = $1
            RETURNING *`,
          [row.id, mode, leaseOwner, leaseToken, leaseMs]
        );
        workflows.push({
          ...this.parseClaimWorkflowRow(updated.rows[0]),
          recovery_lease_taken_over: leaseTakenOver
        });
      }
      await client.query("COMMIT");
      return { workflows, manualReviewCount };
    } catch (error) {
      await rollbackQuietly(client);
      throw error;
    } finally {
      client.release();
    }
  }

  async prepareMailClaimRecoveryGrant(mailId, recoveryLeaseToken, outcome = {}) {
    if (!isValidLeaseToken(recoveryLeaseToken)) return null;
    if (!this.pool) {
      const workflow = this.memoryClaimWorkflows.get(mailId);
      if (!hasActiveRecoveryLease(workflow, recoveryLeaseToken, new Date())) return null;
      workflow.status = "reconciliation_pending";
      workflow.attempts = (Number(workflow.attempts) || 0) + 1;
      workflow.lease_owner = null;
      workflow.lease_token = null;
      workflow.lease_expires_at = null;
      workflow.recovery_mode = "grant";
      workflow.last_trace_id = outcome.traceId || workflow.last_trace_id;
      applyClaimQueryEvidence(workflow, outcome);
      workflow.updated_at = new Date();
      this.memoryClaimWorkflows.set(mailId, workflow);
      return this.parseClaimWorkflowRow(workflow);
    }

    const { rows } = await this.pool.query(
      `UPDATE mail_claim_workflows
          SET status = 'reconciliation_pending',
              attempts = attempts + 1,
              lease_owner = NULL,
              lease_token = NULL,
              lease_expires_at = NULL,
              recovery_mode = 'grant',
              last_trace_id = COALESCE($3, last_trace_id),
              last_query_status = COALESCE($4, last_query_status),
              last_query_fingerprint = COALESCE($5, last_query_fingerprint),
              last_query_error_code = $6,
              last_query_result_state = COALESCE($7, last_query_result_state),
              last_query_instance_ids = COALESCE($8::jsonb, last_query_instance_ids),
              updated_at = current_timestamp
        WHERE mail_id = $1
          AND recovery_lease_token = $2
          AND recovery_lease_expires_at > current_timestamp
        RETURNING *`,
      [
        mailId,
        recoveryLeaseToken,
        outcome.traceId || null,
        outcome.queryStatus || null,
        outcome.queryFingerprint || null,
        outcome.queryErrorCode || null,
        outcome.queryResultState || null,
        outcome.queryInstanceIds ? JSON.stringify(outcome.queryInstanceIds) : null
      ]
    );
    return this.parseClaimWorkflowRow(rows[0]);
  }

  async rescheduleMailClaimRecovery(mailId, recoveryLeaseToken, outcome = {}) {
    const status = outcome.status;
    if (!new Set(["retryable_failure", "reconciliation_pending"]).has(status)) {
      throw new Error(`invalid mail claim recovery status: ${status}`);
    }
    const delayMs = Math.max(0, Number(outcome.delayMs) || 0);
    if (!this.pool) {
      const workflow = this.memoryClaimWorkflows.get(mailId);
      const now = new Date();
      if (!hasActiveRecoveryLease(workflow, recoveryLeaseToken, now)) return null;
      workflow.status = status;
      workflow.lease_owner = null;
      workflow.lease_token = null;
      workflow.lease_expires_at = null;
      workflow.recovery_mode = null;
      workflow.recovery_lease_owner = null;
      workflow.recovery_lease_token = null;
      workflow.recovery_lease_expires_at = null;
      workflow.next_recovery_at = new Date(now.getTime() + delayMs);
      workflow.last_trace_id = outcome.traceId || workflow.last_trace_id;
      workflow.last_error_code = outcome.errorCode || null;
      workflow.last_error_category = outcome.errorCategory || null;
      workflow.last_result_state = outcome.resultState || null;
      workflow.last_error_retryable = outcome.retryable ?? true;
      workflow.last_error_message = String(outcome.message || "").slice(0, 512) || null;
      workflow.game_instance_id = outcome.instanceId || workflow.game_instance_id;
      applyClaimQueryEvidence(workflow, outcome);
      workflow.updated_at = now;
      this.memoryClaimWorkflows.set(mailId, workflow);
      return this.parseClaimWorkflowRow(workflow);
    }

    const { rows } = await this.pool.query(
      `UPDATE mail_claim_workflows
          SET status = $3,
              lease_owner = NULL,
              lease_token = NULL,
              lease_expires_at = NULL,
              recovery_mode = NULL,
              recovery_lease_owner = NULL,
              recovery_lease_token = NULL,
              recovery_lease_expires_at = NULL,
              next_recovery_at = current_timestamp + ($4 * interval '1 millisecond'),
              last_trace_id = COALESCE($5, last_trace_id),
              last_error_code = $6,
              last_error_category = $7,
              last_result_state = $8,
              last_error_retryable = $9,
              last_error_message = $10,
              game_instance_id = COALESCE($11, game_instance_id),
              last_query_status = COALESCE($12, last_query_status),
              last_query_fingerprint = COALESCE($13, last_query_fingerprint),
              last_query_error_code = $14,
              last_query_result_state = COALESCE($15, last_query_result_state),
              last_query_instance_ids = COALESCE($16::jsonb, last_query_instance_ids),
              updated_at = current_timestamp
        WHERE mail_id = $1
          AND recovery_lease_token = $2
          AND recovery_lease_expires_at > current_timestamp
        RETURNING *`,
      [
        mailId,
        recoveryLeaseToken,
        status,
        delayMs,
        outcome.traceId || null,
        outcome.errorCode || null,
        outcome.errorCategory || null,
        outcome.resultState || null,
        outcome.retryable ?? true,
        String(outcome.message || "").slice(0, 512) || null,
        outcome.instanceId || null,
        outcome.queryStatus || null,
        outcome.queryFingerprint || null,
        outcome.queryErrorCode || null,
        outcome.queryResultState || null,
        outcome.queryInstanceIds ? JSON.stringify(outcome.queryInstanceIds) : null
      ]
    );
    return this.parseClaimWorkflowRow(rows[0]);
  }

  async completeMailClaimRecovery(mailId, recoveryLeaseToken, outcome = {}) {
    if (!isValidLeaseToken(recoveryLeaseToken)) return { claimed: false, workflow: null, mail: null };
    if (!this.pool) {
      const workflow = this.memoryClaimWorkflows.get(mailId);
      const now = new Date();
      if (!hasActiveRecoveryLease(workflow, recoveryLeaseToken, now)) {
        return { claimed: false, workflow: this.parseClaimWorkflowRow(workflow), mail: null };
      }
      workflow.status = "claimed";
      workflow.lease_owner = null;
      workflow.lease_token = null;
      workflow.lease_expires_at = null;
      workflow.recovery_mode = null;
      workflow.recovery_lease_owner = null;
      workflow.recovery_lease_token = null;
      workflow.recovery_lease_expires_at = null;
      workflow.next_recovery_at = null;
      workflow.last_trace_id = outcome.traceId || workflow.last_trace_id;
      workflow.last_error_code = null;
      workflow.last_error_category = null;
      workflow.last_result_state = "applied";
      workflow.last_error_retryable = false;
      workflow.last_error_message = null;
      workflow.result_summary = cloneAttachments(outcome.resultSummary);
      workflow.game_instance_id = outcome.instanceId || workflow.game_instance_id;
      applyClaimQueryEvidence(workflow, outcome);
      workflow.last_recovery_at = now;
      workflow.updated_at = now;
      workflow.completed_at = now;
      this.memoryClaimWorkflows.set(mailId, workflow);
      const mail = this.memory.get(mailId);
      if (mail) {
        mail.status = "claimed";
        mail.read_at ||= now;
        mail.claimed_at ||= now;
        this.memory.set(mailId, mail);
      }
      return { claimed: true, workflow: this.parseClaimWorkflowRow(workflow), mail: this.parseMailRow(mail) };
    }

    const client = await this.pool.connect();
    try {
      await client.query("BEGIN");
      const updated = await client.query(
        `UPDATE mail_claim_workflows
            SET status = 'claimed',
                lease_owner = NULL,
                lease_token = NULL,
                lease_expires_at = NULL,
                recovery_mode = NULL,
                recovery_lease_owner = NULL,
                recovery_lease_token = NULL,
                recovery_lease_expires_at = NULL,
                next_recovery_at = NULL,
                last_trace_id = COALESCE($3, last_trace_id),
                last_error_code = NULL,
                last_error_category = NULL,
                last_result_state = 'applied',
                last_error_retryable = false,
                last_error_message = NULL,
                result_summary = $4::jsonb,
                game_instance_id = COALESCE($5, game_instance_id),
                last_query_status = COALESCE($6, last_query_status),
                last_query_fingerprint = COALESCE($7, last_query_fingerprint),
                last_query_error_code = $8,
                last_query_result_state = COALESCE($9, last_query_result_state),
                last_query_instance_ids = COALESCE($10::jsonb, last_query_instance_ids),
                last_recovery_at = current_timestamp,
                updated_at = current_timestamp,
                completed_at = current_timestamp
          WHERE mail_id = $1
            AND recovery_lease_token = $2
            AND recovery_lease_expires_at > current_timestamp
          RETURNING *`,
        [
          mailId,
          recoveryLeaseToken,
          outcome.traceId || null,
          outcome.resultSummary ? JSON.stringify(outcome.resultSummary) : null,
          outcome.instanceId || null,
          outcome.queryStatus || null,
          outcome.queryFingerprint || null,
          outcome.queryErrorCode || null,
          outcome.queryResultState || null,
          outcome.queryInstanceIds ? JSON.stringify(outcome.queryInstanceIds) : null
        ]
      );
      if (updated.rows.length === 0) {
        await client.query("COMMIT");
        return { claimed: false, workflow: await this.getMailClaimWorkflow(mailId), mail: null };
      }
      const mailResult = await client.query(
        `UPDATE mails
            SET status = 'claimed',
                read_at = COALESCE(read_at, current_timestamp),
                claimed_at = COALESCE(claimed_at, current_timestamp)
          WHERE mail_id = $1 AND status <> 'claimed' AND claimed_at IS NULL
          RETURNING *`,
        [mailId]
      );
      await client.query("COMMIT");
      return {
        claimed: true,
        workflow: this.parseClaimWorkflowRow(updated.rows[0]),
        mail: this.parseMailRow(mailResult.rows[0])
      };
    } catch (error) {
      await rollbackQuietly(client);
      throw error;
    } finally {
      client.release();
    }
  }

  async markMailClaimRecoveryManualReview(mailId, recoveryLeaseToken, outcome = {}) {
    if (!isValidLeaseToken(recoveryLeaseToken)) return null;
    if (!this.pool) {
      const workflow = this.memoryClaimWorkflows.get(mailId);
      const now = new Date();
      if (!hasActiveRecoveryLease(workflow, recoveryLeaseToken, now)) return null;
      moveClaimWorkflowToManualReview(workflow, now, outcome);
      applyClaimQueryEvidence(workflow, outcome);
      this.memoryClaimWorkflows.set(mailId, workflow);
      return this.parseClaimWorkflowRow(workflow);
    }

    const { rows } = await this.pool.query(
      `UPDATE mail_claim_workflows
          SET status = 'manual_review',
              lease_owner = NULL,
              lease_token = NULL,
              lease_expires_at = NULL,
              recovery_mode = NULL,
              recovery_lease_owner = NULL,
              recovery_lease_token = NULL,
              recovery_lease_expires_at = NULL,
              next_recovery_at = NULL,
              manual_review_at = current_timestamp,
              last_recovery_at = current_timestamp,
              last_trace_id = COALESCE($3, last_trace_id),
              last_error_code = $4,
              last_error_category = $5,
              last_result_state = $6,
              last_error_retryable = false,
              last_error_message = $7,
              game_instance_id = COALESCE($8, game_instance_id),
              last_query_status = COALESCE($9, last_query_status),
              last_query_fingerprint = COALESCE($10, last_query_fingerprint),
              last_query_error_code = $11,
              last_query_result_state = COALESCE($12, last_query_result_state),
              last_query_instance_ids = COALESCE($13::jsonb, last_query_instance_ids),
              updated_at = current_timestamp
        WHERE mail_id = $1
          AND recovery_lease_token = $2
          AND recovery_lease_expires_at > current_timestamp
        RETURNING *`,
      [
        mailId,
        recoveryLeaseToken,
        outcome.traceId || null,
        outcome.errorCode || "MAIL_CLAIM_MANUAL_REVIEW_REQUIRED",
        outcome.errorCategory || "RESULT_UNKNOWN",
        outcome.resultState || "unknown",
        String(outcome.message || "mail claim requires manual review").slice(0, 512),
        outcome.instanceId || null,
        outcome.queryStatus || null,
        outcome.queryFingerprint || null,
        outcome.queryErrorCode || null,
        outcome.queryResultState || null,
        outcome.queryInstanceIds ? JSON.stringify(outcome.queryInstanceIds) : null
      ]
    );
    return this.parseClaimWorkflowRow(rows[0]);
  }

  claimWorkflowReservation(workflow, mail, flags = {}) {
    return {
      acquired: false,
      alreadyClaimed: false,
      inProgress: false,
      reconciliationPending: false,
      manualReview: false,
      notFound: false,
      ownerMismatch: false,
      characterMismatch: false,
      expired: false,
      attachmentChanged: false,
      leaseTakenOver: false,
      ...flags,
      workflow: this.parseClaimWorkflowRow(workflow),
      mail: this.parseMailRow(mail)
    };
  }

  async deleteMail(mailId) {
    if (!this.pool) {
      return this.memory.delete(mailId);
    }

    const sql = `DELETE FROM mails WHERE mail_id = $1`;
    const result = await this.pool.query(sql, [mailId]);
    return result.rowCount > 0;
  }

  async countUnread(playerId) {
    if (!this.pool) {
      return Array.from(this.memory.values())
        .filter((mail) => mail.to_player_id === playerId && mail.status === "unread")
        .length;
    }

    const sql = `SELECT COUNT(*) AS count FROM mails WHERE to_player_id = $1 AND status = 'unread'`;
    const { rows } = await this.pool.query(sql, [playerId]);
    return Number.parseInt(String(rows[0].count), 10) || 0;
  }

  parseMailRow(row) {
    if (!row) {
      return null;
    }

    const normalizedSenderId = row.sender_id && row.sender_id.toLowerCase() === "system"
      ? "system"
      : (row.sender_id || row.from_player_id);
    const isSystemSender = normalizedSenderId === "system";

    return {
      id: row.id,
      mail_id: row.mail_id,
      sender_type: row.sender_type || (isSystemSender ? "system" : "player"),
      sender_id: normalizedSenderId,
      sender_name: row.sender_name || (isSystemSender ? "系统" : normalizedSenderId),
      from_player_id: row.from_player_id,
      to_player_id: row.to_player_id,
      title: row.title,
      content: row.content,
      attachments: parseAttachments(row.attachments),
      mail_type: row.mail_type,
      created_by_type: row.created_by_type || (isSystemSender ? "system" : "player"),
      created_by_id: row.created_by_id || normalizedSenderId,
      created_by_name: row.created_by_name || row.sender_name || (isSystemSender ? "系统" : normalizedSenderId),
      status: row.status,
      created_at: row.created_at,
      read_at: row.read_at,
      claimed_at: row.claimed_at,
      expires_at: row.expires_at
    };
  }

  parseOutboxRow(row) {
    if (!row) {
      return null;
    }

    return {
      id: row.id,
      mail_id: row.mail_id,
      to_player_id: row.to_player_id,
      payload: parseJson(row.payload),
      event_id: row.event_id,
      event_version: row.event_version || 1,
      trace_id: row.trace_id,
      occurred_at: row.occurred_at,
      status: row.status,
      attempts: row.attempts || 0,
      max_attempts: row.max_attempts || this.outboxMaxAttempts,
      next_attempt_at: row.next_attempt_at,
      locked_until: row.locked_until,
      lease_owner: row.lease_owner,
      lease_token: row.lease_token,
      lease_taken_over: row.lease_taken_over === true,
      attempts_exhausted: row.attempts_exhausted === true,
      last_error: row.last_error,
      created_at: row.created_at,
      sent_at: row.sent_at,
      terminal_at: row.terminal_at
    };
  }

  parseClaimWorkflowRow(row) {
    if (!row) {
      return null;
    }

    return {
      id: row.id,
      mail_id: row.mail_id,
      player_id: row.player_id,
      claim_request_id: row.claim_request_id,
      character_id: row.character_id,
      attachments_snapshot: cloneAttachments(parseJson(row.attachments_snapshot)),
      attachments_fingerprint: row.attachments_fingerprint,
      status: row.status,
      attempts: Number(row.attempts) || 0,
      lease_owner: row.lease_owner,
      lease_token: row.lease_token,
      lease_expires_at: row.lease_expires_at,
      last_trace_id: row.last_trace_id,
      last_error_code: row.last_error_code,
      last_error_category: row.last_error_category,
      last_result_state: row.last_result_state,
      last_error_retryable: row.last_error_retryable,
      last_error_message: row.last_error_message,
      result_summary: cloneAttachments(parseJson(row.result_summary)),
      game_instance_id: row.game_instance_id,
      recovery_attempts: Number(row.recovery_attempts) || 0,
      next_recovery_at: row.next_recovery_at,
      recovery_mode: row.recovery_mode,
      recovery_lease_owner: row.recovery_lease_owner,
      recovery_lease_token: row.recovery_lease_token,
      recovery_lease_expires_at: row.recovery_lease_expires_at,
      recovery_started_at: row.recovery_started_at,
      last_recovery_at: row.last_recovery_at,
      last_query_status: row.last_query_status,
      last_query_fingerprint: row.last_query_fingerprint,
      last_query_error_code: row.last_query_error_code,
      last_query_result_state: row.last_query_result_state,
      last_query_instance_ids: cloneAttachments(parseJson(row.last_query_instance_ids)),
      manual_review_at: row.manual_review_at,
      created_at: row.created_at,
      updated_at: row.updated_at,
      completed_at: row.completed_at
    };
  }
}

function isClaimRecoveryDue(workflow, now) {
  if (!workflow || !new Set(["processing", "retryable_failure", "reconciliation_pending"]).has(workflow.status)) {
    return false;
  }
  if (
    workflow.status === "processing" &&
    (!toDateOrNull(workflow.lease_expires_at) || toDateOrNull(workflow.lease_expires_at).getTime() > now.getTime())
  ) {
    return false;
  }
  if (toDateOrNull(workflow.next_recovery_at)?.getTime() > now.getTime()) {
    return false;
  }
  return !toDateOrNull(workflow.recovery_lease_expires_at) ||
    toDateOrNull(workflow.recovery_lease_expires_at).getTime() <= now.getTime();
}

function compareClaimRecoveryRows(left, right) {
  const leftNext = toDateOrNull(left.next_recovery_at)?.getTime() ?? 0;
  const rightNext = toDateOrNull(right.next_recovery_at)?.getTime() ?? 0;
  if (leftNext !== rightNext) return leftNext - rightNext;
  const leftUpdated = toDateOrNull(left.updated_at)?.getTime() ?? 0;
  const rightUpdated = toDateOrNull(right.updated_at)?.getTime() ?? 0;
  if (leftUpdated !== rightUpdated) return leftUpdated - rightUpdated;
  return Number(left.id) - Number(right.id);
}

function hasActiveRecoveryLease(workflow, token, now) {
  return Boolean(
    workflow &&
    workflow.recovery_lease_token === token &&
    toDateOrNull(workflow.recovery_lease_expires_at)?.getTime() > now.getTime()
  );
}

function applyClaimQueryEvidence(workflow, outcome = {}) {
  if (outcome.queryStatus) workflow.last_query_status = outcome.queryStatus;
  if (outcome.queryFingerprint) workflow.last_query_fingerprint = outcome.queryFingerprint;
  workflow.last_query_error_code = outcome.queryErrorCode || null;
  if (outcome.queryResultState) workflow.last_query_result_state = outcome.queryResultState;
  if (outcome.queryInstanceIds) workflow.last_query_instance_ids = cloneAttachments(outcome.queryInstanceIds);
}

function moveClaimWorkflowToManualReview(workflow, now, outcome = {}) {
  workflow.status = "manual_review";
  workflow.lease_owner = null;
  workflow.lease_token = null;
  workflow.lease_expires_at = null;
  workflow.recovery_mode = null;
  workflow.recovery_lease_owner = null;
  workflow.recovery_lease_token = null;
  workflow.recovery_lease_expires_at = null;
  workflow.next_recovery_at = null;
  workflow.manual_review_at = now;
  workflow.last_recovery_at = now;
  workflow.last_trace_id = outcome.traceId || workflow.last_trace_id;
  if (!outcome.preserveLastError || !workflow.last_error_code) {
    workflow.last_error_code = outcome.errorCode || "MAIL_CLAIM_MANUAL_REVIEW_REQUIRED";
    workflow.last_error_category = outcome.errorCategory || "RESULT_UNKNOWN";
    workflow.last_result_state = outcome.resultState || "unknown";
    workflow.last_error_message = String(outcome.message || "mail claim requires manual review").slice(0, 512);
  }
  workflow.last_error_retryable = false;
  workflow.game_instance_id = outcome.instanceId || workflow.game_instance_id;
  workflow.updated_at = now;
}
