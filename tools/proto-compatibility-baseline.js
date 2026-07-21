import { createHash } from "node:crypto";
import { existsSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const BASELINE_FORMAT = "myserver.protobuf.compatibility-baseline/v1";
const __dirname = path.dirname(fileURLToPath(import.meta.url));
const rootDir = path.resolve(__dirname, "..");
const inventoryPath = path.join(rootDir, "packages", "proto", "compatibility", "inventory.json");

function fail(message) {
  throw new Error(message);
}

function normalizePath(value) {
  return value.replaceAll("\\", "/");
}

function stableValue(value) {
  if (Array.isArray(value)) {
    return value.map(stableValue);
  }
  if (value && typeof value === "object") {
    return Object.fromEntries(
      Object.keys(value)
        .sort()
        .map((key) => [key, stableValue(value[key])])
    );
  }
  return value;
}

function stableJson(value, spacing = 0) {
  return JSON.stringify(stableValue(value), null, spacing);
}

function digest(value) {
  return `sha256:${createHash("sha256").update(stableJson(value)).digest("hex")}`;
}

function tokenize(source, sourcePath) {
  const tokens = [];
  let offset = 0;

  while (offset < source.length) {
    const character = source[offset];
    if (/\s/.test(character)) {
      offset += 1;
      continue;
    }
    if (source.startsWith("//", offset)) {
      offset = source.indexOf("\n", offset + 2);
      if (offset === -1) {
        break;
      }
      continue;
    }
    if (source.startsWith("/*", offset)) {
      const end = source.indexOf("*/", offset + 2);
      if (end === -1) {
        fail(`${sourcePath}: unterminated block comment`);
      }
      offset = end + 2;
      continue;
    }
    if (character === '"' || character === "'") {
      const quote = character;
      let value = quote;
      offset += 1;
      let escaped = false;
      while (offset < source.length) {
        const next = source[offset];
        value += next;
        offset += 1;
        if (!escaped && next === quote) {
          break;
        }
        escaped = !escaped && next === "\\";
        if (next !== "\\") {
          escaped = false;
        }
      }
      if (!value.endsWith(quote)) {
        fail(`${sourcePath}: unterminated string literal`);
      }
      tokens.push(value);
      continue;
    }
    const identifier = source.slice(offset).match(/^[A-Za-z_][A-Za-z0-9_]*/);
    if (identifier) {
      tokens.push(identifier[0]);
      offset += identifier[0].length;
      continue;
    }
    const number = source.slice(offset).match(/^-?(?:0[xX][0-9A-Fa-f]+|\d+)/);
    if (number) {
      tokens.push(number[0]);
      offset += number[0].length;
      continue;
    }
    if ("{}()[];=,.<>".includes(character)) {
      tokens.push(character);
      offset += 1;
      continue;
    }
    fail(`${sourcePath}: unsupported token ${JSON.stringify(character)} at offset ${offset}`);
  }

  return tokens;
}

class ProtoParser {
  constructor(tokens, sourcePath) {
    this.tokens = tokens;
    this.sourcePath = sourcePath;
    this.position = 0;
  }

  peek() {
    return this.tokens[this.position];
  }

  next() {
    const value = this.tokens[this.position];
    this.position += 1;
    return value;
  }

  expect(value) {
    const actual = this.next();
    if (actual !== value) {
      fail(`${this.sourcePath}: expected ${value}, received ${actual ?? "end of file"}`);
    }
  }

  identifier() {
    const value = this.next();
    if (!value || !/^[A-Za-z_][A-Za-z0-9_]*$/.test(value)) {
      fail(`${this.sourcePath}: expected identifier, received ${value ?? "end of file"}`);
    }
    return value;
  }

  number() {
    const value = this.next();
    if (!value || !/^-?(?:0[xX][0-9A-Fa-f]+|\d+)$/.test(value)) {
      fail(`${this.sourcePath}: expected integer, received ${value ?? "end of file"}`);
    }
    return Number(value);
  }

  skipBalanced(open, close) {
    this.expect(open);
    let depth = 1;
    while (depth > 0) {
      const token = this.next();
      if (!token) {
        fail(`${this.sourcePath}: unterminated ${open}${close} block`);
      }
      if (token === open) {
        depth += 1;
      } else if (token === close) {
        depth -= 1;
      }
    }
  }

  skipStatement() {
    let parentheses = 0;
    let brackets = 0;
    let braces = 0;
    while (this.peek()) {
      const token = this.next();
      if (token === "(") parentheses += 1;
      if (token === ")") parentheses -= 1;
      if (token === "[") brackets += 1;
      if (token === "]") brackets -= 1;
      if (token === "{") braces += 1;
      if (token === "}") {
        if (braces === 0 && parentheses === 0 && brackets === 0) {
          this.position -= 1;
          return;
        }
        braces -= 1;
      }
      if (token === ";" && parentheses === 0 && brackets === 0 && braces === 0) {
        return;
      }
    }
  }

  dottedName() {
    let name = "";
    if (this.peek() === ".") {
      name = this.next();
    }
    name += this.identifier();
    while (this.peek() === ".") {
      this.next();
      name += `.${this.identifier()}`;
    }
    return name;
  }

  parse() {
    const result = { package: "", messages: [], enums: [], services: [] };
    while (this.peek()) {
      const token = this.peek();
      if (token === "syntax" || token === "edition" || token === "import" || token === "option") {
        this.skipStatement();
      } else if (token === "package") {
        this.next();
        result.package = this.dottedName();
        this.expect(";");
      } else if (token === "message") {
        this.parseMessage(result, "");
      } else if (token === "enum") {
        this.parseEnum(result, "");
      } else if (token === "service") {
        this.parseService(result, "");
      } else if (token === "extend") {
        this.skipStatement();
        if (this.peek() === "{") {
          this.skipBalanced("{", "}");
        }
      } else if (token === ";") {
        this.next();
      } else {
        fail(`${this.sourcePath}: unsupported top-level declaration ${token}`);
      }
    }
    return result;
  }

  parseMessage(result, parentName) {
    this.expect("message");
    const localName = this.identifier();
    const name = parentName ? `${parentName}.${localName}` : localName;
    const message = { name, fields: [], reserved: { names: [], numbers: [], ranges: [] } };
    result.messages.push(message);
    this.expect("{");
    while (this.peek() && this.peek() !== "}") {
      const token = this.peek();
      if (token === "message") {
        this.parseMessage(result, name);
      } else if (token === "enum") {
        this.parseEnum(result, name);
      } else if (token === "oneof") {
        this.parseOneof(message);
      } else if (token === "reserved") {
        this.parseReserved(message.reserved);
      } else if (token === "option" || token === "extensions") {
        this.skipStatement();
      } else {
        this.parseField(message, null);
      }
    }
    this.expect("}");
  }

  parseOneof(message) {
    this.expect("oneof");
    const oneof = this.identifier();
    this.expect("{");
    while (this.peek() && this.peek() !== "}") {
      if (this.peek() === "option") {
        this.skipStatement();
      } else {
        this.parseField(message, oneof);
      }
    }
    this.expect("}");
  }

  parseType() {
    if (this.peek() === "map") {
      this.next();
      this.expect("<");
      const keyType = this.dottedName();
      this.expect(",");
      const valueType = this.dottedName();
      this.expect(">");
      return `map<${keyType},${valueType}>`;
    }
    return this.dottedName();
  }

  parseField(message, oneof) {
    let label = "singular";
    if (["optional", "required", "repeated"].includes(this.peek())) {
      label = this.next();
    }
    const type = this.parseType();
    const name = this.identifier();
    this.expect("=");
    const number = this.number();
    if (this.peek() === "[") {
      this.skipBalanced("[", "]");
    }
    this.expect(";");
    message.fields.push({ label, name, number, oneof: oneof ?? "", type });
  }

  parseReserved(reserved) {
    this.expect("reserved");
    while (this.peek() && this.peek() !== ";") {
      if (this.peek() === ",") {
        this.next();
        continue;
      }
      const value = this.next();
      if (value.startsWith('"') || value.startsWith("'")) {
        reserved.names.push(value.slice(1, -1));
        continue;
      }
      if (!/^-?(?:0[xX][0-9A-Fa-f]+|\d+)$/.test(value)) {
        fail(`${this.sourcePath}: invalid reserved value ${value}`);
      }
      const start = Number(value);
      if (this.peek() === "to") {
        this.next();
        const endToken = this.next();
        const end = endToken === "max" ? "max" : Number(endToken);
        if (typeof end === "number" && !Number.isFinite(end)) {
          fail(`${this.sourcePath}: invalid reserved range end ${endToken}`);
        }
        reserved.ranges.push({ end, start });
      } else {
        reserved.numbers.push(start);
      }
    }
    this.expect(";");
  }

  parseEnum(result, parentName) {
    this.expect("enum");
    const localName = this.identifier();
    const name = parentName ? `${parentName}.${localName}` : localName;
    const enumeration = { name, reserved: { names: [], numbers: [], ranges: [] }, values: [] };
    result.enums.push(enumeration);
    this.expect("{");
    while (this.peek() && this.peek() !== "}") {
      if (this.peek() === "option") {
        this.skipStatement();
      } else if (this.peek() === "reserved") {
        this.parseReserved(enumeration.reserved);
      } else {
        const valueName = this.identifier();
        this.expect("=");
        const number = this.number();
        if (this.peek() === "[") {
          this.skipBalanced("[", "]");
        }
        this.expect(";");
        enumeration.values.push({ name: valueName, number });
      }
    }
    this.expect("}");
  }

  parseService(result, parentName) {
    this.expect("service");
    const localName = this.identifier();
    const name = parentName ? `${parentName}.${localName}` : localName;
    const service = { name, rpcs: [] };
    result.services.push(service);
    this.expect("{");
    while (this.peek() && this.peek() !== "}") {
      if (this.peek() === "option") {
        this.skipStatement();
      } else if (this.peek() === "rpc") {
        this.next();
        const rpcName = this.identifier();
        const request = this.parseRpcType();
        this.expect("returns");
        const response = this.parseRpcType();
        if (this.peek() === "{") {
          this.skipBalanced("{", "}");
        } else {
          this.expect(";");
        }
        service.rpcs.push({ name: rpcName, request, response });
      } else {
        fail(`${this.sourcePath}: unsupported service member ${this.peek()}`);
      }
    }
    this.expect("}");
  }

  parseRpcType() {
    this.expect("(");
    const stream = this.peek() === "stream";
    if (stream) {
      this.next();
    }
    const type = this.dottedName();
    this.expect(")");
    return { stream, type };
  }
}

function sortDeclarations(parsed) {
  for (const message of parsed.messages) {
    message.fields.sort((left, right) => left.number - right.number || left.name.localeCompare(right.name));
    message.reserved.names.sort();
    message.reserved.numbers.sort((left, right) => left - right);
    message.reserved.ranges.sort((left, right) => left.start - right.start || String(left.end).localeCompare(String(right.end)));
  }
  for (const enumeration of parsed.enums) {
    enumeration.values.sort((left, right) => left.number - right.number || left.name.localeCompare(right.name));
    enumeration.reserved.names.sort();
    enumeration.reserved.numbers.sort((left, right) => left - right);
    enumeration.reserved.ranges.sort((left, right) => left.start - right.start || String(left.end).localeCompare(String(right.end)));
  }
  for (const service of parsed.services) {
    service.rpcs.sort((left, right) => left.name.localeCompare(right.name));
  }
  parsed.messages.sort((left, right) => left.name.localeCompare(right.name));
  parsed.enums.sort((left, right) => left.name.localeCompare(right.name));
  parsed.services.sort((left, right) => left.name.localeCompare(right.name));
  return parsed;
}

export function parseProto(source, sourcePath = "in-memory.proto") {
  return sortDeclarations(new ProtoParser(tokenize(source, sourcePath), sourcePath).parse());
}

export function readInventory(filePath = inventoryPath) {
  return JSON.parse(readFileSync(filePath, "utf8"));
}

function inventoryProtoPaths(inventory) {
  if (!Array.isArray(inventory.protocols) || inventory.protocols.length === 0) {
    fail("protocol inventory must declare at least one protocol");
  }
  const paths = inventory.protocols.map((protocol) => protocol.file);
  if (paths.some((filePath) => typeof filePath !== "string" || !filePath.startsWith("packages/proto/") || !filePath.endsWith(".proto"))) {
    fail("protocol inventory contains an invalid proto path");
  }
  if (new Set(paths).size !== paths.length) {
    fail("protocol inventory contains duplicate proto paths");
  }
  return [...paths].sort();
}

function listProtoFiles(directory, relativeBase = "") {
  const result = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    if ([".git", "node_modules", "target"].includes(entry.name)) {
      continue;
    }
    const relativePath = path.join(relativeBase, entry.name);
    const absolutePath = path.join(directory, entry.name);
    if (entry.isDirectory()) {
      result.push(...listProtoFiles(absolutePath, relativePath));
    } else if (entry.isFile() && entry.name.endsWith(".proto")) {
      result.push(normalizePath(relativePath));
    }
  }
  return result;
}

