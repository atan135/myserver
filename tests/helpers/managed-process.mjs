import net from "node:net";
import path from "node:path";
import { spawn } from "node:child_process";

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

export function resolveProjectExecutable(projectRoot, override, fallback) {
  if (!override) return fallback;
  return path.isAbsolute(override) ? path.normalize(override) : path.resolve(projectRoot, override);
}

export function spawnManaged(name, command, args, { cwd, env = {} } = {}) {
  const stdout = [];
  const stderr = [];
  const processRef = { name, child: null, stdout, stderr, spawnError: null };
  const child = spawn(command, args, {
    cwd,
    env: { ...process.env, ...env },
    stdio: ["ignore", "pipe", "pipe"]
  });
  processRef.child = child;
  child.once("error", (error) => {
    processRef.spawnError = error;
  });
  const append = (target, chunk) => {
    target.push(chunk.toString());
    while (target.join("").length > 100_000) target.shift();
  };
  child.stdout.on("data", (chunk) => append(stdout, chunk));
  child.stderr.on("data", (chunk) => append(stderr, chunk));
  return processRef;
}

export async function waitForManagedPort(port, {
  host = "127.0.0.1",
  processRef,
  timeoutMs = 60_000,
  intervalMs = 100
} = {}) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (processRef?.spawnError) {
      throw new Error(`${processRef.name} failed to spawn: ${processRef.spawnError.message}`, {
        cause: processRef.spawnError
      });
    }
    if (processRef?.child.exitCode !== null || processRef?.child.signalCode !== null) {
      throw new Error(
        `${processRef.name} exited with ${processRef.child.exitCode ?? processRef.child.signalCode}: ` +
        processRef.stderr.join("").slice(-2000)
      );
    }

    const connected = await new Promise((resolve) => {
      const socket = net.createConnection({ host, port });
      socket.once("connect", () => {
        socket.destroy();
        resolve(true);
      });
      socket.once("error", () => resolve(false));
    });
    if (connected) return;
    await delay(intervalMs);
  }
  throw new Error(`timed out waiting for ${host}:${port}`);
}
