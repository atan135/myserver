import assert from "node:assert/strict";
import crypto from "node:crypto";
import { EventEmitter } from "node:events";
import test from "node:test";
import Fastify from "fastify";

import { MyforgeConnection } from "./myforge-connection.js";
import { MYFORGE_SUBPROTOCOL, parseStrictJson, serializeMessage, signMessage, verifyMessageSignature } from "./protocol.js";
import { validateMessageSchema } from "./schemas.js";
import {
  MyforgeWebsocketGateway,
  negotiateMyforgeLimits,
  registerMyforgeWebsocket
} from "./myforge-websocket.js";

const NOW = 1783694400000;
const AGENT_ID = "dev-pc-001";
const PROJECT_ID = "myforge-local";
const REQUEST_ID = "2d0465b1-dc92-46d2-bc45-c90ed9724f5a";

function nonce(value) {
  return Buffer.alloc(16, value).toString("base64url");
}

function generateKeys() {
  return crypto.generateKeyPairSync("ed25519");
}

function fingerprint(publicKey) {
  return crypto.createHash("sha256")
    .update(publicKey.export({ format: "der", type: "spki" }))
    .digest("hex");
}

function createConfig(serverKeys, agentKeys, enabled = true) {
  const configured = {
    agentId: AGENT_ID,
    projectId: PROJECT_ID,
    label: "Development agent",
    publicKey: agentKeys.publicKey,
    publicKeyFingerprint: fingerprint(agentKeys.publicKey)
  };
  return {
    enabled,
    authTtlMs: 60000,
    commandTtlMs: 60000,
    clockSkewMs: 5000,
    heartbeatIntervalMs: 15000,
    heartbeatTimeoutMs: 45000,
    queueTtlMs: 900000,
    commandTimeoutMs: 120000,
    cancelTimeoutMs: 10000,
    maxOutputBytes: 1048576,
    wsMaxMessageBytes: 16777216,
    wsWriteTimeoutMs: 5000,
    serverPrivateKey: enabled ? serverKeys.privateKey : null,
    serverPublicKey: enabled ? serverKeys.publicKey : null,
    agents: enabled ? [configured] : [],
    agentsById: new Map(enabled ? [[AGENT_ID, configured]] : [])
  };
}

class FakeSocket extends EventEmitter {
  constructor() {
    super();
    this.protocol = MYFORGE_SUBPROTOCOL;
    this.sent = [];
    this.pendingWrites = [];
    this.blockWrites = false;
    this.failNextWrite = false;
    this.closed = false;
    this.closeCode = null;
    this.closeReason = null;
  }

  send(frame, callback) {
    this.sent.push(frame);
    if (this.failNextWrite) {
      this.failNextWrite = false;
      queueMicrotask(() => callback(new Error("fake writer failed")));
    } else if (this.blockWrites) {
      this.pendingWrites.push(callback);
    } else {
      queueMicrotask(() => callback());
    }
  }

  releaseWrite(error = null) {
    const callback = this.pendingWrites.shift();
    assert.ok(callback, "a pending write is available");
    callback(error);
  }

  close(code, reason) {
    if (this.closed) return;
    this.closed = true;
    this.closeCode = code;
    this.closeReason = reason;
    queueMicrotask(() => this.emit("close", code, Buffer.from(reason)));
  }

  terminate() {
    this.close(1006, "terminated");
  }

  receive(message) {
    const frame = typeof message === "string" ? message : serializeMessage(message);
    this.emit("message", Buffer.from(frame, "utf8"), false);
  }
}

class FakeStore {
  constructor() {
    this.agentConnectionId = null;
    this.registerCalls = [];
    this.heartbeatCalls = [];
    this.offlineCalls = [];
    this.tasks = new Map();
    this.startedCalls = 0;
    this.resultCalls = 0;
    this.errorCalls = 0;
    this.startedGate = null;
    this.startedEntered = false;
  }

  async registerAgent(value) {
    const replacedConnectionId = this.agentConnectionId && this.agentConnectionId !== value.connectionId
      ? this.agentConnectionId
      : null;
    this.agentConnectionId = value.connectionId;
    this.registerCalls.push(value);
    return { agent: { status: "online" }, replacedConnectionId };
  }

  async heartbeatAgent(value) {
    this.heartbeatCalls.push(value);
    return { agent: this.agentConnectionId === value.connectionId ? { status: "online" } : null, staleConnection: this.agentConnectionId !== value.connectionId };
  }

  async markAgentOffline(value) {
    this.offlineCalls.push(value);
    const staleConnection = this.agentConnectionId !== value.connectionId;
    if (!staleConnection) this.agentConnectionId = null;
    for (const task of this.tasks.values()) {
      if (task.connectionId === value.connectionId && new Set(["dispatched", "running"]).has(task.status)) {
        task.status = "failed";
        task.errorCode = task.cancelRequestedAt ? "MYFORGE_CANCEL_UNCONFIRMED" : "MYFORGE_AGENT_DISCONNECTED";
      }
    }
    return { staleConnection, failedTasks: [] };
  }

  async getTask(requestId) {
    return this.tasks.get(requestId) ?? null;
  }

  assertIdentity(task, value) {
    if (task.agentId !== value.agentId || task.projectId !== value.projectId ||
        task.connectionId !== value.connectionId || task.executionMode !== value.executionMode) {
      const error = new Error("identity mismatch");
      error.code = "MYFORGE_IDENTITY_MISMATCH";
      throw error;
    }
  }

  async markTaskStarted(value) {
    this.startedCalls += 1;
    this.startedEntered = true;
    if (this.startedGate) await this.startedGate;
    const task = this.tasks.get(value.requestId);
    this.assertIdentity(task, value);
    if (task.status === "running" && new Date(task.startedAt).getTime() === new Date(value.startedAt).getTime()) {
      return { outcome: "duplicate", task };
    }
    if (task.status !== "dispatched") {
      const error = new Error("invalid transition");
      error.code = "MYFORGE_PROTOCOL_STATE_INVALID";
      throw error;
    }
    task.status = "running";
    task.startedAt = value.startedAt.toISOString();
    return { outcome: "updated", task };
  }

