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

function buildMailNotificationPayload(mail) {
  return {
    to_player_id: mail.to_player_id,
    mail: {
      mail_id: mail.mail_id,
      sender_id: mail.sender_id,
      sender_name: mail.sender_name,
      from_player_id: mail.from_player_id,
      to_player_id: mail.to_player_id,
      title: mail.title,
      mail_type: mail.mail_type || "system",
      created_at: mail.created_at
    }
  };
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

async function rollbackQuietly(client) {
  try {
    await client.query("ROLLBACK");
  } catch {
    // Keep the original transaction error visible to callers.
  }
}

export class DbMailStore {
  constructor(pool) {
    this.pool = pool;
    this.memory = new Map();
    this.memoryNextId = 1;
    this.memoryOutbox = new Map();
    this.memoryOutboxNextId = 1;
  }

  async createMail(mail) {
    return this.createMailWithNotificationOutbox(mail).then((result) => result.mailId);
  }

  async createMailWithNotificationOutbox(mail) {
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
        payload: buildMailNotificationPayload(mail),
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
          (mail_id, to_player_id, payload, status, next_attempt_at)
         VALUES ($1, $2, $3::jsonb, 'pending', current_timestamp)
         RETURNING id`,
        [
          mail.mail_id,
          mail.to_player_id,
          JSON.stringify(buildMailNotificationPayload(mail))
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
      payload: JSON.parse(JSON.stringify(entry.payload)),
      status: "pending",
      attempts: 0,
      next_attempt_at: now,
      locked_until: null,
      last_error: null,
      created_at: now,
      sent_at: null
    };
    this.memoryOutbox.set(id, row);
    return this.parseOutboxRow(row);
  }

  async reservePendingMailNotificationOutbox(limit = 20) {
    if (!this.pool) {
      const now = Date.now();
      const reserved = Array.from(this.memoryOutbox.values())
        .filter((row) => row.status !== "sent")
        .filter((row) => !row.next_attempt_at || new Date(row.next_attempt_at).getTime() <= now)
        .filter((row) => row.status !== "sending" || !row.locked_until || new Date(row.locked_until).getTime() <= now)
        .sort((a, b) => a.id - b.id)
        .slice(0, limit);

      for (const row of reserved) {
        row.status = "sending";
        row.attempts += 1;
        row.locked_until = new Date(now + 30_000);
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
          WHERE status <> 'sent'
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
                  attempts = attempts + 1,
                  locked_until = current_timestamp + interval '30 seconds'
            WHERE id = ANY($1::bigint[])`,
          [ids]
        );
      }

      await client.query("COMMIT");
      return rows.map((row) => this.parseOutboxRow({
        ...row,
        status: "sending",
        attempts: row.attempts + 1
      }));
    } catch (error) {
      await rollbackQuietly(client);
      throw error;
    } finally {
      client.release();
    }
  }

  async markMailNotificationOutboxSent(outboxId) {
    if (!this.pool) {
      const row = this.memoryOutbox.get(outboxId);
      if (!row) {
        return false;
      }

      row.status = "sent";
      row.sent_at = new Date();
      row.locked_until = null;
      this.memoryOutbox.set(outboxId, row);
      return true;
    }

    const result = await this.pool.query(
      `UPDATE mail_notification_outbox
          SET status = 'sent',
              sent_at = current_timestamp,
              locked_until = NULL
        WHERE id = $1`,
      [outboxId]
    );
    return result.rowCount > 0;
  }

  async markMailNotificationOutboxFailed(outboxId, errorMessage) {
    if (!this.pool) {
      const row = this.memoryOutbox.get(outboxId);
      if (!row) {
        return false;
      }

      const delayMs = Math.min(60_000, 1000 * (2 ** Math.max(0, row.attempts - 1)));
      row.status = "failed";
      row.last_error = String(errorMessage || "").slice(0, 512);
      row.next_attempt_at = new Date(Date.now() + delayMs);
      row.locked_until = null;
      this.memoryOutbox.set(outboxId, row);
      return true;
    }

    const result = await this.pool.query(
      `UPDATE mail_notification_outbox
          SET status = 'failed',
              last_error = $1,
              next_attempt_at = current_timestamp + (
                LEAST(60, POWER(2, GREATEST(attempts - 1, 0))) * interval '1 second'
              ),
              locked_until = NULL
        WHERE id = $2`,
      [String(errorMessage || "").slice(0, 512), outboxId]
    );
    return result.rowCount > 0;
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

  async claimAttachments(mailId) {
    if (!this.pool) {
      const mail = this.memory.get(mailId);
      if (!mail) {
        return {
          claimed: false,
          mail: null
        };
      }

      const claimed = mail.status !== "claimed" && !mail.claimed_at;
      if (claimed) {
        const now = new Date();
        mail.status = "claimed";
        mail.read_at ||= now;
        mail.claimed_at ||= now;
        this.memory.set(mailId, mail);
      }

      return {
        claimed,
        mail: this.parseMailRow(mail)
      };
    }

    const updateSql = `UPDATE mails
      SET status = 'claimed',
          read_at = COALESCE(read_at, current_timestamp),
          claimed_at = COALESCE(claimed_at, current_timestamp)
      WHERE mail_id = $1
        AND status <> 'claimed'
        AND claimed_at IS NULL`;

    const updateResult = await this.pool.query(updateSql, [mailId]);
    const mail = await this.getMailById(mailId);

    return {
      claimed: updateResult.rowCount > 0,
      mail
    };
  }

  async beginClaimAttachments(mailId) {
    if (!this.pool) {
      const mail = this.memory.get(mailId);
      if (!mail) {
        return {
          reserved: false,
          alreadyClaimed: false,
          inProgress: false,
          mail: null
        };
      }

      if (mail.status === "claimed" || mail.claimed_at) {
        return {
          reserved: false,
          alreadyClaimed: true,
          inProgress: false,
          mail: this.parseMailRow(mail)
        };
      }

      if (mail.status === "claiming") {
        return {
          reserved: false,
          alreadyClaimed: false,
          inProgress: true,
          mail: this.parseMailRow(mail)
        };
      }

      mail.status = "claiming";
      this.memory.set(mailId, mail);

      return {
        reserved: true,
        alreadyClaimed: false,
        inProgress: false,
        mail: this.parseMailRow(mail)
      };
    }

    const updateSql = `UPDATE mails
      SET status = 'claiming'
      WHERE mail_id = $1
        AND status <> 'claimed'
        AND status <> 'claiming'
        AND claimed_at IS NULL`;

    const updateResult = await this.pool.query(updateSql, [mailId]);
    const mail = await this.getMailById(mailId);

    return {
      reserved: updateResult.rowCount > 0,
      alreadyClaimed: !!mail && (mail.status === "claimed" || !!mail.claimed_at),
      inProgress: !!mail && mail.status === "claiming" && updateResult.rowCount === 0,
      mail
    };
  }

  async completeClaimAttachments(mailId) {
    if (!this.pool) {
      const mail = this.memory.get(mailId);
      if (!mail) {
        return {
          claimed: false,
          mail: null
        };
      }

      const claimed = mail.status === "claiming" && !mail.claimed_at;
      if (claimed) {
        const now = new Date();
        mail.status = "claimed";
        mail.read_at ||= now;
        mail.claimed_at ||= now;
        this.memory.set(mailId, mail);
      }

      return {
        claimed,
        mail: this.parseMailRow(mail)
      };
    }

    const updateSql = `UPDATE mails
      SET status = 'claimed',
          read_at = COALESCE(read_at, current_timestamp),
          claimed_at = COALESCE(claimed_at, current_timestamp)
      WHERE mail_id = $1
        AND status = 'claiming'
        AND claimed_at IS NULL`;

    const updateResult = await this.pool.query(updateSql, [mailId]);
    const mail = await this.getMailById(mailId);

    return {
      claimed: updateResult.rowCount > 0,
      mail
    };
  }

  async releaseClaimAttachments(mailId) {
    if (!this.pool) {
      const mail = this.memory.get(mailId);
      if (!mail || mail.status !== "claiming") {
        return false;
      }

      mail.status = mail.read_at ? "read" : "unread";
      this.memory.set(mailId, mail);
      return true;
    }

    const sql = `UPDATE mails
      SET status = CASE WHEN read_at IS NULL THEN 'unread' ELSE 'read' END
      WHERE mail_id = $1
        AND status = 'claiming'
        AND claimed_at IS NULL`;

    const result = await this.pool.query(sql, [mailId]);
    return result.rowCount > 0;
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
      status: row.status,
      attempts: row.attempts || 0,
      next_attempt_at: row.next_attempt_at,
      locked_until: row.locked_until,
      last_error: row.last_error,
      created_at: row.created_at,
      sent_at: row.sent_at
    };
  }
}
