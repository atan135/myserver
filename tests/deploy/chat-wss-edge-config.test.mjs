import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const read = (path) => readFile(new URL(`../../${path}`, import.meta.url), "utf8");

test("chat Caddy site only proxies an upgraded root request", async () => {
  const caddyfile = await read("deploy/docker/caddy/Caddyfile");

  assert.match(caddyfile, /\{\$CADDY_CHAT_HOST\}\s*\{/);
  assert.match(caddyfile, /@chat_websocket\s*\{[\s\S]*?method GET[\s\S]*?path \/[\s\S]*?header Connection \*Upgrade\*[\s\S]*?header Upgrade websocket[\s\S]*?\}/);
  assert.match(caddyfile, /handle @chat_websocket\s*\{[\s\S]*?reverse_proxy chat-server:9011/);
  assert.match(caddyfile, /handle\s*\{[\s\S]*?respond 426/);
});

test("chat Caddy site bounds handshakes and strips credential-bearing log fields", async () => {
  const caddyfile = await read("deploy/docker/caddy/Caddyfile");

  assert.match(caddyfile, /read_header 5s/);
  assert.match(caddyfile, /idle 2m/);
  assert.match(caddyfile, /max_header_size 16KB/);
  assert.match(caddyfile, /dial_timeout 3s/);
  assert.match(caddyfile, /response_header_timeout 5s/);
  assert.match(caddyfile, /request>uri delete/);
  for (const header of ["Authorization", "Cookie", "Sec-Websocket-Protocol"]) {
    assert.match(caddyfile, new RegExp(`request>headers>${header} delete`));
  }
  assert.match(caddyfile, /header_up X-Request-ID \{http\.request\.uuid\}/);
  assert.match(caddyfile, /header_up X-Real-IP \{http\.request\.remote\.host\}/);
});

test("production Compose keeps both chat listeners off the host network", async () => {
  const compose = await read("deploy/docker/compose.production.yml");
  const chatService = compose.match(/\r?\n  chat-server:\r?\n([\s\S]*?)\r?\n  mail-service:/)?.[1];
  const caddyService = compose.match(/\r?\n  caddy:\r?\n([\s\S]*?)\r?\n  migration-runner:/)?.[1];

  assert.ok(chatService, "chat-server service must exist");
  assert.ok(caddyService, "caddy service must exist");
  assert.match(chatService, /CHAT_BIND_ADDR: 0\.0\.0\.0:9001/);
  assert.match(chatService, /CHAT_WS_BIND_ADDR: 0\.0\.0\.0:9011/);
  assert.match(chatService, /HEARTBEAT_TIMEOUT_SECS: "30"/);
  assert.match(chatService, /CHAT_WS_TRUSTED_PROXY_CIDRS: \$\{CHAT_WS_TRUSTED_PROXY_CIDRS:-172\.30\.0\.0\/24\}/);
  assert.doesNotMatch(chatService, /\n\s+ports:/);
  assert.match(caddyService, /CADDY_CHAT_HOST: \$\{CADDY_CHAT_HOST:\?set CADDY_CHAT_HOST\}/);
  assert.match(caddyService, /networks:\s*\n\s+- edge\s*\n\s+- internal/);
  assert.match(caddyService, /chat-server:\s*\n\s+condition: service_started/);
  assert.doesNotMatch(compose, /["']?9011:9011(?:\/tcp)?["']?/);
  assert.doesNotMatch(compose, /["']?9001:9001(?:\/tcp)?["']?/);
});

test("production operations contract keeps chat ports private and reconnect isolated", async () => {
  const topology = await read("docs/后台与运维/生产拓扑与Room迁移设计.md");
  const operations = await read("docs/后台与运维/Docker部署/服务器Docker初始化与更新.md");

  assert.match(topology, /`A` 记录必须指向现有 Caddy 入口服务器的公网 IPv4/);
  assert.match(topology, /只有该服务器已配置可达的公网 IPv6[\s\S]*才发布 `AAAA`/);
  assert.match(topology, /带随机抖动的指数退避重连/);
  assert.match(topology, /不改动或重启 `game-proxy` 的 `4000\/UDP` KCP 链路/);
  assert.match(operations, /公网业务入站只允许 `80\/TCP`、`443\/TCP` 和 `4000\/UDP`/);
  assert.match(operations, /`9001`、`9011`/);
});

test("release tooling requires and renders the Caddy chat host", async () => {
  const [render, createBundle, upload, envExample, compose] = await Promise.all([
    read("scripts/docker/render-release-env.mjs"),
    read("scripts/docker/create-release-bundle.sh"),
    read("scripts/docker/upload-release-bundle.sh"),
    read("deploy/docker/compose.production.env.example"),
    read("deploy/docker/compose.production.yml")
  ]);

  for (const source of [render, createBundle, upload]) {
    assert.match(source, /caddy-chat-host/);
  }
  assert.match(render, /\["CADDY_CHAT_HOST", envValue\("caddy-chat-host"\)\]/);
  assert.match(upload, /MYSERVER_CADDY_CHAT_HOST/);
  assert.match(envExample, /^CADDY_CHAT_HOST=chat\.example\.com$/m);
  assert.match(compose, /AUTH_PUBLIC_CHAT_HOST: \$\{CADDY_CHAT_HOST:\?set CADDY_CHAT_HOST\}/);
  assert.match(compose, /AUTH_PUBLIC_CHAT_PORT: "443"/);
});
