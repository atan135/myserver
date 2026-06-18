import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const DEFAULT_ROOT_DIR = path.resolve(__dirname, "..");

const STRICT_ENV_NAMES = new Set(["production", "prod", "staging", "stage", "test", "testing"]);
const LOCAL_FALLBACK_CONTEXT_PATTERN =
  /\b(local|development|dev|manual|fallback|本地|手工|兼容|调试|联调)\b/i;
const STRICT_CONTEXT_PATTERN = /\b(strict|test|production|prod|staging|线上|生产|预发|测试)\b/i;

const LEGACY_DIRECT_CONFIG_BY_SERVICE = {
  "auth-http": [
    "GAME_PROXY_HOST",
    "GAME_PROXY_PORT",
    "GAME_SERVER_ADMIN_HOST",
    "GAME_SERVER_ADMIN_PORT"
  ],
  "admin-api": [
    "GAME_SERVER_ADMIN_HOST",
    "GAME_SERVER_ADMIN_PORT",
    "GAME_PROXY_ADMIN_HOST",
    "GAME_PROXY_ADMIN_PORT"
  ],
  "mail-service": ["GAME_SERVER_ADMIN_HOST", "GAME_SERVER_ADMIN_PORT"],
  "game-server": ["MATCH_SERVICE_ADDR"],
  "game-proxy": ["UPSTREAM_SERVER_ID", "UPSTREAM_LOCAL_SOCKET_NAME"],
  "match-service": ["GAME_SERVER_INTERNAL_SOCKET_NAME", "GAME_INTERNAL_SOCKET_NAME"]
};

const LEGACY_DIRECT_CONFIG_REASONS = {
  GAME_PROXY_HOST: "auth-http must discover game-proxy.client from the service registry in strict environments",
  GAME_PROXY_PORT: "auth-http must discover game-proxy.client from the service registry in strict environments",
  GAME_SERVER_ADMIN_HOST: "control-plane consumers must discover game-server.admin from the service registry in strict environments",
  GAME_SERVER_ADMIN_PORT: "control-plane consumers must discover game-server.admin from the service registry in strict environments",
  GAME_PROXY_ADMIN_HOST: "admin-api must discover game-proxy.admin from the service registry in strict environments",
  GAME_PROXY_ADMIN_PORT: "admin-api must discover game-proxy.admin from the service registry in strict environments",
  MATCH_SERVICE_ADDR: "game-server must discover match-service.grpc from the service registry in strict environments",
  UPSTREAM_SERVER_ID: "game-proxy must discover game-server.proxy-local routes from the service registry in strict environments",
  UPSTREAM_LOCAL_SOCKET_NAME: "game-proxy must discover game-server.proxy-local routes from the service registry in strict environments",
  GAME_SERVER_INTERNAL_SOCKET_NAME: "match-service must discover game-server.internal local_socket endpoints from the service registry in strict environments",
  GAME_INTERNAL_SOCKET_NAME: "match-service legacy internal socket alias is local fallback only"
};

const ROLLOUT_MANUAL_FALLBACK_ENV_NAMES = new Set([
  "MYSERVER_OLD_GAME_ADMIN_HOST",
  "MYSERVER_OLD_GAME_ADMIN_PORT",
  "MYSERVER_NEW_GAME_ADMIN_HOST",
  "MYSERVER_NEW_GAME_ADMIN_PORT",
  "MYSERVER_AUTH_BASE_URL",
  "MYSERVER_PROXY_ADMIN_URL"
]);

const AMBIGUOUS_SELF_BIND_ENV_NAMES = new Set([
  "GAME_INTERNAL_SOCKET_NAME"
]);

const EXCLUDED_DIR_NAMES = new Set([
  ".git",
  ".tmp",
  "node_modules",
  "logs",
  "target",
  "dist",
  "build",
  "coverage",
  "历史归档"
]);

const SCANNABLE_EXTENSIONS = new Set([
  "",
  ".env",
  ".example",
  ".local",
  ".production",
  ".staging",
  ".test",
  ".testing",
  ".json",
  ".yaml",
  ".yml",
  ".toml",
  ".conf",
  ".config",
  ".ini",
  ".ps1",
  ".sh",
  ".cmd",
  ".bat"
]);

