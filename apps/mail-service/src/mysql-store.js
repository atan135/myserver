export class MySqlMailStore {
  constructor(pool) {
    this.pool = pool;
  }

  async createMail(mail) {
    const sql = `INSERT INTO mails
      (mail_id, from_player_id, to_player_id, title, content, attachments, mail_type, expires_at)
      VALUES (?, ?, ?, ?, ?, ?, ?, ?)`;

    const attachments = mail.attachments
      ? JSON.stringify(mail.attachments)
      : null;

    const [result] = await this.pool.execute(sql, [
      mail.mail_id,
      mail.from_player_id,
      mail.to_player_id,
      mail.title,
      mail.content || null,
      attachments,
      mail.mail_type || "system",
      mail.expires_at || null
    ]);

    return result.insertId;
  }

  async getMailById(mailId) {
    const sql = `SELECT * FROM mails WHERE mail_id = ?`;
    const [rows] = await this.pool.execute(sql, [mailId]);

    if (rows.length === 0) {
      return null;
    }

    return this.parseMailRow(rows[0]);
  }

  async getMailsByPlayerId(playerId, options = {}) {
    const { status, limit = 50, offset = 0 } = options;

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
    const sql = `UPDATE mails SET status = 'read', read_at = NOW(3) WHERE mail_id = ? AND status = 'unread'`;
    const [result] = await this.pool.execute(sql, [mailId]);
    return result.affectedRows > 0;
  }

  async deleteMail(mailId) {
    const sql = `DELETE FROM mails WHERE mail_id = ?`;
    const [result] = await this.pool.execute(sql, [mailId]);
    return result.affectedRows > 0;
  }

  async countUnread(playerId) {
    const sql = `SELECT COUNT(*) as count FROM mails WHERE to_player_id = ? AND status = 'unread'`;
    const [rows] = await this.pool.execute(sql, [playerId]);
    return rows[0].count;
  }

  parseMailRow(row) {
    return {
      id: row.id,
      mail_id: row.mail_id,
      from_player_id: row.from_player_id,
      to_player_id: row.to_player_id,
      title: row.title,
      content: row.content,
      attachments: row.attachments ? JSON.parse(row.attachments) : null,
      mail_type: row.mail_type,
      status: row.status,
      created_at: row.created_at,
      read_at: row.read_at,
      expires_at: row.expires_at
    };
  }
}
