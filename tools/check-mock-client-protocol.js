import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

import {
  MESSAGE_TYPE,
  MOVE_INPUT_TYPE,
  MOVEMENT_CORRECTION_KIND,
  MOVEMENT_CORRECTION_REASON
} from "./mock-client/src/constants.js";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const rootDir = path.resolve(__dirname, "..");

const PROTO_ENUMS = [
  {
    enumName: "MoveInputType",
    prefix: "MOVE_INPUT_TYPE_",
    actual: MOVE_INPUT_TYPE
  },
  {
    enumName: "MovementCorrectionKind",
    prefix: "MOVEMENT_CORRECTION_KIND_",
    actual: MOVEMENT_CORRECTION_KIND
  },
  {
    enumName: "MovementCorrectionReason",
    prefix: "MOVEMENT_CORRECTION_REASON_",
    actual: MOVEMENT_CORRECTION_REASON
  }
];

function readRepoFile(relativePath) {
  return readFileSync(path.join(rootDir, relativePath), "utf8");
}

function stripComments(source) {
  return source
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .replace(/\/\/.*$/gm, "");
}

function parseProtoEnum(source, enumName, prefix) {
  const cleanSource = stripComments(source);
  const enumPattern = new RegExp(`enum\\s+${enumName}\\s*\\{([\\s\\S]*?)\\}`, "m");
  const match = cleanSource.match(enumPattern);
  if (!match) {
    throw new Error(`enum ${enumName} not found in packages/proto/game.proto`);
  }

  const values = {};
  const valuePattern = /([A-Z][A-Z0-9_]*)\s*=\s*(\d+)\s*;/g;
  for (const valueMatch of match[1].matchAll(valuePattern)) {
    const protoName = valueMatch[1];
    if (!protoName.startsWith(prefix)) {
      throw new Error(`${enumName}.${protoName} does not start with expected prefix ${prefix}`);
    }
    values[protoName.slice(prefix.length)] = Number(valueMatch[2]);
  }
  return values;
}

function rustVariantToConstantName(name) {
  return name
    .replace(/([a-z0-9])([A-Z])/g, "$1_$2")
    .replace(/([A-Z]+)([A-Z][a-z])/g, "$1_$2")
    .toUpperCase();
}

function parseRustMessageTypes(source) {
  const cleanSource = stripComments(source);
  const enumMatch = cleanSource.match(/pub\s+enum\s+MessageType\s*\{([\s\S]*?)\n\}/m);
  if (!enumMatch) {
    throw new Error("MessageType enum not found in apps/game-server/src/protocol/message_type.rs");
  }

  const values = {};
  const valuePattern = /([A-Za-z][A-Za-z0-9_]*)\s*=\s*(\d+)\s*,/g;
  for (const valueMatch of enumMatch[1].matchAll(valuePattern)) {
    values[rustVariantToConstantName(valueMatch[1])] = Number(valueMatch[2]);
  }
  return values;
}

function compareObject(label, expected, actual) {
  const errors = [];
  for (const [key, expectedValue] of Object.entries(expected)) {
    if (!(key in actual)) {
      errors.push(`${label}.${key} missing, expected ${expectedValue}`);
      continue;
    }
    if (actual[key] !== expectedValue) {
      errors.push(`${label}.${key} = ${actual[key]}, expected ${expectedValue}`);
    }
  }
  return errors;
}

function main() {
  const errors = [];
  const protoSource = readRepoFile("packages/proto/game.proto");
  const rustSource = readRepoFile("apps/game-server/src/protocol/message_type.rs");

  const expectedMessageTypes = parseRustMessageTypes(rustSource);
  errors.push(...compareObject("MESSAGE_TYPE", expectedMessageTypes, MESSAGE_TYPE));

  for (const enumSpec of PROTO_ENUMS) {
    const expectedEnum = parseProtoEnum(protoSource, enumSpec.enumName, enumSpec.prefix);
    errors.push(...compareObject(enumSpec.enumName, expectedEnum, enumSpec.actual));
  }

  if (errors.length > 0) {
    console.error("mock-client protocol constants drift detected:");
    for (const error of errors) {
      console.error(`- ${error}`);
    }
    process.exit(1);
  }

  console.log("mock-client protocol constants are in sync with proto/Rust sources.");
}

main();
