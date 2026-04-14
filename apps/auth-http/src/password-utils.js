import crypto from "node:crypto";

const LOGIN_NAME_PATTERN = /^[a-z0-9._-]{3,64}$/;
const GUEST_ID_PATTERN = /^[a-z0-9._-]{3,128}$/;

export function normalizeLoginName(value) {
  return String(value || "")
    .trim()
    .toLowerCase();
}

export function normalizeGuestId(value) {
  return String(value || "").trim();
}

export function assertValidLoginName(value) {
  const normalized = normalizeLoginName(value);
  if (!LOGIN_NAME_PATTERN.test(normalized)) {
    throw new Error(
      "loginName must be 3-64 chars and only contain a-z, 0-9, dot, underscore or dash"
    );
  }

  return normalized;
}

export function assertValidGuestId(value) {
  const normalized = normalizeGuestId(value);
  if (!GUEST_ID_PATTERN.test(normalized)) {
    throw new Error(
      "guestId must be 3-128 chars and only contain a-z, 0-9, dot, underscore or dash"
    );
  }
  return normalized;
}

export function createPasswordSalt() {
  return crypto.randomBytes(16).toString("hex");
}

export function hashPassword(password, salt) {
  return crypto.scryptSync(String(password || ""), String(salt || ""), 64).toString("hex");
}

export function verifyPassword(password, salt, expectedHash) {
  if (!salt || !expectedHash) {
    return false;
  }

  const actualBuffer = Buffer.from(hashPassword(password, salt), "hex");
  const expectedBuffer = Buffer.from(String(expectedHash), "hex");

  if (actualBuffer.length !== expectedBuffer.length) {
    return false;
  }

  return crypto.timingSafeEqual(actualBuffer, expectedBuffer);
}
