export function badRequest(res, error, message) {
  return res.status(400).json({ ok: false, error, message });
}

export function unauthorized(res, error = "UNAUTHORIZED", message) {
  return res.status(401).json({ ok: false, error, message });
}

export function forbidden(res, error = "FORBIDDEN", message) {
  return res.status(403).json({ ok: false, error, message });
}

export function notFound(res, error = "NOT_FOUND", message) {
  return res.status(404).json({ ok: false, error, message });
}