const CONFIG_PATH_HINT_PATTERN =
  /(^|[\\/])(apps|scripts|tools|deploy|deployment|deployments|config|configs|helm|k8s|kubernetes|docker)([\\/]|$)/i;
const SCRIPT_TARGET_SCAN_PATTERN =
  /(^|\/)(scripts\/.*|tools\/mock-client\/src\/.*|tools\/mock-client\/help_rollout\.txt|package\.json)$/i;
const ROLLOUT_DIRECT_TARGET_ARGS = [
  "--old-admin-host",
  "--old-admin-port",
  "--new-admin-host",
  "--new-admin-port",
  "--proxy-admin-url"
];
const ROLLOUT_DIRECT_TARGET_MARKERS = [
  "--resolved-control-targets",
  "--local-debug-targets"
];
const ROLLOUT_FIXED_DEFAULT_PATTERNS = [
  {
    pattern: /oldAdminPort\s*:\s*(?:parseNumber\([^,\n]+,\s*)?7500\b/,
    reason: "rollout tool default old game-server admin target must be game-server.admin plus instance id"
  },
  {
    pattern: /newAdminPort\s*:\s*(?:parseNumber\([^,\n]+,\s*)?7501\b/,
    reason: "rollout tool default new game-server admin target must be game-server.admin plus instance id"
  },
  {
    pattern: /proxyAdminUrl\s*:\s*[^,\n]*127\.0\.0\.1:7101/,
    reason: "rollout tool default game-proxy admin target must be game-proxy.admin"
  },
  {
    pattern: /\|\|\s*["']http:\/\/127\.0\.0\.1:7101["']/,
    reason: "rollout tool must not silently fall back to fixed game-proxy admin URL"
  },
  {
    pattern: /\|\|\s*750[01]\b/,
    reason: "rollout tool must not silently fall back to fixed game-server admin port"
  },
  {
    pattern: /local\/manual fallback default:.*(?:127\.0\.0\.1|750[01]|7101)/,
    reason: "help text must not describe fixed local endpoints as defaults"
  }
];

const ALL_LEGACY_ENV_NAMES = new Set([
  ...Object.values(LEGACY_DIRECT_CONFIG_BY_SERVICE).flat(),
  ...ROLLOUT_MANUAL_FALLBACK_ENV_NAMES
]);

export function scanDiscoveryConfig(options = {}) {
  const rootDir = path.resolve(options.rootDir ?? DEFAULT_ROOT_DIR);
  const files = listCandidateFiles(rootDir);
  const violations = [];
  const allowedLocalFallbacks = [];
  const checkedFiles = [];
  const strictFiles = [];
  const localExampleFiles = [];
  const scriptFixedTargetViolations = [];

  for (const filePath of files) {
    const relativePath = toPosixRelative(rootDir, filePath);
    const text = fs.readFileSync(filePath, "utf8");
    const lines = splitLines(text);
    const assignments = parseAssignments(lines);
    scriptFixedTargetViolations.push(...scanScriptTargetViolations(relativePath, lines));
    if (assignments.length === 0 && !isEnvLikePath(relativePath)) {
      continue;
    }

    const serviceName = inferServiceName(relativePath, assignments);
    const localExample = isLocalExamplePath(relativePath, assignments);
    const strictContext = getStrictContext(relativePath, assignments, { localExample });
    const strictConfigFile = strictContext.strict;
    let checked = false;

    if (strictConfigFile) {
      checked = true;
      strictFiles.push(relativePath);
      for (const assignment of assignments) {
        const violation = strictViolationForAssignment({
          assignment,
          serviceName,
          relativePath,
          strictContext
        });
        if (violation) {
          violations.push(violation);
        }
      }
    }

    if (localExample) {
      checked = true;
      localExampleFiles.push(relativePath);
      for (const assignment of assignments) {
        const ruleServiceName = serviceNameForLegacyEnv(assignment.name, serviceName);
        if (!ruleServiceName) {
          continue;
        }
        if (hasLocalFallbackContext(lines, assignment.lineNumber)) {
          allowedLocalFallbacks.push({
            file: relativePath,
            line: assignment.lineNumber,
            variable: assignment.name,
            service: ruleServiceName,
            reason: "active local fallback example is explicitly documented as local-only"
          });
          continue;
        }
        violations.push({
          file: relativePath,
          line: assignment.lineNumber,
          variable: assignment.name,
          service: ruleServiceName,
          rule: "local_fallback_example_requires_annotation",
          severity: "error",
          reason:
            "legacy direct config may appear in a base .env.example only when nearby comments mark it as local fallback only",
          remediation:
            "Either comment out the variable or add clear local fallback only wording; strict/test/production overlays must not define it."
        });
      }
    }

    if (checked) {
      checkedFiles.push(relativePath);
    }

  }

  violations.push(...scriptFixedTargetViolations);
  const uniqueCheckedFiles = uniqueSorted(checkedFiles);
  return {
    ok: violations.length === 0,
    summary: {
      checkedFiles: uniqueCheckedFiles.length,
      strictFiles: uniqueSorted(strictFiles).length,
      localExampleFiles: uniqueSorted(localExampleFiles).length,
      allowedLocalFallbacks: allowedLocalFallbacks.length,
      scriptFixedTargetViolations: scriptFixedTargetViolations.length,
      violations: violations.length
    },
    checkedFiles: uniqueCheckedFiles,
    strictFiles: uniqueSorted(strictFiles),
    localExampleFiles: uniqueSorted(localExampleFiles),
    allowedLocalFallbacks,
    scriptFixedTargetViolations,
    violations
  };
}

function listCandidateFiles(rootDir) {
  const files = [];

  function walk(dir) {
    let entries;
    try {
      entries = fs.readdirSync(dir, { withFileTypes: true });
    } catch {
      return;
    }

    for (const entry of entries) {
      if (entry.isDirectory()) {
        if (!EXCLUDED_DIR_NAMES.has(entry.name)) {
          walk(path.join(dir, entry.name));
        }
        continue;
      }
      if (!entry.isFile()) {
        continue;
      }

      const fullPath = path.join(dir, entry.name);
      const relativePath = toPosixRelative(rootDir, fullPath);
      if (shouldScanFile(relativePath)) {
        files.push(fullPath);
      }
    }
  }

  walk(rootDir);
  return files.sort();
}

function shouldScanFile(relativePath) {
  if (SCRIPT_TARGET_SCAN_PATTERN.test(relativePath)) {
    return true;
  }

  const baseName = path.posix.basename(relativePath);
  if (baseName.startsWith(".env")) {
    return true;
  }
  if (!CONFIG_PATH_HINT_PATTERN.test(relativePath)) {
    return false;
  }

  const lower = baseName.toLowerCase();
  if (lower.includes(".env")) {
    return true;
  }

  const extension = path.posix.extname(lower);
  return SCANNABLE_EXTENSIONS.has(extension);
}

function scanScriptTargetViolations(relativePath, lines) {
  if (!SCRIPT_TARGET_SCAN_PATTERN.test(relativePath)) {
    return [];
  }

  const violations = [];
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index];
    for (const rule of ROLLOUT_FIXED_DEFAULT_PATTERNS) {
      if (rule.pattern.test(line)) {
        violations.push({
          file: relativePath,
          line: index + 1,
          variable: "rollout-control-target",
          service: "rollout-script",
          rule: "script_fixed_control_target_default_forbidden",
          severity: "error",
          reason: rule.reason,
          remediation:
            "Use registry target semantics by default, for example instance id plus game-server.admin/game-proxy.admin. Fixed host/port examples must be explicitly marked local debug fallback."
        });
        break;
      }
    }
  }

  for (const block of commandBlocks(lines)) {
    if (!block.text.includes("rollout-transfer-cli.js") && !block.text.includes("rollout-fault-drill-cli.js")) {
      continue;
    }
    if (!ROLLOUT_DIRECT_TARGET_ARGS.some((arg) => block.text.includes(arg))) {
      continue;
    }
    if (ROLLOUT_DIRECT_TARGET_MARKERS.some((arg) => block.text.includes(arg))) {
      continue;
    }

    violations.push({
      file: relativePath,
      line: block.startLine,
      variable: "rollout-control-target",
      service: "rollout-script",
      rule: "script_direct_control_target_requires_marker",
      severity: "error",
      reason:
        "rollout CLI command examples or invocations that pass host/port/url must mark them as pre-resolved registry inputs or local debug fallback",
      remediation:
        "Add --resolved-control-targets when passing endpoints resolved by registry discovery, or --local-debug-targets for manual local fallback examples."
    });
  }

  return violations;
}

function commandBlocks(lines) {
  const blocks = [];
  let current = null;

  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index];
    const startsBlock = /rollout-(?:transfer|fault-drill)-cli\.js/.test(line) ||
      /^\s*\$[A-Za-z0-9_]+\s*=\s*@\(/.test(line);

    if (!current && startsBlock) {
      current = { startLine: index + 1, lines: [line] };
      if (!continuesCommandBlock(line)) {
        blocks.push({ startLine: current.startLine, text: current.lines.join("\n") });
        current = null;
      }
      continue;
    }

    if (!current) {
      continue;
    }

    current.lines.push(line);
    if (!continuesCommandBlock(line) && !/^\s*["'][^"']+["']\s*,?\s*$/.test(line)) {
      blocks.push({ startLine: current.startLine, text: current.lines.join("\n") });
      current = null;
    }
  }

  if (current) {
    blocks.push({ startLine: current.startLine, text: current.lines.join("\n") });
  }

  return blocks;
}