  async recordTaskResult(value) {
    this.resultCalls += 1;
    const task = this.tasks.get(value.requestId);
    this.assertIdentity(task, value);
    if (new Set(["completed", "completed_with_errors", "failed", "cancelled"]).has(task.status)) {
      if (task.resultDigest === value.resultDigest) return { outcome: "duplicate", task };
      const error = new Error("duplicate result conflict");
      error.code = "MYFORGE_DUPLICATE_RESULT_CONFLICT";
      throw error;
    }
    if (task.status !== "running" && !(task.status === "dispatched" && value.status === "cancelled")) {
      const error = new Error("result requires running task");
      error.code = "MYFORGE_PROTOCOL_STATE_INVALID";
      throw error;
    }
    task.status = value.status;
    task.resultDigest = value.resultDigest;
    task.completedAt = value.completedAt.toISOString();
    return { outcome: "updated", task };
  }

  async recordTaskError(value) {
    this.errorCalls += 1;
    const task = this.tasks.get(value.requestId);
    this.assertIdentity(task, value);
    if (task.status !== "dispatched") {
      const error = new Error("command.error requires dispatched");
      error.code = "MYFORGE_PROTOCOL_STATE_INVALID";
      throw error;
    }
    task.status = "failed";
    task.errorCode = value.errorCode;
    return { outcome: "updated", task };
  }
}

class FakeAdminStore {
  constructor() {
    this.events = [];
  }

  async appendSecurityAuditLog(event) {
    this.events.push(event);
  }
}

async function waitFor(predicate, message = "condition", attempts = 200) {
  for (let index = 0; index < attempts; index += 1) {
    if (predicate()) return;
    await new Promise((resolve) => setImmediate(resolve));
  }
  assert.fail(`timed out waiting for ${message}`);
}

function createHarness({ enabled = true, clock = () => NOW, replayCacheMaxEntries = 65536 } = {}) {
  const serverKeys = generateKeys();
  const agentKeys = generateKeys();
  const config = createConfig(serverKeys, agentKeys, enabled);
  const store = new FakeStore();
  const adminStore = new FakeAdminStore();
  const gateway = new MyforgeWebsocketGateway({ config, store, adminStore, clock, replayCacheMaxEntries });
  return { serverKeys, agentKeys, config, store, adminStore, gateway };
}

function agentEnvelope(type, fields, privateKey, nonceValue, timestampMs = NOW) {
  return signMessage({
    protocolVersion: 1,
    type,
    ...fields,
    timestampMs,
    expiresAtMs: timestampMs + 60000,
    nonce: nonce(nonceValue)
  }, privateKey);
}

function requestFor(socket) {
  return {
    query: { agentId: AGENT_ID, projectId: PROJECT_ID },
    headers: { upgrade: "websocket", "sec-websocket-protocol": MYFORGE_SUBPROTOCOL },
    ip: "127.0.0.1",
    socket
  };
}

async function handshake(harness, socket = new FakeSocket(), registerOverrides = {}) {
  const connection = harness.gateway.acceptSocket(socket, requestFor(socket));
  await waitFor(() => socket.sent.length === 1, "server challenge");
  const challenge = parseStrictJson(socket.sent[0]);
  validateMessageSchema(challenge, "server.challenge");
  verifyMessageSignature(challenge, harness.serverKeys.publicKey);

  const hello = agentEnvelope("agent.hello", {
    challengeId: challenge.challengeId,
    challenge: challenge.challenge,
    agentId: AGENT_ID,
    projectId: PROJECT_ID
  }, harness.agentKeys.privateKey, 1);
  socket.receive(hello);
  await waitFor(() => connection.state === "authenticated", "authenticated state");

  const registration = agentEnvelope("agent.register", {
    connectionId: challenge.challengeId,
    agentId: AGENT_ID,
    projectId: PROJECT_ID,
    hostname: "DESKTOP-TEST",
    platform: "windows",
    agentVersion: "0.1.0",
    forgeRootSummary: { name: "myforge", configured: true },
    capabilities: {
      profiles: ["codex_exec"],
      codexExec: true,
      fangyuanBlueprint: true,
      audit: "unavailable",
      dryRun: false,
      maxConcurrentTasks: 1
    },
    limits: {
      authTtlMs: 60000,
      commandTtlMs: 60000,
      clockSkewMs: 5000,
      heartbeatIntervalMs: 15000,
      maxCommandTimeoutMs: 120000,
      cancelTimeoutMs: 10000,
      maxOutputBytes: 1048576,
      wsMaxMessageBytes: 16777216
    },
    ...registerOverrides
  }, harness.agentKeys.privateKey, 2);
  socket.receive(registration);
  await waitFor(() => connection.state === "registered" || connection.closed, "registered state");
  return { socket, connection, challenge };
}

function startedMessage(connection, privateKey, nonceValue = 3) {
  return agentEnvelope("command.started", {
    connectionId: connection.connectionId,
    requestId: REQUEST_ID,
    agentId: AGENT_ID,
    projectId: PROJECT_ID,
    executionMode: "codex_exec",
    startedAtMs: NOW + 2000
  }, privateKey, nonceValue, NOW + 2000);
}

