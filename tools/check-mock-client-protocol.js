import { existsSync, readFileSync } from "node:fs";
import { fileURLToPath, pathToFileURL } from "node:url";
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

const FIELD_HELPERS = {
  encodeStringField: "string",
  encodeBoolField: "bool",
  encodeInt64Field: "varint64",
  encodeUInt32Field: "varint32",
  encodeInt32Field: "varint32",
  encodeFloatField: "float",
  readString: "string",
  readStringList: "repeated_string",
  readBool: "bool",
  readInt64: "varint64",
  readUInt32: "varint32",
  readInt32List: "repeated_varint32",
  readFloat: "float"
};

const PROTO_SCALARS = {
  string: "string",
  bytes: "bytes",
  bool: "bool",
  int32: "varint32",
  uint32: "varint32",
  sint32: "varint32",
  int64: "varint64",
  uint64: "varint64",
  sint64: "varint64",
  float: "float",
  double: "double"
};

function readRepoFile(relativePath) {
  return readFileSync(path.join(rootDir, relativePath), "utf8");
}

function readFile(filePath) {
  return readFileSync(filePath, "utf8");
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

function parseProtoEnumNames(source) {
  const cleanSource = stripComments(source);
  const enumNames = new Set();
  const enumPattern = /enum\s+([A-Za-z_][A-Za-z0-9_]*)\s*\{/g;
  for (const match of cleanSource.matchAll(enumPattern)) {
    enumNames.add(match[1]);
  }
  return enumNames;
}

function parseProtoMessages(source) {
  const cleanSource = stripComments(source);
  const messages = new Map();
  const messagePattern = /message\s+([A-Za-z_][A-Za-z0-9_]*)\s*\{([\s\S]*?)\}/g;
  const fieldPattern = /(?:(repeated|optional)\s+)?([A-Za-z_][A-Za-z0-9_.]*)\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(\d+)\s*;/g;

  for (const messageMatch of cleanSource.matchAll(messagePattern)) {
    const fields = new Map();
    for (const fieldMatch of messageMatch[2].matchAll(fieldPattern)) {
      const [, label, type, fieldName, tag] = fieldMatch;
      fields.set(Number(tag), {
        name: fieldName,
        type,
        repeated: label === "repeated",
        optional: label === "optional"
      });
    }
    messages.set(messageMatch[1], fields);
  }
  return messages;
}

function mergeProtoMessages(...messageMaps) {
  const merged = new Map();
  for (const messages of messageMaps) {
    for (const [messageName, fields] of messages.entries()) {
      const existing = merged.get(messageName);
      if (!existing) {
        merged.set(messageName, fields);
        continue;
      }
      if (JSON.stringify([...existing.entries()]) !== JSON.stringify([...fields.entries()])) {
        throw new Error(`conflicting proto message definitions for ${messageName}`);
      }
    }
  }
  return merged;
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
    throw new Error("MessageType enum not found in Rust source");
  }

  const values = {};
  const valuePattern = /([A-Za-z][A-Za-z0-9_]*)\s*=\s*(\d+)\s*,/g;
  for (const valueMatch of enumMatch[1].matchAll(valuePattern)) {
    values[rustVariantToConstantName(valueMatch[1])] = Number(valueMatch[2]);
  }
  return values;
}

function parseRustMessageTypeFromU16(source) {
  const cleanSource = stripComments(source);
  const values = {};
  const valuePattern = /(\d+)\s*=>\s*Some\(Self::([A-Za-z][A-Za-z0-9_]*)\),/g;
  for (const valueMatch of cleanSource.matchAll(valuePattern)) {
    values[rustVariantToConstantName(valueMatch[2])] = Number(valueMatch[1]);
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

function compareSubset(label, expected, actual) {
  const errors = [];
  for (const [key, actualValue] of Object.entries(actual)) {
    if (!(key in expected)) {
      errors.push(`${label}.${key} is defined as ${actualValue}, but is missing from canonical game-server MessageType`);
      continue;
    }
    if (expected[key] !== actualValue) {
      errors.push(`${label}.${key} = ${actualValue}, expected ${expected[key]}`);
    }
  }
  return errors;
}

function firstExistingPath(baseDir, relativePaths) {
  for (const relativePath of relativePaths) {
    const candidate = path.join(baseDir, relativePath);
    if (existsSync(candidate)) {
      return candidate;
    }
  }
  return null;
}

function addFieldUsage(usages, messageName, fieldNumber, category) {
  if (!messageName || !fieldNumber || !category) {
    return;
  }
  let messageUsage = usages.get(messageName);
  if (!messageUsage) {
    messageUsage = new Map();
    usages.set(messageName, messageUsage);
  }
  let fieldUsage = messageUsage.get(Number(fieldNumber));
  if (!fieldUsage) {
    fieldUsage = new Set();
    messageUsage.set(Number(fieldNumber), fieldUsage);
  }
  fieldUsage.add(category);
}

function pascalCaseFromConstantName(name) {
  return name
    .split("_")
    .map((part) => part.charAt(0) + part.slice(1).toLowerCase())
    .join("");
}

function parseJsFunctionBodies(source, functionPattern) {
  const bodies = new Map();
  for (const match of source.matchAll(functionPattern)) {
    const name = match[1];
    let position = match.index + match[0].length;
    let depth = 1;
    while (position < source.length && depth > 0) {
      const char = source[position];
      if (char === "{") {
        depth += 1;
      } else if (char === "}") {
        depth -= 1;
      }
      position += 1;
    }
    bodies.set(name, source.slice(match.index + match[0].length, position - 1));
  }
  return bodies;
}

function extractFieldUsagesFromBody(body, fieldsName, decoderMessages) {
  const fieldsPattern = fieldsName.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const usages = [];
  const helperPattern = new RegExp(
    `\\b(${Object.keys(FIELD_HELPERS).join("|")})\\s*\\(\\s*${fieldsPattern}\\s*,\\s*(\\d+)`,
    "g"
  );

  for (const match of body.matchAll(helperPattern)) {
    usages.push({ fieldNumber: Number(match[2]), category: FIELD_HELPERS[match[1]] });
  }

  const repeatedMessagePattern = new RegExp(
    `decodeRepeatedMessage\\s*\\(\\s*${fieldsPattern}\\s*,\\s*(\\d+)\\s*,\\s*(decode[A-Za-z0-9_]+)`,
    "g"
  );
  for (const match of body.matchAll(repeatedMessagePattern)) {
    if (decoderMessages.has(match[2])) {
      usages.push({ fieldNumber: Number(match[1]), category: "message" });
    }
  }

  const directMessagePattern = new RegExp(
    `${fieldsPattern}\\.get\\(\\s*(\\d+)\\s*\\)[\\s\\S]{0,160}?\\b(decode[A-Za-z0-9_]+)\\s*\\(`,
    "g"
  );
  for (const match of body.matchAll(directMessagePattern)) {
    if (decoderMessages.has(match[2])) {
      usages.push({ fieldNumber: Number(match[1]), category: "message" });
    }
  }

  const rawFieldPattern = new RegExp(`\\bconst\\s+([A-Za-z_][A-Za-z0-9_]*)\\s*=\\s*${fieldsPattern}\\.get\\(\\s*(\\d+)\\s*\\)`, "g");
  for (const match of body.matchAll(rawFieldPattern)) {
    const [, variableName, fieldNumber] = match;
    const variablePattern = new RegExp(
      `\\b${variableName}\\b[\\s\\S]{0,320}?\\b(decode[A-Za-z0-9_]+)\\s*\\(`,
      "m"
    );
    const variableMatch = body.match(variablePattern);
    usages.push({
      fieldNumber: Number(fieldNumber),
      category: variableMatch && decoderMessages.has(variableMatch[1]) ? "message" : "length_delimited"
    });
  }

  return usages;
}

function parseDecodeByMessageTypeCases(body) {
  const cases = [];
  const casePattern = /case\s+MESSAGE_TYPE\.([A-Z0-9_]+)\s*:/g;
  const matches = [...body.matchAll(casePattern)];
  let pendingLabels = [];

  for (let index = 0; index < matches.length; index += 1) {
    const match = matches[index];
    pendingLabels.push(match[1]);
    const segmentStart = match.index + match[0].length;
    const segmentEnd = index + 1 < matches.length ? matches[index + 1].index : body.length;
    const segment = body.slice(segmentStart, segmentEnd);
    if (!/\breturn\b/.test(segment)) {
      continue;
    }
    const delegateMatch = segment.match(/\breturn\s+(decode[A-Za-z0-9_]+)\s*\(\s*fields\s*\)\s*;?/);
    for (const label of pendingLabels) {
      cases.push({
        messageName: pascalCaseFromConstantName(label),
        body: segment,
        delegatedDecoder: delegateMatch?.[1] ?? null
      });
    }
    pendingLabels = [];
  }

  return cases;
}

export function parseMockClientFieldUsages(source) {
  const cleanSource = stripComments(source);
  const usages = new Map();
  const functionBodies = parseJsFunctionBodies(cleanSource, /\b(?:export\s+)?function\s+([A-Za-z_][A-Za-z0-9_]*)\s*\([^)]*\)\s*\{/g);
  const decoderMessages = new Map();
  const decodeByMessageType = functionBodies.get("decodeByMessageType");
  const messageCases = decodeByMessageType ? parseDecodeByMessageTypeCases(decodeByMessageType) : [];
  const delegatedDecoders = new Set(messageCases.map((messageCase) => messageCase.delegatedDecoder).filter(Boolean));

  for (const [functionName] of functionBodies.entries()) {
    const decoderMatch = functionName.match(/^decode(.+)$/);
    if (decoderMatch && functionName !== "decodeByMessageType") {
      decoderMessages.set(functionName, decoderMatch[1]);
    }
  }

  for (const [functionName, body] of functionBodies.entries()) {
    const encoderMatch = functionName.match(/^encode(.+)$/);
    if (encoderMatch) {
      const messageName = encoderMatch[1];
      const helperPattern = /\b(encodeStringField|encodeBoolField|encodeInt64Field|encodeUInt32Field|encodeInt32Field|encodeFloatField)\s*\(\s*(\d+)/g;
      for (const match of body.matchAll(helperPattern)) {
        addFieldUsage(usages, messageName, Number(match[2]), FIELD_HELPERS[match[1]]);
      }
      continue;
    }

    const messageName = decoderMessages.get(functionName);
    if (messageName && !delegatedDecoders.has(functionName)) {
      for (const usage of extractFieldUsagesFromBody(body, "fields", decoderMessages)) {
        addFieldUsage(usages, messageName, usage.fieldNumber, usage.category);
      }
    }
  }

  for (const messageCase of messageCases) {
    for (const usage of extractFieldUsagesFromBody(messageCase.body, "fields", decoderMessages)) {
      addFieldUsage(usages, messageCase.messageName, usage.fieldNumber, usage.category);
    }
    if (messageCase.delegatedDecoder) {
      const delegatedBody = functionBodies.get(messageCase.delegatedDecoder);
      if (!delegatedBody) {
        continue;
      }
      for (const usage of extractFieldUsagesFromBody(delegatedBody, "fields", decoderMessages)) {
        addFieldUsage(usages, messageCase.messageName, usage.fieldNumber, usage.category);
      }
    }
  }

  return usages;
}

function protoFieldCategory(field, messageNames, enumNames) {
  const scalarCategory = PROTO_SCALARS[field.type];
  if (scalarCategory) {
    return scalarCategory;
  }
  if (enumNames.has(field.type)) {
    return "varint32";
  }
  if (messageNames.has(field.type)) {
    return "message";
  }
  return "unknown";
}

function fieldUsageCompatible(actualCategory, field, expectedCategory) {
  if (actualCategory === expectedCategory) {
    return true;
  }
  if (actualCategory === "repeated_string") {
    return field.repeated && expectedCategory === "string";
  }
  if (actualCategory === "repeated_varint32") {
    return field.repeated && expectedCategory === "varint32";
  }
  if (actualCategory === "length_delimited") {
    return expectedCategory === "message" || expectedCategory === "string" || expectedCategory === "bytes";
  }
  return false;
}

function checkMockClientMessageFields(protoMessages, enumNames) {
  const errors = [];
  const usages = parseMockClientFieldUsages(readRepoFile("tools/mock-client/src/messages.js"));
  const messageNames = new Set(protoMessages.keys());

  for (const [messageName, fieldUsages] of usages.entries()) {
    const protoFields = protoMessages.get(messageName);
    if (!protoFields) {
      errors.push(`mock-client message schema ${messageName} is missing from packages/proto`);
      continue;
    }

    for (const [fieldNumber, categories] of fieldUsages.entries()) {
      const protoField = protoFields.get(fieldNumber);
      if (!protoField) {
        errors.push(`mock-client ${messageName} uses field ${fieldNumber}, but packages/proto does not define it`);
        continue;
      }
      const expectedCategory = protoFieldCategory(protoField, messageNames, enumNames);
      if (expectedCategory === "unknown") {
        errors.push(`packages/proto ${messageName}.${protoField.name} has unsupported field type ${protoField.type}`);
        continue;
      }
      for (const actualCategory of categories) {
        if (!fieldUsageCompatible(actualCategory, protoField, expectedCategory)) {
          errors.push(
            `mock-client ${messageName}.${protoField.name} field ${fieldNumber} uses ${actualCategory}, expected ${protoField.repeated ? "repeated " : ""}${expectedCategory}`
          );
        }
      }
    }
  }

  return errors;
}

function readLocalHelpClientRoot() {
  const localHelpPath = path.join(rootDir, "local_help.txt");
  if (!existsSync(localHelpPath)) {
    return null;
  }

  const lines = readFile(localHelpPath)
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line && !line.startsWith("#"));
  for (const line of lines) {
    const match = line.match(/^MYSERVER_CLIENT_ROOT\s*=\s*(.+)$/);
    if (match) {
      return match[1].trim();
    }
  }
  return lines[0] ?? null;
}

function validateMybevyBuildScript(buildSource, buildPath) {
  const errors = [];
  if (!/compile_protos\s*\(/.test(buildSource)) {
    errors.push(`${buildPath} does not call prost_build::Config::compile_protos`);
  }
  if (!/game\.proto/.test(buildSource)) {
    errors.push(`${buildPath} does not reference game.proto`);
  }
  if (!/join\("MyServer"\)[\s\S]*join\("packages"\)[\s\S]*join\("proto"\)/m.test(buildSource)) {
    errors.push(`${buildPath} does not resolve proto input from MyServer/packages/proto`);
  }
  return errors;
}

function checkMybevyClientProtocol(expectedMessageTypes) {
  const rawClientRoot = process.env.MYSERVER_CLIENT_ROOT?.trim() || readLocalHelpClientRoot();
  if (!rawClientRoot) {
    return {
      checkedFiles: [],
      errors: [],
      skipped: "MYSERVER_CLIENT_ROOT is not set and local_help.txt has no client root"
    };
  }

  const clientRoot = path.resolve(rawClientRoot);
  if (!existsSync(clientRoot)) {
    return {
      checkedFiles: [],
      errors: [`mybevy root does not exist: ${clientRoot}`],
      skipped: null
    };
  }

  const protocolPath = firstExistingPath(clientRoot, [
    "project/src/game/myserver/protocol.rs",
    "project/src/myserver/protocol.rs",
    "src/game/myserver/protocol.rs",
    "src/myserver/protocol.rs"
  ]);
  const buildPath = firstExistingPath(clientRoot, ["project/build.rs", "build.rs"]);
  const errors = [];
  const checkedFiles = [];

  if (!protocolPath) {
    errors.push(`mybevy protocol.rs not found under ${clientRoot}`);
  } else {
    checkedFiles.push(protocolPath);
    const mybevyProtocolSource = readFile(protocolPath);
    const mybevyMessageTypes = parseRustMessageTypes(mybevyProtocolSource);
    // 1407/1408 are permanently reserved retired ItemAdd numbers. External clients can retain
    // their historical enum names during a rolling upgrade, but this repository no longer
    // requires or validates their payload implementation.
    const retiredItemAddNames = new Set([
      "DEPRECATED_ITEM_ADD_REQ",
      "DEPRECATED_ITEM_ADD_RES",
      "ITEM_ADD_REQ",
      "ITEM_ADD_RES"
    ]);
    const activeExpectedMessageTypes = Object.fromEntries(
      Object.entries(expectedMessageTypes).filter(([name]) => !retiredItemAddNames.has(name))
    );
    const activeMybevyMessageTypes = Object.fromEntries(
      Object.entries(mybevyMessageTypes).filter(([name]) => !retiredItemAddNames.has(name))
    );
    errors.push(...compareSubset("mybevy MessageType", activeExpectedMessageTypes, activeMybevyMessageTypes));
    errors.push(
      ...compareObject(
        "mybevy MessageType::from_u16",
        activeMybevyMessageTypes,
        Object.fromEntries(
          Object.entries(parseRustMessageTypeFromU16(mybevyProtocolSource)).filter(
            ([name]) => !retiredItemAddNames.has(name)
          )
        )
      )
    );
  }

  if (!buildPath) {
    errors.push(`mybevy build.rs not found under ${clientRoot}`);
  } else {
    checkedFiles.push(buildPath);
    errors.push(...validateMybevyBuildScript(readFile(buildPath), buildPath));
  }

  return {
    checkedFiles: checkedFiles.map((filePath) => path.relative(clientRoot, filePath)),
    errors,
    skipped: null
  };
}

function checkChatSharedProto() {
  const errors = [];
  const sharedChatProto = path.join(rootDir, "packages/proto/chat.proto");
  const localChatProto = path.join(rootDir, "apps/chat-server/src/proto/chat.proto");
  const chatBuildPath = path.join(rootDir, "apps/chat-server/build.rs");

  if (!existsSync(sharedChatProto)) {
    errors.push("packages/proto/chat.proto missing");
  }
  if (existsSync(localChatProto)) {
    errors.push("apps/chat-server/src/proto/chat.proto still exists; chat proto must live in packages/proto");
  }
  if (!existsSync(chatBuildPath)) {
    errors.push("apps/chat-server/build.rs missing");
  } else {
    const buildSource = readFile(chatBuildPath);
    if (!/packages"\)\.join\("proto"\)/.test(buildSource)) {
      errors.push("apps/chat-server/build.rs does not resolve packages/proto");
    }
    if (!/chat\.proto/.test(buildSource)) {
      errors.push("apps/chat-server/build.rs does not compile chat.proto");
    }
    if (/read_dir\("src\/proto"\)/.test(buildSource)) {
      errors.push("apps/chat-server/build.rs still scans src/proto for local proto files");
    }
  }
  return errors;
}

function main() {
  const errors = [];
  const protoSource = readRepoFile("packages/proto/game.proto");
  const chatProtoSource = readRepoFile("packages/proto/chat.proto");
  const adminProtoSource = readRepoFile("packages/proto/admin.proto");
  const gameServerRustSource = readRepoFile("apps/game-server/src/protocol/message_type.rs");
  const gameProxyRustSource = readRepoFile("apps/game-proxy/src/protocol.rs");

  const expectedMessageTypes = parseRustMessageTypes(gameServerRustSource);
  errors.push(...compareObject("MESSAGE_TYPE", expectedMessageTypes, MESSAGE_TYPE));
  errors.push(
    ...compareObject(
      "game-server MessageType::from_u16",
      expectedMessageTypes,
      parseRustMessageTypeFromU16(gameServerRustSource)
    )
  );

  const gameProxyMessageTypes = parseRustMessageTypes(gameProxyRustSource);
  errors.push(...compareSubset("game-proxy MessageType", expectedMessageTypes, gameProxyMessageTypes));
  errors.push(...compareSubset("game-proxy MessageType", MESSAGE_TYPE, gameProxyMessageTypes));
  errors.push(
    ...compareObject(
      "game-proxy MessageType::from_u16",
      gameProxyMessageTypes,
      parseRustMessageTypeFromU16(gameProxyRustSource)
    )
  );

  for (const enumSpec of PROTO_ENUMS) {
    const expectedEnum = parseProtoEnum(protoSource, enumSpec.enumName, enumSpec.prefix);
    errors.push(...compareObject(enumSpec.enumName, expectedEnum, enumSpec.actual));
  }

  const protoMessages = mergeProtoMessages(
    parseProtoMessages(protoSource),
    parseProtoMessages(chatProtoSource),
    parseProtoMessages(adminProtoSource)
  );
  const protoEnumNames = new Set([
    ...parseProtoEnumNames(protoSource),
    ...parseProtoEnumNames(chatProtoSource),
    ...parseProtoEnumNames(adminProtoSource)
  ]);
  errors.push(...checkMockClientMessageFields(protoMessages, protoEnumNames));

  const mybevyResult = checkMybevyClientProtocol(expectedMessageTypes);
  errors.push(...mybevyResult.errors);
  errors.push(...checkChatSharedProto());

  if (errors.length > 0) {
    console.error("protocol drift detected:");
    for (const error of errors) {
      console.error(`- ${error}`);
    }
    process.exit(1);
  }

  if (mybevyResult.skipped) {
    console.log(`mybevy protocol check skipped: ${mybevyResult.skipped}.`);
  } else {
    console.log(`mybevy protocol checked: ${mybevyResult.checkedFiles.join(", ")}.`);
  }
  console.log(
    "protocol constants and mock-client field schemas are in sync across mock-client, game-server, game-proxy, optional mybevy, and proto definitions."
  );
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main();
}
