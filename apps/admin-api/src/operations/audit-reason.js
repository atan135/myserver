const EXPLICIT_CREDENTIAL_PATTERNS = [
  /\b(?:password|passwd|pwd|token|secret|api[-_]?key|private[-_]?key|authorization|cookie|ticket|session(?:[-_]?id)?)\b\s*(?:=|:)\s*(?:"[^"]+"|'[^']+'|[^\s,;]{4,})/i,
  /\b(?:bearer|basic)\s+[A-Za-z0-9._~+/=-]{8,}\b/i,
  /-----BEGIN(?: [A-Z0-9]+)* PRIVATE KEY-----/i,
  /\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b/
];

export function containsSensitiveAuditReason(value) {
  return typeof value === "string" && EXPLICIT_CREDENTIAL_PATTERNS.some((pattern) => pattern.test(value));
}

export function redactAuditReason(value) {
  return containsSensitiveAuditReason(value) ? "[REDACTED: potential credential]" : value;
}