function resultMessage(connection, privateKey, nonceValue = 5, overrides = {}) {
  return agentEnvelope("command.result", {
    connectionId: connection.connectionId,
    requestId: REQUEST_ID,
    agentId: AGENT_ID,
    projectId: PROJECT_ID,
    executionMode: "codex_exec",
    status: "completed",
    exitCode: 0,
    stdoutPreview: "generated",
    stderrPreview: "",
    stdoutBytes: 9,
    stderrBytes: 0,
    stdoutTruncated: false,
    stderrTruncated: false,
    artifactFile: "artifacts/fangyuan/home.ron",
    consumerTargetFile: null,
    artifact: {
      exists: true,
      sha256: "a".repeat(64),
      bytes: 42,
      modifiedAtMs: NOW + 3000
    },
    audit: {
      status: "passed",
      errors: 0,
      warnings: 0,
      primitiveCount: 3,
      mainCode: null,
      reasonCode: null,
      findingsPreview: []
    },
    errorCode: null,
    errorMessage: null,
    startedAtMs: NOW + 2000,
    completedAtMs: NOW + 3000,
    ...overrides
  }, privateKey, nonceValue, NOW + 3000);
}

function cancelledResultMessage(connection, privateKey, nonceValue, completedAtMs, overrides = {}) {
  return resultMessage(connection, privateKey, nonceValue, {
    status: "cancelled",
    exitCode: null,
    stdoutPreview: "",
    stdoutBytes: 0,
    artifact: { exists: false, sha256: null, bytes: null, modifiedAtMs: null },
    audit: {
      status: "skipped",
      errors: null,
      warnings: null,
      primitiveCount: null,
      mainCode: null,
      reasonCode: "cancelled",
      findingsPreview: []
    },
    errorCode: "MYFORGE_COMMAND_CANCELLED",
    errorMessage: "cancelled before start",
    startedAtMs: null,
    completedAtMs,
    ...overrides
  });
}

function commandInput() {
  return {
    artifactFile: "artifacts/fangyuan/home.ron",
    consumerTargetFile: null,
    rulesFile: "rules/fangyuan/rules.md",
    prompt: {
      theme: "fire cave",
      primitiveLimit: 20,
      bounds: { width: 40, depth: 40, height: 20 },
      requirements: ["central furnace"]
    },
    renderedPrompt: "Generate the constrained blueprint."
  };
}

function installDispatchedTask(store, connection) {
  store.tasks.set(REQUEST_ID, {
    requestId: REQUEST_ID,
    agentId: AGENT_ID,
    projectId: PROJECT_ID,
    connectionId: connection.connectionId,
    executionMode: "codex_exec",
    status: "dispatched",
    dispatchedAt: new Date(NOW + 1000).toISOString(),
    commandExpiresAt: new Date(NOW + 61000).toISOString(),
    startedAt: null,
    cancelRequestedAt: null,
    cancelDeadlineAt: null,
    artifactFile: "artifacts/fangyuan/home.ron",
    consumerTargetFile: null
  });
}

test("challenge, hello, register, negotiated limits, heartbeat, and disconnect lifecycle", async (t) => {
  const harness = createHarness();
  t.after(() => harness.gateway.shutdown());
  const { socket, connection, challenge } = await handshake(harness);

  assert.equal(challenge.limits.authTtlMs, 60000);
  assert.equal(connection.effectiveLimits.commandTimeoutMs, 120000);
  assert.equal(connection.effectiveLimits.maxOutputBytes, 1048576);
  assert.equal(harness.store.registerCalls[0].connectionId, connection.connectionId);

  socket.receive(agentEnvelope("agent.heartbeat", {
    connectionId: connection.connectionId,
    agentId: AGENT_ID,
    projectId: PROJECT_ID,
    sequence: 1,
    state: "idle",
    activeRequestId: null
  }, harness.agentKeys.privateKey, 3));
  await waitFor(() => harness.store.heartbeatCalls.length === 1, "heartbeat persistence");

  await harness.gateway.handleConnectionDeadline(connection, "heartbeat_timeout");
  assert.equal(socket.closeReason, "heartbeat_timeout");
  assert.equal(harness.store.offlineCalls.length, 1);
  assert.equal(harness.store.offlineCalls[0].connectionId, connection.connectionId);
});

test("limit negotiation rejects incompatible heartbeat and derives frame-constrained output budget", () => {
  const server = {
    authTtlMs: 60000,
    commandTtlMs: 60000,
    clockSkewMs: 5000,
    heartbeatIntervalMs: 15000,
    heartbeatTimeoutMs: 45000,
    commandTimeoutMs: 120000,
    cancelTimeoutMs: 10000,
    maxOutputBytes: 4194304,
    wsMaxMessageBytes: 524288
  };
  const agent = {
    authTtlMs: 60000,
    commandTtlMs: 60000,
    clockSkewMs: 5000,
    heartbeatIntervalMs: 15000,
    maxCommandTimeoutMs: 60000,
    cancelTimeoutMs: 5000,
    maxOutputBytes: 4194304,
    wsMaxMessageBytes: 524288
  };
  const effective = negotiateMyforgeLimits(server, agent);
  assert.equal(effective.commandTimeoutMs, 60000);
  assert.equal(effective.cancelTimeoutMs, 5000);
  assert.equal(effective.maxOutputBytes, Math.floor((524288 - 262144) / 12));
  assert.ok(12 * effective.maxOutputBytes + 262144 <= effective.wsMaxMessageBytes);
  assert.ok(12 * (effective.maxOutputBytes + 1) + 262144 > effective.wsMaxMessageBytes);
  assert.throws(() => negotiateMyforgeLimits(server, { ...agent, heartbeatIntervalMs: 10000 }), {
    code: "MYFORGE_LIMIT_MISMATCH"
  });
});

