import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const workflowPath = path.resolve(__dirname, "..", "..", ".github", "workflows", "protocol-compatibility.yml");

function pathFiltersFor(source, trigger) {
  const match = source.match(new RegExp(
    `^  ${trigger}:\\r?\\n    paths:\\r?\\n([\\s\\S]*?)(?=^  [A-Za-z_]+:|^jobs:)`,
    "m"
  ));
  assert.ok(match, `${trigger}.paths was not found in ${workflowPath}`);
  return [...match[1].matchAll(/^      - "([^"]+)"\r?$/gm)].map((item) => item[1]);
}

test("protocol compatibility workflow keeps Windows gate fields and symmetric protocol-consumer paths", () => {
  const source = readFileSync(workflowPath, "utf8");
  const pullRequestPaths = pathFiltersFor(source, "pull_request");
  const pushPaths = pathFiltersFor(source, "push");

  assert.deepEqual(pushPaths, pullRequestPaths, "push and pull_request path filters must remain symmetric");
  for (const expectedPath of [
    "packages/proto/**",
    "tools/check-proto.js",
    "tests/proto/**",
    "apps/game-server/src/**",
    "apps/game-proxy/src/**",
    "apps/match-service/src/**"
  ]) {
    assert.ok(pullRequestPaths.includes(expectedPath), `missing workflow path filter ${expectedPath}`);
  }

  assert.match(source, /^name: Protocol Compatibility$/m);
  assert.match(source, /^on:$/m);
  assert.match(source, /^    runs-on: windows-latest$/m);
  assert.match(source, /^      MYSERVER_CLIENT_ROOT: ""$/m);
  assert.match(source, /^      - uses: actions\/checkout@v4$/m);
  assert.match(source, /^          node-version: 22$/m);
  assert.match(source, /^      - uses: dtolnay\/rust-toolchain@stable$/m);
  assert.match(source, /^        run: npm ci$/m);
  assert.match(source, /^        run: npm run check:proto$/m);
});
