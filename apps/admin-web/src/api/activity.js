import { activityApi } from "./index.js";

export const ACTIVITY_STATUSES = ["draft", "published", "offline"];
export const ACTIVITY_TYPES = ["login_reward", "lottery"];

export function activityError(error, fallback = "活动操作失败") {
  const response = error?.response;
  const payload = response?.data;
  const status = response?.status;
  const code = payload?.error || (status === 403 ? "FORBIDDEN" : status === 409 ? "VERSION_CONFLICT" : "NETWORK_ERROR");
  const details = Array.isArray(payload?.details) ? payload.details : [];
  const message = payload?.message || (status === 403 ? "当前账号无权限执行该操作" : status === 409 ? "活动版本已被其他操作员修改，请刷新后重试" : fallback);
  return { code, status, message, details, retryable: !response || status >= 500 };
}

export function normalizeActivityListResponse(payload) {
  const value = payload?.data ?? payload ?? {};
  return {
    items: Array.isArray(value.items) ? value.items : [],
    total: Number.isFinite(Number(value.total)) ? Number(value.total) : 0,
    limit: Number(value.limit) || 50,
    offset: Number(value.offset) || 0
  };
}

export function filterActivities(items, filters = {}) {
  const key = String(filters.key ?? "").trim().toLowerCase();
  const from = filters.from ? Date.parse(filters.from) : Number.NEGATIVE_INFINITY;
  const to = filters.to ? Date.parse(filters.to) : Number.POSITIVE_INFINITY;
  return (Array.isArray(items) ? items : []).filter((item) => {
    if (filters.status && item.status !== filters.status) return false;
    if (filters.activityType && item.activityType !== filters.activityType) return false;
    if (key && !String(item.key ?? "").toLowerCase().includes(key)) return false;
    const start = Date.parse(item.startAt ?? item.draft?.startAt ?? "");
    const end = Date.parse(item.endAt ?? item.draft?.endAt ?? "");
    if (Number.isFinite(from) && Number.isFinite(end) && end < from) return false;
    if (Number.isFinite(to) && Number.isFinite(start) && start > to) return false;
    return true;
  });
}

export function buildVersionCommand(detail, reason) {
  return {
    version: Number(detail?.version),
    ifMatch: detail?.etag,
    reason: String(reason ?? "").trim()
  };
}

export function draftIsDirty(current, snapshot) {
  return JSON.stringify(current ?? {}) !== String(snapshot ?? "");
}

export { activityApi };
