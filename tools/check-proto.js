import { spawnSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

export const PROTOCOL_CHECKS = Object.freeze([
  {
    name: "candidate compatibility baseline",
    args: ["tools/proto-compatibility-baseline.js", "--check"]
  },
  {
    name: "generated-code drift",
    args: ["tools/proto-generate.js", "--check"]
  },
  {
    name: "published breaking changes",
    args: ["tools/check-proto-breaking-changes.js", "--check"]
  },
  {
    name: "message type and routing consistency",
    args: ["tools/check-protocol-routing-consistency.js"]
  },
  {
    name: "binary compatibility fixtures",
    args: ["tools/check-proto-compatibility-fixtures.js"]
  },
  {
    name: "client protocol version policy",
    args: ["tools/client-protocol-version-policy.js"]
  }
]);

export function runProtocolChecks({ checks = PROTOCOL_CHECKS, root = rootDir } = {}) {
  for (const [index, check] of checks.entries()) {
    const command = [process.execPath, ...check.args].join(" ");
    console.log(`\n[protocol ${index + 1}/${checks.length}] ${check.name}`);
    console.log(`$ ${command}`);

    // Inherit output so a child check's rule/file/message/field diagnostic reaches local users and CI unchanged.
    const result = spawnSync(process.execPath, check.args, { cwd: root, stdio: "inherit" });
    if (result.error) {
      console.error(`Unable to start ${check.name}: ${result.error.message}`);
      process.exitCode = 1;
      return false;
    }
    if (result.status !== 0) {
      const status = result.status ?? 1;
      console.error(`Protocol check failed at ${check.name} (exit status ${status}).`);
      process.exitCode = status;
      return false;
    }
  }

  console.log("\nAll protocol compatibility checks passed.");
  return true;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  runProtocolChecks();
}
