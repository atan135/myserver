import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const read = (path) => readFile(new URL(`../../${path}`, import.meta.url), "utf8");

function authSite(caddyfile) {
  const match = caddyfile.match(/\{\$CADDY_AUTH_HOST\}\s*\{([\s\S]*?)\n\}\n\s*\{\$CADDY_ADMIN_HOST\}/);
  assert.ok(match, "CADDY_AUTH_HOST site must exist");
  return match[1];
}

test("auth Caddy site sends only the four player mail routes to mail-service", async () => {
  const site = authSite(await read("deploy/docker/caddy/Caddyfile"));
  const allowedRoute = site.match(/@mail_player_route expression `([^`]+)`/);

  assert.ok(allowedRoute, "mail player route must use an exact method/path expression");
  assert.match(allowedRoute[1], /method\('GET'\).*\^\/api\/v1\/mails\$/);
  assert.match(allowedRoute[1], /method\('GET'\).*\^\/api\/v1\/mails\/\[A-Za-z0-9:_-\]\{1,64\}\$/);
  assert.match(allowedRoute[1], /method\('PUT'\).*\/read\$/);
  assert.match(allowedRoute[1], /method\('POST'\).*\/claim\$/);
  assert.equal((site.match(/reverse_proxy mail-service:9003/g) || []).length, 2);

  const mailRouteIndex = site.indexOf("@mail_namespace path /api/v1/mails*");
  const authProxyIndex = site.indexOf("reverse_proxy auth-http:3000");
  assert.ok(mailRouteIndex >= 0 && mailRouteIndex < authProxyIndex, "mail handling must precede auth fallback");
  assert.match(site, /@mail_known_path_wrong_method expression/);
  assert.match(site, /respond @mail_known_path_wrong_method 405/);
  assert.match(site, /@mail_unknown_path expression/);
  assert.match(site, /respond @mail_unknown_path 404/);
});

test("mail Caddy diagnostic is an exact test-only route with isolated token forwarding", async () => {
  const [caddyfile, compose] = await Promise.all([
    read("deploy/docker/caddy/Caddyfile"),
    read("deploy/docker/compose.production.yml")
  ]);
  const site = authSite(caddyfile);
  const caddy = compose.match(/\r?\n  caddy:\r?\n([\s\S]*?)\r?\n  migration-runner:/)?.[1];
  const diagnostic = site.match(/handle @mail_load_test_route \{([\s\S]*?)\n\s*\}\n\s*# Classify the route before credentials/);

  assert.ok(caddy, "caddy service must exist");
  assert.ok(diagnostic, "diagnostic path must have an isolated handler");
  assert.match(site, /@mail_load_test_disabled expression `path_regexp\('\^\/api\/v1\/mails\/load-test\/notification\$'\) && "\{\$MYSERVER_RUNTIME_ENV:production\}" != "test"`/);
  assert.match(site, /respond @mail_load_test_disabled 404/);
  assert.match(site, /@mail_load_test_wrong_method expression `path_regexp\('\^\/api\/v1\/mails\/load-test\/notification\$'\) && !method\('POST'\)`/);
  assert.match(site, /respond @mail_load_test_wrong_method 404/);
  assert.match(site, /@mail_load_test_route expression `path_regexp\('\^\/api\/v1\/mails\/load-test\/notification\$'\) && method\('POST'\)`/);
  assert.match(diagnostic[1], /@mail_load_test_repeated_singletons expression .*X-Mail-Load-Test-Token.*contains\(","\)/);
  assert.match(diagnostic[1], /@mail_load_test_missing_token expression `size\(\{http\.request\.header\.X-Mail-Load-Test-Token\}\) == 0`/);
  assert.match(diagnostic[1], /@mail_load_test_invalid_token expression `size\(\{http\.request\.header\.X-Mail-Load-Test-Token\}\) < 16 \|\| size\(\{http\.request\.header\.X-Mail-Load-Test-Token\}\) > 512`/);
  assert.match(diagnostic[1], /@mail_load_test_conflicting_credentials expression .*X-Mail-Operations-Token.*X-Mail-High-Risk-Token/);
  assert.match(diagnostic[1], /request_body\s*\{\s*max_size 1024\s*\}/);
  assert.match(diagnostic[1], /header_up X-Forwarded-For \{http\.request\.remote\.host\}/);
  assert.match(diagnostic[1], /header_up X-Request-ID \{http\.request\.uuid\}/);
  assert.match(diagnostic[1], /header_up X-Mail-Load-Test-Token \{http\.request\.header\.X-Mail-Load-Test-Token\}/);
  assert.match(diagnostic[1], /header_down Cache-Control "private, no-store"/);
  assert.match(site, /@mail_load_test_token_on_player_route header X-Mail-Load-Test-Token \*/);
  assert.match(site, /respond @mail_load_test_token_on_player_route 400/);
  assert.match(site, /header_up -X-Mail-Load-Test-Token/);
  assert.match(site, /request>headers>X-Mail-Load-Test-Token delete/);
  assert.match(caddy, /^      MYSERVER_RUNTIME_ENV: \$\{MYSERVER_RUNTIME_ENV:\?set MYSERVER_RUNTIME_ENV to production or test\}$/m);
});

test("mail Caddy route rejects control-plane paths and request bypasses before proxying", async () => {
  const site = authSite(await read("deploy/docker/caddy/Caddyfile"));

  assert.match(site, /@internal_api path \/api\/v1\/internal \/api\/v1\/internal\/\*/);
  assert.match(site, /@mail_unsafe_path expression/);
  assert.match(site, /%2f\|%5c\|%2e/);
  assert.match(site, /@mail_x_http_method_override header X-HTTP-Method-Override \*/);
  assert.match(site, /@mail_x_method_override header X-Method-Override \*/);
  assert.match(site, /@mail_query_method_override query _method=\*/);
  assert.match(site, /@mail_transfer_encoding header Transfer-Encoding \*/);
  assert.match(site, /@mail_repeated_singletons expression/);
  assert.match(site, /@mail_conflicting_credentials expression/);
  assert.match(site, /X-Service-Token/);
  assert.match(site, /X-Admin-Token/);
  assert.match(site, /@mail_missing_ticket expression/);
  assert.match(site, /@mail_invalid_ticket expression/);
  assert.match(site, /@mail_read_with_body expression `method\('GET'\).*Content-Length.*!= "0"`/);
});

test("mail Caddy route bounds requests, rebuilds proxy headers, and avoids private-response caching", async () => {
  const caddyfile = await read("deploy/docker/caddy/Caddyfile");
  const site = authSite(caddyfile);

  assert.match(caddyfile, /max_header_size 16KB/);
  assert.match(site, /@mail_uri_too_long expression `size\(\{http\.request\.orig_uri\}\) > 2048`/);
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
  for (const header of ["X-Forwarded-For", "X-Forwarded-Proto", "X-Forwarded-Host", "X-Real-IP", "X-Request-ID"]) {
    assert.doesNotMatch(site, new RegExp(`header_up -${header}`));
  }
  assert.match(site, /header_down Cache-Control "private, no-store"/);
  assert.match(site, /handle_errors\s*\{[\s\S]*?respond @mail_upstream_error 503/);

  const mailHandle = site.match(/handle @mail_namespace \{([\s\S]*?)\n\s*\}\n\s*handle \{/);
  assert.ok(mailHandle, "mail route must be isolated from the auth handler");
  assert.doesNotMatch(mailHandle[1], /\bencode\s+(?:zstd|gzip)/);
  assert.match(site, /handle \{\s*# Mail is intentionally outside this handler[\s\S]*?encode zstd gzip[\s\S]*?reverse_proxy auth-http:3000/);
});

test("mail Caddy access logs remove request URI and credentials, and production keeps port 9003 internal", async () => {
  const [caddyfile, compose] = await Promise.all([
    read("deploy/docker/caddy/Caddyfile"),
    read("deploy/docker/compose.production.yml")
  ]);
  const site = authSite(caddyfile);
  const authHttp = compose.match(/\r?\n  auth-http:\r?\n([\s\S]*?)\r?\n  admin-api:/)?.[1];
  const mailService = compose.match(/\r?\n  mail-service:\r?\n([\s\S]*?)\r?\n  announce-service:/)?.[1];
  const caddyService = compose.match(/\r?\n  caddy:\r?\n([\s\S]*?)\r?\n  migration-runner:/)?.[1];

  assert.ok(authHttp, "auth-http service must exist");
  assert.ok(mailService, "mail-service service must exist");
  assert.ok(caddyService, "caddy service must exist");
  for (const header of [
    "Authorization",
    "Cookie",
    "X-Game-Ticket",
    "X-Service-Token",
    "X-Admin-Token",
    "X-Mail-Operations-Token",
    "X-Mail-High-Risk-Token",
    "X-Mail-Load-Test-Token"
  ]) {
    assert.match(site, new RegExp(`request>headers>${header} delete`));
  }
  assert.match(site, /request>uri delete/);
  assert.match(authHttp, /AUTH_PUBLIC_MAIL_HOST: \$\{CADDY_AUTH_HOST:\?set CADDY_AUTH_HOST\}/);
  assert.match(authHttp, /AUTH_PUBLIC_MAIL_PORT: "443"/);
  assert.match(mailService, /MAIL_PLAYER_AUTH_REQUIRED: "true"/);
  assert.match(mailService, /MAIL_TRUST_PROXY: "true"/);
  assert.match(mailService, /MAIL_TRUSTED_PROXY_CIDRS: 172\.30\.0\.0\/24/);
  assert.match(mailService, /MAIL_PUBLIC_RATE_LIMIT_ENABLED: "true"/);
  assert.doesNotMatch(caddyService, /depends_on:/);
  assert.doesNotMatch(mailService, /\n\s+ports:/);
  assert.doesNotMatch(compose, /["']?9003:9003(?:\/tcp)?["']?/);
});
