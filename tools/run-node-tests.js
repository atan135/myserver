import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { globSync } from "node:fs";

const patterns = process.argv.slice(2);

if (patterns.length === 0) {
  console.error("Usage: node tools/run-node-tests.js <test-file-or-glob> [...]");
  process.exit(1);
}

const files = [];
const seen = new Set();

for (const pattern of patterns) {
  const matches = globSync(pattern, { nodir: true });
  const resolved = matches.length > 0
    ? matches
    : existsSync(pattern)
      ? [pattern]
      : [];

  for (const file of resolved) {
    if (!seen.has(file)) {
      seen.add(file);
      files.push(file);
    }
  }
}

files.sort();

if (files.length === 0) {
  console.error(`No test files matched: ${patterns.join(", ")}`);
  process.exit(1);
}

console.log(`Running ${files.length} test file(s) sequentially`);

for (const file of files) {
  console.log(`\n# ${file}`);
  const result = spawnSync(
    process.execPath,
    [
      "--test",
      "--experimental-test-isolation=none",
      "--test-concurrency=1",
      file
    ],
    { stdio: "inherit" }
  );

  if (result.error) {
    console.error(result.error.message);
    process.exit(1);
  }

  if (result.status !== 0) {
    process.exit(result.status ?? 1);
  }
}

console.log(`\nCompleted ${files.length} test file(s)`);
