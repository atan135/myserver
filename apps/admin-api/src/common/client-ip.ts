function normalizeIp(value: unknown): string | null {
  if (!value) {
    return null;
  }

  let ip = String(value).trim();
  if (!ip) {
    return null;
  }

  if (ip.startsWith("[") && ip.includes("]")) {
    ip = ip.slice(1, ip.indexOf("]"));
  } else {
    const ipv4WithPort = ip.match(/^(\d+\.\d+\.\d+\.\d+):\d+$/);
    if (ipv4WithPort) {
      ip = ipv4WithPort[1];
    }
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

function ipv4ToInt(ip: string): number | null {
  const parts = ip.split(".");
  if (parts.length !== 4) {
    return null;
  }

  let result = 0;
  for (const part of parts) {
    if (!/^\d+$/.test(part)) {
      return null;
    }
    const value = Number.parseInt(part, 10);
    if (value < 0 || value > 255) {
      return null;
    }
    result = (result << 8) + value;
  }

  return result >>> 0;
}

export function ipMatchesEntry(ipValue: unknown, entryValue: unknown): boolean {
  const ip = normalizeIp(ipValue);
  const entry = normalizeIp(entryValue);
  if (!ip || !entry) {
    return false;
  }

  const cidrParts = entry.split("/");
  if (cidrParts.length === 1) {
    return ip === entry;
  }

  if (cidrParts.length !== 2) {
    return false;
  }

  const [networkIp, prefixText] = cidrParts;
  const prefix = Number.parseInt(prefixText, 10);
  const ipInt = ipv4ToInt(ip);
  const networkInt = ipv4ToInt(networkIp);
  if (ipInt === null || networkInt === null || !Number.isInteger(prefix) || prefix < 0 || prefix > 32) {
    return false;
  }

  const mask = prefix === 0 ? 0 : (0xffffffff << (32 - prefix)) >>> 0;
  return (ipInt & mask) === (networkInt & mask);
}

export function ipMatchesAny(ipValue: unknown, entries: unknown[] = []): boolean {
  return entries.some((entry) => ipMatchesEntry(ipValue, entry));
}

export function isTrustedProxy(req: any, config: any = {}): boolean {
  if (!config.trustProxy) {
    return false;
  }

  const remoteIp = getRemoteIp(req);
  const trustedProxies = config.trustedProxies || [];
  return Boolean(remoteIp && trustedProxies.length > 0 && ipMatchesAny(remoteIp, trustedProxies));
}

export function getRemoteIp(req: any): string | null {
  return normalizeIp(req?.socket?.remoteAddress || req?.raw?.socket?.remoteAddress || req?.ip || req?.raw?.ip);
}

export function getClientIp(req: any, config: any = {}): string | null {
  const remoteIp = getRemoteIp(req);

  if (!isTrustedProxy(req, config)) {
    return remoteIp;
  }

  const forwardedIps = parseCsv(req?.headers?.["x-forwarded-for"]);
  return forwardedIps[0] || remoteIp;
}

export function isRequestSecure(req: any, config: any = {}): boolean {
  if (req?.secure === true || req?.socket?.encrypted === true || req?.raw?.socket?.encrypted === true) {
    return true;
  }

  if (!isTrustedProxy(req, config)) {
    return false;
  }

  const forwardedProto = String(req?.headers?.["x-forwarded-proto"] || "")
    .split(",")[0]
    .trim()
    .toLowerCase();
  return forwardedProto === "https";
}
