import { existsSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import {
  checkBaseline,
  readInventory,
  validateBaselineSnapshot
} from "./proto-compatibility-baseline.js";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const rootDir = path.resolve(__dirname, "..");
const EXEMPTION_SCHEMA_VERSION = 1;
const RELEASE_REFERENCE_SCHEMA_VERSION = 1;

export const BREAKING_RULES = new Set([
  "PROTO_FILE_REMOVED",
  "PROTO_PACKAGE_CHANGED",
  "MESSAGE_REMOVED",
  "FIELD_NUMBER_REUSED",
  "FIELD_TYPE_CHANGED",
  "FIELD_LABEL_CHANGED",
  "FIELD_ONEOF_CHANGED",
  "ONEOF_REMOVED",
  "FIELD_REMOVED_NOT_RESERVED",
  "ENUM_REMOVED",
  "ENUM_NUMBER_REUSED",
  "ENUM_VALUE_DELETED",
  "ENUM_VALUE_NUMBER_CHANGED",
  "SERVICE_REMOVED",
  "RPC_REMOVED",
  "RPC_REQUEST_TYPE_CHANGED",
  "RPC_RESPONSE_TYPE_CHANGED",
  "RPC_REQUEST_STREAM_CHANGED",
  "RPC_RESPONSE_STREAM_CHANGED"
]);

function fail(message) {
  throw new Error(message);
}

function normalizePath(value) {
  return value.replaceAll("\\", "/");
}

function isPlainObject(value) {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

function isNonEmptyString(value) {
  return typeof value === "string" && value.trim().length > 0;
}

function isIsoDate(value) {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(value ?? "")) {
    return false;
  }
  const parsed = new Date(`${value}T00:00:00.000Z`);
  return !Number.isNaN(parsed.getTime()) && parsed.toISOString().slice(0, 10) === value;
}

function todayUtc() {
  return new Date().toISOString().slice(0, 10);
}

function mapBy(values, property) {
  return new Map(values.map((value) => [value[property], value]));
}

function rangeContains(reserved, number) {
  if (reserved.numbers.includes(number)) {
    return true;
  }
  return reserved.ranges.some(({ start, end }) => number >= start && (end === "max" || number <= end));
}

function protocolByFile(inventory, file) {
  return inventory.protocols?.find((protocol) => protocol.file === file) ?? null;
}

function riskForFile(inventory, file) {
  const protocol = protocolByFile(inventory, file);
  const classifications = protocol?.classification ?? [];
  if (classifications.includes("client_gameplay")) {
    return {
      audience: "PLAYER_CLIENT",
      classifications,
      guidance: "Published player traffic may include old clients; preserve wire compatibility and coordinate mybevy before release."
    };
  }
  return {
    audience: "COORDINATED_INTERNAL",
    classifications,
    guidance: "Internal consumers can be rolled forward together, but producer/consumer deployment order and rollback behavior require review."
  };
}

function breakingDiagnostic(inventory, rule, file, target, detail) {
  return {
    ...riskForFile(inventory, file),
    detail,
    file,
    rule,
    target
  };
}

function configDiagnostic(rule, detail, exemptionId = "") {
  return {
    audience: "CONFIGURATION",
    classifications: [],
    detail,
    exemptionId,
    file: "packages/proto/compatibility/breaking-exemptions.json",
    rule,
    target: exemptionId ? { exemptionId } : {}
  };
}

function compareMessages(inventory, file, released, candidate, diagnostics) {
  const candidateMessages = mapBy(candidate.messages, "name");
  for (const releasedMessage of released.messages) {
    const currentMessage = candidateMessages.get(releasedMessage.name);
    if (!currentMessage) {
      diagnostics.push(breakingDiagnostic(
        inventory,
        "MESSAGE_REMOVED",
        file.path,
        { file: file.path, message: releasedMessage.name },
        `message ${releasedMessage.name} was removed`
      ));
      continue;
    }

    const candidateFields = mapBy(currentMessage.fields, "number");
    for (const releasedField of releasedMessage.fields) {
      const currentField = candidateFields.get(releasedField.number);
      const target = {
        fieldNumber: releasedField.number,
        fieldName: releasedField.name,
        file: file.path,
        message: releasedMessage.name
      };
      if (!currentField) {
        if (!rangeContains(currentMessage.reserved, releasedField.number)) {
          diagnostics.push(breakingDiagnostic(
            inventory,
            "FIELD_REMOVED_NOT_RESERVED",
            file.path,
            target,
            `field ${releasedMessage.name}.${releasedField.name} (#${releasedField.number}) was removed without reserving #${releasedField.number} in the same message`
          ));
        }
        continue;
      }
      if (currentField.name !== releasedField.name) {
        diagnostics.push(breakingDiagnostic(
          inventory,
          "FIELD_NUMBER_REUSED",
          file.path,
          target,
          `field #${releasedField.number} changed name from ${releasedField.name} to ${currentField.name}`
        ));
      }
      if (currentField.type !== releasedField.type) {
        diagnostics.push(breakingDiagnostic(
          inventory,
          "FIELD_TYPE_CHANGED",
          file.path,
          target,
          `field ${releasedMessage.name}.${releasedField.name} (#${releasedField.number}) changed type from ${releasedField.type} to ${currentField.type}`
        ));
      }
      if (currentField.label !== releasedField.label) {
        diagnostics.push(breakingDiagnostic(
          inventory,
          "FIELD_LABEL_CHANGED",
          file.path,
          target,
          `field ${releasedMessage.name}.${releasedField.name} (#${releasedField.number}) changed label from ${releasedField.label} to ${currentField.label}`
        ));
      }
      if ((currentField.oneof || "") !== (releasedField.oneof || "")) {
        diagnostics.push(breakingDiagnostic(
          inventory,
          "FIELD_ONEOF_CHANGED",
          file.path,
          target,
          `field ${releasedMessage.name}.${releasedField.name} (#${releasedField.number}) changed oneof from ${releasedField.oneof || "none"} to ${currentField.oneof || "none"}`
        ));
      }
    }
    const releasedOneofs = new Set(releasedMessage.fields.map((field) => field.oneof).filter(Boolean));
    const candidateOneofs = new Set(currentMessage.fields.map((field) => field.oneof).filter(Boolean));
    for (const oneof of releasedOneofs) {
      if (!candidateOneofs.has(oneof)) {
        diagnostics.push(breakingDiagnostic(
          inventory,
          "ONEOF_REMOVED",
          file.path,
          { file: file.path, message: releasedMessage.name, oneof },
          `oneof ${releasedMessage.name}.${oneof} no longer exists in the candidate message`
        ));
      }
    }
  }
}

function compareEnums(inventory, file, released, candidate, diagnostics) {
  const candidateEnums = mapBy(candidate.enums, "name");
  for (const releasedEnum of released.enums) {
    const currentEnum = candidateEnums.get(releasedEnum.name);
    if (!currentEnum) {
      diagnostics.push(breakingDiagnostic(
        inventory,
        "ENUM_REMOVED",
        file.path,
        { enum: releasedEnum.name, file: file.path },
        `enum ${releasedEnum.name} was removed`
      ));
      continue;
    }
    const candidateByName = mapBy(currentEnum.values, "name");
    const candidateByNumber = mapBy(currentEnum.values, "number");
    for (const releasedValue of releasedEnum.values) {
      const currentValue = candidateByName.get(releasedValue.name);
      const target = {
        enum: releasedEnum.name,
        file: file.path,
        valueName: releasedValue.name,
        valueNumber: releasedValue.number
      };
      const currentAtReleasedNumber = candidateByNumber.get(releasedValue.number);
      if (!currentValue) {
        if (currentAtReleasedNumber) {
          diagnostics.push(breakingDiagnostic(
            inventory,
            "ENUM_NUMBER_REUSED",
            file.path,
            target,
            `enum ${releasedEnum.name} value #${releasedValue.number} changed from ${releasedValue.name} to ${currentAtReleasedNumber.name}`
          ));
        } else {
          diagnostics.push(breakingDiagnostic(
            inventory,
            "ENUM_VALUE_DELETED",
            file.path,
            target,
            `enum ${releasedEnum.name} value ${releasedValue.name} (#${releasedValue.number}) was removed`
          ));
        }
        continue;
      }
      if (currentValue.number !== releasedValue.number) {
        diagnostics.push(breakingDiagnostic(
          inventory,
          "ENUM_VALUE_NUMBER_CHANGED",
          file.path,
          target,
          `enum ${releasedEnum.name} value ${releasedValue.name} changed number from ${releasedValue.number} to ${currentValue.number}`
        ));
        if (currentAtReleasedNumber && currentAtReleasedNumber.name !== releasedValue.name) {
          diagnostics.push(breakingDiagnostic(
            inventory,
            "ENUM_NUMBER_REUSED",
            file.path,
            target,
            `enum ${releasedEnum.name} value #${releasedValue.number} changed from ${releasedValue.name} to ${currentAtReleasedNumber.name}`
          ));
        }
      }
    }
  }
}

function compareServices(inventory, file, released, candidate, diagnostics) {
  const candidateServices = mapBy(candidate.services, "name");
  for (const releasedService of released.services) {
    const currentService = candidateServices.get(releasedService.name);
    if (!currentService) {
      diagnostics.push(breakingDiagnostic(
        inventory,
        "SERVICE_REMOVED",
        file.path,
        { file: file.path, service: releasedService.name },
        `service ${releasedService.name} was removed`
      ));
      continue;
    }
    const candidateRpcs = mapBy(currentService.rpcs, "name");
    for (const releasedRpc of releasedService.rpcs) {
      const currentRpc = candidateRpcs.get(releasedRpc.name);
      const target = { file: file.path, rpc: releasedRpc.name, service: releasedService.name };
      if (!currentRpc) {
        diagnostics.push(breakingDiagnostic(
          inventory,
          "RPC_REMOVED",
          file.path,
          target,
          `RPC ${releasedService.name}.${releasedRpc.name} was removed`
        ));
        continue;
      }
      const comparisons = [
        ["RPC_REQUEST_TYPE_CHANGED", "request type", releasedRpc.request.type, currentRpc.request.type],
        ["RPC_RESPONSE_TYPE_CHANGED", "response type", releasedRpc.response.type, currentRpc.response.type],
        ["RPC_REQUEST_STREAM_CHANGED", "request streaming", releasedRpc.request.stream, currentRpc.request.stream],
        ["RPC_RESPONSE_STREAM_CHANGED", "response streaming", releasedRpc.response.stream, currentRpc.response.stream]
      ];
      for (const [rule, name, before, after] of comparisons) {
        if (before !== after) {
          diagnostics.push(breakingDiagnostic(
            inventory,
            rule,
            file.path,
            target,
            `RPC ${releasedService.name}.${releasedRpc.name} changed ${name} from ${before} to ${after}`
          ));
        }
      }
    }
  }
}

export function compareProtocolBaselines(released, candidate, inventory) {
  const diagnostics = [];
  const candidateFiles = mapBy(candidate.files, "path");
  for (const releasedFile of released.files) {
    const candidateFile = candidateFiles.get(releasedFile.path);
    if (!candidateFile) {
      diagnostics.push(breakingDiagnostic(
        inventory,
        "PROTO_FILE_REMOVED",
        releasedFile.path,
        { file: releasedFile.path },
        `proto file ${releasedFile.path} was removed from the candidate baseline`
      ));
      continue;
    }
    if (candidateFile.package !== releasedFile.package) {
      diagnostics.push(breakingDiagnostic(
        inventory,
        "PROTO_PACKAGE_CHANGED",
        releasedFile.path,
        { file: releasedFile.path },
        `package changed from ${releasedFile.package} to ${candidateFile.package}`
      ));
    }
    compareMessages(inventory, releasedFile, releasedFile, candidateFile, diagnostics);
    compareEnums(inventory, releasedFile, releasedFile, candidateFile, diagnostics);
    compareServices(inventory, releasedFile, releasedFile, candidateFile, diagnostics);
  }
  return diagnostics;
}

const EXEMPTION_TARGET_FIELDS = new Set([
  "file",
  "message",
  "oneof",
  "fieldName",
  "fieldNumber",
  "enum",
  "valueName",
  "valueNumber",
  "service",
  "rpc"
]);

function validateExemptionTarget(target, rule) {
  if (!isPlainObject(target)) {
    return "target must be an object";
  }
  const keys = Object.keys(target);
  if (keys.some((key) => !EXEMPTION_TARGET_FIELDS.has(key))) {
    return `target contains unsupported field(s): ${keys.filter((key) => !EXEMPTION_TARGET_FIELDS.has(key)).join(", ")}`;
  }
  if (!isNonEmptyString(target.file)) {
    return "target.file must be a non-empty packages/proto path";
  }
  if (!target.file.startsWith("packages/proto/") || target.file.includes("..")) {
    return "target.file must be inside packages/proto";
  }
  for (const key of ["message", "oneof", "fieldName", "enum", "valueName", "service", "rpc"]) {
    if (key in target && !isNonEmptyString(target[key])) {
      return `target.${key} must be a non-empty string`;
    }
  }
  for (const key of ["fieldNumber", "valueNumber"]) {
    if (key in target && (!Number.isInteger(target[key]) || target[key] < 0)) {
      return `target.${key} must be a non-negative integer`;
    }
  }
  const requiredByRule = {
    ENUM_NUMBER_REUSED: ["enum", "valueNumber"],
    ENUM_REMOVED: ["enum"],
    ENUM_VALUE_DELETED: ["enum", "valueNumber"],
    ENUM_VALUE_NUMBER_CHANGED: ["enum", "valueNumber"],
    FIELD_LABEL_CHANGED: ["message", "fieldNumber"],
    FIELD_NUMBER_REUSED: ["message", "fieldNumber"],
    FIELD_ONEOF_CHANGED: ["message", "fieldNumber"],
    FIELD_REMOVED_NOT_RESERVED: ["message", "fieldNumber"],
    FIELD_TYPE_CHANGED: ["message", "fieldNumber"],
    MESSAGE_REMOVED: ["message"],
    ONEOF_REMOVED: ["message", "oneof"],
    RPC_REMOVED: ["service", "rpc"],
    RPC_REQUEST_STREAM_CHANGED: ["service", "rpc"],
    RPC_REQUEST_TYPE_CHANGED: ["service", "rpc"],
    RPC_RESPONSE_STREAM_CHANGED: ["service", "rpc"],
    RPC_RESPONSE_TYPE_CHANGED: ["service", "rpc"],
    SERVICE_REMOVED: ["service"]
  };
  const required = requiredByRule[rule] ?? [];
  const missing = required.filter((key) => !(key in target));
  if (missing.length > 0) {
    return `target for ${rule} must include ${missing.join(", ")}`;
  }
  return null;
}

export function validateExemptions(document, today = todayUtc()) {
  const diagnostics = [];
  const valid = [];
  if (!isPlainObject(document) || document.schemaVersion !== EXEMPTION_SCHEMA_VERSION || !Array.isArray(document.exemptions)) {
    return {
      diagnostics: [configDiagnostic("EXEMPTION_DOCUMENT_INVALID", "breaking exemptions must be an object with schemaVersion 1 and an exemptions array")],
      valid
    };
  }
  const seenIds = new Set();
  document.exemptions.forEach((entry, index) => {
    const fallbackId = `entry-${index + 1}`;
    const id = isNonEmptyString(entry?.id) ? entry.id.trim() : fallbackId;
    const allowed = new Set(["id", "rule", "target", "reason", "owner", "expiresAt"]);
    let problem = !isPlainObject(entry) ? "entry must be an object" : "";
    if (!problem && Object.keys(entry).some((key) => !allowed.has(key))) {
      problem = `contains unsupported field(s): ${Object.keys(entry).filter((key) => !allowed.has(key)).join(", ")}`;
    }
    if (!problem && !isNonEmptyString(entry.id)) problem = "id must be a non-empty string";
    if (!problem && seenIds.has(id)) problem = `duplicate exemption id ${id}`;
    if (!problem && !BREAKING_RULES.has(entry.rule)) problem = `rule must be one of the known breaking rules, received ${JSON.stringify(entry.rule)}`;
    if (!problem) problem = validateExemptionTarget(entry.target, entry.rule);
    if (!problem && !isNonEmptyString(entry.reason)) problem = "reason must be a non-empty string";
    if (!problem && !isNonEmptyString(entry.owner)) problem = "owner must be a non-empty string";
    if (!problem && !isIsoDate(entry.expiresAt)) problem = "expiresAt must be a valid YYYY-MM-DD date";
    if (problem) {
      diagnostics.push(configDiagnostic("EXEMPTION_INVALID", `exemption ${id}: ${problem}`, id));
      return;
    }
    seenIds.add(id);
    const exemption = { ...entry, id };
    if (exemption.expiresAt < today) {
      diagnostics.push(configDiagnostic(
        "EXEMPTION_EXPIRED",
        `exemption ${id} expired on ${exemption.expiresAt}; renew with a new owner-approved expiry or remove it`,
        id
      ));
      return;
    }
    valid.push(exemption);
  });
  return { diagnostics, valid };
}

function targetMatches(target, diagnosticTarget) {
  return Object.entries(target).every(([key, value]) => diagnosticTarget[key] === value);
}

export function applyExemptions(diagnostics, document, today = todayUtc()) {
  const validation = validateExemptions(document, today);
  const matchedIds = new Set();
  const remaining = [];
  const configDiagnostics = [...validation.diagnostics];
  for (const diagnostic of diagnostics) {
    const matches = validation.valid.filter((exemption) => exemption.rule === diagnostic.rule && targetMatches(exemption.target, diagnostic.target));
    if (matches.length === 0) {
      remaining.push(diagnostic);
    } else if (matches.length === 1) {
      matchedIds.add(matches[0].id);
    } else {
      configDiagnostics.push(configDiagnostic(
        "EXEMPTION_AMBIGUOUS",
        `diagnostic ${diagnostic.rule} at ${formatTarget(diagnostic.target)} matches multiple exemptions: ${matches.map((entry) => entry.id).join(", ")}`
      ));
      remaining.push(diagnostic);
    }
  }
  for (const exemption of validation.valid) {
    if (!matchedIds.has(exemption.id)) {
      configDiagnostics.push(configDiagnostic(
        "EXEMPTION_UNUSED",
        `exemption ${exemption.id} does not match a current ${exemption.rule} diagnostic`,
        exemption.id
      ));
    }
  }
  return { diagnostics: remaining, exempted: [...matchedIds], configDiagnostics };
}

function compatibilityPaths(inventory, root) {
  const reference = inventory.publishedReference;
  if (!isPlainObject(reference)) {
    fail("protocol inventory must define publishedReference");
  }
  const paths = {
    exemption: inventory.breakingExemptions?.file,
    manifest: reference.manifestFile,
    release: reference.baselineFile
  };
  for (const [name, relativePath] of Object.entries(paths)) {
    if (!isNonEmptyString(relativePath) || !relativePath.startsWith("packages/proto/compatibility/") || relativePath.includes("..")) {
      fail(`protocol inventory has an invalid ${name} compatibility path`);
    }
    paths[name] = path.join(root, relativePath);
  }
  return paths;
}

function readJson(filePath, label) {
  if (!existsSync(filePath)) {
    fail(`${label} does not exist: ${normalizePath(filePath)}`);
  }
  try {
    return JSON.parse(readFileSync(filePath, "utf8"));
  } catch (error) {
    fail(`${label} is not valid JSON: ${normalizePath(filePath)} (${error.message})`);
  }
}

export function loadPublishedReference(inventory, root = rootDir) {
  const paths = compatibilityPaths(inventory, root);
  const manifest = readJson(paths.manifest, "published protocol reference manifest");
  if (!isPlainObject(manifest) || manifest.schemaVersion !== RELEASE_REFERENCE_SCHEMA_VERSION || !isPlainObject(manifest.release)) {
    fail("published protocol reference manifest has an unsupported format");
  }
  const expectedReleasePath = normalizePath(path.relative(root, paths.release));
  if (manifest.baselineFile !== expectedReleasePath) {
    fail(`published protocol reference manifest baselineFile must be ${expectedReleasePath}`);
  }
  for (const key of ["releaseId", "reason", "approvedBy"]) {
    if (!isNonEmptyString(manifest.release[key])) {
      fail(`published protocol reference manifest release.${key} must be a non-empty string`);
    }
  }
  if (!isIsoDate(manifest.release.approvedAt)) {
    fail("published protocol reference manifest release.approvedAt must be a valid YYYY-MM-DD date");
  }
  const baseline = validateBaselineSnapshot(readJson(paths.release, "published protocol reference baseline"), "published protocol reference baseline");
  if (manifest.baselineDigest !== baseline.digest) {
    fail("published protocol reference manifest baselineDigest does not match release baseline");
  }
  return { baseline, manifest, paths };
}

function readExemptions(inventory, root) {
  const paths = compatibilityPaths(inventory, root);
  return readJson(paths.exemption, "breaking exemption document");
}

export function checkBreakingChanges({ candidate, exemptions, inventory, released, today = todayUtc() }) {
  const breaking = compareProtocolBaselines(released, candidate, inventory);
  const exempted = applyExemptions(breaking, exemptions, today);
  return {
    diagnostics: exempted.diagnostics,
    exempted: exempted.exempted,
    errors: [...exempted.configDiagnostics, ...exempted.diagnostics],
    released: released.digest
  };
}

export function checkBreakingChangesForRepository(inventory = readInventory(), root = rootDir, today = todayUtc()) {
  // Candidate baseline must match the working proto before comparison with the immutable release reference.
  const candidate = checkBaseline(inventory, root);
  const released = loadPublishedReference(inventory, root);
  return checkBreakingChanges({
    candidate,
    exemptions: readExemptions(inventory, root),
    inventory,
    released: released.baseline,
    today
  });
}

function formatTarget(target) {
  const entries = Object.entries(target).map(([key, value]) => `${key}=${value}`);
  return entries.join(" ");
}

export function formatDiagnostic(diagnostic) {
  const classifications = diagnostic.classifications.length > 0
    ? ` classifications=[${diagnostic.classifications.join(",")}]`
    : "";
  const target = Object.keys(diagnostic.target).length > 0 ? ` ${formatTarget(diagnostic.target)}` : "";
  const guidance = diagnostic.audience === "CONFIGURATION" ? "" : ` ${diagnostic.guidance}`;
  return `${diagnostic.audience} ${diagnostic.rule}: ${diagnostic.file}${target} - ${diagnostic.detail}.${classifications}${guidance}`;
}

function requireCliValue(args, name) {
  const indexes = args.flatMap((value, index) => value === name ? [index] : []);
  if (indexes.length !== 1 || !isNonEmptyString(args[indexes[0] + 1])) {
    fail(`--promote-release requires ${name} <value>`);
  }
  return args[indexes[0] + 1].trim();
}

function ensurePromotionArguments(args) {
  const valueOptions = new Set(["--release-id", "--reason", "--approved-by", "--approved-at"]);
  for (let index = 0; index < args.length; index += 1) {
    const argument = args[index];
    if (argument === "--promote-release") continue;
    if (!valueOptions.has(argument) || index + 1 >= args.length) {
      fail("Usage: node tools/check-proto-breaking-changes.js [--check] | --promote-release --release-id <id> --reason <reason> --approved-by <owner> --approved-at <YYYY-MM-DD>");
    }
    index += 1;
  }
}

export function promotePublishedReference(inventory, root, metadata) {
  if (!isIsoDate(metadata.approvedAt)) {
    fail("--approved-at must be a valid YYYY-MM-DD date");
  }
  const candidate = checkBaseline(inventory, root);
  const paths = compatibilityPaths(inventory, root);
  const hasManifest = existsSync(paths.manifest);
  const hasRelease = existsSync(paths.release);
  if (hasManifest !== hasRelease) {
    fail("published reference is incomplete: release baseline and manifest must be added or updated together");
  }
  if (hasManifest) {
    const result = checkBreakingChangesForRepository(inventory, root);
    if (result.errors.length > 0) {
      fail(`refusing to promote a candidate with unresolved compatibility diagnostics:\n${result.errors.map(formatDiagnostic).join("\n")}`);
    }
  }
  writeFileSync(paths.release, `${JSON.stringify(candidate, null, 2)}\n`, "utf8");
  const manifest = {
    baselineDigest: candidate.digest,
    baselineFile: normalizePath(path.relative(root, paths.release)),
    release: {
      approvedAt: metadata.approvedAt,
      approvedBy: metadata.approvedBy,
      reason: metadata.reason,
      releaseId: metadata.releaseId
    },
    schemaVersion: RELEASE_REFERENCE_SCHEMA_VERSION
  };
  writeFileSync(paths.manifest, `${JSON.stringify(manifest, null, 2)}\n`, "utf8");
  return manifest;
}

function runCli() {
  const args = process.argv.slice(2);
  const promote = args.includes("--promote-release");
  if (promote) {
    ensurePromotionArguments(args);
    const metadata = {
      approvedAt: requireCliValue(args, "--approved-at"),
      approvedBy: requireCliValue(args, "--approved-by"),
      reason: requireCliValue(args, "--reason"),
      releaseId: requireCliValue(args, "--release-id")
    };
    const manifest = promotePublishedReference(readInventory(), rootDir, metadata);
    console.log(`promoted published protocol reference ${manifest.release.releaseId} (${manifest.baselineDigest})`);
    return;
  }
  if (args.length > 0 && !(args.length === 1 && args[0] === "--check")) {
    fail("Usage: node tools/check-proto-breaking-changes.js [--check] | --promote-release --release-id <id> --reason <reason> --approved-by <owner> --approved-at <YYYY-MM-DD>");
  }
  const result = checkBreakingChangesForRepository();
  if (result.errors.length > 0) {
    fail(`published protocol compatibility check found ${result.errors.length} issue(s):\n${result.errors.map(formatDiagnostic).join("\n")}`);
  }
  console.log(`published protocol compatibility check passed against ${result.released}`);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    runCli();
  } catch (error) {
    console.error(`proto breaking compatibility check failed: ${error.message}`);
    process.exitCode = 1;
  }
}
