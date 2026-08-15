import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const read = (path) => readFile(new URL(`../../${path}`, import.meta.url), "utf8");

function authSite(caddyfile) {
  const match = caddyfile.match(/\{\$CADDY_AUTH_HOST\}\s*\{([\s\S]*?)\n\}\n\s*\{\$CADDY_ADMIN_HOST\}/);
  assert.ok(match, "CADDY_AUTH_HOST site must exist");
  return match[1];
}

test("auth Caddy site sends only player announcement reads to announce-service", async () => {
  const site = authSite(await read("deploy/docker/caddy/Caddyfile"));
  const allowedRoute = site.match(/@announce_player_route expression `([^`]+)`/);

  assert.ok(allowedRoute, "announcement player route must use an exact method/path expression");
  assert.match(allowedRoute[1], /method\('GET'\).*\^\/api\/v1\/announcements\$/);
  assert.match(allowedRoute[1], /method\('GET'\).*\^\/api\/v1\/announcements\/\[A-Za-z0-9:_-\]\{1,64\}\$/);
  assert.equal((site.match(/reverse_proxy announce-service:9004/g) || []).length, 1);

  const announcementRouteIndex = site.indexOf("@announce_namespace path /api/v1/announcements*");
  const authProxyIndex = site.indexOf("reverse_proxy auth-http:3000");
  assert.ok(announcementRouteIndex >= 0 && announcementRouteIndex < authProxyIndex, "announcement handling must precede auth fallback");
  assert.match(site, /@announce_known_path_wrong_method expression/);
  assert.match(site, /respond @announce_known_path_wrong_method 405/);
  assert.match(site, /@announce_unknown_path expression/);
  assert.match(site, /respond @announce_unknown_path 404/);
});

test("announcement Caddy route rejects control-plane paths and request bypasses before proxying", async () => {
  const site = authSite(await read("deploy/docker/caddy/Caddyfile"));

  assert.match(site, /@internal_api path \/api\/v1\/internal \/api\/v1\/internal\/\*/);
  assert.match(site, /@announce_unsafe_path expression/);
  assert.match(site, /%2f\|%5c\|%2e/);
  assert.match(site, /@announce_x_http_method_override header X-HTTP-Method-Override \*/);
  assert.match(site, /@announce_x_method_override header X-Method-Override \*/);
  assert.match(site, /@announce_query_method_override query _method=\*/);
  assert.match(site, /@announce_transfer_encoding header Transfer-Encoding \*/);
  assert.match(site, /@announce_repeated_singletons expression/);
  assert.match(site, /@announce_conflicting_credentials expression/);
  for (const header of ["Authorization", "X-Read-Token", "X-Service-Token", "X-Admin-Token", "Cookie"]) {
    assert.match(site, new RegExp(`size\\(\\{http\\.request\\.header\\.${header}\\}\\) > 0`));
  }
  assert.match(site, /@announce_missing_ticket expression/);
  assert.match(site, /@announce_invalid_ticket expression/);
  assert.match(site, /@announce_read_with_body expression `method\('GET'\).*Content-Length.*!= "0"`/);
});

test("announcement Caddy route bounds reads, rebuilds proxy headers, and avoids private-response caching", async () => {
  const caddyfile = await read("deploy/docker/caddy/Caddyfile");
  const site = authSite(caddyfile);

  assert.match(caddyfile, /max_header_size 16KB/);
  assert.match(site, /@announce_uri_too_long expression `size\(\{http\.request\.orig_uri\}\) > 2048`/);
  assert.match(site, /request_body\s*\{\s*max_size 1024\s*\}/);
  assert.match(site, /max_response_header 16KB/);
  assert.match(site, /dial_timeout 3s/);
  assert.match(site, /response_header_timeout 5s/);
  assert.match(site, /read_timeout 12s/);
  assert.match(site, /write_timeout 5s/);
  assert.match(site, /header_up X-Forwarded-For \{http\.request\.remote\.host\}/);
  assert.match(site, /header_up X-Forwarded-Proto https/);
  assert.match(site, /header_up X-Real-IP \{http\.request\.remote\.host\}/);
  assert.match(site, /header_up X-Request-ID \{http\.request\.uuid\}/);
  assert.match(site, /header_down Cache-Control "private, no-store"/);
  assert.match(site, /@announce_upstream_error path \/api\/v1\/announcements\*/);
  assert.match(site, /respond @announce_upstream_error 503/);

  const announceHandle = site.match(/handle @announce_namespace \{([\s\S]*?)\n\s*\}\n\s*handle \{/);
  assert.ok(announceHandle, "announcement route must be isolated from the auth handler");
  assert.doesNotMatch(announceHandle[1], /\bencode\s+(?:zstd|gzip)/);
});

test("announcement deployment descriptor and edge logs keep its service private", async () => {
  const [caddyfile, compose] = await Promise.all([
    read("deploy/docker/caddy/Caddyfile"),
    read("deploy/docker/compose.production.yml")
  ]);
  const site = authSite(caddyfile);
  const authHttp = compose.match(/\r?\n  auth-http:\r?\n([\s\S]*?)\r?\n  admin-api:/)?.[1];
  const announceService = compose.match(/\r?\n  announce-service:\r?\n([\s\S]*?)\r?\n  metrics-collector:/)?.[1];

  assert.ok(authHttp, "auth-http service must exist");
  assert.ok(announceService, "announce-service service must exist");
  for (const header of [
    "Authorization",
    "Cookie",
    "X-Game-Ticket",
    "X-Read-Token",
    "X-Service-Token",
    "X-Admin-Token"
  ]) {
    assert.match(site, new RegExp(`request>headers>${header} delete`));
  }
  assert.match(site, /request>uri delete/);
  assert.match(authHttp, /AUTH_PUBLIC_ANNOUNCE_HOST: \$\{CADDY_AUTH_HOST:\?set CADDY_AUTH_HOST\}/);
  assert.match(authHttp, /AUTH_PUBLIC_ANNOUNCE_PORT: "443"/);
  assert.doesNotMatch(announceService, /\n\s+ports:/);
  assert.doesNotMatch(compose, /["']?9004:9004(?:\/tcp)?["']?/);
});
