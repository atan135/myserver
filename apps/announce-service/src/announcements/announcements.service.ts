import { Inject, Injectable } from "@nestjs/common";

import { badRequest, notFound } from "../common/http-exception.js";
import { generateAnnouncementId } from "../global-id.js";
import { log } from "../logger.js";
import { ANNOUNCE_CONFIG, ANNOUNCE_REDIS, ANNOUNCE_STORE } from "../tokens.js";

const ANNOUNCEMENT_LIST_CACHE_PREFIX = "announce:list:";

function parseBoolean(value: any, fallback: boolean) {
  if (value === undefined) {
    return fallback;
  }

  if (typeof value === "boolean") {
    return value;
  }

  if (typeof value === "string") {
    return !["false", "0", "no"].includes(value.trim().toLowerCase());
  }

  return Boolean(value);
}

function parseOptionalInteger(value: any, fieldName: string) {
  if (value === undefined || value === null || value === "") {
    return null;
  }

  const parsed = Number.parseInt(String(value), 10);
  if (!Number.isInteger(parsed)) {
    throw new Error(`${fieldName} must be an integer`);
  }

  return parsed;
}

function parseDateInput(value: any, fieldName: string): Date | null {
  if (value === undefined || value === null || value === "") {
    return null;
  }

  if (value instanceof Date) {
    if (Number.isNaN(value.getTime())) {
      throw new Error(`${fieldName} is invalid`);
    }
    return value;
  }

  if (typeof value === "number") {
    const timestamp = value < 1_000_000_000_000 ? value * 1000 : value;
    const date = new Date(timestamp);
    if (Number.isNaN(date.getTime())) {
      throw new Error(`${fieldName} is invalid`);
    }
    return date;
  }

  if (typeof value === "string") {
    const trimmed = value.trim();
    if (trimmed.length === 0) {
      return null;
    }

    const numeric = Number(trimmed);
    if (Number.isFinite(numeric)) {
      return parseDateInput(numeric, fieldName);
    }

    const date = new Date(trimmed);
    if (Number.isNaN(date.getTime())) {
      throw new Error(`${fieldName} is invalid`);
    }
    return date;
  }

  throw new Error(`${fieldName} is invalid`);
}

function normalizeWindow(body: any, existing: any = null) {
  const startInput =
    body.start_time !== undefined ? body.start_time : existing?.start_time;
  const startTime = parseDateInput(startInput, "start_time") || new Date();

  let endTime: Date | null = null;
  if (body.end_time !== undefined) {
    endTime = parseDateInput(body.end_time, "end_time");
  } else if (body.duration_seconds !== undefined) {
    const durationSeconds = parseOptionalInteger(
      body.duration_seconds,
      "duration_seconds"
    );
    if (durationSeconds === null || durationSeconds <= 0) {
      throw new Error("duration_seconds must be a positive integer");
    }
    endTime = new Date(startTime.getTime() + durationSeconds * 1000);
  } else if (existing?.end_time) {
    endTime = parseDateInput(existing.end_time, "end_time");
  }

  if (!endTime) {
    throw new Error("end_time or duration_seconds is required");
  }

  if (endTime.getTime() <= startTime.getTime()) {
    throw new Error("end_time must be later than start_time");
  }

  return {
    start_time: startTime.toISOString(),
    end_time: endTime.toISOString()
  };
}

function normalizeCreatePayload(body: any = {}) {
  if (!body.title || typeof body.title !== "string" || body.title.trim().length === 0) {
    throw new Error("title is required");
  }

  if (!body.content || typeof body.content !== "string" || body.content.trim().length === 0) {
    throw new Error("content is required");
  }

  const window = normalizeWindow(body);
  const priority = parseOptionalInteger(body.priority, "priority") ?? 0;

  return {
    announce_id: generateAnnouncementId(),
    locale:
      typeof body.locale === "string" && body.locale.trim().length > 0
        ? body.locale.trim()
        : "default",
    title: body.title.trim(),
    content: body.content.trim(),
    priority,
    type:
      typeof body.type === "string" && body.type.trim().length > 0
        ? body.type.trim()
        : "banner",
    target_group:
      typeof body.target_group === "string" && body.target_group.trim().length > 0
        ? body.target_group.trim()
        : "all",
    start_time: window.start_time,
    end_time: window.end_time
  };
}

