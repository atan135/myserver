import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import test from "node:test";

import {
  runRequestServerShutdown,
  waitForProcessExit,
  waitForShutdownProcessExit
} from "../../tools/mock-client/src/scenarios/room.js";

test("waitForShutdownProcessExit skips when pid is not provided", async () => {
  const result = await waitForShutdownProcessExit(
    { shutdownWaitPid: 0, shutdownWaitTimeoutMs: 100 },
    { ok: true }
  );

  assert.deepEqual(result, {
    requested: false,
    ok: true,
    skipped: true,
    reason: "no shutdownWaitPid"
  });
});

test("waitForShutdownProcessExit refuses to wait when shutdown gate failed", async () => {
  const result = await waitForShutdownProcessExit(
    { shutdownWaitPid: process.pid, shutdownWaitTimeoutMs: 100 },
    { ok: false, errorCode: "SHUTDOWN_OWNED_ROOMS_REMAIN" }
  );

  assert.equal(result.requested, true);
  assert.equal(result.ok, false);
  assert.equal(result.skipped, true);
  assert.equal(result.pid, process.pid);
  assert.equal(result.errorCode, "SHUTDOWN_REQUEST_NOT_OK");
});

test("runRequestServerShutdown fails when shutdown safety gate returns ok=false without pid", async () => {
  const originalFetch = globalThis.fetch;
  const originalLog = console.log;
  globalThis.fetch = async () => new Response(
    JSON.stringify({ ok: false, errorCode: "SHUTDOWN_OWNED_ROOMS_REMAIN" }),
    { status: 200, headers: { "content-type": "application/json" } }
  );
  console.log = () => {};

  try {
    await assert.rejects(
      () => runRequestServerShutdown({
        httpBaseUrl: "http://127.0.0.1:1",
        shutdownReason: "test",
        shutdownWaitPid: 0,
        shutdownWaitTimeoutMs: 100,
        serviceToken: ""
      }),
      /SHUTDOWN_OWNED_ROOMS_REMAIN/
    );
  } finally {
    globalThis.fetch = originalFetch;
    console.log = originalLog;
  }
});

test("runRequestServerShutdown returns machine-readable failure envelope for json output", async () => {
  const originalFetch = globalThis.fetch;
  const originalLog = console.log;
  const logs = [];
  globalThis.fetch = async () => new Response(
    JSON.stringify({ ok: false, errorCode: "SHUTDOWN_CONNECTIONS_REMAIN" }),
    { status: 200, headers: { "content-type": "application/json" } }
  );
  console.log = (value) => logs.push(value);

  try {
    const result = await runRequestServerShutdown({
      httpBaseUrl: "http://127.0.0.1:1",
      shutdownReason: "test",
      shutdownWaitPid: 0,
      shutdownWaitTimeoutMs: 100,
      serviceToken: "",
      jsonOutput: true
    });

    assert.equal(result.ok, false);
    assert.equal(result.shutdown.errorCode, "SHUTDOWN_CONNECTIONS_REMAIN");
    assert.equal(result.processExit.errorCode, "SHUTDOWN_CONNECTIONS_REMAIN");
    assert.equal(logs.length, 1);
    assert.equal(JSON.parse(logs[0]).ok, false);
  } finally {
    globalThis.fetch = originalFetch;
    console.log = originalLog;
  }
});

test("waitForProcessExit observes a local process exiting", async () => {
  const child = spawn(process.execPath, ["-e", "setTimeout(() => process.exit(0), 100)"], {
    stdio: "ignore",
    windowsHide: true
  });

  try {
    const result = await waitForProcessExit(child.pid, 5000);

    assert.equal(result.ok, true);
    assert.equal(result.pid, child.pid);
    assert.equal(result.exited, true);
    assert.equal(result.errorCode, "");
  } finally {
    if (!child.killed) {
      child.kill();
    }
  }
});

test("waitForProcessExit times out for a still-running process", async () => {
  const result = await waitForProcessExit(process.pid, 100);

  assert.equal(result.ok, false);
  assert.equal(result.pid, process.pid);
  assert.equal(result.exited, false);
  assert.equal(result.errorCode, "PROCESS_EXIT_TIMEOUT");
});