test("invalid signature, expired message, and replay are fatal and audited", async (t) => {
  await t.test("invalid signature", async () => {
    const harness = createHarness();
    const socket = new FakeSocket();
    const connection = harness.gateway.acceptSocket(socket, requestFor(socket));
    await waitFor(() => socket.sent.length === 1, "challenge");
    const challenge = parseStrictJson(socket.sent[0]);
    const attacker = generateKeys();
    socket.receive(agentEnvelope("agent.hello", {
      challengeId: challenge.challengeId,
      challenge: challenge.challenge,
      agentId: AGENT_ID,
      projectId: PROJECT_ID
    }, attacker.privateKey, 1));
    await waitFor(() => connection.closed, "invalid signature close");
    assert.ok(harness.adminStore.events.some((event) => event.eventType === "MYFORGE_AGENT_SIGNATURE_INVALID"));
  });

  await t.test("expired hello", async () => {
    const harness = createHarness();
    const socket = new FakeSocket();
    const connection = harness.gateway.acceptSocket(socket, requestFor(socket));
    await waitFor(() => socket.sent.length === 1, "challenge");
    const challenge = parseStrictJson(socket.sent[0]);
    const timestamp = NOW - 65001;
    socket.receive(agentEnvelope("agent.hello", {
      challengeId: challenge.challengeId,
      challenge: challenge.challenge,
      agentId: AGENT_ID,
      projectId: PROJECT_ID
    }, harness.agentKeys.privateKey, 1, timestamp));
    await waitFor(() => connection.closed, "expired close");
    assert.ok(harness.adminStore.events.some((event) => event.eventType === "MYFORGE_MESSAGE_EXPIRED"));
  });

  await t.test("replayed heartbeat", async () => {
    const harness = createHarness();
    const { socket, connection } = await handshake(harness);
    const heartbeat = agentEnvelope("agent.heartbeat", {
      connectionId: connection.connectionId,
      agentId: AGENT_ID,
      projectId: PROJECT_ID,
      sequence: 1,
      state: "idle",
      activeRequestId: null
    }, harness.agentKeys.privateKey, 3);
    socket.receive(heartbeat);
    await waitFor(() => harness.store.heartbeatCalls.length === 1, "first heartbeat");
    socket.receive(heartbeat);
    await waitFor(() => connection.closed, "replay close");
    assert.ok(harness.adminStore.events.some((event) => event.eventType === "MYFORGE_REPLAY_DETECTED"));
  });
});

test("gateway accepts only canonical JCS text frames after signature verification", async (t) => {
  async function runCase(encode, accepted) {
    const harness = createHarness();
    const socket = new FakeSocket();
    const connection = harness.gateway.acceptSocket(socket, requestFor(socket));
    await waitFor(() => socket.sent.length === 1, "challenge for canonical frame test");
    const challenge = parseStrictJson(socket.sent[0]);
    const hello = agentEnvelope("agent.hello", {
      challengeId: challenge.challengeId,
      challenge: challenge.challenge,
      agentId: AGENT_ID,
      projectId: PROJECT_ID
    }, harness.agentKeys.privateKey, 1);
    socket.receive(encode(hello));
    if (accepted) {
      await waitFor(() => connection.state === "authenticated", "canonical hello accepted");
      assert.equal(harness.gateway.replayCache.size, 1);
      await harness.gateway.shutdown();
    } else {
      await waitFor(() => connection.closed, "non-canonical hello rejected");
      assert.equal(harness.gateway.replayCache.size, 0);
      assert.ok(harness.adminStore.events.some((event) => event.eventType === "MYFORGE_MESSAGE_IJSON_INVALID"));
    }
  }

  await t.test("canonical", () => runCase((message) => serializeMessage(message), true));
  await t.test("pretty-printed", () => runCase((message) => JSON.stringify(message, null, 2), false));
  await t.test("leading and trailing whitespace", () => runCase((message) => ` ${serializeMessage(message)}\n`, false));
  await t.test("non-canonical field order", () => runCase(
    (message) => JSON.stringify(Object.fromEntries(Object.entries(message).reverse())),
    false
  ));
});

test("replay cache capacity exhaustion fails closed and audits the source connection", async () => {
  const harness = createHarness({ replayCacheMaxEntries: 3 });
  const { socket, connection } = await handshake(harness);
  socket.receive(agentEnvelope("agent.heartbeat", {
    connectionId: connection.connectionId,
    agentId: AGENT_ID,
    projectId: PROJECT_ID,
    sequence: 1,
    state: "idle",
    activeRequestId: null
  }, harness.agentKeys.privateKey, 3));
  await waitFor(() => harness.store.heartbeatCalls.length === 1, "heartbeat filling replay cache");

  socket.receive(agentEnvelope("agent.heartbeat", {
    connectionId: connection.connectionId,
    agentId: AGENT_ID,
    projectId: PROJECT_ID,
    sequence: 2,
    state: "idle",
    activeRequestId: null
  }, harness.agentKeys.privateKey, 4));
  await waitFor(() => connection.closed, "replay capacity close");
  assert.ok(harness.adminStore.events.some((event) => event.eventType === "MYFORGE_AGENT_BUSY"));
});

test("unknown agent upgrade is rejected and security-audited", async () => {
  const harness = createHarness();
  let statusCode = null;
  let body = null;
  const reply = {
    code(value) {
      statusCode = value;
      return this;
    },
    send(value) {
      body = value;
      return value;
    }
  };
  await harness.gateway.preValidateUpgrade({
    headers: { upgrade: "websocket", "sec-websocket-protocol": MYFORGE_SUBPROTOCOL },
    query: { agentId: "unknown-agent", projectId: PROJECT_ID },
    ip: "127.0.0.2"
  }, reply);
  assert.equal(statusCode, 404);
  assert.equal(body.error, "MYFORGE_AGENT_UNKNOWN");
  assert.equal(harness.adminStore.events.length, 1);
  assert.equal(harness.adminStore.events[0].eventType, "MYFORGE_AGENT_UNKNOWN");
  assert.equal(harness.adminStore.events[0].targetValue, "unknown-agent");
});