function normalizeUpdatePayload(body: any = {}, existing: any) {
  const next = {
    locale:
      body.locale !== undefined
        ? (typeof body.locale === "string" && body.locale.trim().length > 0
            ? body.locale.trim()
            : "default")
        : existing.locale,
    title:
      body.title !== undefined
        ? (typeof body.title === "string" && body.title.trim().length > 0
            ? body.title.trim()
            : "")
        : existing.title,
    content:
      body.content !== undefined
        ? (typeof body.content === "string" && body.content.trim().length > 0
            ? body.content.trim()
            : "")
        : existing.content,
    priority:
      body.priority !== undefined
        ? (parseOptionalInteger(body.priority, "priority") ?? 0)
        : existing.priority,
    type:
      body.type !== undefined
        ? (typeof body.type === "string" && body.type.trim().length > 0
            ? body.type.trim()
            : "banner")
        : existing.type,
    target_group:
      body.target_group !== undefined
        ? (typeof body.target_group === "string" && body.target_group.trim().length > 0
            ? body.target_group.trim()
            : "all")
        : existing.target_group
  };

  if (!next.title) {
    throw new Error("title cannot be empty");
  }

  if (!next.content) {
    throw new Error("content cannot be empty");
  }

  const window = normalizeWindow(body, existing);

  return {
    ...next,
    start_time: window.start_time,
    end_time: window.end_time
  };
}

function buildListCacheKey(options: any) {
  return `${ANNOUNCEMENT_LIST_CACHE_PREFIX}${JSON.stringify({
    locale: options.locale ?? null,
    target_group: options.targetGroup ?? null,
    min_priority: options.minPriority ?? null,
    active_only: Boolean(options.activeOnly),
    limit: options.limit,
    offset: options.offset
  })}`;
}

@Injectable()
export class AnnouncementsService {
  constructor(
    @Inject(ANNOUNCE_STORE) private readonly announcementStore: any,
    @Inject(ANNOUNCE_REDIS) private readonly redis: any = null,
    @Inject(ANNOUNCE_CONFIG) private readonly config: any = {}
  ) {}

  private getListCacheTtlSeconds() {
    const ttl = Number.parseInt(
      String(this.config?.announceCacheTtlSeconds ?? 10),
      10
    );

    return Number.isFinite(ttl) ? ttl : 10;
  }

  private async getCachedList(cacheKey: string) {
    if (!this.redis) {
      return null;
    }

    try {
      const cached = await this.redis.get(cacheKey);
      if (!cached) {
        return null;
      }

      return JSON.parse(cached);
    } catch (error: any) {
      log("warn", "announcement.cache_get_failed", { error: error.message });
      return null;
    }
  }

  private async setCachedList(cacheKey: string, value: any, ttlSeconds: number) {
    if (!this.redis || ttlSeconds <= 0) {
      return;
    }

    try {
      await this.redis.set(cacheKey, JSON.stringify(value), "EX", ttlSeconds);
    } catch (error: any) {
      log("warn", "announcement.cache_set_failed", { error: error.message });
    }
  }

  private async invalidateListCache() {
    if (!this.redis) {
      return;
    }

    try {
      let cursor = "0";
      do {
        const [nextCursor, keys] = await this.redis.scan(
          cursor,
          "MATCH",
          `${ANNOUNCEMENT_LIST_CACHE_PREFIX}*`,
          "COUNT",
          100
        );
        cursor = nextCursor;

        if (keys.length > 0) {
          await this.redis.del(...keys);
        }
      } while (cursor !== "0");
    } catch (error: any) {
      log("warn", "announcement.cache_invalidate_failed", {
        error: error.message
      });
    }
  }

