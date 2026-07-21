import { existsSync, readdirSync, readFileSync } from "node:fs";
import { fileURLToPath, pathToFileURL } from "node:url";
import path from "node:path";

import { validateInventory } from "./proto-compatibility-baseline.js";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const rootDir = path.resolve(__dirname, "..");
const inventoryPath = "packages/proto/compatibility/inventory.json";
const routingConfigPath = "packages/proto/compatibility/routing-consistency.json";

function normalizePath(filePath) {
  return filePath.split(path.sep).join("/");
}

function readRepoFile(relativePath, root = rootDir) {
  return readFileSync(path.join(root, relativePath), "utf8");
}

function readJson(relativePath, root = rootDir) {
  return JSON.parse(readRepoFile(relativePath, root));
}

function listFiles(relativeDirectory, extension, root = rootDir) {
  const absoluteDirectory = path.join(root, relativeDirectory);
  if (!existsSync(absoluteDirectory)) {
    return [];
  }

  const files = [];
  for (const entry of readdirSync(absoluteDirectory, { withFileTypes: true })) {
    if (["node_modules", "target", ".git"].includes(entry.name)) {
      continue;
    }
    const relativePath = path.join(relativeDirectory, entry.name);
    if (entry.isDirectory()) {
      files.push(...listFiles(relativePath, extension, root));
    } else if (entry.isFile() && relativePath.endsWith(extension)) {
      files.push(normalizePath(relativePath));
    }
  }
  return files.sort();
}

function findBalancedBlock(source, openingIndex, opening = "{", closing = "}") {
  if (source[openingIndex] !== opening) {
    throw new Error(`expected ${opening} at offset ${openingIndex}`);
  }

  let depth = 0;
  let quote = null;
  let escaped = false;
  for (let index = openingIndex; index < source.length; index += 1) {
    const character = source[index];
    if (quote) {
      if (escaped) {
        escaped = false;
      } else if (character === "\\") {
        escaped = true;
      } else if (character === quote) {
        quote = null;
      }
      continue;
    }
    if (character === '"' || character === "'") {
      quote = character;
      continue;
    }
    if (character === opening) {
      depth += 1;
    } else if (character === closing) {
      depth -= 1;
      if (depth === 0) {
        return source.slice(openingIndex + 1, index);
      }
    }
  }
  throw new Error(`unclosed ${opening}${closing} block at offset ${openingIndex}`);
}

function findFunctionBody(source, functionName) {
  const pattern = new RegExp(
    `(?:(?:pub\\s+)?(?:async\\s+)?fn|(?:export\\s+)?(?:async\\s+)?function)\\s+${functionName}\\b[\\s\\S]*?\\{`,
    "m"
  );
  const match = pattern.exec(source);
  if (!match) {
    throw new Error(`function ${functionName} not found`);
  }
  const openingIndex = match.index + match[0].lastIndexOf("{");
  return findBalancedBlock(source, openingIndex);
}