test("new registration replaces old socket without taking the new connection offline", async (t) => {
  const harness = createHarness();
  t.after(() => harness.gateway.shutdown());
  const first = await handshake(harness, new FakeSocket());
  const second = await handshake(harness, new FakeSocket());

  assert.equal(first.connection.closed, true);
  assert.equal(harness.gateway.getRegisteredConnection(AGENT_ID, PROJECT_ID), second.connection);
  assert.equal(harness.store.agentConnectionId, second.connection.connectionId);
  assert.ok(harness.store.offlineCalls.some((call) => call.connectionId === first.connection.connectionId));
});

test("inbound FIFO commits started before result and preserves idempotency/conflicts", async (t) => {
  const harness = createHarness();
  t.after(() => harness.gateway.shutdown());
  const { socket, connection } = await handshake(harness);
  installDispatchedTask(harness.store, connection);

  let releaseStarted;
  harness.store.startedGate = new Promise((resolve) => { releaseStarted = resolve; });
  socket.receive(startedMessage(connection, harness.agentKeys.privateKey, 3));
  socket.receive(startedMessage(connection, harness.agentKeys.privateKey, 4));
  const result = resultMessage(connection, harness.agentKeys.privateKey, 5);
  socket.receive(result);

  await waitFor(() => harness.store.startedEntered, "paused started transaction");
  assert.equal(harness.store.resultCalls, 0);
  releaseStarted();
  await waitFor(() => harness.store.tasks.get(REQUEST_ID).status === "completed", "result commit");
  assert.equal(harness.store.startedCalls, 2);
  assert.equal(harness.store.resultCalls, 1);

  socket.receive(resultMessage(connection, harness.agentKeys.privateKey, 6));
  await waitFor(() => harness.store.resultCalls === 2, "duplicate result");
  assert.equal(connection.closed, false);

  socket.receive(resultMessage(connection, harness.agentKeys.privateKey, 7, {
    stdoutPreview: "changed",
    stdoutBytes: 7
  }));
  await waitFor(() => connection.closed, "conflicting result close");
  assert.ok(harness.adminStore.events.some((event) => event.eventType === "MYFORGE_DUPLICATE_RESULT_CONFLICT"));
});

test("pre-start cancellation result enforces persisted lower and upper completion bounds", async (t) => {
  async function runCase(completedAtMs, receiveNowMs, accepted) {
    let nowMs = NOW;
    const harness = createHarness({ clock: () => nowMs });
    const { socket, connection } = await handshake(harness);
    installDispatchedTask(harness.store, connection);
    const task = harness.store.tasks.get(REQUEST_ID);
    task.cancelRequestedAt = new Date(NOW + 2000).toISOString();
    task.cancelDeadlineAt = new Date(NOW + 12000).toISOString();
    nowMs = receiveNowMs;
    socket.receive(cancelledResultMessage(connection, harness.agentKeys.privateKey, 8, completedAtMs));
    if (accepted) {
      await waitFor(() => harness.store.resultCalls === 1, "accepted pre-start cancellation");
      assert.equal(task.status, "cancelled");
      await harness.gateway.shutdown();
    } else {
      await waitFor(() => connection.closed, "rejected pre-start cancellation");
      assert.equal(harness.store.resultCalls, 0);
      assert.ok(harness.adminStore.events.some((event) => event.eventType === "MYFORGE_PROTOCOL_STATE_INVALID"));
    }
  }

  await t.test("accepts exact lower bound", () => runCase(NOW - 3000, NOW + 5000, true));
  await t.test("rejects before lower bound", () => runCase(NOW - 3001, NOW + 5000, false));
  await t.test("accepts exact cancel deadline plus skew", () => runCase(NOW + 17000, NOW + 12000, true));
  await t.test("rejects after cancel deadline plus skew", () => runCase(NOW + 17001, NOW + 12001, false));
});

test("started and cancellation accept both legal wire linearizations and reject terminal started", async (t) => {
  await t.test("cancel before started accepts nullable startedAt and rejects later started", async () => {
    const harness = createHarness();
    const { socket, connection } = await handshake(harness);
    installDispatchedTask(harness.store, connection);
    const task = harness.store.tasks.get(REQUEST_ID);
    task.cancelRequestedAt = new Date(NOW + 2000).toISOString();
    task.cancelDeadlineAt = new Date(NOW + 12000).toISOString();

    socket.receive(cancelledResultMessage(connection, harness.agentKeys.privateKey, 8, NOW + 3000));
    await waitFor(() => task.status === "cancelled", "pre-start cancelled result");
    assert.equal(task.startedAt, null);
    assert.equal(connection.closed, false);

    socket.receive(startedMessage(connection, harness.agentKeys.privateKey, 9));
    await waitFor(() => connection.closed, "terminal started rejection");
    assert.ok(harness.adminStore.events.some((event) => event.eventType === "MYFORGE_PROTOCOL_STATE_INVALID"));
  });

  await t.test("started after persisted cancel completes before non-null cancelled result", async () => {
    const harness = createHarness();
    t.after(() => harness.gateway.shutdown());
    const { socket, connection } = await handshake(harness);
    installDispatchedTask(harness.store, connection);
    const task = harness.store.tasks.get(REQUEST_ID);
    task.cancelRequestedAt = new Date(NOW + 2000).toISOString();
    task.cancelDeadlineAt = new Date(NOW + 12000).toISOString();

    socket.receive(startedMessage(connection, harness.agentKeys.privateKey, 8));
    await waitFor(() => task.status === "running", "started after cancel request");
    assert.equal(task.startedAt, new Date(NOW + 2000).toISOString());
    assert.equal(task.cancelRequestedAt, new Date(NOW + 2000).toISOString());

    socket.receive(cancelledResultMessage(connection, harness.agentKeys.privateKey, 9, NOW + 3000, {
      startedAtMs: NOW + 2000,
      errorMessage: "command was cancelled"
    }));
    await waitFor(() => task.status === "cancelled", "post-start cancelled result");
    assert.equal(connection.closed, false);
  });
});

