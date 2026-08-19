const SENSITIVE_TEXT = /\b(?:password|passwd|pwd|token|secret|api[-_]?key|private[-_]?key|authorization|cookie|session(?:[-_]?id)?)\b\s*(?:=|:)|\b(?:bearer|basic)\s+[A-Za-z0-9._~+/=-]{8,}\b|-----BEGIN(?: [A-Z0-9]+)* PRIVATE KEY-----|\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b/i;

function text(value, maxLength = 512) {
  const normalized = typeof value === "string" ? value.trim() : "";
  return normalized && normalized.length <= maxLength && !SENSITIVE_TEXT.test(normalized) ? normalized : "";
}

export function approvalEvidenceSummary(value) {
  const summary = text(value);
  return summary ? { summary } : null;
}

export function rejectionReason(value) {
  return text(value);
}

export function isSelfApproval(operation, currentAdminId) {
  const requesterId = operation?.requester?.adminId;
  return requesterId !== undefined && requesterId !== null && String(requesterId) === String(currentAdminId ?? "");
}

export function canDecideApproval(operation, currentAdminId, evidence, rejection = "", status = "approved") {
  if (operation?.approvalStatus !== "pending" || isSelfApproval(operation, currentAdminId)) return false;
  if (!approvalEvidenceSummary(evidence)) return false;
  return status !== "rejected" || Boolean(rejectionReason(rejection));
}

export function approvalDecisionPayload(status, evidence, rejection = "") {
  const evidenceSummary = approvalEvidenceSummary(evidence);
  if (!evidenceSummary || !["approved", "rejected"].includes(status)) return null;
  if (status === "rejected") {
    const rejectionReasonText = rejectionReason(rejection);
    if (!rejectionReasonText) return null;
    return { status, evidenceSummary, rejectionReason: rejectionReasonText };
  }
  return { status, evidenceSummary };
}

export function approvalStatusType(status) {
  return {
    pending: "warning",
    approved: "success",
    rejected: "danger",
    not_required: "info"
  }[status] || "info";
}

export function operationStatusType(status) {
  return {
    preflighted: "warning",
    approved: "success",
    executing: "warning",
    succeeded: "success",
    failed: "danger",
    execution_uncertain: "danger",
    cancelled: "info"
  }[status] || "info";
}
