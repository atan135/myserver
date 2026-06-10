function normalizeIp(value) {
  if (!value) {
    return null;
  }

  const ip = String(value).trim();
  if (!ip) {
    return null;
  }

  if (ip.startsWith("::ffff:")) {
    return ip.slice("::ffff:".length);
  }

  return ip;
}

function parseCsv(value) {
  if (typeof value !== "string") {
    return [];
  }

  return value
    .split(",")
    .map((item) => normalizeIp(item))
    .filter(Boolean);
}

export function getClientIp(req, config = {}) {
  const remoteIp = normalizeIp(req?.ip || req?.socket?.remoteAddress);
  const trustProxy = Boolean(config.trustProxy);

  if (!trustProxy) {
    return remoteIp;
  }

  const trustedProxies = config.trustedProxies || [];
  if (trustedProxies.length > 0 && !trustedProxies.includes(remoteIp)) {
    return remoteIp;
  }

  const forwardedFor = req?.headers?.["x-forwarded-for"];
  const forwardedIps = parseCsv(forwardedFor);
  return forwardedIps[0] || remoteIp;
}
