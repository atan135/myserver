export function badRequest(res, error, message) {
  return res.status(400).json({
    ok: false,
    error,
    message: message || error
  });
}

export function unauthorized(res, error, message) {
  return res.status(401).json({
    ok: false,
    error,
    message: message || error
  });
}

export function forbidden(res, error, message) {
  return res.status(403).json({
    ok: false,
    error,
    message: message || error
  });
}

export function notFound(res, error, message) {
  return res.status(404).json({
    ok: false,
    error,
    message: message || error
  });
}

export function conflict(res, error, message) {
  return res.status(409).json({
    ok: false,
    error,
    message: message || error
  });
}

export function rateLimited(res, error, message) {
  return res.status(429).json({
    ok: false,
    error,
    message: message || error
  });
}
