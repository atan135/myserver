import { randomUUID } from "node:crypto";

import { badRequest, unauthorized } from "../common/http-exception.js";

export const MAIL_ID_PATTERN = /^[A-Za-z0-9:_-]{1,64}$/;

const MAX_PUBLIC_HEADER_BYTES = 16 * 1024;
const MAX_GAME_TICKET_BYTES = 4096;
const MAX_PUBLIC_BODY_BYTES = 1024;
const MAX_LOCALE_BYTES = 128;
const LOCALE_PATTERN = /^(?:\*|[A-Za-z]{2,3}(?:-[A-Za-z0-9]{2,8})?)(?:\s*;\s*q=(?:0(?:\.\d{1,3})?|1(?:\.0{1,3})?))?(?:\s*,\s*(?:\*|[A-Za-z]{2,3}(?:-[A-Za-z0-9]{2,8})?)(?:\s*;\s*q=(?:0(?:\.\d{1,3})?|1(?:\.0{1,3})?))?)*$/;
const REQUEST_ID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const SINGLETON_HEADERS = new Set([
  "x-game-ticket",
  "authorization",
  "proxy-authorization",
  "x-service-token",
  "x-admin-token",
  "content-length",
  "transfer-encoding",
  "host",
  "x-request-id",
  "x-forwarded-for",
  "x-forwarded-proto",
  "x-forwarded-host",
  "x-real-ip"
]);
const FORBIDDEN_PLAYER_AUTH_HEADERS = new Set([
  "authorization",
  "proxy-authorization",
  "x-service-token",
  "x-admin-token",
  "cookie"
]);
const LIST_QUERY_KEYS = new Set(["status", "limit", "offset"]);
const MAIL_STATUSES = new Set(["unread", "read", "claiming", "claimed"]);

function publicBadRequest() {
  return badRequest("INVALID_PLAYER_MAIL_REQUEST", "Mail request is invalid");
}

function publicTicketInvalid() {
  return unauthorized("MAIL_PLAYER_TICKET_INVALID", "Player ticket is invalid");
}

function requestRawHeaders(request) {
  const rawHeaders = request?.raw?.rawHeaders ?? request?.rawHeaders;
  return Array.isArray(rawHeaders) ? rawHeaders : null;
}

function normalizedHeaderName(name) {
  return String(name || "").trim().toLowerCase();
}

function headerValues(headers = {}, request, name) {
  const normalizedName = normalizedHeaderName(name);
  const rawHeaders = requestRawHeaders(request);
  if (rawHeaders) {
    const values = [];
    for (let index = 0; index + 1 < rawHeaders.length; index += 2) {
      if (normalizedHeaderName(rawHeaders[index]) === normalizedName) {
        values.push(String(rawHeaders[index + 1] ?? ""));
      }
    }
    return values;
  }

  const value = headers?.[normalizedName] ?? headers?.[name];
  if (Array.isArray(value)) return value.map((item) => String(item));
  return value === undefined ? [] : [String(value)];
}

function headerByteLength(headers = {}, request) {
  const rawHeaders = requestRawHeaders(request);
  if (rawHeaders) {
    let total = 0;
    for (let index = 0; index + 1 < rawHeaders.length; index += 2) {
      total += Buffer.byteLength(String(rawHeaders[index]), "utf8");
      total += Buffer.byteLength(String(rawHeaders[index + 1]), "utf8");
      total += 4;
    }
    return total;
  }

  return Object.entries(headers || {}).reduce(
    (total, [name, value]) => total + Buffer.byteLength(String(name), "utf8") + Buffer.byteLength(String(value), "utf8") + 4,
    0
  );
}

function onlyHeaderValue(headers, request, name) {
  const values = headerValues(headers, request, name);
  if (values.length !== 1 || values[0].includes(",")) {
    throw publicBadRequest();
  }
  return values[0].trim();
}

function optionalHeaderValue(headers, request, name, allowCommas = false) {
  const values = headerValues(headers, request, name);
  if (values.length === 0) return "";
  if (values.length !== 1 || (!allowCommas && values[0].includes(","))) {
    throw publicBadRequest();
  }
  return values[0].trim();
}

