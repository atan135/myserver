import { spawnSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const projectRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const cargoTargetDir = path.join(projectRoot, ".tmp", "cargo-target", "mail-server-self-test");
const executableSuffix = process.platform === "win32" ? ".exe" : "";
const cargoCommand = process.platform === "win32" ? "cargo.exe" : "cargo";
const npmCliPath = process.env.npm_execpath;

if (!npmCliPath) {
  console.error("npm_execpath is unavailable; run this entry through npm run test:mail:server.");
  process.exit(1);
}

function npmStep(name, script) {
  return {
    name,
    command: process.execPath,
    args: [npmCliPath, "run", script]
  };
}

const steps = [
  npmStep("server-only protocol compatibility", "check:proto:server"),
  npmStep("mail unit and static checks", "test:mail:unit"),
  npmStep("mail isolated core flows", "test:mail:core"),
  {
    name: "isolated game-server build",
    command: cargoCommand,
    args: [
      "build",
      "--manifest-path",
      "apps/game-server/Cargo.toml",
      "--target-dir",
      cargoTargetDir
    ]
  },
  {
    name: "isolated chat-server build",
    command: cargoCommand,
    args: [
      "build",
      "--manifest-path",
      "apps/chat-server/Cargo.toml",
      "--target-dir",
      cargoTargetDir
    ]
  },
  {
    name: "mail reliability fault drill",
    command: process.execPath,
    args: [npmCliPath, "run", "test:mail:reliability"],
    env: {
      TEST_GAME_SERVER_BIN: path.join(
        cargoTargetDir,
        "debug",
        `game-server${executableSuffix}`
      ),
      TEST_CHAT_SERVER_BIN: path.join(
        cargoTargetDir,
        "debug",
        `chat-server${executableSuffix}`
      )
    }
  }
];

for (const [index, step] of steps.entries()) {
  console.log(`\n[mail server ${index + 1}/${steps.length}] ${step.name}`);
  const result = spawnSync(step.command, step.args, {
    cwd: projectRoot,
    env: { ...process.env, ...step.env },
    stdio: "inherit"
  });

  if (result.error) {
    console.error(`Unable to start ${step.name}: ${result.error.message}`);
    process.exit(result.status ?? 1);
  }
  if (result.status !== 0) {
    console.error(`${step.name} failed with exit status ${result.status ?? 1}.`);
    process.exit(result.status ?? 1);
  }
}

console.log("\nAll mail server self-tests passed.");
