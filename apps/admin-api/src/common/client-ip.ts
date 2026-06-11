function normalizeIp(value: unknown): string | null {
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

function parseCsv(value: unknown): string[] {
  if (typeof value !== "string") {
    return [];
  }

  return value
    .split(",")
    .map((item) => normalizeIp(item))
    .filter(Boolean) as string[];
}

export function getClientIp(req: any, config: any = {}): string | null {
  const remoteIp = normalizeIp(req?.ip || req?.socket?.remoteAddress);
  const trustProxy = Boolean(config.trustProxy);

  if (!trustProxy) {
    return remoteIp;
  }

  const trustedProxies = (config.trustedProxies || [])
    .map((item: unknown) => normalizeIp(item))
    .filter(Boolean);
  if (trustedProxies.length === 0 || !trustedProxies.includes(remoteIp)) {
    return remoteIp;
  }

  const forwardedIps = parseCsv(req?.headers?.["x-forwarded-for"]);
  return forwardedIps[0] || remoteIp;
}