test("terminal cancelled result remains semantically idempotent after its receive deadline", async (t) => {
  await t.test("same semantic retry is accepted and different semantic retry conflicts", async () => {
    let nowMs = NOW;
    const harness = createHarness({ clock: () => nowMs });
    const { socket, connection } = await handshake(harness);
    installDispatchedTask(harness.store, connection);
    const task = harness.store.tasks.get(REQUEST_ID);
    task.cancelRequestedAt = new Date(NOW + 2000).toISOString();
    task.cancelDeadlineAt = new Date(NOW + 12000).toISOString();

    nowMs = NOW + 12000;
    socket.receive(cancelledResultMessage(connection, harness.agentKeys.privateKey, 8, NOW + 12000));
    await waitFor(() => harness.store.resultCalls === 1, "first cancelled result");
    assert.equal(task.status, "cancelled");

    nowMs = NOW + 20000;
    socket.receive(cancelledResultMessage(connection, harness.agentKeys.privateKey, 9, NOW + 12000));
    await waitFor(() => harness.store.resultCalls === 2, "late semantic duplicate");
    assert.equal(connection.closed, false);

    socket.receive(cancelledResultMessage(connection, harness.agentKeys.privateKey, 10, NOW + 12000, {
      stdoutPreview: "changed",
      stdoutBytes: 7
    }));
    await waitFor(() => connection.closed, "late semantic conflict close");
    assert.equal(harness.store.resultCalls, 3);
    assert.ok(harness.adminStore.events.some((event) => event.eventType === "MYFORGE_DUPLICATE_RESULT_CONFLICT"));
  });

  await t.test("first cancelled result after receive deadline is rejected", async () => {
    let nowMs = NOW;
    const harness = createHarness({ clock: () => nowMs });
    const { socket, connection } = await handshake(harness);
    installDispatchedTask(harness.store, connection);
    const task = harness.store.tasks.get(REQUEST_ID);
    task.cancelRequestedAt = new Date(NOW + 2000).toISOString();
    task.cancelDeadlineAt = new Date(NOW + 12000).toISOString();
    nowMs = NOW + 17001;
    socket.receive(cancelledResultMessage(connection, harness.agentKeys.privateKey, 8, NOW + 12000));
    await waitFor(() => connection.closed, "late first cancellation close");
    assert.equal(harness.store.resultCalls, 0);
    assert.ok(harness.adminStore.events.some((event) => event.eventType === "MYFORGE_DUPLICATE_RESULT_CONFLICT"));
  });
});

test("signed command.error fails only the matching dispatched task", async (t) => {
  const harness = createHarness();
  t.after(() => harness.gateway.shutdown());
  const { socket, connection } = await handshake(harness);
  installDispatchedTask(harness.store, connection);

  socket.receive(agentEnvelope("command.error", {
    connectionId: connection.connectionId,
    requestId: REQUEST_ID,
    agentId: AGENT_ID,
    projectId: PROJECT_ID,
    errorCode: "MYFORGE_TARGET_PATH_INVALID",
    errorMessage: "artifactFile is outside the allowed path",
    retryable: false
  }, harness.agentKeys.privateKey, 3));

  await waitFor(() => harness.store.errorCalls === 1, "command.error transaction");
  assert.equal(harness.store.tasks.get(REQUEST_ID).status, "failed");
  assert.equal(harness.store.tasks.get(REQUEST_ID).errorCode, "MYFORGE_TARGET_PATH_INVALID");
});

test("gateway notifies orchestration after registration, task terminal state, and disconnect", async () => {
  const harness = createHarness();
  const calls = [];
  harness.gateway.setTaskOrchestrator({
    async onAgentRegistered(value) { calls.push({ type: "registered", value }); },
    async onTaskTerminal(value) { calls.push({ type: "terminal", value }); },
    async onAgentDisconnected(value) { calls.push({ type: "disconnected", value }); },
    stop() {}
  });
  const { socket, connection } = await handshake(harness);
  await waitFor(() => calls.some((entry) => entry.type === "registered"), "registration orchestration callback");
  installDispatchedTask(harness.store, connection);
  socket.receive(agentEnvelope("command.error", {
    connectionId: connection.connectionId,
    requestId: REQUEST_ID,
    agentId: AGENT_ID,
    projectId: PROJECT_ID,
    errorCode: "MYFORGE_TARGET_PATH_INVALID",
    errorMessage: "artifactFile is outside the allowed path",
    retryable: false
  }, harness.agentKeys.privateKey, 3));
  await waitFor(() => calls.some((entry) => entry.type === "terminal"), "terminal orchestration callback");
  await connection.close(1000, "test_complete");
  await waitFor(() => calls.some((entry) => entry.type === "disconnected"), "disconnect orchestration callback");

  assert.equal(calls.find((entry) => entry.type === "registered").value.connectionId, connection.connectionId);
  assert.equal(calls.find((entry) => entry.type === "terminal").value.status, "failed");
  assert.equal(calls.find((entry) => entry.type === "disconnected").value.agentId, AGENT_ID);
});

