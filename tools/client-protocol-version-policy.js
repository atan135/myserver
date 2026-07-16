import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

export const VERSION_POLICY_PATH = path.join(
  rootDir,
  "packages",
  "proto",
  "compatibility",
  "version-policy.json"
);

export function readClientProtocolVersionPolicy(policyPath = VERSION_POLICY_PATH) {
  return JSON.parse(readFileSync(policyPath, "utf8"));
}

export function validateClientProtocolVersionPolicy(policy) {
  const application = policy?.applicationProtocol;
  const tooOld = policy?.rejections?.tooOld;
  const tooNew = policy?.rejections?.tooNew;
  const errors = [];

  if (policy?.schemaVersion !== 1) errors.push("schemaVersion must be 1");
  if (policy?.packetHeaderVersion !== 1) errors.push("packetHeaderVersion must remain 1");
  for (const [name, value] of Object.entries({
    currentClientProtocolVersion: application?.currentClientProtocolVersion,
    minimumClientProtocolVersion: application?.minimumClientProtocolVersion,
    legacyImplicitProtocolVersion: application?.legacyImplicitProtocolVersion
  })) {
    if (!Number.isInteger(value) || value < 1) errors.push(`${name} must be a positive integer`);
  }
  if (application?.minimumClientProtocolVersion > application?.currentClientProtocolVersion) {
    errors.push("minimumClientProtocolVersion cannot exceed currentClientProtocolVersion");
  }
  for (const [name, rejection] of Object.entries({ tooOld, tooNew })) {
    if (!/^[A-Z][A-Z0-9_]+$/.test(rejection?.errorCode || "")) {
      errors.push(`${name}.errorCode must be an uppercase protocol error code`);
    }
    if (typeof rejection?.defaultUpgradeMessage !== "string" || !rejection.defaultUpgradeMessage.trim()) {
      errors.push(`${name}.defaultUpgradeMessage must be non-empty`);
    }
    if (typeof rejection?.defaultUpgradeUrl !== "string") {
      errors.push(`${name}.defaultUpgradeUrl must be a string`);
    }
  }
  return errors;
}

export function negotiateClientProtocolVersion(declaredVersion, policy = readClientProtocolVersionPolicy()) {
  const errors = validateClientProtocolVersionPolicy(policy);
  if (errors.length > 0) throw new Error(`invalid client protocol version policy: ${errors.join("; ")}`);

  const application = policy.applicationProtocol;
  const source = declaredVersion === 0 ? "legacy_implicit" : "explicit";
  const effectiveVersion = declaredVersion === 0
    ? application.legacyImplicitProtocolVersion
    : declaredVersion;
  if (effectiveVersion < application.minimumClientProtocolVersion) {
    return {
      accepted: false,
      effectiveVersion,
      errorCode: policy.rejections.tooOld.errorCode,
      source,
      upgradeMessage: policy.rejections.tooOld.defaultUpgradeMessage,
      upgradeUrl: policy.rejections.tooOld.defaultUpgradeUrl
    };
  }
  if (effectiveVersion > application.currentClientProtocolVersion) {
    return {
      accepted: false,
      effectiveVersion,
      errorCode: policy.rejections.tooNew.errorCode,
      source,
      upgradeMessage: policy.rejections.tooNew.defaultUpgradeMessage,
      upgradeUrl: policy.rejections.tooNew.defaultUpgradeUrl
    };
  }
  return { accepted: true, effectiveVersion, source };
}

export function verifyClientProtocolVersionImplementation({
  policy = readClientProtocolVersionPolicy(),
  protoSource = readFileSync(path.join(rootDir, "packages", "proto", "game.proto"), "utf8"),
  rustPolicySource = readFileSync(path.join(rootDir, "packages", "proto", "compatibility", "version-policy.rs"), "utf8")
} = {}) {
  const errors = validateClientProtocolVersionPolicy(policy);
  const application = policy.applicationProtocol;
  const expectedProtoFields = [
    /uint32\s+client_protocol_version\s*=\s*2\s*;/,
    /uint32\s+server_protocol_version\s*=\s*4\s*;/,
    /uint32\s+minimum_client_protocol_version\s*=\s*5\s*;/,
    /string\s+upgrade_message\s*=\s*6\s*;/,
    /string\s+upgrade_url\s*=\s*7\s*;/
  ];
  for (const pattern of expectedProtoFields) {
    if (!pattern.test(protoSource)) errors.push(`game.proto lacks required version field ${pattern}`);
  }
  const expectedRustConstants = [
    ["PACKET_HEADER_VERSION", policy.packetHeaderVersion],
    ["CURRENT_CLIENT_PROTOCOL_VERSION", application.currentClientProtocolVersion],
    ["MINIMUM_CLIENT_PROTOCOL_VERSION", application.minimumClientProtocolVersion],
    ["LEGACY_IMPLICIT_PROTOCOL_VERSION", application.legacyImplicitProtocolVersion]
  ];
  for (const [name, value] of expectedRustConstants) {
    if (!new RegExp(`pub const ${name}: \\w+ = ${value};`).test(rustPolicySource)) {
      errors.push(`version-policy.rs does not match ${name}=${value}`);
    }
  }
  for (const errorCode of [policy.rejections.tooOld.errorCode, policy.rejections.tooNew.errorCode]) {
    if (!rustPolicySource.includes(`"${errorCode}"`)) {
      errors.push(`version-policy.rs does not contain ${errorCode}`);
    }
  }
  return errors;
}

function main() {
  const errors = verifyClientProtocolVersionImplementation();
  if (errors.length > 0) {
    throw new Error(`client protocol version policy check failed:\n${errors.map((error) => `- ${error}`).join("\n")}`);
  }
  process.stdout.write("client protocol version policy check passed.\n");
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    main();
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exit(1);
  }
}
