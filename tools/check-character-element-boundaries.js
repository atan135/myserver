import { existsSync, readdirSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const sourceRoot = "apps/game-server/src";
const businessRoot = "apps/game-server/src/business/character_element";
const domainRoot = `${businessRoot}/domain`;
const adapterRepository =
  "apps/game-server/src/adapters/persistence/character_element_repository.rs";
const repositoryPortSymbols = [
  "CharacterElementRepository",
  "ApplyCharacterElementChangeInTransaction",
  "CharacterElementRepositoryApplyError",
  "CharacterElementRepositoryReadError",
  "CharacterElementsRead",
  "RepositoryFuture"
];
const failures = [];

function normalizePath(filePath) {
  return filePath.split(path.sep).join("/");
}

function listRustFiles(relativeDirectory) {
  const absoluteDirectory = path.join(rootDir, relativeDirectory);
  const files = [];
  for (const entry of readdirSync(absoluteDirectory, { withFileTypes: true })) {
    const relativePath = normalizePath(path.join(relativeDirectory, entry.name));
    if (entry.isDirectory()) {
      files.push(...listRustFiles(relativePath));
    } else if (entry.isFile() && entry.name.endsWith(".rs")) {
      files.push(relativePath);
    }
  }
  return files.sort();
}

function read(relativePath) {
  return readFileSync(path.join(rootDir, relativePath), "utf8");
}

function lineNumber(source, offset) {
  return source.slice(0, offset).split("\n").length;
}

function rejectMatches(relativePath, source, pattern, message) {
  for (const match of source.matchAll(pattern)) {
    failures.push(`${relativePath}:${lineNumber(source, match.index)} ${message}: ${match[0]}`);
  }
}

function requireAbsent(relativePath, message) {
  if (existsSync(path.join(rootDir, relativePath))) {
    failures.push(`${relativePath}:1 ${message}`);
  }
}

requireAbsent(
  "apps/game-server/src/core/character_element.rs",
  "legacy core::character_element implementation must be deleted"
);
requireAbsent(
  "apps/game-server/src/core/service/character_element_service.rs",
  "legacy character element service module must be deleted"
);

const rustFiles = listRustFiles(sourceRoot);
const sourceByPath = new Map(rustFiles.map((relativePath) => [relativePath, read(relativePath)]));

for (const [relativePath, source] of sourceByPath) {
  rejectMatches(
    relativePath,
    source,
    /\bcore::character_element(?:\b|::)/g,
    "legacy core character element path is forbidden"
  );
  rejectMatches(
    relativePath,
    source,
    /\bcore::service::character_element_service(?:\b|::)/g,
    "legacy character element service path is forbidden"
  );
  rejectMatches(
    relativePath,
    source,
    /\bcharacter_element_compatibility_service\b/g,
    "legacy ServiceContext compatibility field is forbidden"
  );

  if (!relativePath.startsWith(`${businessRoot}/`)) {
    rejectMatches(
      relativePath,
      source,
      /\bbusiness::character_element::(?:domain|application)(?:\b|::)/g,
      "business implementation internals must not be imported outside the module"
    );
  }

  if (relativePath !== adapterRepository) {
    rejectMatches(
      relativePath,
      source,
      /\badapters::persistence::character_element_repository(?:\b|::)/g,
      "persistence adapter internals must not be imported outside the adapter"
    );
  }

  if (!relativePath.startsWith(`${businessRoot}/`) && relativePath !== adapterRepository) {
    for (const symbol of repositoryPortSymbols) {
      rejectMatches(
        relativePath,
        source,
        new RegExp(`\\b${symbol}\\b`, "g"),
        "character element repository ports may only be used by business and the persistence adapter"
      );
    }
  }
}

for (const [relativePath, source] of sourceByPath) {
  if (!relativePath.startsWith(`${domainRoot}/`)) {
    continue;
  }

  rejectMatches(relativePath, source, /\bsqlx(?:\b|::)/g, "domain must not depend on SQLx");
  rejectMatches(relativePath, source, /\btokio(?:\b|::)/g, "domain must not depend on Tokio");
  rejectMatches(relativePath, source, /\bprost(?:\b|::)/g, "domain must not depend on Protobuf");
  rejectMatches(relativePath, source, /\bcrate::pb(?:\b|::)/g, "domain must not depend on protocol types");
  rejectMatches(
    relativePath,
    source,
    /\b(?:ConnectionContext|AuthenticatedSessionIdentity|ConfigTableRuntime)\b/g,
    "domain must not depend on runtime or session context"
  );
  rejectMatches(relativePath, source, /\bcrate::core(?:\b|::)/g, "domain must not depend on core");
  rejectMatches(
    relativePath,
    source,
    /\bcrate::business::(?!character_element(?:\b|::))/g,
    "domain must not depend on another business module"
  );
}

const moduleRoot = `${businessRoot}/mod.rs`;
const moduleSource = sourceByPath.get(moduleRoot);
if (!moduleSource) {
  failures.push(`${moduleRoot}:1 character element module root is missing`);
} else {
  rejectMatches(
    moduleRoot,
    moduleSource,
    /\bpub(?:\([^)]*\))?\s+mod\s+(?:api|domain)\b/g,
    "api and domain modules must remain private"
  );
  rejectMatches(
    moduleRoot,
    moduleSource,
    /\bpub(?:\([^)]*\))?\s+mod\s+application\b(?!;)/g,
    "application module visibility must not exceed pub(super)"
  );
  if (!/\bpub\(super\)\s+mod\s+application\s*;/.test(moduleSource)) {
    failures.push(`${moduleRoot}:1 application module must be pub(super) for internal use only`);
  }
}

for (const [relativePath, source] of sourceByPath) {
  const allowedWritePath =
    relativePath === adapterRepository || relativePath.startsWith(`${businessRoot}/`);
  if (!allowedWritePath) {
    rejectMatches(
      relativePath,
      source,
      /\.apply_change\s*\(/g,
      "permanent character element writes must use CharacterElementFacade and repository port"
    );
  }
}

if (failures.length > 0) {
  console.error("character element boundary check failed:");
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log("character element boundary checks passed.");
