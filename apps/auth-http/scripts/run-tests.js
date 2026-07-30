import { spawnSync } from "node:child_process";

const testFiles = [
  "src/config.test.js",
  "src/registry-client.test.js",
  "src/service-discovery.test.js",
  "src/game-admin-client.test.js",
  "src/internal/internal.controller.test.js",
  "src/characters/characters.service.test.js",
  "src/auth-store.register.test.js",
  "src/auth-store.ban-expiry.test.js",
  "src/common/client-ip.test.js",
  "src/common/tls-required.middleware.test.js"
];

const result = spawnSync(process.execPath, [
  "--test",
  "--experimental-test-isolation=none",
  "--test-concurrency=1",
  "--loader",
  "ts-node/esm",
  ...testFiles
], {
  stdio: "inherit",
  env: {
    ...process.env,
    TS_NODE_TRANSPILE_ONLY: process.env.TS_NODE_TRANSPILE_ONLY || "true"
  }
});

if (result.error) {
  throw result.error;
}
process.exitCode = result.status ?? 1;