function normalizeIp(value) {
  if (!value) return null;
  const ip = String(value).trim();
  if (!ip) return null;
  if (ip.startsWith("[") && ip.includes("]")) return ip.slice(1, ip.indexOf("]"));
  const ipv4WithPort = ip.match(/^(\d+\.\d+\.\d+\.\d+):\d+$/);
  if (ipv4WithPort) return ipv4WithPort[1];
  return ip.startsWith("::ffff:") ? ip.slice("::ffff:".length) : ip;
}

function ipv4ToInt(ip) {
  const parts = String(ip || "").split(".");
  if (parts.length !== 4) return null;
  let result = 0;
  for (const part of parts) {
    if (!/^\d+$/.test(part)) return null;
    const value = Number.parseInt(part, 10);
    if (value < 0 || value > 255) return null;
    result = (result << 8) + value;
  }
  return result >>> 0;
}

export function ipMatchesTrustedProxy(ipValue, entryValue) {
  const ip = normalizeIp(ipValue);
  const entry = normalizeIp(entryValue);
  if (!ip || !entry) return false;
  const parts = entry.split("/");
  if (parts.length === 1) return ip === entry;
  if (parts.length !== 2) return false;
  const [networkIp, prefixText] = parts;
  const prefix = Number.parseInt(prefixText, 10);
  const ipInt = ipv4ToInt(ip);
  const networkInt = ipv4ToInt(networkIp);
  if (ipInt === null || networkInt === null || !Number.isInteger(prefix) || prefix < 0 || prefix > 32) {
    return false;
  }
  const mask = prefix === 0 ? 0 : (0xffffffff << (32 - prefix)) >>> 0;
  return (ipInt & mask) === (networkInt & mask);
}

export function isValidTrustedProxyEntry(value) {
  const normalized = String(value || "").trim();
  if (!normalized) return false;
  const [address, prefix] = normalized.split("/");
  if (normalized.split("/").length > 2 || !normalizeIp(address)) return false;
  if (prefix === undefined) return Boolean(ipv4ToInt(normalizeIp(address))) || /^[0-9a-f:]+$/i.test(normalizeIp(address));
  return /^\d+$/.test(prefix) && Number(prefix) >= 0 && Number(prefix) <= 32 && ipv4ToInt(normalizeIp(address)) !== null;
}

export function getRequestRemoteIp(request) {
  return normalizeIp(
    request?.raw?.socket?.remoteAddress ?? request?.socket?.remoteAddress ?? request?.ip ?? request?.raw?.ip
  );
}

export function publicMailRouteTemplate(pathname) {
  const path = String(pathname || "").split("?", 1)[0];
  if (path === "/api/v1/mails") return "mail_list";
  if (new RegExp(`^/api/v1/mails/${MAIL_ID_PATTERN.source.slice(1, -1)}$`).test(path)) return "mail_detail";
  if (new RegExp(`^/api/v1/mails/${MAIL_ID_PATTERN.source.slice(1, -1)}/read$`).test(path)) return "mail_read";
  if (new RegExp(`^/api/v1/mails/${MAIL_ID_PATTERN.source.slice(1, -1)}/claim$`).test(path)) return "mail_claim";
  return null;
}

export function publicMailPathForLog(pathname) {
  return publicMailRouteTemplate(pathname) || "mail_unknown";
}