function continuesCommandBlock(line) {
  return /[`^]\s*$/.test(line) || /^\s*\$[A-Za-z0-9_]+\s*=\s*@\(/.test(line) || !/\)\s*$/.test(line);
}

function parseAssignments(lines) {
  const assignments = [];
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index].replace(/^\uFEFF/, "");
    const parsed =
      parseEnvAssignment(line) ??
      parsePowershellEnvAssignment(line) ??
      parseYamlAssignment(line) ??
      parseJsonAssignment(line);
    if (!parsed) {
      continue;
    }
    assignments.push({
      ...parsed,
      lineNumber: index + 1,
      raw: line
    });
  }
  return assignments;
}

function parseEnvAssignment(line) {
  const trimmed = line.trim();
  if (!trimmed || trimmed.startsWith("#") || trimmed.startsWith("//")) {
    return null;
  }
  const match = trimmed.match(/^(?:export\s+)?([A-Z][A-Z0-9_]*)\s*=\s*(.*)$/);
  if (!match) {
    return null;
  }
  return { name: match[1], value: cleanupValue(match[2]) };
}

function parsePowershellEnvAssignment(line) {
  const trimmed = line.trim();
  if (!trimmed || trimmed.startsWith("#")) {
    return null;
  }
  const match = trimmed.match(/^\$env:([A-Z][A-Z0-9_]*)\s*=\s*(.+)$/i);
  if (!match) {
    return null;
  }
  return { name: match[1].toUpperCase(), value: cleanupValue(match[2]) };
}

function parseYamlAssignment(line) {
  const trimmed = line.trim();
  if (!trimmed || trimmed.startsWith("#") || trimmed.startsWith("//")) {
    return null;
  }
  const match = trimmed.match(/^-?\s*([A-Z][A-Z0-9_]*)\s*:\s*(.+)$/);
  if (!match) {
    return null;
  }
  return { name: match[1], value: cleanupValue(match[2]) };
}

function parseJsonAssignment(line) {
  const trimmed = line.trim();
  if (!trimmed || trimmed.startsWith("//")) {
    return null;
  }
  const match = trimmed.match(/^"([A-Z][A-Z0-9_]*)"\s*:\s*"?([^",}]*)"?\s*,?$/);
  if (!match) {
    return null;
  }
  return { name: match[1], value: cleanupValue(match[2]) };
}

function cleanupValue(value) {
  let result = String(value ?? "").trim();
  result = result.replace(/\s+#.*$/, "").trim();
  result = result.replace(/\s+\/\/.*$/, "").trim();
  if (
    (result.startsWith('"') && result.endsWith('"')) ||
    (result.startsWith("'") && result.endsWith("'"))
  ) {
    result = result.slice(1, -1);
  }
  return result.trim();
}

function inferServiceName(relativePath, assignments) {
  const appMatch = relativePath.match(/(?:^|\/)apps\/([^/]+)\//);
  if (appMatch) {
    return appMatch[1];
  }

  const serviceName = assignmentValue(assignments, "SERVICE_NAME");
  if (serviceName && LEGACY_DIRECT_CONFIG_BY_SERVICE[serviceName]) {
    return serviceName;
  }

  const normalizedPath = relativePath.toLowerCase();
  for (const name of Object.keys(LEGACY_DIRECT_CONFIG_BY_SERVICE)) {
    if (normalizedPath.includes(name)) {
      return name;
    }
  }
  return "";
}

function getStrictContext(relativePath, assignments, options = {}) {
  const reasons = [];
  const lowerPath = relativePath.toLowerCase();
  const baseName = path.posix.basename(lowerPath);
  const strictPath =
    /\.env\.(production|prod|staging|stage|test|testing)(\.example)?$/.test(baseName) ||
    /(^|[._/-])(production|prod|staging|stage|test|testing)([._/-]|$)/.test(lowerPath);
  if (strictPath) {
    reasons.push("strict path/name");
  }

  const nodeEnv = assignmentValue(assignments, "NODE_ENV");
  const appEnv = assignmentValue(assignments, "APP_ENV");
  for (const [name, value] of [
    ["NODE_ENV", nodeEnv],
    ["APP_ENV", appEnv]
  ]) {
    if (value && STRICT_ENV_NAMES.has(value.trim().toLowerCase())) {
      reasons.push(`${name}=${value}`);
    }
  }

  if (isTruthy(assignmentValue(assignments, "DISCOVERY_REQUIRED")) && !options.localExample) {
    reasons.push("DISCOVERY_REQUIRED=true");
  }
  if (isTruthy(assignmentValue(assignments, "DISALLOW_LEGACY_DIRECT_CONFIG"))) {
    reasons.push("DISALLOW_LEGACY_DIRECT_CONFIG=true");
  }

  return {
    strict: reasons.length > 0,
    reasons
  };
}

function isLocalExamplePath(relativePath, assignments) {
  const lowerPath = relativePath.toLowerCase();
  if (!/(^|\/)apps\/[^/]+\/\.env\.example$/.test(lowerPath)) {
    return false;
  }
  if (isTruthy(assignmentValue(assignments, "DISALLOW_LEGACY_DIRECT_CONFIG"))) {
    return false;
  }
  const envValue = assignmentValue(assignments, "NODE_ENV") || assignmentValue(assignments, "APP_ENV") || "";
  return !envValue || ["development", "dev", "local"].includes(envValue.trim().toLowerCase());
}

function isEnvLikePath(relativePath) {
  return path.posix.basename(relativePath).toLowerCase().startsWith(".env");
}

function assignmentValue(assignments, name) {
  const assignment = assignments.find((item) => item.name === name);
  return assignment?.value;
}

function strictViolationForAssignment({ assignment, serviceName, relativePath, strictContext }) {
  if (assignment.name === "REGISTRY_ENABLED" && isFalsy(assignment.value) && serviceName !== "metrics-collector") {
    return {
      file: relativePath,
      line: assignment.lineNumber,
      variable: assignment.name,
      service: serviceName || "unknown",
      rule: "strict_registry_must_be_enabled",
      severity: "error",
      reason: "strict/test/production discovery requires REGISTRY_ENABLED=true",
      remediation: "Set REGISTRY_ENABLED=true and publish/consume endpoints through the Redis service registry."
    };
  }

  if (assignment.name === "DISCOVERY_REQUIRED" && isFalsy(assignment.value) && serviceName !== "metrics-collector") {
    return {
      file: relativePath,
      line: assignment.lineNumber,
      variable: assignment.name,
      service: serviceName || "unknown",
      rule: "strict_discovery_must_be_required",
      severity: "error",
      reason: "strict/test/production discovery must not disable DISCOVERY_REQUIRED",
      remediation: "Set DISCOVERY_REQUIRED=true for test, staging, and production configs."
    };
  }

  if (ROLLOUT_MANUAL_FALLBACK_ENV_NAMES.has(assignment.name)) {
    return {
      file: relativePath,
      line: assignment.lineNumber,
      variable: assignment.name,
      service: "rollout-three-process-drill",
      rule: "strict_rollout_manual_fallback_forbidden",
      severity: "error",
      reason:
        "rollout drill strict/test/production runs must resolve control endpoints from registry, not manual local fallback inputs",
      remediation:
        "Remove the manual endpoint variable from strict configs; provide registry instances and, when needed, explicit instance ids instead."
    };
  }

  const ruleServiceName = serviceNameForLegacyEnv(assignment.name, serviceName);
  if (!ruleServiceName) {
    return null;
  }
  if (!serviceName && AMBIGUOUS_SELF_BIND_ENV_NAMES.has(assignment.name)) {
    return null;
  }

  return {
    file: relativePath,
    line: assignment.lineNumber,
    variable: assignment.name,
    service: ruleServiceName,
    rule: "strict_legacy_direct_config_forbidden",
    severity: "error",
    reason: LEGACY_DIRECT_CONFIG_REASONS[assignment.name] ?? "legacy direct service config is forbidden in strict environments",
    remediation:
      "Remove this variable from strict/test/production config and use Redis service registry endpoints. Local fallback examples belong only in development .env.example with local-only comments.",
    strictContext: strictContext.reasons
  };
}

function serviceNameForLegacyEnv(envName, serviceName) {
  if (!ALL_LEGACY_ENV_NAMES.has(envName)) {
    return "";
  }
  if (serviceName && LEGACY_DIRECT_CONFIG_BY_SERVICE[serviceName]?.includes(envName)) {
    return serviceName;
  }

  if (serviceName && LEGACY_DIRECT_CONFIG_BY_SERVICE[serviceName]) {
    return "";
  }

  const owners = Object.entries(LEGACY_DIRECT_CONFIG_BY_SERVICE)
    .filter(([, names]) => names.includes(envName))
    .map(([name]) => name);
  if (owners.length === 1) {
    return owners[0];
  }
  return owners.length > 1 ? "unknown-consumer" : "";
}

function hasLocalFallbackContext(lines, lineNumber) {
  const start = Math.max(0, lineNumber - 8);
  const end = Math.min(lines.length, lineNumber + 3);
  const context = lines.slice(start, end).join("\n");
  return LOCAL_FALLBACK_CONTEXT_PATTERN.test(context) && STRICT_CONTEXT_PATTERN.test(context);
}

function isTruthy(value) {
  return /^(1|true|yes|on)$/i.test(String(value ?? "").trim());
}

function isFalsy(value) {
  return /^(0|false|no|off)$/i.test(String(value ?? "").trim());
}

function splitLines(text) {
  return text.replace(/\r\n/g, "\n").replace(/\r/g, "\n").split("\n");
}

function toPosixRelative(rootDir, filePath) {
  return path.relative(rootDir, filePath).split(path.sep).join("/");
}

function uniqueSorted(values) {
  return [...new Set(values)].sort();
}

function parseArgs(argv) {
  const args = {
    rootDir: DEFAULT_ROOT_DIR,
    json: true,
    pretty: true
  };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--root") {
      args.rootDir = path.resolve(argv[index + 1]);
      index += 1;
    } else if (arg === "--compact") {
      args.pretty = false;
    } else if (arg === "--text") {
      args.json = false;
    } else if (arg === "--json") {
      args.json = true;
    } else {
      throw new Error(`unknown argument: ${arg}`);
    }
  }

  return args;
}

function printTextReport(result) {
  if (result.ok) {
    console.log(
      `discovery config check passed: ${result.summary.checkedFiles} files checked, ${result.summary.allowedLocalFallbacks} local fallback examples allowed`
    );
    return;
  }

  console.error(`discovery config check failed: ${result.summary.violations} violation(s)`);
  for (const violation of result.violations) {
    console.error(
      `- ${violation.file}:${violation.line} ${violation.variable} (${violation.service}) ${violation.reason}`
    );
    console.error(`  remediation: ${violation.remediation}`);
  }
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  const result = scanDiscoveryConfig({ rootDir: args.rootDir });

  if (args.json) {
    console.log(JSON.stringify(result, null, args.pretty ? 2 : 0));
  } else {
    printTextReport(result);
  }

  if (!result.ok) {
    process.exitCode = 1;
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main();
}