function findPacketMessageTypeMatchBody(source) {
  const match = /match\s+packet\.message_type\(\)\s*\{/.exec(source);
  if (!match) {
    throw new Error("packet.message_type() match not found");
  }
  const openingIndex = match.index + match[0].lastIndexOf("{");
  return findBalancedBlock(source, openingIndex);
}

function toConstantName(name) {
  return name
    .replace(/([a-z0-9])([A-Z])/g, "$1_$2")
    .replace(/([A-Z]+)([A-Z][a-z])/g, "$1_$2")
    .toUpperCase();
}

function toSnakeCase(name) {
  return toConstantName(name).toLowerCase();
}

function parseRustMessageTypeEnum(source) {
  const enumMatch = /pub\s+enum\s+MessageType\s*\{/.exec(source);
  if (!enumMatch) {
    throw new Error("canonical MessageType enum not found");
  }
  const openingIndex = enumMatch.index + enumMatch[0].lastIndexOf("{");
  const body = findBalancedBlock(source, openingIndex);
  const entries = [];
  const pattern = /\b([A-Za-z][A-Za-z0-9_]*)\s*=\s*(\d+)\s*,/g;
  for (const match of body.matchAll(pattern)) {
    entries.push({ id: Number(match[2]), name: match[1] });
  }
  return entries;
}

function parseRustFromU16(source) {
  const methodMatch = /pub\s+fn\s+from_u16\s*\([^)]*\)\s*->\s*Option<Self>\s*\{/.exec(source);
  if (!methodMatch) {
    throw new Error("MessageType::from_u16 not found");
  }
  const openingIndex = methodMatch.index + methodMatch[0].lastIndexOf("{");
  const body = findBalancedBlock(source, openingIndex);
  const entries = [];
  const pattern = /(\d+)\s*=>\s*Some\(Self::([A-Za-z][A-Za-z0-9_]*)\)/g;
  for (const match of body.matchAll(pattern)) {
    entries.push({ id: Number(match[1]), name: match[2] });
  }
  return entries;
}

function parseDispatchCases(source, functionName) {
  const functionBody = findFunctionBody(source, functionName);
  const matchBody = findPacketMessageTypeMatchBody(functionBody);
  const entries = [];
  const somePattern = /Some\s*\(\s*([\s\S]*?)\s*\)\s*=>/g;
  for (const someMatch of matchBody.matchAll(somePattern)) {
    for (const typeMatch of someMatch[1].matchAll(/MessageType::([A-Za-z][A-Za-z0-9_]*)/g)) {
      entries.push(typeMatch[1]);
    }
  }
  return entries;
}

function parseMockClientConstants(source) {
  const declaration = /export\s+const\s+MESSAGE_TYPE\s*=\s*\{/.exec(source);
  if (!declaration) {
    throw new Error("mock-client MESSAGE_TYPE constants not found");
  }
  const openingIndex = declaration.index + declaration[0].lastIndexOf("{");
  const body = findBalancedBlock(source, openingIndex);
  const constants = new Map();
  const pattern = /\b([A-Z][A-Z0-9_]*)\s*:\s*(\d+)\s*,?/g;
  for (const match of body.matchAll(pattern)) {
    constants.set(match[1], Number(match[2]));
  }
  return constants;
}

function parseMockClientDecodeCases(source) {
  const functionBody = findFunctionBody(source, "decodeByMessageType");
  return new Set(
    [...functionBody.matchAll(/case\s+MESSAGE_TYPE\.([A-Z][A-Z0-9_]*)\s*:/g)].map((match) => match[1])
  );
}

function parseMockClientEncoders(source) {
  return new Set(
    [...source.matchAll(/\bexport\s+function\s+encode([A-Za-z][A-Za-z0-9_]*)\s*\(/g)].map((match) => match[1])
  );
}

function extractCallArguments(source, functionPattern) {
  const calls = [];
  for (const match of source.matchAll(functionPattern)) {
    const openingIndex = match.index + match[0].lastIndexOf("(");
    try {
      calls.push(findBalancedBlock(source, openingIndex, "(", ")"));
    } catch {
      // Syntax validation belongs to the JavaScript toolchain. The protocol report must still
      // identify every complete call it can parse from a partially edited source file.
    }
  }
  return calls;
}

function parseMockClientSendPairs(files, readSource = readRepoFile) {
  const pairs = [];
  for (const file of files) {
    const source = readSource(file);
    for (const argumentsSource of extractCallArguments(source, /\b(?:[A-Za-z_$][\w$]*\.)?send\s*\(/g)) {
      const constant = /\bMESSAGE_TYPE\.([A-Z][A-Z0-9_]*)/.exec(argumentsSource)?.[1];
      if (!constant) {
        continue;
      }
      const encoder = /\bencode([A-Za-z][A-Za-z0-9_]*)\s*\(/.exec(argumentsSource)?.[1] ?? null;
      pairs.push({ constant, encoder, file });
    }
  }
  return pairs;
}

function parseMockClientObservedTypes(files, readSource = readRepoFile) {
  const observed = new Map();
  for (const file of files) {
    const source = readSource(file);
    for (const match of source.matchAll(/\bMESSAGE_TYPE\.([A-Z][A-Z0-9_]*)/g)) {
      const name = match[1];
      if (!observed.has(name)) {
        observed.set(name, new Set());
      }
      observed.get(name).add(file);
    }
  }
  return observed;
}

function parseProtoServices(source) {
  const services = [];
  const servicePattern = /\bservice\s+([A-Za-z_][A-Za-z0-9_]*)\s*\{/g;
  for (const serviceMatch of source.matchAll(servicePattern)) {
    const openingIndex = serviceMatch.index + serviceMatch[0].lastIndexOf("{");
    const body = findBalancedBlock(source, openingIndex);
    const rpcs = [];
    const rpcPattern = /\brpc\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(\s*(stream\s+)?([A-Za-z_.][A-Za-z0-9_.]*)\s*\)\s+returns\s*\(\s*(stream\s+)?([A-Za-z_.][A-Za-z0-9_.]*)\s*\)\s*;/g;
    for (const rpcMatch of body.matchAll(rpcPattern)) {
      rpcs.push({
        name: rpcMatch[1],
        request: rpcMatch[3],
        response: rpcMatch[5],
        requestStream: Boolean(rpcMatch[2]),
        responseStream: Boolean(rpcMatch[4])
      });
    }
    services.push({ name: serviceMatch[1], rpcs });
  }
  return services;
}

function parseTraitImplementationMethods(source, trait) {
  const implementation = new RegExp(`impl\\s+${trait}\\s+for\\s+[A-Za-z_][A-Za-z0-9_]*\\s*\\{`).exec(source);
  if (!implementation) {
    return new Set();
  }
  const openingIndex = implementation.index + implementation[0].lastIndexOf("{");
  const body = findBalancedBlock(source, openingIndex);
  return new Set([...body.matchAll(/\basync\s+fn\s+([a-z][a-z0-9_]*)\s*\(/g)].map((match) => match[1]));
}

function parseClientRpcMethods(source, receiver) {
  const receiverPattern = receiver.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new Set(
    [...source.matchAll(new RegExp(`\\.${receiverPattern}\\s*\\.\\s*([a-z][a-z0-9_]*)\\s*\\(`, "g"))].map((match) => match[1])
  );
}

function parseProtoErrorCodeFields(source) {
  const fields = [];
  const messagePattern = /\bmessage\s+([A-Za-z_][A-Za-z0-9_]*)\s*\{/g;
  for (const messageMatch of source.matchAll(messagePattern)) {
    const openingIndex = messageMatch.index + messageMatch[0].lastIndexOf("{");
    const body = findBalancedBlock(source, openingIndex);
    for (const fieldMatch of body.matchAll(/\bstring\s+error_code\s*=\s*(\d+)\s*;/g)) {
      fields.push({ message: messageMatch[1], number: Number(fieldMatch[1]) });
    }
  }
  return fields;
}

function lineNumber(source, index) {
  return source.slice(0, index).split("\n").length;
}

function parseLiteralErrorCodeUses(source, file) {
  const codes = new Map();
  const patterns = [
    /\b(?:queue_error|write_error|audit_then_write_error|auth_response)\s*\([\s\S]{0,280}?\"([A-Z][A-Z0-9_]+)\"/g,
    /\berror_code\s*:\s*\"([A-Z][A-Z0-9_]+)\"(?:\.to_string\(\))?/g,
    /\bErr\s*\(\s*\"([A-Z][A-Z0-9_]+)\"\s*\)/g
  ];
  for (const pattern of patterns) {
    for (const match of source.matchAll(pattern)) {
      const code = match[1];
      if (!codes.has(code)) {
        codes.set(code, []);
      }
      codes.get(code).push({ file, line: lineNumber(source, match.index) });
    }
  }
  return codes;
}

function parseMessageTypeProducers(source) {
  const producers = [];
  const producerCalls = /\b(?:[A-Za-z_][A-Za-z0-9_]*\.)?(?:queue_message|write_message|audit_then_write_message|broadcast_to_room|broadcast_to_characters|send_to_character|send_message|push_to_online_character|record)\s*\(/g;
  for (const argumentsSource of extractCallArguments(source, producerCalls)) {
    for (const match of argumentsSource.matchAll(/\bMessageType::([A-Za-z][A-Za-z0-9_]*)\b/g)) {
      producers.push(match[1]);
    }
  }
  for (const match of source.matchAll(/\bmessage_type\s*:\s*MessageType::([A-Za-z][A-Za-z0-9_]*)\b/g)) {
    producers.push(match[1]);
  }
  return producers;
}

function mergeLocations(target, source) {
  for (const [value, locations] of source.entries()) {
    if (!target.has(value)) {
      target.set(value, []);
    }
    target.get(value).push(...locations);
  }
}

function createDiagnostic(rule, message, detail = {}) {
  return { rule, message, ...detail };
}

function duplicateValues(entries, valueSelector) {
  const grouped = new Map();
  for (const entry of entries) {
    const value = valueSelector(entry);
    if (!grouped.has(value)) {
      grouped.set(value, []);
    }
    grouped.get(value).push(entry);
  }
  return [...grouped.entries()].filter(([, values]) => values.length > 1);
}

function canonicalMap(entries) {
  return new Map(entries.map((entry) => [entry.name, entry.id]));
}

function isRequestMessage(name) {
  return name.endsWith("Req");
}

function isOutboundMessage(name) {
  return name.endsWith("Res") || name.endsWith("Push");
}

function configuredControlMessages(config) {
  return new Map(
    (config.packetRoutes?.preDispatchControls ?? []).map((entry) => [entry.messageType, entry])
  );
}

function configuredDeferredOutboundMessages(config) {
  return new Map(
    (config.packetRoutes?.deferredOutboundMessages ?? []).map((entry) => [entry.messageType, entry])
  );
}

function validateRoutingConfig(config) {
  const diagnostics = [];
  if (config.schemaVersion !== 1) {
    diagnostics.push(createDiagnostic("ROUTING_CONFIG_SCHEMA", "routing consistency config schemaVersion must be 1"));
  }
  if (!Array.isArray(config.packetRoutes?.dispatches) || config.packetRoutes.dispatches.length === 0) {
    diagnostics.push(createDiagnostic("ROUTING_CONFIG_DISPATCHES", "routing consistency config needs at least one packet dispatch"));
  }
  return diagnostics;
}

export function analyzePacketRouting({ canonicalSource, dispatchSources, config, producerSources = [] }) {
  const diagnostics = [];
  const canonicalEntries = parseRustMessageTypeEnum(canonicalSource);
  const canonicalByName = canonicalMap(canonicalEntries);
  const canonicalIds = new Set(canonicalEntries.map((entry) => entry.id));

  for (const [id, entries] of duplicateValues(canonicalEntries, (entry) => entry.id)) {
    diagnostics.push(createDiagnostic(
      "MESSAGE_TYPE_ID_DUPLICATE",
      `canonical MessageType id ${id} is assigned to ${entries.map((entry) => entry.name).join(", ")}`
    ));
  }
  for (const [name, entries] of duplicateValues(canonicalEntries, (entry) => entry.name)) {
    diagnostics.push(createDiagnostic(
      "MESSAGE_TYPE_NAME_DUPLICATE",
      `canonical MessageType variant ${name} is declared ${entries.length} times`
    ));
  }

  const fromU16Entries = parseRustFromU16(canonicalSource);
  for (const [id, entries] of duplicateValues(fromU16Entries, (entry) => entry.id)) {
    diagnostics.push(createDiagnostic(
      "MESSAGE_TYPE_FROM_U16_DUPLICATE",
      `MessageType::from_u16 maps id ${id} more than once (${entries.map((entry) => entry.name).join(", ")})`
    ));
  }
  const fromU16ById = new Map(fromU16Entries.map((entry) => [entry.id, entry.name]));
  for (const entry of canonicalEntries) {
    if (fromU16ById.get(entry.id) !== entry.name) {
      diagnostics.push(createDiagnostic(
        "MESSAGE_TYPE_FROM_U16_MISSING_OR_DRIFTED",
        `MessageType::from_u16(${entry.id}) must return ${entry.name}`,
        { messageType: entry.name, id: entry.id }
      ));
    }
  }
  for (const entry of fromU16Entries) {
    if (!canonicalIds.has(entry.id) || canonicalByName.get(entry.name) !== entry.id) {
      diagnostics.push(createDiagnostic(
        "MESSAGE_TYPE_FROM_U16_UNKNOWN",
        `MessageType::from_u16(${entry.id}) references non-canonical ${entry.name}`,
        { messageType: entry.name, id: entry.id }
      ));
    }
  }

  const dispatches = new Map();
  for (const dispatch of config.packetRoutes?.dispatches ?? []) {
    const source = dispatchSources.get(dispatch.name);
    if (typeof source !== "string") {
      diagnostics.push(createDiagnostic("DISPATCH_SOURCE_MISSING", `configured ${dispatch.name} dispatch source is unavailable`));
      continue;
    }
    let entries;
    try {
      entries = parseDispatchCases(source, dispatch.function);
    } catch (error) {
      diagnostics.push(createDiagnostic(
        "DISPATCH_PARSE_FAILED",
        `cannot audit ${dispatch.name} dispatch (${dispatch.function}): ${error.message}`
      ));
      continue;
    }
    dispatches.set(dispatch.name, entries);
    for (const [name, occurrences] of duplicateValues(entries.map((name) => ({ name })), (entry) => entry.name)) {
      diagnostics.push(createDiagnostic(
        "DISPATCH_MESSAGE_DUPLICATE",
        `${dispatch.name} dispatch has ${occurrences.length} branches for ${name}`,
        { messageType: name, route: dispatch.name }
      ));
    }
    for (const name of entries) {
      if (!canonicalByName.has(name)) {
        diagnostics.push(createDiagnostic(
          "DISPATCH_MESSAGE_UNKNOWN",
          `${dispatch.name} dispatch references ${name}, which is absent from canonical MessageType`,
          { messageType: name, route: dispatch.name }
        ));
      }
    }
  }

  const controls = configuredControlMessages(config);
  const deferredOutbound = configuredDeferredOutboundMessages(config);
  const routesByMessage = new Map();
  for (const [route, entries] of dispatches.entries()) {
    for (const name of entries) {
      if (!routesByMessage.has(name)) {
        routesByMessage.set(name, new Set());
      }
      routesByMessage.get(name).add(route);
    }
  }
  for (const entry of canonicalEntries) {
    if (isRequestMessage(entry.name) && !routesByMessage.has(entry.name) && !controls.has(entry.name)) {
      diagnostics.push(createDiagnostic(
        "PACKET_REQUEST_WITHOUT_CONSUMER",
        `${entry.name} (${entry.id}) has no player/internal/admin dispatch or pre-dispatch control classification`,
        { messageType: entry.name, id: entry.id }
      ));
    }
  }
  for (const [name, control] of controls.entries()) {
    if (!canonicalByName.has(name)) {
      diagnostics.push(createDiagnostic("PRE_DISPATCH_CONTROL_UNKNOWN", `pre-dispatch control ${name} is not canonical`));
    } else if (!isRequestMessage(name)) {
      diagnostics.push(createDiagnostic("PRE_DISPATCH_CONTROL_NOT_REQUEST", `pre-dispatch control ${name} must be a request message`));
    } else if (routesByMessage.has(name)) {
      diagnostics.push(createDiagnostic(
        "PRE_DISPATCH_CONTROL_REDUNDANT",
        `pre-dispatch control ${name} is also present in ${[...routesByMessage.get(name)].join(", ")} dispatch`,
        { messageType: name }
      ));
    } else if (!control.reason) {
      diagnostics.push(createDiagnostic("PRE_DISPATCH_CONTROL_UNEXPLAINED", `pre-dispatch control ${name} needs a reason`));
    }
  }

  const producerCounts = new Map(canonicalEntries.map((entry) => [entry.name, 0]));
  for (const source of producerSources) {
    for (const name of parseMessageTypeProducers(source)) {
      if (producerCounts.has(name)) {
        producerCounts.set(name, producerCounts.get(name) + 1);
      }
    }
  }
  const deferredOutboundReport = [];
  for (const entry of canonicalEntries) {
    if (isOutboundMessage(entry.name) && producerCounts.get(entry.name) === 0) {
      const deferred = deferredOutbound.get(entry.name);
      if (deferred?.reason && deferred?.owner) {
        deferredOutboundReport.push({ id: entry.id, messageType: entry.name, ...deferred });
      } else {
        diagnostics.push(createDiagnostic(
          "PACKET_OUTBOUND_ORPHAN",
          `${entry.name} (${entry.id}) has no game-server producer outside the canonical mapping`,
          { messageType: entry.name, id: entry.id }
        ));
      }
    }
  }
  for (const [name, deferred] of deferredOutbound.entries()) {
    if (!canonicalByName.has(name)) {
      diagnostics.push(createDiagnostic("DEFERRED_OUTBOUND_UNKNOWN", `deferred outbound ${name} is not canonical`));
    } else if (!isOutboundMessage(name)) {
      diagnostics.push(createDiagnostic("DEFERRED_OUTBOUND_NOT_OUTBOUND", `deferred outbound ${name} must be a response or push`));
    } else if (producerCounts.get(name) > 0) {
      diagnostics.push(createDiagnostic(
        "DEFERRED_OUTBOUND_REDUNDANT",
        `deferred outbound ${name} now has ${producerCounts.get(name)} game-server producer references; remove its metadata`,
        { messageType: name }
      ));
    } else if (!deferred.reason || !deferred.owner) {
      diagnostics.push(createDiagnostic("DEFERRED_OUTBOUND_UNEXPLAINED", `deferred outbound ${name} needs owner and reason`));
    }
  }

  return {
    canonicalEntries,
    diagnostics,
    dispatches,
    deferredOutboundReport,
    producerCounts,
    routesByMessage
  };
}

export function analyzeMockClient({
  constantsSource,
  messagesSource,
  sourceFiles,
  canonicalEntries,
  config,
  readSource = readRepoFile,
  routesByMessage = new Map()
}) {
  const diagnostics = [];
  const constants = parseMockClientConstants(constantsSource);
  const decoders = parseMockClientDecodeCases(messagesSource);
  const encoders = parseMockClientEncoders(messagesSource);
  const sends = parseMockClientSendPairs(sourceFiles, readSource);
  const observed = parseMockClientObservedTypes(sourceFiles, readSource);
  const canonicalByName = canonicalMap(canonicalEntries);
  const canonicalByConstant = new Map(canonicalEntries.map((entry) => [toConstantName(entry.name), entry]));
  const nonGameFloor = config.mockClient?.nonGameMessageIdFloor ?? Number.MAX_SAFE_INTEGER;

  for (const [constant, value] of constants.entries()) {
    if (value < nonGameFloor) {
      const canonical = canonicalByConstant.get(constant);
      if (!canonical) {
        diagnostics.push(createDiagnostic(
          "MOCK_CLIENT_CONSTANT_UNKNOWN",
          `mock-client ${constant}=${value} is not in canonical game-server MessageType`,
          { constant, id: value }
        ));
      } else if (canonical.id !== value) {
        diagnostics.push(createDiagnostic(
          "MOCK_CLIENT_CONSTANT_DRIFT",
          `mock-client ${constant}=${value}, expected canonical ${canonical.name}=${canonical.id}`,
          { constant, id: value }
        ));
      }
    }
  }
  for (const entry of canonicalEntries) {
    const constant = toConstantName(entry.name);
    if (!constants.has(constant)) {
      diagnostics.push(createDiagnostic(
        "MOCK_CLIENT_CONSTANT_MISSING",
        `mock-client MESSAGE_TYPE is missing ${constant} for canonical ${entry.name}=${entry.id}`,
        { constant, messageType: entry.name, id: entry.id }
      ));
    }
  }

  for (const send of sends) {
    const canonical = canonicalByConstant.get(send.constant);
    if (!canonical) {
      if ((constants.get(send.constant) ?? 0) < nonGameFloor) {
        diagnostics.push(createDiagnostic("MOCK_CLIENT_SEND_UNKNOWN", `${send.file} sends unknown ${send.constant}`, { constant: send.constant }));
      }
      continue;
    }
    if (!isRequestMessage(canonical.name)) {
      diagnostics.push(createDiagnostic(
        "MOCK_CLIENT_SEND_NOT_REQUEST",
        `${send.file} sends ${send.constant}, but canonical ${canonical.name} is not a request`,
        { constant: send.constant, messageType: canonical.name }
      ));
    }
    if (!routesByMessage.get(canonical.name)?.has("player")) {
      diagnostics.push(createDiagnostic(
        "MOCK_CLIENT_SEND_WITHOUT_PLAYER_ROUTE",
        `${send.file} sends ${send.constant}, but canonical ${canonical.name} has no player dispatch route`,
        { constant: send.constant, messageType: canonical.name }
      ));
    }
    if (!send.encoder) {
      diagnostics.push(createDiagnostic(
        "MOCK_CLIENT_SEND_WITHOUT_ENCODER",
        `${send.file} sends ${send.constant} without a named encode* function`,
        { constant: send.constant, messageType: canonical.name }
      ));
    } else if (!encoders.has(send.encoder)) {
      diagnostics.push(createDiagnostic(
        "MOCK_CLIENT_ENCODER_MISSING",
        `${send.file} sends ${send.constant} through missing encode${send.encoder}`,
        { constant: send.constant, encoder: send.encoder }
      ));
    } else if (send.encoder !== canonical.name) {
      diagnostics.push(createDiagnostic(
        "MOCK_CLIENT_ENCODER_ROUTE_DRIFT",
        `${send.file} sends ${send.constant} with encode${send.encoder}, expected encode${canonical.name}`,
        { constant: send.constant, encoder: send.encoder, messageType: canonical.name }
      ));
    }
  }

  for (const constant of decoders) {
    const canonical = canonicalByConstant.get(constant);
    if (!canonical && (constants.get(constant) ?? 0) < nonGameFloor) {
      diagnostics.push(createDiagnostic("MOCK_CLIENT_DECODER_UNKNOWN", `decodeByMessageType handles unknown ${constant}`, { constant }));
    }
    if (canonical && !isOutboundMessage(canonical.name)) {
      diagnostics.push(createDiagnostic(
        "MOCK_CLIENT_DECODER_NOT_OUTBOUND",
        `decodeByMessageType handles ${constant}, but canonical ${canonical.name} is not a response or push`,
        { constant, messageType: canonical.name }
      ));
    }
  }

  for (const [constant, files] of observed.entries()) {
    const canonical = canonicalByConstant.get(constant);
    if (canonical && isOutboundMessage(canonical.name) && !decoders.has(constant)) {
      diagnostics.push(createDiagnostic(
        "MOCK_CLIENT_OBSERVED_WITHOUT_DECODER",
        `mock-client observes ${constant} in ${[...files].join(", ")} but decodeByMessageType has no decoder`,
        { constant, messageType: canonical.name }
      ));
    }
  }

  return { constants, decoders, diagnostics, encoders, observed, sends };
}

export function analyzeRpcs({ matchProtoSource, config, readSource = readRepoFile }) {
  const diagnostics = [];
  const services = parseProtoServices(matchProtoSource);
  const report = [];
  for (const service of services) {
    const consumer = config.rpcConsumers?.[service.name];
    if (!consumer?.implementation) {
      for (const rpc of service.rpcs) {
        diagnostics.push(createDiagnostic("RPC_CONSUMER_UNDECLARED", `${service.name}.${rpc.name} has no declared implementation consumer`));
      }
      continue;
    }
    const implementationSource = readSource(consumer.implementation.path);
    const implementationMethods = parseTraitImplementationMethods(implementationSource, consumer.implementation.trait);
    const clientMethods = consumer.client
      ? parseClientRpcMethods(readSource(consumer.client.path), consumer.client.receiver)
      : new Set();
    for (const rpc of service.rpcs) {
      const method = toSnakeCase(rpc.name);
      const implemented = implementationMethods.has(method);
      const client = consumer.client ? clientMethods.has(method) : false;
      report.push({ client, implemented, method, rpc: rpc.name, service: service.name });
      if (!implemented && !client) {
        diagnostics.push(createDiagnostic(
          "RPC_WITHOUT_CONSUMER",
          `${service.name}.${rpc.name} has neither ${consumer.implementation.trait} implementation nor configured generated-client caller`,
          { rpc: rpc.name, service: service.name }
        ));
      } else if (!implemented) {
        diagnostics.push(createDiagnostic(
          "RPC_IMPLEMENTATION_MISSING",
          `${service.name}.${rpc.name} has a client call but no ${consumer.implementation.trait} implementation`,
          { rpc: rpc.name, service: service.name }
        ));
      } else if (consumer.client && !client) {
        diagnostics.push(createDiagnostic(
          "RPC_CONFIGURED_CLIENT_MISSING",
          `${service.name}.${rpc.name} is implemented but its configured game-server client has no call`,
          { rpc: rpc.name, service: service.name }
        ));
      }
    }
  }
  return { diagnostics, report, services };
}

export function analyzeErrorCodes({ protoSources, implementationSources, config }) {
  const diagnostics = [];
  const fields = [];
  for (const [file, source] of Object.entries(protoSources)) {
    for (const field of parseProtoErrorCodeFields(source)) {
      fields.push({ ...field, file });
    }
  }
  const literalCodes = new Map();
  for (const [file, source] of Object.entries(implementationSources)) {
    mergeLocations(literalCodes, parseLiteralErrorCodeUses(source, file));
  }

  if (config.errorCodes?.definitionMode !== "implementation_literals") {
    diagnostics.push(createDiagnostic(
      "ERROR_CODE_CATALOG_MODE_UNKNOWN",
      "error code definitionMode must be implementation_literals until a shared enum catalog is introduced"
    ));
  }
  if (fields.length === 0) {
    diagnostics.push(createDiagnostic("ERROR_CODE_FIELD_MISSING", "no shared proto error_code fields were found"));
  }

  const configuredCodes = config.errorCodes?.staticCodes;
  const definedCodes = new Set();
  if (!Array.isArray(configuredCodes)) {
    diagnostics.push(createDiagnostic("ERROR_CODE_CATALOG_MISSING", "error code staticCodes catalog must be an array"));
  } else {
    for (const code of configuredCodes) {
      if (typeof code !== "string" || !/^[A-Z][A-Z0-9_]+$/.test(code)) {
        diagnostics.push(createDiagnostic("ERROR_CODE_CATALOG_INVALID", `invalid error code catalog entry ${JSON.stringify(code)}`));
        continue;
      }
      if (definedCodes.has(code)) {
        diagnostics.push(createDiagnostic("ERROR_CODE_CATALOG_DUPLICATE", `duplicate error code catalog entry ${code}`));
        continue;
      }
      definedCodes.add(code);
    }
  }
  const undefinedCodes = [...literalCodes.keys()].filter((code) => !definedCodes.has(code)).sort();
  const unusedCodes = [...definedCodes].filter((code) => !literalCodes.has(code)).sort();
  for (const code of undefinedCodes) {
    diagnostics.push(createDiagnostic(
      "ERROR_CODE_UNDEFINED",
      `implementation uses ${code}, but it is absent from errorCodes.staticCodes`,
      { code, locations: literalCodes.get(code) }
    ));
  }
  for (const code of unusedCodes) {
    diagnostics.push(createDiagnostic(
      "ERROR_CODE_UNUSED",
      `errorCodes.staticCodes declares ${code}, but no packet-boundary implementation use was found`,
      { code }
    ));
  }

  const dynamicSources = config.errorCodes?.dynamicSources ?? [];
  return {
    diagnostics,
    dynamicSources,
    definedCodes,
    fields,
    literalCodes,
    unusedCodes,
    undefinedCodes
  };
}

function collectProducerSources(root = rootDir) {
  return listFiles("apps/game-server/src", ".rs", root)
    .filter((file) => file !== "apps/game-server/src/protocol/message_type.rs")
    .map((file) => readRepoFile(file, root));
}

function collectImplementationSources(root = rootDir) {
  const files = [
    ...listFiles("apps/game-server/src", ".rs", root),
    ...listFiles("apps/game-proxy/src", ".rs", root),
    ...listFiles("apps/match-service/src", ".rs", root)
  ];
  return Object.fromEntries(files.map((file) => [file, readRepoFile(file, root)]));
}

export function checkProtocolRoutingConsistency(root = rootDir) {
  const inventory = readJson(inventoryPath, root);
  const config = readJson(routingConfigPath, root);
  const diagnostics = [...validateRoutingConfig(config)];
  try {
    validateInventory(inventory, root);
  } catch (error) {
    diagnostics.push(createDiagnostic("LOCAL_PROTO_DRIFT", error.message));
  }

  const canonicalPath = config.packetRoutes?.canonical;
  const canonicalSource = readRepoFile(canonicalPath, root);
  const dispatchSources = new Map(
    (config.packetRoutes?.dispatches ?? []).map((dispatch) => [dispatch.name, readRepoFile(dispatch.path, root)])
  );
  const packet = analyzePacketRouting({
    canonicalSource,
    config,
    dispatchSources,
    producerSources: collectProducerSources(root)
  });
  diagnostics.push(...packet.diagnostics);

  const mockDirectory = config.mockClient?.sourceDirectory;
  const mockFiles = listFiles(mockDirectory, ".js", root);
  const mock = analyzeMockClient({
    canonicalEntries: packet.canonicalEntries,
    config,
    constantsSource: readRepoFile(`${mockDirectory}/constants.js`, root),
    messagesSource: readRepoFile(`${mockDirectory}/messages.js`, root),
    routesByMessage: packet.routesByMessage,
    sourceFiles: mockFiles
  });
  diagnostics.push(...mock.diagnostics);

  const rpcs = analyzeRpcs({
    config,
    matchProtoSource: readRepoFile("packages/proto/match.proto", root),
    readSource: (relativePath) => readRepoFile(relativePath, root)
  });
  diagnostics.push(...rpcs.diagnostics);

  const protoSources = Object.fromEntries(
    inventory.protocols.map((protocol) => [protocol.file, readRepoFile(protocol.file, root)])
  );
  const errorCodes = analyzeErrorCodes({
    config,
    implementationSources: collectImplementationSources(root),
    protoSources
  });
  diagnostics.push(...errorCodes.diagnostics);

  return { config, diagnostics, errorCodes, inventory, mock, packet, rpcs };
}

export function formatRoutingConsistencyReport(result) {
  const lines = [
    "protocol routing consistency report:",
    `- canonical MessageType entries: ${result.packet.canonicalEntries.length}`,
    ...[...result.packet.dispatches.entries()].map(([route, entries]) => `- ${route} dispatch consumers: ${entries.length}`),
    `- mock-client send routes: ${result.mock.sends.length}; decoder cases: ${result.mock.decoders.size}`,
    `- match RPCs: ${result.rpcs.report.length}; implemented: ${result.rpcs.report.filter((entry) => entry.implemented).length}; configured client calls: ${result.rpcs.report.filter((entry) => entry.client).length}`,
    `- error_code proto fields: ${result.errorCodes.fields.length}; static catalog: ${result.errorCodes.definedCodes.size}; implementation literal codes: ${result.errorCodes.literalCodes.size}`,
    `- undefined static error codes: ${result.errorCodes.undefinedCodes.length}; unused catalog error codes: ${result.errorCodes.unusedCodes.length}`,
    `- dynamic error-code sources: ${result.errorCodes.dynamicSources.length}`,
    `- local proto ownership: ${result.diagnostics.some((item) => item.rule === "LOCAL_PROTO_DRIFT") ? "drift detected" : "inventory matches shared proto scan"}`,
    `- deferred outbound messages: ${result.packet.deferredOutboundReport.length}`,
    `- routing diagnostics: ${result.diagnostics.length}`
  ];
  return lines.join("\n");
}

function main() {
  const result = checkProtocolRoutingConsistency();
  console.log(formatRoutingConsistencyReport(result));
  if (result.diagnostics.length > 0) {
    console.error("protocol routing consistency violations:");
    for (const diagnostic of result.diagnostics) {
      console.error(`- [${diagnostic.rule}] ${diagnostic.message}`);
    }
    process.exitCode = 1;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main();
}
