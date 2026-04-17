import { Router } from "express";
import { v4 as uuidv4 } from "uuid";

import { badRequest, notFound } from "./http-errors.js";
import { log } from "./logger.js";

function parseBoolean(value, fallback) {
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

function parseOptionalInteger(value, fieldName) {
  if (value === undefined || value === null || value === "") {
    return null;
  }

  const parsed = Number.parseInt(String(value), 10);
  if (!Number.isInteger(parsed)) {
    throw new Error(`${fieldName} must be an integer`);
  }

  return parsed;
}

function parseDateInput(value, fieldName) {
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

function normalizeWindow(body, existing = null) {
  const startInput =
    body.start_time !== undefined ? body.start_time : existing?.start_time;
  const startTime = parseDateInput(startInput, "start_time") || new Date();

  let endTime = null;
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

function normalizeCreatePayload(body = {}) {
  if (!body.title || typeof body.title !== "string" || body.title.trim().length === 0) {
    throw new Error("title is required");
  }

  if (!body.content || typeof body.content !== "string" || body.content.trim().length === 0) {
    throw new Error("content is required");
  }

  const window = normalizeWindow(body);
  const priority = parseOptionalInteger(body.priority, "priority") ?? 0;

  return {
    announce_id: uuidv4(),
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

function normalizeUpdatePayload(body = {}, existing) {
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

export function createRoutes(config, announcementStore) {
  const router = Router();

  router.get("/healthz", (_req, res) => {
    res.json({
      ok: true,
      service: config.appName,
      env: config.env,
      storage: config.mysqlEnabled ? "mysql" : "memory"
    });
  });

  router.get("/api/v1/announcements", async (req, res) => {
    let limit;
    let offset;
    let minPriority;
    let activeOnly;

    try {
      limit = Math.max(
        1,
        Math.min(parseOptionalInteger(req.query.limit, "limit") ?? 50, 100)
      );
      offset = Math.max(
        parseOptionalInteger(req.query.offset, "offset") ?? 0,
        0
      );
      minPriority = parseOptionalInteger(req.query.priority, "priority");
      activeOnly = parseBoolean(req.query.active_only, true);
    } catch (error) {
      return badRequest(res, "INVALID_QUERY", error.message);
    }

    try {
      const announcements = await announcementStore.listAnnouncements({
        locale:
          typeof req.query.locale === "string" && req.query.locale.trim().length > 0
            ? req.query.locale.trim()
            : null,
        targetGroup:
          typeof req.query.target_group === "string" &&
          req.query.target_group.trim().length > 0
            ? req.query.target_group.trim()
            : null,
        minPriority,
        activeOnly,
        limit,
        offset
      });

      return res.json({
        ok: true,
        announcements,
        limit,
        offset
      });
    } catch (error) {
      log("error", "route.list_announcements_failed", {
        error: error.message
      });
      return res.status(500).json({
        ok: false,
        error: "INTERNAL_ERROR"
      });
    }
  });

  router.get("/api/v1/announcements/:announceId", async (req, res) => {
    try {
      const announcement = await announcementStore.getAnnouncementById(
        req.params.announceId
      );

      if (!announcement) {
        return notFound(
          res,
          "ANNOUNCEMENT_NOT_FOUND",
          "Announcement not found"
        );
      }

      return res.json({
        ok: true,
        announcement
      });
    } catch (error) {
      log("error", "route.get_announcement_failed", {
        error: error.message
      });
      return res.status(500).json({
        ok: false,
        error: "INTERNAL_ERROR"
      });
    }
  });

  router.post("/api/v1/announcements", async (req, res) => {
    try {
      let announcement;
      try {
        announcement = normalizeCreatePayload(req.body || {});
      } catch (error) {
        return badRequest(res, "INVALID_ANNOUNCEMENT", error.message);
      }

      const created = await announcementStore.createAnnouncement(announcement);

      log("info", "announcement.created", {
        announceId: created.announce_id,
        locale: created.locale,
        targetGroup: created.target_group
      });

      return res.status(201).json({
        ok: true,
        announcement: created
      });
    } catch (error) {
      log("error", "route.create_announcement_failed", {
        error: error.message
      });
      return res.status(500).json({
        ok: false,
        error: "INTERNAL_ERROR"
      });
    }
  });

  router.put("/api/v1/announcements/:announceId", async (req, res) => {
    try {
      const existing = await announcementStore.getAnnouncementById(
        req.params.announceId
      );

      if (!existing) {
        return notFound(
          res,
          "ANNOUNCEMENT_NOT_FOUND",
          "Announcement not found"
        );
      }

      let patch;
      try {
        patch = normalizeUpdatePayload(req.body || {}, existing);
      } catch (error) {
        return badRequest(res, "INVALID_ANNOUNCEMENT", error.message);
      }

      const updated = await announcementStore.updateAnnouncement(
        req.params.announceId,
        patch
      );

      log("info", "announcement.updated", {
        announceId: req.params.announceId
      });

      return res.json({
        ok: true,
        announcement: updated
      });
    } catch (error) {
      log("error", "route.update_announcement_failed", {
        error: error.message
      });
      return res.status(500).json({
        ok: false,
        error: "INTERNAL_ERROR"
      });
    }
  });

  router.delete("/api/v1/announcements/:announceId", async (req, res) => {
    try {
      const deleted = await announcementStore.deleteAnnouncement(
        req.params.announceId
      );

      if (!deleted) {
        return notFound(
          res,
          "ANNOUNCEMENT_NOT_FOUND",
          "Announcement not found"
        );
      }

      log("info", "announcement.deleted", {
        announceId: req.params.announceId
      });

      return res.json({
        ok: true,
        deleted: true
      });
    } catch (error) {
      log("error", "route.delete_announcement_failed", {
        error: error.message
      });
      return res.status(500).json({
        ok: false,
        error: "INTERNAL_ERROR"
      });
    }
  });

  return router;
}
