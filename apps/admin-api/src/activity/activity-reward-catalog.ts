import { readFileSync } from "node:fs";

import { parse } from "csv-parse/sync";

import { resolveActivityRewardCatalogPath } from "./activity-reward-catalog-path.js";

export type ActivityRewardBinding = "unbound" | "character_bound";

export interface ActivityRewardCatalogEntry {
  itemId: number;
  bindType: "Never" | "Pickup" | "Equip";
}

export interface ActivityRewardCatalog {
  find(itemId: number): ActivityRewardCatalogEntry | undefined;
}

export class CsvActivityRewardCatalog implements ActivityRewardCatalog {
  private readonly entries = new Map<number, ActivityRewardCatalogEntry>();

  constructor(filePath: string) {
    const rows = parse(readFileSync(filePath, "utf8"), {
      comment: "#",
      skip_empty_lines: true,
      trim: true
    }) as string[][];
    const headers = rows[0] ?? [];
    const itemIdIndex = headers.indexOf("Id");
    const bindTypeIndex = headers.indexOf("BindType");
    if (itemIdIndex < 0 || bindTypeIndex < 0 || rows[1]?.[itemIdIndex] !== "int") {
      throw new Error("ACTIVITY_REWARD_CATALOG_INVALID_HEADER");
    }

    for (const row of rows.slice(2)) {
      const itemId = Number(row[itemIdIndex]);
      const bindType = row[bindTypeIndex];
      if (!Number.isInteger(itemId) || itemId <= 0 || !["Never", "Pickup", "Equip"].includes(bindType)) {
        throw new Error("ACTIVITY_REWARD_CATALOG_INVALID");
      }
      if (this.entries.has(itemId)) {
        throw new Error("ACTIVITY_REWARD_CATALOG_DUPLICATE_ITEM");
      }
      this.entries.set(itemId, { itemId, bindType: bindType as ActivityRewardCatalogEntry["bindType"] });
    }

    if (this.entries.size === 0) {
      throw new Error("ACTIVITY_REWARD_CATALOG_EMPTY");
    }
  }

  find(itemId: number): ActivityRewardCatalogEntry | undefined {
    return this.entries.get(itemId);
  }
}

export function catalogAcceptsBinding(entry: ActivityRewardCatalogEntry, binding: ActivityRewardBinding): boolean {
  return entry.bindType !== "Pickup" || binding === "character_bound";
}

export function defaultActivityRewardCatalog(): ActivityRewardCatalog {
  return new CsvActivityRewardCatalog(resolveActivityRewardCatalogPath());
}
