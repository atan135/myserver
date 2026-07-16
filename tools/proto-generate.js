import { spawnSync } from "node:child_process";
import { mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const generatedRustFilePattern = /^myserver\.[A-Za-z0-9_]+\.rs$/;

export const RUST_PROTO_TOOLCHAIN = Object.freeze({
  protocBinVendored: "3.2.0",
  prostBuild: "0.13.5",
  tonicBuild: "0.12.3"
});

export const RUST_PROTO_TARGETS = Object.freeze([
  {
    name: "game-server",
    manifest: "apps/game-server/Cargo.toml",
    output: "apps/game-server/src/proto",
    inputs: ["packages/proto/game.proto", "packages/proto/admin.proto", "packages/proto/match.proto"]
  },
  {
    name: "game-proxy",
    manifest: "apps/game-proxy/Cargo.toml",
    output: "apps/game-proxy/src/proto",
    inputs: ["packages/proto/game.proto"]
  },
  {
    name: "chat-server",
    manifest: "apps/chat-server/Cargo.toml",
    output: "apps/chat-server/src/proto",
    inputs: ["packages/proto/chat.proto"]
  },
  {
    name: "match-service",
    manifest: "apps/match-service/Cargo.toml",
    output: "apps/match-service/src/proto",
    inputs: ["packages/proto/game.proto", "packages/proto/match.proto"]
  }
]);

export function parseMode(args) {
  if (args.length !== 1 || !["--check", "--write"].includes(args[0])) {
    throw new Error("Usage: node tools/proto-generate.js --check|--write");
  }
  return args[0];
}

function relativeDisplay(filePath) {
  return path.relative(rootDir, filePath).split(path.sep).join("/");
}

export function listGeneratedRustFiles(directory) {
  try {
    return readdirSync(directory, { withFileTypes: true })
      .filter((entry) => entry.isFile() && generatedRustFilePattern.test(entry.name))
      .map((entry) => entry.name)
      .sort();
  } catch (error) {
    if (error.code === "ENOENT") {
      return [];
    }
    throw error;
  }
}

export function compareGeneratedRustDirectories(expectedDirectory, actualDirectory) {
  const expectedFiles = listGeneratedRustFiles(expectedDirectory);
  const actualFiles = listGeneratedRustFiles(actualDirectory);
  const fileNames = [...new Set([...expectedFiles, ...actualFiles])].sort();
  const differences = [];

  for (const fileName of fileNames) {
    const expectedPath = path.join(expectedDirectory, fileName);
    const actualPath = path.join(actualDirectory, fileName);
    const expectedExists = expectedFiles.includes(fileName);
    const actualExists = actualFiles.includes(fileName);
    if (!actualExists) {
      differences.push(`${relativeDisplay(actualPath)} is missing`);
      continue;
    }
    if (!expectedExists) {
      differences.push(`${relativeDisplay(actualPath)} is stale and is no longer generated`);
      continue;
    }
    if (!readFileSync(expectedPath).equals(readFileSync(actualPath))) {
      differences.push(`${relativeDisplay(actualPath)} differs from deterministic generated output`);
    }
  }

  return differences;
}

export function formatGeneratorFailure(target, detail) {
  return [
    `Protocol generation failed for ${target.name}.`,
    `Inputs: ${target.inputs.join(", ")}.`,
    `Output target: ${target.output}.`,
    `Required locked tools: cargo, protoc-bin-vendored ${RUST_PROTO_TOOLCHAIN.protocBinVendored}, prost-build ${RUST_PROTO_TOOLCHAIN.prostBuild}, tonic-build ${RUST_PROTO_TOOLCHAIN.tonicBuild}.`,
    detail
  ].join(" ");
}

function runRustGenerator(target, outputDirectory) {
  console.log(`Generating ${target.name}: ${target.inputs.join(", ")} -> ${relativeDisplay(outputDirectory)}`);
  console.log(
    `  Rust toolchain: protoc-bin-vendored ${RUST_PROTO_TOOLCHAIN.protocBinVendored}; prost-build ${RUST_PROTO_TOOLCHAIN.prostBuild}; tonic-build ${RUST_PROTO_TOOLCHAIN.tonicBuild}.`
  );

  const cargo = process.platform === "win32" ? "cargo.exe" : "cargo";
  const result = spawnSync(cargo, ["check", "--manifest-path", target.manifest, "--locked"], {
    cwd: rootDir,
    env: {
      ...process.env,
      MYSERVER_PROTO_OUT_DIR: outputDirectory,
      MYSERVER_PROTOCOL_ONLY: "1"
    },
    encoding: "utf8"
  });

  if (result.error) {
    throw new Error(
      formatGeneratorFailure(
        target,
        `Unable to start ${cargo}: ${result.error.message}. Install the Rust toolchain with cargo available on PATH.`
      )
    );
  }
  if (result.status !== 0) {
    const detail = [result.stdout, result.stderr].filter(Boolean).join("\n").trim();
    throw new Error(formatGeneratorFailure(target, detail || `${cargo} exited with status ${result.status}.`));
  }
}

function runHandwrittenNodeCheck() {
  console.log("Checking Node.js/mock-client hand-written protobuf codecs; no Node.js generated source is produced.");
  const result = spawnSync(process.execPath, ["tools/check-mock-client-protocol.js"], {
    cwd: rootDir,
    encoding: "utf8"
  });
  if (result.error) {
    throw new Error(
      `Node.js/mock-client protocol check could not start ${process.execPath}: ${result.error.message}. Target files: tools/mock-client/src/messages.js and tools/mock-client/src/constants.js.`
    );
  }
  if (result.status !== 0) {
    const detail = [result.stdout, result.stderr].filter(Boolean).join("\n").trim();
    throw new Error(
      `Node.js/mock-client hand-written protobuf check failed for tools/mock-client/src/messages.js and tools/mock-client/src/constants.js. ${detail}`
    );
  }
  process.stdout.write(result.stdout);
  process.stderr.write(result.stderr);
}

function writeGeneratedRust() {
  for (const target of RUST_PROTO_TARGETS) {
    runRustGenerator(target, path.join(rootDir, target.output));
  }
}

function checkGeneratedRust() {
  const temporaryRoot = mkdtempSync(path.join(os.tmpdir(), "myserver-proto-generation-"));
  try {
    const differences = [];
    for (const target of RUST_PROTO_TARGETS) {
      const temporaryOutput = path.join(temporaryRoot, target.name);
      runRustGenerator(target, temporaryOutput);
      differences.push(
        ...compareGeneratedRustDirectories(temporaryOutput, path.join(rootDir, target.output))
      );
    }
    if (differences.length > 0) {
      throw new Error(
        `Protocol generated-code drift detected:\n${differences.map((difference) => `- ${difference}`).join("\n")}\nRun npm run generate:proto, review the generated Rust diff, then rerun npm run check:generated-proto.`
      );
    }
  } finally {
    rmSync(temporaryRoot, { force: true, recursive: true });
  }
}

export function main(args = process.argv.slice(2)) {
  const mode = parseMode(args);
  if (mode === "--write") {
    writeGeneratedRust();
  } else {
    checkGeneratedRust();
  }
  runHandwrittenNodeCheck();
  console.log(`Protocol ${mode === "--write" ? "generation" : "generated-code drift check"} passed.`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  try {
    main();
  } catch (error) {
    console.error(error.message);
    process.exit(1);
  }
}
