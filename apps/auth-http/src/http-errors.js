export function badRequest(res, error, message) {
  return res.status(400).json({
    ok: false,
    error,
    message
  });
}

export function unauthorized(res, error = "UNAUTHORIZED") {
  return res.status(401).json({
    ok: false,
    error
  });
}

export function rateLimited(res, error = "RATE_LIMITED", message) {
  return res.status(429).json({
    ok: false,
    error,
    message
  });
}

export function forbidden(res, error = "FORBIDDEN", message) {
  return res.status(403).json({
    ok: false,
    error,
    message
  });
}
