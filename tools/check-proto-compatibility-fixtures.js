import { createHash } from "node:crypto";
import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

import {
  PROTO_COMPATIBILITY_FIXTURES,
  buildFixtureManifest
} from "./generate-proto-compatibility-fixtures.js";

const REPOSITORY_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
export const DEFAULT_FIXTURE_DIRECTORY = path.join(REPOSITORY_ROOT, "tests", "proto", "fixtures", "compatibility");
export const DEFAULT_MANIFEST_PATH = path.join(DEFAULT_FIXTURE_DIRECTORY, "manifest.json");

const SENSITIVE_PATTERNS = [
  {
    code: "FIXTURE_SENSITIVE_JWT",
    expression: /\beyJ[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}\b/,
    description: "JWT-like value"
  },
  {
    code: "FIXTURE_SENSITIVE_BEARER",
    expression: /\bBearer\s+[A-Za-z0-9._~-]{16,}\b/i,
    description: "Bearer credential"
  },
  {
    code: "FIXTURE_SENSITIVE_EMAIL",
    expression: /\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b/i,
    description: "email-like account identifier"
  },
  {
    code: "FIXTURE_SENSITIVE_PRIVATE_KEY",
    expression: /-----BEGIN(?: [A-Z]+)? PRIVATE KEY-----/,
    description: "private key material"
  },
  {
    code: "FIXTURE_SENSITIVE_TICKET",
    expression: /\b(?:game[_-]?)?ticket\s*[:=]\s*["']?[A-Za-z0-9._~-]{12,}/i,
    description: "ticket-like credential"
  }
];

function digest(body) {
  return `sha256:${createHash("sha256").update(body).digest("hex")}`;
}

function diagnostic(code, subject, message) {
  return { code, subject, message };
}

function isSyntheticIdentityKey(key) {
  return /(?:account|player|character|room|server|sender|target|owner)(?:[_-]?id)?$/i.test(key);
}

function collectSyntheticIdentityDiagnostics(value, subject, diagnostics, key = "") {
  if (Array.isArray(value)) {
    for (const entry of value) {
      collectSyntheticIdentityDiagnostics(entry, subject, diagnostics, key);
    }
    return;
  }

  if (value && typeof value === "object") {
    for (const [childKey, childValue] of Object.entries(value)) {
      collectSyntheticIdentityDiagnostics(childValue, subject, diagnostics, childKey);
    }
    return;
  }

  if (typeof value === "string" && isSyntheticIdentityKey(key) && !/^(fixture|fake)_[a-z0-9_:-]+$/i.test(value)) {
    diagnostics.push(diagnostic(
      "FIXTURE_IDENTITY_NOT_SYNTHETIC",
      subject,
      `${key} must use a fixture_ or fake_ identifier, received ${JSON.stringify(value)}`
    ));
  }
}

function listFixtureBins(directory) {
  const files = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const entryPath = path.join(directory, entry.name);
    if (entry.isDirectory()) {
      files.push(...listFixtureBins(entryPath));
    } else if (entry.isFile() && entry.name.endsWith(".bin")) {
      files.push(entryPath);
    }
  }
  return files;
}

export function scanFixtureContent(content, subject = "fixture") {
  const text = Buffer.isBuffer(content) ? content.toString("utf8") : String(content);
  const diagnostics = [];
  for (const pattern of SENSITIVE_PATTERNS) {
    if (pattern.expression.test(text)) {
      diagnostics.push(diagnostic(pattern.code, subject, `${pattern.description} is forbidden in protocol fixtures`));
    }
  }
  return diagnostics;
}

