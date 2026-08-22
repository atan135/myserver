import path from "node:path";
import { fileURLToPath } from "node:url";

const moduleDirectory = path.dirname(fileURLToPath(import.meta.url));
const repositoryRoot = path.resolve(moduleDirectory, "../../../..");

export const DEFAULT_ACTIVITY_REWARD_CATALOG_RELATIVE_PATH = "apps/game-server/csv/ItemTable.csv";

export function resolveActivityRewardCatalogPath(configuredPath = process.env.ACTIVITY_REWARD_CATALOG_PATH) {
  const value = typeof configuredPath === "string" ? configuredPath.trim() : "";
  const candidate = value || DEFAULT_ACTIVITY_REWARD_CATALOG_RELATIVE_PATH;
  return path.isAbsolute(candidate) ? candidate : path.resolve(repositoryRoot, candidate);
}