  async list(query: any) {
    let limit;
    let offset;
    let minPriority;
    let activeOnly;

    try {
      limit = Math.max(
        1,
        Math.min(parseOptionalInteger(query.limit, "limit") ?? 50, 100)
      );
      offset = Math.max(
        parseOptionalInteger(query.offset, "offset") ?? 0,
        0
      );
      minPriority = parseOptionalInteger(query.priority, "priority");
      activeOnly = parseBoolean(query.active_only, true);
    } catch (error: any) {
      throw badRequest("INVALID_QUERY", error.message);
    }

    try {
      const listOptions = {
        locale:
          typeof query.locale === "string" && query.locale.trim().length > 0
            ? query.locale.trim()
            : null,
        targetGroup:
          typeof query.target_group === "string" &&
          query.target_group.trim().length > 0
            ? query.target_group.trim()
            : null,
        minPriority,
        activeOnly,
        limit,
        offset
      };

      const cacheTtlSeconds = this.getListCacheTtlSeconds();
      const cacheKey = buildListCacheKey(listOptions);
      if (cacheTtlSeconds > 0) {
        const cached = await this.getCachedList(cacheKey);
        if (cached) {
          return cached;
        }
      }

      const announcements = await this.announcementStore.listAnnouncements(
        listOptions
      );

      const result = {
        ok: true,
        announcements,
        limit,
        offset
      };

      await this.setCachedList(cacheKey, result, cacheTtlSeconds);
      return result;
    } catch (error: any) {
      log("error", "route.list_announcements_failed", { error: error.message });
      throw error;
    }
  }

  async get(announceId: string) {
    try {
      const announcement = await this.announcementStore.getAnnouncementById(announceId);

      if (!announcement) {
        throw notFound("ANNOUNCEMENT_NOT_FOUND", "Announcement not found");
      }

      return {
        ok: true,
        announcement
      };
    } catch (error: any) {
      if (error?.getStatus?.()) {
        throw error;
      }
      log("error", "route.get_announcement_failed", { error: error.message });
      throw error;
    }
  }

  async create(body: any) {
    let announcement;
    try {
      announcement = normalizeCreatePayload(body || {});
    } catch (error: any) {
      throw badRequest("INVALID_ANNOUNCEMENT", error.message);
    }

    try {
      const created = await this.announcementStore.createAnnouncement(announcement);

      log("info", "announcement.created", {
        announceId: created.announce_id,
        locale: created.locale,
        targetGroup: created.target_group
      });

      await this.invalidateListCache();

      return {
        ok: true,
        announcement: created
      };
    } catch (error: any) {
      log("error", "route.create_announcement_failed", { error: error.message });
      throw error;
    }
  }

  async update(announceId: string, body: any) {
    try {
      const existing = await this.announcementStore.getAnnouncementById(announceId);

      if (!existing) {
        throw notFound("ANNOUNCEMENT_NOT_FOUND", "Announcement not found");
      }

      let patch;
      try {
        patch = normalizeUpdatePayload(body || {}, existing);
      } catch (error: any) {
        throw badRequest("INVALID_ANNOUNCEMENT", error.message);
      }

      const updated = await this.announcementStore.updateAnnouncement(announceId, patch);

      log("info", "announcement.updated", { announceId });

      await this.invalidateListCache();

      return {
        ok: true,
        announcement: updated
      };
    } catch (error: any) {
      if (error?.getStatus?.()) {
        throw error;
      }
      log("error", "route.update_announcement_failed", { error: error.message });
      throw error;
    }
  }

  async delete(announceId: string) {
    try {
      const deleted = await this.announcementStore.deleteAnnouncement(announceId);

      if (!deleted) {
        throw notFound("ANNOUNCEMENT_NOT_FOUND", "Announcement not found");
      }

      log("info", "announcement.deleted", { announceId });

      await this.invalidateListCache();

      return {
        ok: true,
        deleted: true
      };
    } catch (error: any) {
      if (error?.getStatus?.()) {
        throw error;
      }
      log("error", "route.delete_announcement_failed", { error: error.message });
      throw error;
    }
  }
}
