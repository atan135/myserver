export function badRequest(res, error, message) {
  return res.status(400).json({
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
