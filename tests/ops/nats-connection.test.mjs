import assert from "node:assert/strict";
import test from "node:test";

import { natsConnectOptions } from "../../packages/nats-client/node.js";

test("NATS token URL becomes a token connection option", () => {
  assert.deepEqual(
    natsConnectOptions("nats://secret-token@nats:4222", "metrics-collector"),
    {
      servers: "nats://nats:4222",
      name: "metrics-collector",
      token: "secret-token"
    }
  );
});

test("NATS token URL decodes URL userinfo", () => {
  assert.deepEqual(
    natsConnectOptions("nats://token%2Fwith%2Bchars@nats:4222", "test"),
    {
      servers: "nats://nats:4222",
      name: "test",
      token: "token/with+chars"
    }
  );
});

test("plain and username/password URLs retain native client behavior", () => {
  assert.deepEqual(natsConnectOptions("nats://127.0.0.1:4222", "test"), {
    servers: "nats://127.0.0.1:4222",
    name: "test"
  });
  assert.deepEqual(natsConnectOptions("nats://user:password@127.0.0.1:4222", "test"), {
    servers: "nats://user:password@127.0.0.1:4222",
    name: "test"
  });
});
