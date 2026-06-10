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

export class MySqlMailStore {
  constructor(pool) {
    this.pool = pool;
    this.memory = new Map();
    this.memoryNextId = 1;
  }

  async createMail(mail) {
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

      return id;
    }

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
      VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`;

    const attachments = mail.attachments
      ? JSON.stringify(mail.attachments)
      : null;

    const [result] = await this.pool.execute(sql, [
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

    return result.insertId;
  }

  async getMailById(mailId) {
    if (!this.pool) {
      return this.parseMailRow(this.memory.get(mailId));
    }

    const sql = `SELECT * FROM mails WHERE mail_id = ?`;
    const [rows] = await this.pool.execute(sql, [mailId]);

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

    let sql = `SELECT * FROM mails WHERE to_player_id = ?`;
    const params = [playerId];

    if (status) {
      sql += ` AND status = ?`;
      params.push(status);
    }

    sql += ` ORDER BY created_at DESC LIMIT ? OFFSET ?`;
    params.push(limit, offset);

    const [rows] = await this.pool.execute(sql, params);
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

    const sql = `UPDATE mails SET status = 'read', read_at = NOW(3) WHERE mail_id = ? AND status = 'unread'`;
    const [result] = await this.pool.execute(sql, [mailId]);
    return result.affectedRows > 0;
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
          read_at = COALESCE(read_at, NOW(3)),
          claimed_at = COALESCE(claimed_at, NOW(3))
      WHERE mail_id = ?
        AND status <> 'claimed'
        AND claimed_at IS NULL`;

    const [updateResult] = await this.pool.execute(updateSql, [mailId]);
    const mail = await this.getMailById(mailId);

    return {
      claimed: updateResult.affectedRows > 0,
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
      WHERE mail_id = ?
        AND status <> 'claimed'
        AND status <> 'claiming'
        AND claimed_at IS NULL`;

    const [updateResult] = await this.pool.execute(updateSql, [mailId]);
    const mail = await this.getMailById(mailId);

    return {
      reserved: updateResult.affectedRows > 0,
      alreadyClaimed: !!mail && (mail.status === "claimed" || !!mail.claimed_at),
      inProgress: !!mail && mail.status === "claiming" && updateResult.affectedRows === 0,
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
          read_at = COALESCE(read_at, NOW(3)),
          claimed_at = COALESCE(claimed_at, NOW(3))
      WHERE mail_id = ?
        AND status = 'claiming'
        AND claimed_at IS NULL`;

    const [updateResult] = await this.pool.execute(updateSql, [mailId]);
    const mail = await this.getMailById(mailId);

    return {
      claimed: updateResult.affectedRows > 0,
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
      WHERE mail_id = ?
        AND status = 'claiming'
        AND claimed_at IS NULL`;

    const [result] = await this.pool.execute(sql, [mailId]);
    return result.affectedRows > 0;
  }

  async deleteMail(mailId) {
    if (!this.pool) {
      return this.memory.delete(mailId);
    }

    const sql = `DELETE FROM mails WHERE mail_id = ?`;
    const [result] = await this.pool.execute(sql, [mailId]);
    return result.affectedRows > 0;
  }

  async countUnread(playerId) {
    if (!this.pool) {
      return Array.from(this.memory.values())
        .filter((mail) => mail.to_player_id === playerId && mail.status === "unread")
        .length;
    }

    const sql = `SELECT COUNT(*) as count FROM mails WHERE to_player_id = ? AND status = 'unread'`;
    const [rows] = await this.pool.execute(sql, [playerId]);
    return rows[0].count;
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
}