test("prepared execute and cancel expire while queued without reaching the wire", async (t) => {
  async function runCase(kind) {
    let nowMs = NOW;
    const harness = createHarness({ clock: () => nowMs });
    const { socket, connection } = await handshake(harness);
    connection.config.wsWriteTimeoutMs = 120000;
    socket.blockWrites = true;
    const blocker = connection.enqueueOutbound("blocker", { expiresAtMs: NOW + 120000 });
    await waitFor(() => socket.pendingWrites.length === 1, `${kind} blocker write`);

    const send = harness.gateway.withConnectionOperation({ agentId: AGENT_ID, projectId: PROJECT_ID }, async (operation) => {
      const prepared = kind === "execute"
        ? operation.prepareExecute({
          requestId: REQUEST_ID,
          taskType: "fangyuan.blueprint.generate",
          profile: "codex_exec",
          input: commandInput(),
          timeoutMs: operation.connection.effectiveLimits.commandTimeoutMs,
          maxOutputBytes: operation.connection.effectiveLimits.maxOutputBytes
        })
        : operation.prepareCancel({
          requestId: REQUEST_ID,
          cancelRequestedAtMs: NOW,
          cancelDeadlineAtMs: NOW + operation.connection.effectiveLimits.cancelTimeoutMs
        });
      await operation.send(prepared);
    });
    await waitFor(() => connection.outbound.length === 1, `${kind} queued behind blocker`);
    nowMs = kind === "execute" ? NOW + 60000 : NOW + 10000;
    socket.releaseWrite();
    await blocker;
    await assert.rejects(send, (error) => error.code === "MYFORGE_MESSAGE_EXPIRED");
    assert.equal(socket.sent.length, 2);
    assert.equal(socket.sent[1], "blocker");
    await waitFor(() => connection.closed, `${kind} expiry closes failed writer`);
  }

  await t.test("execute", () => runCase("execute"));
  await t.test("cancel", () => runCase("cancel"));
});

test("different socket dispatchers and writers make progress independently", async () => {
  const serverKeys = generateKeys();
  const agentKeys = generateKeys();
  const config = createConfig(serverKeys, agentKeys);
  let releaseFirstHandler;
  let releaseSecondHandler;
  const firstHandlerGate = new Promise((resolve) => { releaseFirstHandler = resolve; });
  const secondHandlerGate = new Promise((resolve) => { releaseSecondHandler = resolve; });
  const entered = [];
  const gateway = {
    async handleFrame(connection, frame) {
      const value = frame.toString();
      entered.push(value);
      await (value === "first-in" ? firstHandlerGate : secondHandlerGate);
    },
    async handleConnectionError(_connection, error) { throw error; },
    async handleConnectionDeadline() {},
    async onConnectionClosed() {}
  };
  const firstSocket = new FakeSocket();
  const secondSocket = new FakeSocket();
  const configured = config.agents[0];
  const first = new MyforgeConnection({
    socket: firstSocket,
    request: requestFor(firstSocket),
    gateway,
    configuredAgent: configured,
    config,
    clock: () => NOW
  });
  const second = new MyforgeConnection({
    socket: secondSocket,
    request: requestFor(secondSocket),
    gateway,
    configuredAgent: configured,
    config,
    clock: () => NOW
  });

  first.enqueueInbound(Buffer.from("first-in"), false);
  second.enqueueInbound(Buffer.from("second-in"), false);
  await waitFor(() => entered.length === 2, "both independent dispatchers");
  assert.deepEqual(new Set(entered), new Set(["first-in", "second-in"]));

  firstSocket.blockWrites = true;
  const firstWrite = first.enqueueOutbound("first-out", { expiresAtMs: NOW + 60000 });
  const secondWrite = second.enqueueOutbound("second-out", { expiresAtMs: NOW + 60000 });
  await secondWrite;
  assert.deepEqual(secondSocket.sent, ["second-out"]);
  assert.equal(firstSocket.pendingWrites.length, 1);
  firstSocket.releaseWrite();
  await firstWrite;

  releaseFirstHandler();
  releaseSecondHandler();
  await Promise.all([first.close(1000, "test_complete"), second.close(1000, "test_complete")]);
});

test("connection operation mutex enforces execute then cancel wire order", async (t) => {
  const harness = createHarness();
  t.after(() => harness.gateway.shutdown());
  const { socket } = await handshake(harness);
  socket.blockWrites = true;
  const input = commandInput();

  const execute = harness.gateway.withConnectionOperation({ agentId: AGENT_ID, projectId: PROJECT_ID }, async (operation) => {
    const prepared = operation.prepareExecute({
      requestId: REQUEST_ID,
      taskType: "fangyuan.blueprint.generate",
      profile: "codex_exec",
      input,
      timeoutMs: operation.connection.effectiveLimits.commandTimeoutMs,
      maxOutputBytes: operation.connection.effectiveLimits.maxOutputBytes
    });
    await operation.send(prepared);
  });
  const cancel = harness.gateway.withConnectionOperation({ agentId: AGENT_ID, projectId: PROJECT_ID }, async (operation) => {
    const prepared = operation.prepareCancel({
      requestId: REQUEST_ID,
      cancelRequestedAtMs: NOW,
      cancelDeadlineAtMs: NOW + operation.connection.effectiveLimits.cancelTimeoutMs
    });
    await operation.send(prepared);
  });

  await waitFor(() => socket.pendingWrites.length === 1, "blocked execute write");
  assert.equal(parseStrictJson(socket.sent.at(-1)).type, "command.execute");
  assert.equal(socket.pendingWrites.length, 1);
  socket.releaseWrite();
  await execute;
  await waitFor(() => socket.pendingWrites.length === 1, "blocked cancel write");
  assert.equal(parseStrictJson(socket.sent.at(-1)).type, "command.cancel");
  socket.releaseWrite();
  await cancel;
});

test("delivery reservation survives a close between database claim and socket enqueue", async () => {
  const harness = createHarness();
  const { connection } = await handshake(harness);
  await harness.gateway.withConnectionOperation({ agentId: AGENT_ID, projectId: PROJECT_ID }, async (operation) => {
    const reservation = operation.reserveDelivery({
      requestId: REQUEST_ID,
      kind: "command.execute"
    });
    await connection.close(1011, "closed_before_enqueue");
    operation.releaseDelivery(reservation);
  });
  assert.deepEqual(harness.store.offlineCalls[0].deliveryFailure, {
    requestId: REQUEST_ID,
    kind: "command.execute"
  });
});