export function buildBaseline(inventory, root = rootDir) {
  const files = inventoryProtoPaths(inventory).map((relativePath) => {
    const sourcePath = path.join(root, relativePath);
    if (!existsSync(sourcePath)) {
      fail(`inventory proto does not exist: ${relativePath}`);
    }
    return { path: relativePath, ...parseProto(readFileSync(sourcePath, "utf8"), relativePath) };
  });
  const projection = {
    files,
    format: BASELINE_FORMAT
  };
  return { ...projection, digest: digest(projection) };
}

export function validateBaselineSnapshot(snapshot, label = "compatibility baseline") {
  if (!snapshot || typeof snapshot !== "object" || Array.isArray(snapshot)) {
    fail(`${label} must be a JSON object`);
  }
  if (snapshot.format !== BASELINE_FORMAT || !Array.isArray(snapshot.files)) {
    fail(`${label} has an unsupported format or missing files`);
  }
  const projection = { files: snapshot.files, format: snapshot.format };
  if (typeof snapshot.digest !== "string" || snapshot.digest !== digest(projection)) {
    fail(`${label} digest is invalid`);
  }
  return snapshot;
}

function compareText(left, right) {
  if (left === right) {
    return null;
  }
  const leftLines = left.split("\n");
  const rightLines = right.split("\n");
  const limit = Math.max(leftLines.length, rightLines.length);
  for (let index = 0; index < limit; index += 1) {
    if (leftLines[index] !== rightLines[index]) {
      return `line ${index + 1}: baseline=${JSON.stringify(leftLines[index] ?? "")} current=${JSON.stringify(rightLines[index] ?? "")}`;
    }
  }
  return "unknown difference";
}

