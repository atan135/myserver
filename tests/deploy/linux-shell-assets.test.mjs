import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFile } from "node:fs/promises";
import path from "node:path";
import test from "node:test";

const root = path.resolve(import.meta.dirname, "../..");

test("tracked Linux shell assets use executable LF shebangs", async () => {
  const tracked = execFileSync("git", ["ls-files", "--", "*.sh"], {
    cwd: root,
    encoding: "utf8"
  }).trim().split(/\r?\n/).filter(Boolean);

  assert.ok(tracked.length > 0, "expected tracked shell assets");
  for (const relativePath of tracked) {
    const source = await readFile(path.join(root, relativePath));
    assert.equal(source.includes(Buffer.from("\r\n")), false, `${relativePath} must use LF line endings`);
    assert.equal(source.subarray(0, 2).toString("ascii"), "#!", `${relativePath} must start with a shebang`);
  }
});