export function validatePublicPlayerHeaders(headers = {}, request, operation, config = {}) {
  if (headerByteLength(headers, request) > MAX_PUBLIC_HEADER_BYTES) {
    throw publicBadRequest();
  }

  for (const name of SINGLETON_HEADERS) {
    const values = headerValues(headers, request, name);
    if (values.length > 1 || values.some((value) => value.includes(","))) {
      throw publicBadRequest();
    }
  }

  for (const name of FORBIDDEN_PLAYER_AUTH_HEADERS) {
    if (headerValues(headers, request, name).length > 0) {
      throw publicBadRequest();
    }
  }

  if (headerValues(headers, request, "transfer-encoding").length > 0) {
    throw publicBadRequest();
  }
  if (headerValues(headers, request, "x-http-method-override").length > 0 ||
      headerValues(headers, request, "x-method-override").length > 0) {
    throw publicBadRequest();
  }

  const ticket = optionalHeaderValue(headers, request, "x-game-ticket");
  if (ticket && (Buffer.byteLength(ticket, "utf8") > MAX_GAME_TICKET_BYTES || !/^[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$/.test(ticket))) {
    throw publicTicketInvalid();
  }

  const locale = optionalHeaderValue(headers, request, "accept-language", true);
  if (locale && (Buffer.byteLength(locale, "utf8") > MAX_LOCALE_BYTES || !LOCALE_PATTERN.test(locale))) {
    throw publicBadRequest();
  }

  const contentLength = optionalHeaderValue(headers, request, "content-length");
  if (contentLength && (!/^\d+$/.test(contentLength) || Number(contentLength) > MAX_PUBLIC_BODY_BYTES)) {
    throw publicBadRequest();
  }
  if ((operation === "list" || operation === "detail") && contentLength && Number(contentLength) !== 0) {
    throw publicBadRequest();
  }

  if (operation === "read" || operation === "claim") {
    const contentType = optionalHeaderValue(headers, request, "content-type");
    if (contentType && !/^application\/json(?:\s*;\s*charset=utf-8)?$/i.test(contentType)) {
      throw publicBadRequest();
    }
  }

  const remoteIp = getRequestRemoteIp(request);
  const trustedProxy = Boolean(
    config.mailTrustProxy === true &&
    remoteIp &&
    (config.mailTrustedProxyCidrs || []).some((entry) => ipMatchesTrustedProxy(remoteIp, entry))
  );
  if (!trustedProxy) {
    return { clientIp: remoteIp || "unknown", requestId: randomUUID(), trustedProxy: false };
  }

  const forwardedFor = onlyHeaderValue(headers, request, "x-forwarded-for");
  const realIp = onlyHeaderValue(headers, request, "x-real-ip");
  const forwardedProto = onlyHeaderValue(headers, request, "x-forwarded-proto").toLowerCase();
  const requestId = onlyHeaderValue(headers, request, "x-request-id");
  const forwardedIp = normalizeIp(forwardedFor);
  if (!forwardedIp || forwardedIp !== normalizeIp(realIp) || forwardedProto !== "https" || !REQUEST_ID_PATTERN.test(requestId)) {
    throw publicBadRequest();
  }
  return { clientIp: forwardedIp, requestId, trustedProxy: true };
}

export function validateMailId(mailId) {
  if (typeof mailId !== "string" || !MAIL_ID_PATTERN.test(mailId)) {
    throw publicBadRequest();
  }
  return mailId;
}

function isPlainObject(value) {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return false;
  const prototype = Object.getPrototypeOf(value);
  return prototype === Object.prototype || prototype === null;
}

export function validateEmptyPlayerMutationBody(body) {
  if (body === undefined || body === null) return {};
  if (!isPlainObject(body) || Object.keys(body).length !== 0) {
    throw publicBadRequest();
  }
  return {};
}

export function validateListQuery(query) {
  if (!isPlainObject(query)) throw publicBadRequest();
  for (const key of Object.keys(query)) {
    if (!LIST_QUERY_KEYS.has(key) || key === "_method") throw publicBadRequest();
  }

  const status = query.status === undefined ? undefined : String(query.status);
  if (status !== undefined && !MAIL_STATUSES.has(status)) throw publicBadRequest();

  const parseBoundedInteger = (value, fallback, min, max) => {
    if (value === undefined) return fallback;
    if (typeof value !== "string" || !/^\d+$/.test(value)) throw publicBadRequest();
    const parsed = Number.parseInt(value, 10);
    if (!Number.isSafeInteger(parsed) || parsed < min || parsed > max) throw publicBadRequest();
    return parsed;
  };

  return {
    ...(status ? { status } : {}),
    limit: parseBoundedInteger(query.limit, 50, 1, 50),
    offset: parseBoundedInteger(query.offset, 0, 0, 10_000)
  };
}

export function validateEmptyPlayerQuery(query) {
  if (!isPlainObject(query) || Object.keys(query).length !== 0) throw publicBadRequest();
  return {};
}

export function publicResultClass(status) {
  if (status === 202) return "accepted";
  if (status === 429) return "rate_limited";
  if (status >= 200 && status < 300) return "success";
  if (status === 503) return "unavailable";
  if (status >= 400 && status < 500) return "client_error";
  return "server_error";
}