export function validateInventory(inventory, root = rootDir) {
  const expected = inventoryProtoPaths(inventory);
  const actual = listProtoFiles(path.join(root, "packages", "proto"))
    .filter((filePath) => !filePath.startsWith("compatibility/"))
    .map((filePath) => `packages/proto/${filePath}`)
    .sort();
  if (stableJson(expected) !== stableJson(actual)) {
    fail(`protocol inventory proto paths differ from packages/proto: inventory=${expected.join(", ")} actual=${actual.join(", ")}`);
  }

  const repositoryProtos = listProtoFiles(root)
    .filter((filePath) => !filePath.startsWith("docs/历史归档/") && !filePath.startsWith("tests/proto/fixtures/"))
    .sort();
  if (stableJson(repositoryProtos) !== stableJson(expected)) {
    const extra = repositoryProtos.filter((filePath) => !expected.includes(filePath));
    const missing = expected.filter((filePath) => !repositoryProtos.includes(filePath));
    fail(`untracked local proto definitions: extra=${extra.join(", ") || "none"} missing=${missing.join(", ") || "none"}`);
  }
}

function baselineFilePath(inventory, root = rootDir) {
  const relativePath = inventory.baseline?.file;
  if (typeof relativePath !== "string" || !relativePath.startsWith("packages/proto/compatibility/")) {
    fail("protocol inventory must define baseline.file under packages/proto/compatibility");
  }
  return path.join(root, relativePath);
}

