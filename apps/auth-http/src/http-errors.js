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
