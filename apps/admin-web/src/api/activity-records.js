export const ACTIVITY_RECORD_STATUS_LABELS = Object.freeze({
  processing: "处理中",
  pending: "处理中",
  retryable_failure: "可重试",
  capacity_insufficient: "容量不足",
  capacity_exhausted: "容量不足",
  capacity_blocked: "容量不足",
  insufficient_capacity: "容量不足",
  manual_review: "人工复核",
  reconciliation_pending: "人工复核",
  permanent_failure: "永久失败",
  granted: "已发放",
  success: "成功",
  failed: "失败"
});

export function recordStatusLabel(status) {
  return ACTIVITY_RECORD_STATUS_LABELS[String(status)] || (status ? String(status) : "未知");
}

export function recordStatusTag(status) {
  if (["granted", "success"].includes(String(status))) return "success";
  if (["processing", "pending"].includes(String(status))) return "info";
  if (["retryable_failure", "capacity_insufficient", "capacity_exhausted", "capacity_blocked", "insufficient_capacity"].includes(String(status))) return "warning";
  if (["manual_review", "reconciliation_pending"].includes(String(status))) return "warning";
  if (["permanent_failure", "failed"].includes(String(status))) return "danger";
  return "info";
}

export function normalizeRecordsResponse(payload) {
  const value = payload?.data ?? payload ?? {};
  return { items: Array.isArray(value.items) ? value.items : [], total: Number(value.total) || 0, limit: Number(value.limit) || 50, offset: Number(value.offset) || 0 };
}

export function normalizePreflightResponse(payload) {
  const value = payload?.data ?? payload ?? {};
  return { valid: value.valid === true, errors: Array.isArray(value.errors) ? value.errors : [], activityId: value.activityId, version: value.version };
}

export function preflightErrorSuggestions(error) {
  const details = Array.isArray(error?.details) ? error.details : Array.isArray(error?.response?.data?.details) ? error.response.data.details : [];
  return details.map((item) => ({
    path: item.path || "活动配置",
    code: item.code || "INVALID",
    message: item.message || "配置不符合要求",
    suggestion: item.suggestion || item.fix || "请根据字段要求修正后重试"
  }));
}
