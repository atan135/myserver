import fs from "node:fs";
import { fileURLToPath } from "node:url";

const DEFAULT_TITLE_TABLE_PATH = fileURLToPath(
  new URL("../../../game-server/csv/TitleTable.csv", import.meta.url)
);

let cachedTitleTable = null;
let cachedMtimeMs = null;
let cachedCsvPath = null;

function parseCsvColumns(line) {
  const columns = [];
  let current = "";
  let inQuotes = false;

  for (let index = 0; index < line.length; index += 1) {
    const char = line[index];

    if (char === "\"") {
      if (inQuotes && line[index + 1] === "\"") {
        current += "\"";
        index += 1;
      } else {
        inQuotes = !inQuotes;
      }
      continue;
    }

    if (char === "," && !inQuotes) {
      columns.push(current);
      current = "";
      continue;
    }

    current += char;
  }

  columns.push(current);
  return columns;
}

function parseBooleanFlag(value) {
  const normalized = String(value ?? "").trim().toLowerCase();
  return normalized === "1" || normalized === "true" || normalized === "yes";
}

function parseInteger(value) {
  const normalized = String(value ?? "").trim();
  if (!/^-?\d+$/.test(normalized)) {
    return null;
  }

  const parsed = Number.parseInt(normalized, 10);
  return Number.isSafeInteger(parsed) ? parsed : null;
}

function parseJsonObject(value) {
  const normalized = String(value ?? "").trim();
  if (normalized.length === 0) {
    return null;
  }

  try {
    const parsed = JSON.parse(normalized);
    return parsed && typeof parsed === "object" && !Array.isArray(parsed) ? parsed : null;
  } catch {
    return null;
  }
}

function titleDefinitionFromRow(row) {
  return {
    title_id: String(row.TitleId || "").trim(),
    name: String(row.Name || "").trim(),
    title_type: String(row.TitleType || "").trim(),
    source_domain_id: String(row.SourceDomainId || "").trim(),
    tier_required: String(row.TierRequired || "").trim(),
    unlock_rules: parseJsonObject(row.UnlockRules),
    rarity: String(row.Rarity || "").trim(),
    icon: String(row.Icon || "").trim(),
    color: String(row.Color || "").trim(),
    hidden: parseBooleanFlag(row.Hidden),
    limited: parseBooleanFlag(row.Limited),
    sort_order: parseInteger(row.SortOrder)
  };
}

function parseTitleDefinitions(content) {
  const lines = content.split(/\r?\n/).filter((line) => line.trim().length > 0);
  if (lines.length < 3) {
    return {};
  }

  const headers = parseCsvColumns(lines[0]).map((header) => header.trim());
  const definitions = {};

  for (const line of lines.slice(2)) {
    const columns = parseCsvColumns(line);
    const row = {};
    headers.forEach((header, index) => {
      row[header] = columns[index] ?? "";
    });

    const definition = titleDefinitionFromRow(row);
    if (definition.title_id.length > 0) {
      definitions[definition.title_id] = definition;
    }
  }

  return definitions;
}

export function getTitleDefinitions(csvPath = DEFAULT_TITLE_TABLE_PATH) {
  const stat = fs.statSync(csvPath);
  if (cachedTitleTable && cachedCsvPath === csvPath && cachedMtimeMs === stat.mtimeMs) {
    return cachedTitleTable;
  }

  cachedTitleTable = parseTitleDefinitions(fs.readFileSync(csvPath, "utf8"));
  cachedMtimeMs = stat.mtimeMs;
  cachedCsvPath = csvPath;
  return cachedTitleTable;
}

export {
  DEFAULT_TITLE_TABLE_PATH,
  parseCsvColumns,
  parseTitleDefinitions
};
