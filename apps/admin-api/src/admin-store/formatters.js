const OPERATION_AUDIT_SENSITIVE_KEY = /password|token|secret|private.?key|authorization|cookie|ticket|nonce|payload|assertion|endpoint|host|port|credential/i;
const OPERATION_AUDIT_UNBOUNDED_TEXT_KEY = /content|message|prompt|broadcast|body/i;

export function toIsoString(value) {
  if (!value) {
    return null;
  }

  if (value instanceof Date) {
    return value.toISOString();
  }

  return String(value);
}

export function toJsonb(value) {
  return value ? JSON.stringify(value) : null;
}

export function toRequiredJsonb(value) {
  return JSON.stringify(value ?? {});
}

export function normalizeJson(value) {
  if (value === undefined || value === null) {
    return null;
  }

  if (typeof value !== "string") {
    return value;
  }

  try {
    return JSON.parse(value);
  } catch {
    return value;
  }
}

export function toNumericId(value) {
  if (value === null || value === undefined) {
    return value;
  }
  const numeric = Number(value);
  return Number.isSafeInteger(numeric) ? numeric : value;
}

export function nextParam(params) {
  return `$${params.length}`;
}

export function operationIsTerminal(status) {
  return new Set(["succeeded", "failed", "execution_uncertain", "cancelled"]).has(status);
}

export function normalizeOptionalString(value) {
  if (typeof value !== "string") {
    return null;
  }

  const normalized = value.trim();
  return normalized.length > 0 ? normalized : null;
}

export function redactOperationAuditValue(value, key = "", depth = 0) {
  if (OPERATION_AUDIT_SENSITIVE_KEY.test(key) || OPERATION_AUDIT_UNBOUNDED_TEXT_KEY.test(key)) {
    return "[REDACTED]";
  }
  if (depth > 6) {
    return "[TRUNCATED]";
  }
  if (value === null || value === undefined || typeof value === "boolean" || typeof value === "number") {
    return value ?? null;
  }
  if (typeof value === "string") {
    return Buffer.byteLength(value, "utf8") <= 1024 ? value : "[TRUNCATED]";
  }
  if (Array.isArray(value)) {
    return value.slice(0, 100).map((entry) => redactOperationAuditValue(entry, "", depth + 1));
  }
  if (typeof value !== "object") {
    return "[REDACTED]";
  }
  return Object.fromEntries(
    Object.entries(value).slice(0, 100).map(([entryKey, entryValue]) => [
      entryKey,
      redactOperationAuditValue(entryValue, entryKey, depth + 1)
    ])
  );
}