export function validateProtoCompatibilityFixtures({
  fixtureDirectory = DEFAULT_FIXTURE_DIRECTORY,
  manifestPath = DEFAULT_MANIFEST_PATH
} = {}) {
  const diagnostics = [];
  if (!existsSync(manifestPath)) {
    return { diagnostics: [diagnostic("FIXTURE_MANIFEST_MISSING", manifestPath, "manifest.json is missing")], fixtures: [] };
  }
  if (!existsSync(fixtureDirectory)) {
    return { diagnostics: [diagnostic("FIXTURE_DIRECTORY_MISSING", fixtureDirectory, "fixture directory is missing")], fixtures: [] };
  }

  let manifest;
  try {
    manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
  } catch (error) {
    return { diagnostics: [diagnostic("FIXTURE_MANIFEST_INVALID", manifestPath, error.message)], fixtures: [] };
  }

  if (manifest.schema !== "myserver.protobuf.binary-fixtures/v1") {
    diagnostics.push(diagnostic("FIXTURE_MANIFEST_SCHEMA", manifestPath, "schema must be myserver.protobuf.binary-fixtures/v1"));
  }
  if (manifest.bodyFormat !== "protobuf_body_without_tcp_header") {
    diagnostics.push(diagnostic("FIXTURE_BODY_FORMAT", manifestPath, "fixtures must store protobuf bodies without TCP headers"));
  }
  if (manifest.syntheticData?.declared !== true || manifest.syntheticData?.identityPrefix !== "fixture_") {
    diagnostics.push(diagnostic("FIXTURE_SYNTHETIC_DECLARATION", manifestPath, "manifest must declare fixture_ synthetic data"));
  }
  if (!Number.isInteger(manifest.limits?.maxFixtureBodyBytes) || manifest.limits.maxFixtureBodyBytes <= 0) {
    diagnostics.push(diagnostic("FIXTURE_BODY_LIMIT", manifestPath, "limits.maxFixtureBodyBytes must be a positive integer"));
  }
  if (!Array.isArray(manifest.fixtures) || manifest.fixtures.length === 0) {
    diagnostics.push(diagnostic("FIXTURE_MANIFEST_EMPTY", manifestPath, "manifest must contain at least one fixture"));
    return { diagnostics, fixtures: [] };
  }

  const generatedManifest = buildFixtureManifest();
  if (JSON.stringify(manifest) !== JSON.stringify(generatedManifest)) {
    diagnostics.push(diagnostic(
      "FIXTURE_MANIFEST_SOURCE_DRIFT",
      manifestPath,
      "manifest.json differs from the reviewed fixture definitions; regenerate deliberately"
    ));
  }
  const generatedFixtures = new Map(PROTO_COMPATIBILITY_FIXTURES.map((fixture) => [fixture.file, fixture]));
  diagnostics.push(...scanFixtureContent(readFileSync(manifestPath), manifestPath));
  const declaredFiles = new Set();
  for (const fixture of manifest.fixtures) {
    const subject = fixture.file || "<unnamed fixture>";
    if (typeof fixture.file !== "string" || fixture.file.includes("..") || path.isAbsolute(fixture.file) || !fixture.file.endsWith(".bin")) {
      diagnostics.push(diagnostic("FIXTURE_FILE_INVALID", subject, "file must be a relative .bin path"));
      continue;
    }
    if (declaredFiles.has(fixture.file)) {
      diagnostics.push(diagnostic("FIXTURE_FILE_DUPLICATE", subject, "fixture file is declared more than once"));
      continue;
    }
    declaredFiles.add(fixture.file);

    if (!Number.isInteger(fixture.messageType) || fixture.messageType <= 0 || !fixture.proto?.file || !fixture.proto?.message) {
      diagnostics.push(diagnostic("FIXTURE_PROTOCOL_METADATA", subject, "messageType and proto file/message are required"));
    }
    if (!fixture.source || !fixture.expectations) {
      diagnostics.push(diagnostic("FIXTURE_READABLE_SOURCE_MISSING", subject, "source and expectations are required for review"));
    }
    collectSyntheticIdentityDiagnostics(fixture.source, subject, diagnostics);

    const bodyPath = path.resolve(fixtureDirectory, fixture.file);
    if (!bodyPath.startsWith(`${path.resolve(fixtureDirectory)}${path.sep}`) || !existsSync(bodyPath)) {
      diagnostics.push(diagnostic("FIXTURE_BODY_MISSING", subject, "declared binary body is missing"));
      continue;
    }
    const body = readFileSync(bodyPath);
    const generatedFixture = generatedFixtures.get(fixture.file);
    if (!generatedFixture) {
      diagnostics.push(diagnostic("FIXTURE_SOURCE_MISSING", subject, "fixture is absent from the reviewed generator definitions"));
    } else if (!body.equals(generatedFixture.body)) {
      diagnostics.push(diagnostic(
        "FIXTURE_BINARY_SOURCE_DRIFT",
        subject,
        "binary differs from the reviewed fixture definition; regenerate deliberately"
      ));
    }
    if (body.length === 0) {
      diagnostics.push(diagnostic("FIXTURE_BODY_EMPTY", subject, "fixture body must be non-empty"));
    }
    if (body.length > manifest.limits.maxFixtureBodyBytes) {
      diagnostics.push(diagnostic("FIXTURE_BODY_TOO_LARGE", subject, `${body.length} bytes exceeds ${manifest.limits.maxFixtureBodyBytes}`));
    }
    if (fixture.byteLength !== body.length) {
      diagnostics.push(diagnostic("FIXTURE_LENGTH_MISMATCH", subject, `manifest=${fixture.byteLength}, actual=${body.length}`));
    }
    if (fixture.sha256 !== digest(body)) {
      diagnostics.push(diagnostic("FIXTURE_DIGEST_MISMATCH", subject, `manifest=${fixture.sha256}, actual=${digest(body)}`));
    }
    diagnostics.push(...scanFixtureContent(body, subject));
  }

  const actualFiles = new Set(listFixtureBins(fixtureDirectory).map((file) => path.relative(fixtureDirectory, file).split(path.sep).join("/")));
  for (const file of actualFiles) {
    if (!declaredFiles.has(file)) {
      diagnostics.push(diagnostic("FIXTURE_BODY_UNDECLARED", file, "binary fixture is not listed in manifest.json"));
    }
  }
  for (const file of declaredFiles) {
    if (!actualFiles.has(file)) {
      diagnostics.push(diagnostic("FIXTURE_BODY_MISSING", file, "manifest fixture is not present on disk"));
    }
  }
  for (const file of generatedFixtures.keys()) {
    if (!declaredFiles.has(file)) {
      diagnostics.push(diagnostic("FIXTURE_SOURCE_UNDECLARED", file, "generator definition is not listed in manifest.json"));
    }
  }

  return { diagnostics, fixtures: manifest.fixtures, manifest };
}

function formatDiagnostics(diagnostics) {
  return diagnostics.map((entry) => `[${entry.code}] ${entry.subject}: ${entry.message}`).join("\n");
}

const invokedPath = process.argv[1] ? path.resolve(process.argv[1]) : "";
if (invokedPath === fileURLToPath(import.meta.url)) {
  const result = validateProtoCompatibilityFixtures();
  if (result.diagnostics.length > 0) {
    console.error(formatDiagnostics(result.diagnostics));
    process.exitCode = 1;
  } else {
    console.log(`Protocol compatibility fixtures passed: ${result.fixtures.length} fixture(s)`);
  }
}
