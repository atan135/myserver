import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { defaultActivityRewardCatalog } from "./activity-reward-catalog.ts";
import {
  DEFAULT_ACTIVITY_REWARD_CATALOG_RELATIVE_PATH,
  resolveActivityRewardCatalogPath
} from "./activity-reward-catalog-path.js";

const testDirectory = path.dirname(fileURLToPath(import.meta.url));
const repositoryRoot = path.resolve(testDirectory, "../../../..");
const authoritativeItemTable = path.join(repositoryRoot, DEFAULT_ACTIVITY_REWARD_CATALOG_RELATIVE_PATH);

test("default activity reward catalog is independent of the workspace process cwd", () => {
  assert.equal(resolveActivityRewardCatalogPath(), authoritativeItemTable);
  assert.equal(fs.existsSync(authoritativeItemTable), true);
  assert.equal(defaultActivityRewardCatalog().find(1001)?.bindType, "Never");
});

test("activity reward catalog overrides use absolute paths directly and relative paths from repository root", () => {
  assert.equal(resolveActivityRewardCatalogPath(authoritativeItemTable), authoritativeItemTable);
  assert.equal(
    resolveActivityRewardCatalogPath(DEFAULT_ACTIVITY_REWARD_CATALOG_RELATIVE_PATH),
    authoritativeItemTable
  );
});
