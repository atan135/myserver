import assert from "node:assert/strict";
import test from "node:test";

import { createShutdownHandler } from "./main-shutdown.js";

test("shutdown closes myforge sockets before HTTP and executes only once across concurrent signals", async () => {
  const events = [];
  let releaseGateway;
  const gatewayGate = new Promise((resolve) => { releaseGateway = resolve; });
  const shutdown = createShutdownHandler({
    shutdownGateway: async () => {
      events.push("gateway:start");
      await gatewayGate;
      events.push("gateway:end");
    },
    closeHttp: async () => { events.push("http"); },
    closeApplication: async () => { events.push("application"); },
    exit: (code) => { events.push(`exit:${code}`); },
    info: (message) => { events.push(`info:${message}`); },
    error: () => { events.push("error"); }
  });

  const first = shutdown("SIGINT");
  const second = shutdown("SIGTERM");
  assert.equal(first, second);
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(events, ["info:Shutdown signal: SIGINT", "gateway:start"]);

  releaseGateway();
  await Promise.all([first, second]);
  assert.deepEqual(events, [
    "info:Shutdown signal: SIGINT",
    "gateway:start",
    "gateway:end",
    "http",
    "application",
    "info:Shutdown complete",
    "exit:0"
  ]);
});