test("writer failure closes connection and shutdown cleans every registered socket", async () => {
  const harness = createHarness();
  const first = await handshake(harness, new FakeSocket());
  first.socket.failNextWrite = true;

  const send = harness.gateway.withConnectionOperation({ agentId: AGENT_ID, projectId: PROJECT_ID }, async (operation) => {
    const prepared = operation.prepareCancel({
      requestId: REQUEST_ID,
      cancelRequestedAtMs: NOW,
      cancelDeadlineAtMs: NOW + operation.connection.effectiveLimits.cancelTimeoutMs
    });
    await operation.send(prepared);
  });
  await assert.rejects(send, /fake writer failed/);
  await waitFor(() => first.connection.closed, "writer failure close");
  assert.deepEqual(harness.store.offlineCalls[0].deliveryFailure, {
    requestId: REQUEST_ID,
    kind: "command.cancel"
  });

  const second = await handshake(harness, new FakeSocket());
  second.socket.blockWrites = true;
  const pendingSend = harness.gateway.withConnectionOperation({ agentId: AGENT_ID, projectId: PROJECT_ID }, async (operation) => {
    const prepared = operation.prepareCancel({
      requestId: REQUEST_ID,
      cancelRequestedAtMs: NOW,
      cancelDeadlineAtMs: NOW + operation.connection.effectiveLimits.cancelTimeoutMs
    });
    await operation.send(prepared);
  });
  await waitFor(() => second.socket.pendingWrites.length === 1, "active shutdown write");
  const shutdown = harness.gateway.shutdown();
  await assert.rejects(pendingSend, /connection closed before send completed/);
  await shutdown;
  assert.equal(second.connection.closed, true);
  assert.equal(harness.gateway.connections.size, 0);
  assert.equal(harness.gateway.connectionSettlements.size, 0);
  assert.equal(harness.store.agentConnectionId, null);
  assert.equal(harness.store.offlineCalls.at(-1).failureReason, "server_shutdown");
});

test("gateway shutdown idempotently waits for orchestrator stop before closing sockets", async () => {
  const harness = createHarness();
  const { connection } = await handshake(harness);
  let releaseStop;
  const stopGate = new Promise((resolve) => { releaseStop = resolve; });
  let stopCalls = 0;
  harness.gateway.setTaskOrchestrator({
    stop() {
      stopCalls += 1;
      return stopGate;
    }
  });

  let shutdownFinished = false;
  const first = harness.gateway.shutdown();
  const second = harness.gateway.shutdown();
  first.then(() => { shutdownFinished = true; });
  assert.equal(first, second);
  assert.equal(stopCalls, 1);
  await Promise.resolve();
  assert.equal(shutdownFinished, false);
  assert.equal(connection.closed, false);

  releaseStop();
  await Promise.all([first, second]);
  assert.equal(shutdownFinished, true);
  assert.equal(connection.closed, true);
  assert.equal(stopCalls, 1);
});

test("gateway shutdown drains an entered inbound orchestration operation before resolving", async () => {
  const harness = createHarness();
  let releaseOperation;
  let markOperationStarted;
  const operationStarted = new Promise((resolve) => { markOperationStarted = resolve; });
  const operationGate = new Promise((resolve) => { releaseOperation = resolve; });
  let terminalCalls = 0;
  let operationCompletions = 0;
  harness.gateway.setTaskOrchestrator({
    stop() { return Promise.resolve(); },
    async onTaskTerminal() {
      terminalCalls += 1;
      await harness.gateway.withConnectionOperation({ agentId: AGENT_ID, projectId: PROJECT_ID }, async () => {
        markOperationStarted();
        await operationGate;
        operationCompletions += 1;
      });
    },
    async onAgentDisconnected() {}
  });
  const { socket, connection } = await handshake(harness);
  installDispatchedTask(harness.store, connection);
  const errorMessage = (sequence) => agentEnvelope("command.error", {
    connectionId: connection.connectionId,
    requestId: REQUEST_ID,
    agentId: AGENT_ID,
    projectId: PROJECT_ID,
    errorCode: "MYFORGE_TARGET_PATH_INVALID",
    errorMessage: "artifactFile is outside the allowed path",
    retryable: false
  }, harness.agentKeys.privateKey, sequence);
  socket.receive(errorMessage(3));
  await operationStarted;

  let shutdownFinished = false;
  const first = harness.gateway.shutdown();
  const second = harness.gateway.shutdown();
  first.then(() => { shutdownFinished = true; });
  assert.equal(first, second);
  await Promise.resolve();
  assert.equal(shutdownFinished, false);

  socket.receive(errorMessage(4));
  releaseOperation();
  await first;
  assert.equal(shutdownFinished, true);
  assert.equal(terminalCalls, 1);
  assert.equal(operationCompletions, 1);
  assert.equal(harness.store.errorCalls, 1);
  assert.equal(connection.backgroundTasks.size, 0);

  socket.receive(errorMessage(5));
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(terminalCalls, 1);
  assert.equal(harness.store.errorCalls, 1);
});

test("disabled upgrade is rejected without loading or using signing keys", async () => {
  const harness = createHarness({ enabled: false });
  let statusCode = null;
  let body = null;
  const reply = {
    code(value) {
      statusCode = value;
      return this;
    },
    send(value) {
      body = value;
      return value;
    }
  };
  await harness.gateway.preValidateUpgrade({
    headers: { upgrade: "websocket", "sec-websocket-protocol": MYFORGE_SUBPROTOCOL },
    query: { agentId: AGENT_ID, projectId: PROJECT_ID },
    ip: "127.0.0.1"
  }, reply);
  assert.equal(statusCode, 503);
  assert.equal(body.error, "MYFORGE_DISABLED");
});

test("Fastify plugin exposes a stable HTTP fallback without opening a listener", async () => {
  const harness = createHarness({ enabled: false });
  const app = Fastify();
  await registerMyforgeWebsocket(app, harness.gateway, { myforge: harness.config });
  const response = await app.inject({ method: "GET", url: "/api/v1/myforge/ws" });
  assert.equal(response.statusCode, 503);
  assert.equal(response.json().error, "MYFORGE_DISABLED");
  await app.close();
});
