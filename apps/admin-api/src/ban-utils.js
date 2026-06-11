export function computeBanExpiresAt(durationSeconds, now = new Date()) {
  return new Date(now.getTime() + durationSeconds * 1000).toISOString();
}
