import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const read = (path) => readFile(new URL(`../../${path}`, import.meta.url), "utf8");

test("landing Caddy site serves only the bundled Rust introduction", async () => {
  const [caddyfile, dockerfile, html] = await Promise.all([
    read("deploy/docker/caddy/Caddyfile"),
    read("deploy/docker/Dockerfile.caddy"),
    read("deploy/docker/caddy/landing/index.html")
  ]);
  const site = caddyfile.match(/\{\$CADDY_LANDING_HOST\}\s*\{([\s\S]*?)\n\}\n\s*\{\$CADDY_AUTH_HOST\}/)?.[1];

  assert.ok(site, "CADDY_LANDING_HOST site must exist");
  assert.match(site, /handle \/\s*\{[\s\S]*?root \* \/srv\/landing[\s\S]*?file_server/);
  assert.match(site, /handle\s*\{\s*respond 404\s*\}/);
  assert.match(site, /Content-Security-Policy/);
  assert.doesNotMatch(site, /reverse_proxy/);
  assert.match(dockerfile, /COPY deploy\/docker\/caddy\/landing \/srv\/landing/);
  assert.match(html, /<h1 id="page-title">Rust<\/h1>/);
  assert.match(html, /为什么选择 Rust/);
  assert.doesNotMatch(html, /<script\b/i);
});

test("release tooling renders the Caddy landing host", async () => {
  const [render, createBundle, upload, envExample, compose] = await Promise.all([
    read("scripts/docker/render-release-env.mjs"),
    read("scripts/docker/create-release-bundle.sh"),
    read("scripts/docker/upload-release-bundle.sh"),
    read("deploy/docker/compose.production.env.example"),
    read("deploy/docker/compose.production.yml")
  ]);

  for (const source of [render, createBundle, upload]) {
    assert.match(source, /caddy-landing-host/);
  }
  assert.match(upload, /MYSERVER_CADDY_LANDING_HOST/);
  assert.match(envExample, /^CADDY_LANDING_HOST=bevy\.example\.com$/m);
  assert.match(compose, /CADDY_LANDING_HOST: \$\{CADDY_LANDING_HOST:\?set CADDY_LANDING_HOST\}/);
});
