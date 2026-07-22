import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";

const projectRoot = process.cwd();
const powershellCommand = process.env.POWERSHELL_BIN || (process.platform === "win32" ? "powershell" : "pwsh");
const probe = spawnSync(powershellCommand, ["-NoProfile", "-Command", "$PSVersionTable.PSVersion.ToString()"], {
  cwd: projectRoot,
  encoding: "utf8"
});
const powershellSkip = !probe.error && probe.status === 0 ? false : "PowerShell is unavailable";

function runDrill(args) {
  const common = process.platform === "win32" ? ["-NoProfile", "-ExecutionPolicy", "Bypass"] : ["-NoProfile"];
  return spawnSync(powershellCommand, [
    ...common,
    "-File", path.join(projectRoot, "scripts", "ops", "rollout-three-process-drill.ps1"),
    ...args
  ], { cwd: projectRoot, encoding: "utf8" });
}

const requiredArgs = [
  "-WorldId", "local",
  "-RolloutEpoch", "rollout-test",
  "-RoomId", "room-test",
  "-OldServerId", "game-server-001",
  "-NewServerId", "game-server-002",
  "-ProxyInstanceId", "game-proxy-001"
];

test("three-process drill dry-run is a control-plane-only local plan", { skip: powershellSkip }, () => {
  const tempRoot = path.join(projectRoot, ".tmp");
  fs.mkdirSync(tempRoot, { recursive: true });
  const directory = fs.mkdtempSync(path.join(tempRoot, "rollout-drill-"));
  const reportPath = path.join(directory, "report.json");
  try {
    const result = runDrill([...requiredArgs, "-ReportPath", reportPath]);
    assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
    const report = JSON.parse(fs.readFileSync(reportPath, "utf8").replace(/^\uFEFF/, ""));
    assert.equal(report.ok, true);
    assert.equal(report.mode, "dry-run");
    assert.equal(report.safety.callsControlPlane, false);
    assert.equal(report.safety.requestsShutdown, false);
    assert.equal(report.inputs.oldServerId, "game-server-001");
    assert.equal(report.transfer.dryRun, true);
  } finally {
    fs.rmSync(directory, { recursive: true, force: true });
  }
});

test("three-process drill refuses execution without a control-plane JWT", { skip: powershellSkip }, () => {
  const result = runDrill([...requiredArgs, "-ExecuteSteps", "-BackupReference", "backup-room-test"]);
  assert.notEqual(result.status, 0);
  assert.match(`${result.stdout}\n${result.stderr}`, /ADMIN_API_TOKEN is required/);
});