export function checkBaseline(inventory = readInventory(), root = rootDir) {
  validateInventory(inventory, root);
  const baselinePath = baselineFilePath(inventory, root);
  if (!existsSync(baselinePath)) {
    fail(`compatibility baseline does not exist: ${normalizePath(path.relative(root, baselinePath))}`);
  }
  const recorded = JSON.parse(readFileSync(baselinePath, "utf8"));
  validateBaselineSnapshot(recorded, `compatibility baseline ${normalizePath(path.relative(root, baselinePath))}`);
  const current = buildBaseline(inventory, root);
  const difference = compareText(stableJson(recorded), stableJson(current));
  if (difference) {
    fail(`protocol compatibility baseline is stale (${difference}). Run node tools/proto-compatibility-baseline.js --write --reason <reason> --approved-by <reviewer> after approval.`);
  }
  return current;
}

function parseWriteMetadata(args) {
  const reasonIndex = args.indexOf("--reason");
  const approvedByIndex = args.indexOf("--approved-by");
  const reason = reasonIndex === -1 ? "" : args[reasonIndex + 1]?.trim();
  const approvedBy = approvedByIndex === -1 ? "" : args[approvedByIndex + 1]?.trim();
  if (!reason || !approvedBy) {
    fail("--write requires non-empty --reason <reason> and --approved-by <reviewer>; review authorization is verified from the committed diff.");
  }
  return { approvedBy, reason };
}

export function writeBaseline(inventory = readInventory(), root = rootDir, metadata) {
  validateInventory(inventory, root);
  if (!metadata?.reason || !metadata?.approvedBy) {
    fail("writeBaseline requires approval metadata");
  }
  const baselinePath = baselineFilePath(inventory, root);
  const baseline = buildBaseline(inventory, root);
  writeFileSync(baselinePath, `${stableJson(baseline, 2)}\n`, "utf8");
  return baseline;
}

function runCli() {
  const args = process.argv.slice(2);
  const write = args.includes("--write");
  if (write && args.includes("--check")) {
    fail("choose either --check or --write");
  }
  const inventory = readInventory();
  if (write) {
    const metadata = parseWriteMetadata(args);
    const baseline = writeBaseline(inventory, rootDir, metadata);
    console.log(`updated ${inventory.baseline.file} (${baseline.digest})`);
    return;
  }
  const baseline = checkBaseline(inventory, rootDir);
  console.log(`protocol compatibility baseline is current (${baseline.digest})`);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    runCli();
  } catch (error) {
    console.error(`proto compatibility baseline check failed: ${error.message}`);
    process.exitCode = 1;
  }
}
