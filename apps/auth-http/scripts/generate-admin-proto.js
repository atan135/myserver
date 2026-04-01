import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawn } from "node:child_process";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const appRoot = path.resolve(__dirname, "..");
const repoRoot = path.resolve(appRoot, "..", "..");
const protoDir = path.join(repoRoot, "packages", "proto");
const protoFile = path.join(protoDir, "admin.proto");
const outputDir = path.join(appRoot, "src", "generated");
const outputJs = path.join(outputDir, "admin_pb.js");
const outputCjs = path.join(outputDir, "admin_pb.cjs");

function runProtoc(args) {
  return new Promise((resolve, reject) => {
    const child = spawn("protoc", args, {
      cwd: repoRoot,
      stdio: ["ignore", "pipe", "pipe"]
    });

    let stdout = "";
    let stderr = "";

    child.stdout.on("data", (chunk) => {
      stdout += chunk.toString();
    });

    child.stderr.on("data", (chunk) => {
      stderr += chunk.toString();
    });

    child.once("error", reject);
    child.once("exit", (code) => {
      if (code === 0) {
        resolve({ stdout, stderr });
        return;
      }

      reject(
        new Error(
          `protoc exited with code ${code}\n[stdout]\n${stdout}\n[stderr]\n${stderr}`
        )
      );
    });
  });
}

async function main() {
  await fs.mkdir(outputDir, { recursive: true });

  await runProtoc([
    `--js_out=import_style=commonjs,binary:${outputDir}`,
    `--proto_path=${protoDir}`,
    protoFile
  ]);

  await fs.rm(outputCjs, { force: true });
  await fs.rename(outputJs, outputCjs);

  console.log(`generated ${path.relative(repoRoot, outputCjs)}`);
}

main().catch((error) => {
  console.error(error.message);
  process.exitCode = 1;
});
