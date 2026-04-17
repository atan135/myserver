function toIsoString(value) {
  if (!value) {
    return null;
  }

  const date = value instanceof Date ? value : new Date(value);
  if (Number.isNaN(date.getTime())) {
    return String(value);
  }

  return date.toISOString();
}

function padDatePart(value, length = 2) {
  return String(value).padStart(length, "0");
}

function toMySqlDateTime(value) {
  if (!value) {
    return null;
  }

  const date = value instanceof Date ? value : new Date(value);
  if (Number.isNaN(date.getTime())) {
    return value;
  }

  return [
    `${date.getFullYear()}-${padDatePart(date.getMonth() + 1)}-${padDatePart(date.getDate())}`,
    `${padDatePart(date.getHours())}:${padDatePart(date.getMinutes())}:${padDatePart(date.getSeconds())}.${padDatePart(date.getMilliseconds(), 3)}`
  ].join(" ");
}

function deriveStatus(startTime, endTime) {
  const now = Date.now();
  const startMs = new Date(startTime).getTime();
  const endMs = new Date(endTime).getTime();

  if (Number.isFinite(startMs) && now < startMs) {
    return "scheduled";
  }

  if (Number.isFinite(endMs) && now > endMs) {
    return "expired";
  }

  return "active";
}

function normalizeAnnouncement(record) {
  if (!record) {
    return null;
  }

  const startTime = toIsoString(record.start_time);
  const endTime = toIsoString(record.end_time);

  return {
    id: record.id ?? null,
    announce_id: record.announce_id,
    locale: record.locale || "default",
    title: record.title,
    content: record.content,
    priority: Number.parseInt(String(record.priority ?? 0), 10) || 0,
    type: record.announce_type || record.type || "banner",
    target_group: record.target_group || "all",
    start_time: startTime,
    end_time: endTime,
    created_at: toIsoString(record.created_at),
    updated_at: toIsoString(record.updated_at),
    status: deriveStatus(startTime, endTime)
  };
}

function announcementComparator(a, b) {
  if (b.priority !== a.priority) {
    return b.priority - a.priority;
  }

  return new Date(b.start_time).getTime() - new Date(a.start_time).getTime();
}

function matchesLocale(announcement, locale) {
  if (!locale) {
    return true;
  }

  return announcement.locale === locale || announcement.locale === "default";
}

function matchesTargetGroup(announcement, targetGroup) {
  if (!targetGroup) {
    return true;
  }

  if (targetGroup === "all") {
    return announcement.target_group === "all";
  }

  return announcement.target_group === "all" || announcement.target_group === targetGroup;
}

function matchesPriority(announcement, minPriority) {
  if (minPriority === null || minPriority === undefined) {
    return true;
  }

  return announcement.priority >= minPriority;
}

function matchesActiveOnly(announcement, activeOnly) {
  if (!activeOnly) {
    return true;
  }

  return announcement.status === "active";
}

export class AnnouncementStore {
  constructor(pool) {
    this.pool = pool;
    this.memory = new Map();
  }

  async listAnnouncements(options = {}) {
    const {
      locale,
      targetGroup,
      minPriority = null,
      activeOnly = true,
      limit = 50,
      offset = 0
    } = options;

    if (this.pool) {
      let sql = "SELECT * FROM announcements WHERE 1=1";
      const params = [];

      if (locale) {
        sql += " AND (locale = ? OR locale = 'default')";
        params.push(locale);
      }

      if (targetGroup) {
        if (targetGroup === "all") {
          sql += " AND target_group = 'all'";
        } else {
          sql += " AND (target_group = ? OR target_group = 'all')";
          params.push(targetGroup);
        }
      }

      if (minPriority !== null && minPriority !== undefined) {
        sql += " AND priority >= ?";
        params.push(minPriority);
      }

      if (activeOnly) {
        sql += " AND start_time <= NOW(3) AND end_time >= NOW(3)";
      }

      sql += " ORDER BY priority DESC, start_time DESC LIMIT ? OFFSET ?";
      params.push(limit, offset);

      const [rows] = await this.pool.execute(sql, params);
      return rows.map((row) => normalizeAnnouncement(row));
    }

    const announcements = Array.from(this.memory.values())
      .map((value) => normalizeAnnouncement(value))
      .filter((announcement) => matchesLocale(announcement, locale))
      .filter((announcement) => matchesTargetGroup(announcement, targetGroup))
      .filter((announcement) => matchesPriority(announcement, minPriority))
      .filter((announcement) => matchesActiveOnly(announcement, activeOnly))
      .sort(announcementComparator);

    return announcements.slice(offset, offset + limit);
  }

  async getAnnouncementById(announceId) {
    if (this.pool) {
      const [rows] = await this.pool.execute(
        "SELECT * FROM announcements WHERE announce_id = ?",
        [announceId]
      );

      if (rows.length === 0) {
        return null;
      }

      return normalizeAnnouncement(rows[0]);
    }

    return normalizeAnnouncement(this.memory.get(announceId));
  }

  async createAnnouncement(announcement) {
    if (this.pool) {
      const sql = `INSERT INTO announcements
        (
          announce_id,
          locale,
          title,
          content,
          priority,
          announce_type,
          target_group,
          start_time,
          end_time
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)`;

      await this.pool.execute(sql, [
        announcement.announce_id,
        announcement.locale,
        announcement.title,
        announcement.content,
        announcement.priority,
        announcement.type,
        announcement.target_group,
        toMySqlDateTime(announcement.start_time),
        toMySqlDateTime(announcement.end_time)
      ]);

      return this.getAnnouncementById(announcement.announce_id);
    }

    const now = new Date().toISOString();
    this.memory.set(announcement.announce_id, {
      ...announcement,
      created_at: now,
      updated_at: now
    });

    return this.getAnnouncementById(announcement.announce_id);
  }

  async updateAnnouncement(announceId, patch) {
    const existing = await this.getAnnouncementById(announceId);
    if (!existing) {
      return null;
    }

    const next = {
      ...existing,
      ...patch,
      announce_id: announceId
    };

    if (this.pool) {
      const sql = `UPDATE announcements
        SET locale = ?,
            title = ?,
            content = ?,
            priority = ?,
            announce_type = ?,
            target_group = ?,
            start_time = ?,
            end_time = ?
        WHERE announce_id = ?`;

      await this.pool.execute(sql, [
        next.locale,
        next.title,
        next.content,
        next.priority,
        next.type,
        next.target_group,
        toMySqlDateTime(next.start_time),
        toMySqlDateTime(next.end_time),
        announceId
      ]);

      return this.getAnnouncementById(announceId);
    }

    this.memory.set(announceId, {
      ...next,
      updated_at: new Date().toISOString()
    });

    return this.getAnnouncementById(announceId);
  }

  async deleteAnnouncement(announceId) {
    if (this.pool) {
      const [result] = await this.pool.execute(
        "DELETE FROM announcements WHERE announce_id = ?",
        [announceId]
      );

      return result.affectedRows > 0;
    }

    return this.memory.delete(announceId);
  }
}
