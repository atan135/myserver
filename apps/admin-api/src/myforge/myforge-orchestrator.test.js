import assert from "node:assert/strict";
import test from "node:test";

import { MyforgeOrchestrator } from "./myforge-orchestrator.js";

const NOW = Date.parse("2026-07-12T01:00:00.000Z");
const AGENT_ID = "dev-pc-001";
const PROJECT_ID = "myforge-local";
const CONNECTION_ID = "22222222-2222-4222-8222-222222222222";

function createError(code, message = code) {
  const error = new Error(message);
  error.code = code;
  return error;
}

function requestBody(overrides = {}) {
  return {
    agentId: AGENT_ID,
    projectId: PROJECT_ID,
    artifactFile: "artifacts/fangyuan/home.ron",
    rulesFile: "rules/fangyuan/rules.md",
    prompt: {
      theme: "home",
      primitiveLimit: 200,
      bounds: { width: 40, depth: 30, height: 20 },
      requirements: ["central furnace"]
    },
    ...overrides
  };
}

function task(overrides = {}) {
  return {
    requestId: "11111111-1111-4111-8111-111111111111",
    taskType: "fangyuan.blueprint.generate",
    projectId: PROJECT_ID,
    agentId: AGENT_ID,
    status: "queued",
    queueReason: null,
    executionMode: null,
    connectionId: null,
    artifactFile: "artifacts/fangyuan/home.ron",
    consumerTargetFile: null,
    rulesFile: "rules/fangyuan/rules.md",
    prompt: {
      theme: "home",
      primitiveLimit: 200,
      bounds: { width: 40, depth: 30, height: 20 },
      requirements: ["central furnace"]
    },
    renderedPrompt: "rendered prompt",
    commandPreview: "codex exec <renderedPrompt>",
    timeoutMs: 120000,
    maxOutputBytes: 1048576,
    stdoutPreview: null,
    stderrPreview: null,
    stdoutBytes: null,
    stderrBytes: null,
    stdoutTruncated: false,
    stderrTruncated: false,
    exitCode: null,
    artifact: null,
    audit: null,
    errorCode: null,
    errorMessage: null,
    createdByAdminId: 7,
    createdByAdminUsername: "admin",
    createdAt: new Date(NOW).toISOString(),
    queueExpiresAt: new Date(NOW + 900000).toISOString(),
    dispatchedAt: null,
    startedAt: null,
    cancelRequestedAt: null,
    cancelDeadlineAt: null,
    completedAt: null,
    ...overrides
  };
}

class MemoryStore {
  constructor({ online = true } = {}) {
    this.online = online;
    this.tasks = [];
    this.sequence = 1;
    this.events = [];
    this.agents = [{
      agentId: AGENT_ID,
      projectId: PROJECT_ID,
      label: "Development PC",
      publicKeyFingerprint: "secret-fingerprint",
      configured: true,
      status: online ? "online" : "offline",
      hostname: online ? "devbox" : null,
      platform: online ? "windows" : null,
      agentVersion: online ? "0.1.0" : null,
      forgeRootSummary: online ? { name: "myforge", configured: true } : null,
      capabilities: online ? { profiles: ["codex_exec"], dryRun: false } : null,
      limits: online ? { commandTtlMs: 60000 } : null,
      effectiveLimits: online ? { commandTimeoutMs: 120000 } : null,
      lastSeenAt: online ? new Date(NOW).toISOString() : null
    }];
    this.watchdogs = {
      cancel: [],
      running: [],
      dispatched: [],
      queue: []
    };
    this.queuedAgentIdentities = [];
  }

  nextId() {
    const suffix = String(this.sequence++).padStart(12, "0");
    return `00000000-0000-4000-8000-${suffix}`;
  }

  async listAgents() {
    return this.agents;
  }

  async createTask(value) {
    const created = task({
      requestId: this.nextId(),
      queueReason: this.online ? null : "agent_offline",
      artifactFile: value.artifactFile,
      consumerTargetFile: value.consumerTargetFile,
      rulesFile: value.rulesFile,
      prompt: value.prompt,
      renderedPrompt: value.renderedPrompt,
      commandPreview: value.commandPreview,
      createdByAdminId: value.createdByAdminId,
      createdByAdminUsername: value.createdByAdminUsername,
      createdAt: value.now.toISOString(),
      queueExpiresAt: new Date(value.now.getTime() + 900000).toISOString()
    });
    this.tasks.push(created);
    this.events.push(`create:${created.requestId}`);
    return created;
  }

  async getTask(requestId) {
    return this.tasks.find((entry) => entry.requestId === requestId) ?? null;
  }

  async listTasks({ projectId, agentId, status, limit, offset }) {
    return this.tasks
      .filter((entry) => !projectId || entry.projectId === projectId)
      .filter((entry) => !agentId || entry.agentId === agentId)
      .filter((entry) => !status || entry.status === status)
      .slice(offset, offset + limit);
  }

  async countTasks({ projectId, agentId, status }) {
    return this.tasks
      .filter((entry) => !projectId || entry.projectId === projectId)
      .filter((entry) => !agentId || entry.agentId === agentId)
      .filter((entry) => !status || entry.status === status)
      .length;
  }

  async findNextQueuedTask({ agentId, projectId, now }) {
    return this.tasks
      .filter((entry) => entry.agentId === agentId && entry.projectId === projectId)
      .filter((entry) => entry.status === "queued" && Date.parse(entry.queueExpiresAt) > now.getTime())
      .sort((left, right) => left.createdAt.localeCompare(right.createdAt) || left.requestId.localeCompare(right.requestId))[0] ?? null;
  }

  async claimTaskDispatched(value) {
    this.events.push(`claim:${value.requestId}`);
    const current = await this.getTask(value.requestId);
    const active = this.tasks.some((entry) => entry.agentId === value.agentId && new Set(["dispatched", "running"]).has(entry.status));
    if (!this.online || active || current?.status !== "queued" || Date.parse(current.queueExpiresAt) <= value.dispatchedAt.getTime()) {
      return null;
    }
    Object.assign(current, {
      status: "dispatched",
      queueReason: null,
      executionMode: value.executionMode,
      connectionId: value.connectionId,
      commandDigest: value.commandDigest,
      commandExpiresAt: value.commandExpiresAt.toISOString(),
      timeoutMs: value.timeoutMs,
      maxOutputBytes: value.maxOutputBytes,
      dispatchedAt: value.dispatchedAt.toISOString()
    });
    return current;
  }

  async setQueuedTasksReasonForAgent({ agentId, projectId, queueReason }) {
    this.events.push(`queue:${queueReason}`);
    const changed = this.tasks.filter((entry) => entry.agentId === agentId && entry.projectId === projectId && entry.status === "queued");
    for (const entry of changed) entry.queueReason = queueReason;
    return changed;
  }

  async failTask({ requestId, expectedStatuses, errorCode, errorMessage, completedAt }) {
    this.events.push(`fail:${errorCode}`);
    const current = await this.getTask(requestId);
    if (["completed", "completed_with_errors", "failed", "cancelled"].includes(current.status)) {
      return { outcome: "terminal", task: current };
    }
    if (!expectedStatuses.includes(current.status)) return { outcome: "not_applicable", task: current };
    Object.assign(current, {
      status: "failed",
      queueReason: null,
      errorCode,
      errorMessage,
      completedAt: completedAt.toISOString()
    });
    return { outcome: "updated", task: current };
  }

  async requestTaskCancellation(value) {
    const current = await this.getTask(value.requestId);
    if (current.status === "cancelled") return { outcome: "duplicate", task: current, sendCancel: false };
    if (["completed", "completed_with_errors", "failed"].includes(current.status)) {
      throw createError("MYFORGE_TASK_NOT_CANCELLABLE");
    }
    if (current.status === "queued") {
      Object.assign(current, {
        status: "cancelled",
        queueReason: null,
        errorCode: "MYFORGE_COMMAND_CANCELLED",
        errorMessage: "Task was cancelled before dispatch",
        completedAt: value.requestedAt.toISOString()
      });
      return { outcome: "cancelled", task: current, sendCancel: false };
    }
    if (value.queuedOnly) return { outcome: "requires_connection", task: current, sendCancel: true };
    if (current.cancelRequestedAt) return { outcome: "duplicate", task: current, sendCancel: true };
    current.cancelRequestedAt = value.requestedAt.toISOString();
    current.cancelDeadlineAt = new Date(value.requestedAt.getTime() + value.cancelTimeoutMs).toISOString();
    return { outcome: "requested", task: current, sendCancel: true };
  }

  async failExpiredCancellationTasks() {
    this.events.push("watchdog:cancel");
    return this.watchdogs.cancel;
  }

  async failTimedOutRunningTasks() {
    this.events.push("watchdog:running");
    return this.watchdogs.running;
  }

  async failExpiredDispatchedTasks() {
    this.events.push("watchdog:dispatched");
    return this.watchdogs.dispatched;
  }

  async failExpiredQueuedTasks() {
    this.events.push("watchdog:queue");
    return this.watchdogs.queue;
  }

  async listQueuedAgentIdentities() {
    this.events.push("watchdog:scan");
    return this.queuedAgentIdentities;
  }
}

class FakeGateway {
  constructor(store, clock) {
    this.store = store;
    this.clock = clock;
    this.online = store.online;
    this.events = store.events;
    this.sent = [];
    this.preparedCancels = [];
    this.prepareFailureKind = null;
    this.preWireFailureKind = null;
    this.failKind = null;
    this.closed = [];
    this.onMissingConnection = null;
    this.deliveryReservation = null;
    this.connection = {
      agentId: AGENT_ID,
      projectId: PROJECT_ID,
      connectionId: CONNECTION_ID,
      capabilities: { dryRun: false, fangyuanBlueprint: true },
      effectiveLimits: {
        commandTimeoutMs: 120000,
        cancelTimeoutMs: 10000,
        maxOutputBytes: 1048576,
        commandTtlMs: 60000
      }
    };
  }

  getRegisteredConnection(agentId, projectId) {
    return this.online && agentId === AGENT_ID && projectId === PROJECT_ID ? this.connection : null;
  }

  async withConnectionOperation(identity, callback) {
    if (!this.getRegisteredConnection(identity.agentId, identity.projectId)) {
      this.onMissingConnection?.();
      throw createError("MYFORGE_AGENT_DISCONNECTED");
    }
    this.events.push("lock:enter");
    try {
      return await callback({
        connection: this.connection,
        prepareExecute: (payload) => {
          this.events.push(`prepare:${payload.requestId}`);
          return {
            kind: "command.execute",
            semanticDigest: "a".repeat(64),
            expiresAt: new Date(this.clock() + 60000),
            message: { timestampMs: this.clock() },
            payload
          };
        },
        prepareCancel: (payload) => {
          this.events.push(`prepare-cancel:${payload.requestId}`);
          if (this.prepareFailureKind === "command.cancel") {
            throw createError("MYFORGE_MESSAGE_EXPIRED", "cancel expired during preparation");
          }
          this.preparedCancels.push({ ...payload });
          return { kind: "command.cancel", payload };
        },
        reserveDelivery: (intent) => {
          assert.equal(this.deliveryReservation, null);
          this.deliveryReservation = { ...intent };
          return this.deliveryReservation;
        },
        releaseDelivery: (reservation) => {
          if (this.deliveryReservation === reservation) this.deliveryReservation = null;
        },
        assertCurrent: () => {
          if (!this.online) throw createError("MYFORGE_AGENT_DISCONNECTED");
        },
        send: async (prepared) => {
          this.events.push(`send:${prepared.kind}`);
          try {
            if (this.preWireFailureKind === prepared.kind) {
              throw createError("MYFORGE_MESSAGE_EXPIRED", "command expired before enqueue");
            }
            if (this.failKind === prepared.kind) throw new Error("writer failed");
            this.sent.push(prepared);
          } finally {
            this.deliveryReservation = null;
          }
        },
        close: async (code, reason) => {
          this.events.push(`close:${reason}`);
          this.online = false;
          this.store.online = false;
          this.closed.push({ code, reason, connectionId: this.connection.connectionId });
          await this.store.setQueuedTasksReasonForAgent({
            agentId: this.connection.agentId,
            projectId: this.connection.projectId,
            queueReason: "agent_offline"
          });
        }
      });
    } finally {
      this.events.push("lock:exit");
    }
  }

  async closeTaskConnection(value) {
    this.closed.push(value);
    return true;
  }
}

function harness({ online = true, setIntervalFn, clearIntervalFn } = {}) {
  let now = NOW;
  const clock = () => now;
  const store = new MemoryStore({ online });
  const gateway = new FakeGateway(store, clock);
  const orchestrator = new MyforgeOrchestrator({
    config: {
      enabled: true,
      commandTimeoutMs: 120000,
      cancelTimeoutMs: 10000,
      maxOutputBytes: 1048576,
      queueTtlMs: 900000,
      clockSkewMs: 5000
    },
    store,
    gateway,
    clock,
    ...(setIntervalFn ? { setIntervalFn } : {}),
    ...(clearIntervalFn ? { clearIntervalFn } : {})
  });
  return {
    store,
    gateway,
    orchestrator,
    setNow(value) { now = value; }
  };
}

test("create persists typed input then prepares, claims, and sends one execute under the connection operation", async () => {
  const value = harness();
  const response = await value.orchestrator.createFangyuanBlueprint(requestBody(), {
    adminId: 7,
    adminUsername: "admin",
    ip: "127.0.0.1"
  });

  assert.equal(response.status, "dispatched");
  assert.equal(response.executionMode, "codex_exec");
  assert.equal(value.gateway.sent.length, 1);
  assert.equal(value.gateway.sent[0].payload.input.consumerTargetFile, null);
  assert.match(value.gateway.sent[0].payload.input.renderedPrompt, /MANDATORY CONSTRAINTS/);
  assert.deepEqual(value.store.events.slice(0, 6), [
    `create:${response.requestId}`,
    "lock:enter",
    `prepare:${response.requestId}`,
    `claim:${response.requestId}`,
    "send:command.execute",
    "queue:agent_busy"
  ]);
  const persisted = await value.store.getTask(response.requestId);
  assert.equal(persisted.commandDigest, "a".repeat(64));
  assert.equal(persisted.timeoutMs, 120000);
  assert.equal(persisted.maxOutputBytes, 1048576);
});

test("offline creation remains queued and reports agent_offline without sending", async () => {
  const value = harness({ online: false });
  const response = await value.orchestrator.createFangyuanBlueprint(requestBody());
  assert.equal(response.status, "queued");
  assert.equal(response.queueReason, "agent_offline");
  assert.equal(response.executionMode, null);
  assert.equal(value.gateway.sent.length, 0);
});

test("busy dispatch preserves FIFO and terminal completion triggers the next queued task", async () => {
  const value = harness();
  const active = task({
    requestId: "99999999-9999-4999-8999-999999999999",
    status: "running",
    executionMode: "codex_exec",
    connectionId: CONNECTION_ID,
    startedAt: new Date(NOW).toISOString()
  });
  value.store.tasks.push(active);
  const first = await value.store.createTask({
    ...requestBody(),
    consumerTargetFile: null,
    renderedPrompt: "first",
    commandPreview: "first",
    now: new Date(NOW + 1)
  });
  const second = await value.store.createTask({
    ...requestBody({ artifactFile: "artifacts/fangyuan/second.ron" }),
    consumerTargetFile: null,
    renderedPrompt: "second",
    commandPreview: "second",
    now: new Date(NOW + 2)
  });

  await value.orchestrator.dispatchNext({ agentId: AGENT_ID, projectId: PROJECT_ID });
  assert.equal(first.queueReason, "agent_busy");
  assert.equal(second.queueReason, "agent_busy");
  active.status = "completed";
  active.completedAt = new Date(NOW + 3).toISOString();
  await value.orchestrator.onTaskTerminal(active);
  assert.equal(first.status, "dispatched");
  assert.equal(second.status, "queued");
  assert.equal(value.gateway.sent.at(-1).payload.requestId, first.requestId);
});

test("execute writer failure leaves the claimed task failed and never returns it to queued", async () => {
  const value = harness();
  value.gateway.failKind = "command.execute";
  const response = await value.orchestrator.createFangyuanBlueprint(requestBody());
  assert.equal(response.status, "failed");
  assert.equal(response.errorCode, "MYFORGE_DISPATCH_FAILED");
  assert.equal((await value.store.getTask(response.requestId)).queueReason, null);
});

test("queued cancellation sends no frame while active cancellation is idempotent with one fixed deadline", async () => {
  const offline = harness({ online: false });
  const queued = await offline.store.createTask({
    ...requestBody(),
    consumerTargetFile: null,
    renderedPrompt: "queued",
    commandPreview: "queued",
    now: new Date(NOW)
  });
  const queuedResponse = await offline.orchestrator.cancelTask(queued.requestId, {}, { adminId: 7 });
  assert.deepEqual(queuedResponse, {
    ok: true,
    requestId: queued.requestId,
    status: "cancelled",
    cancelRequested: false,
    cancelDeadlineAt: null
  });
  assert.equal(offline.gateway.sent.length, 0);

  const fifo = harness();
  const firstQueued = await fifo.store.createTask({
    ...requestBody(),
    consumerTargetFile: null,
    renderedPrompt: "first queued",
    commandPreview: "first queued",
    now: new Date(NOW)
  });
  const secondQueued = await fifo.store.createTask({
    ...requestBody({ artifactFile: "artifacts/fangyuan/second.ron" }),
    consumerTargetFile: null,
    renderedPrompt: "second queued",
    commandPreview: "second queued",
    now: new Date(NOW + 1)
  });
  const fifoResponse = await fifo.orchestrator.cancelTask(firstQueued.requestId, {}, { adminId: 7 });
  assert.equal(fifoResponse.status, "cancelled");
  assert.equal(secondQueued.status, "dispatched");
  assert.equal(fifo.gateway.sent[0].payload.requestId, secondQueued.requestId);

  const online = harness();
  const created = await online.orchestrator.createFangyuanBlueprint(requestBody());
  const first = await online.orchestrator.cancelTask(created.requestId, {}, { adminId: 7 });
  online.setNow(NOW + 2000);
  const second = await online.orchestrator.cancelTask(created.requestId, undefined, { adminId: 7 });
  assert.equal(first.cancelRequested, true);
  assert.equal(first.cancelDeadlineAt, new Date(NOW + 10000).toISOString());
  assert.equal(second.cancelDeadlineAt, first.cancelDeadlineAt);
  assert.equal(online.gateway.preparedCancels.length, 2);
  assert.equal(online.gateway.preparedCancels[0].cancelDeadlineAtMs, online.gateway.preparedCancels[1].cancelDeadlineAtMs);
  online.setNow(NOW + 10000);
  const atDeadline = await online.orchestrator.cancelTask(created.requestId, {}, { adminId: 7 });
  assert.equal(atDeadline.cancelDeadlineAt, first.cancelDeadlineAt);
  assert.equal(online.gateway.preparedCancels.length, 2);
});

test("cancel writer failure becomes MYFORGE_CANCEL_DELIVERY_FAILED", async () => {
  const value = harness();
  const created = await value.orchestrator.createFangyuanBlueprint(requestBody());
  value.gateway.failKind = "command.cancel";
  const response = await value.orchestrator.cancelTask(created.requestId, {}, { adminId: 7 });
  assert.equal(response.status, "failed");
  assert.equal(response.cancelRequested, true);
  assert.equal(response.errorCode, "MYFORGE_CANCEL_DELIVERY_FAILED");
  assert.equal(value.gateway.closed.at(-1).reason, "cancel_delivery_failed");
});

test("cancel preparation and pre-wire expiry failures close the owner and never dispatch the next task", async (t) => {
  for (const failure of ["prepare", "pre_wire"]) {
    await t.test(failure, async () => {
      const value = harness();
      const first = await value.orchestrator.createFangyuanBlueprint(requestBody());
      const second = await value.orchestrator.createFangyuanBlueprint(requestBody({
        artifactFile: `artifacts/fangyuan/${failure}.ron`
      }));
      if (failure === "prepare") value.gateway.prepareFailureKind = "command.cancel";
      else value.gateway.preWireFailureKind = "command.cancel";

      const response = await value.orchestrator.cancelTask(first.requestId, {}, { adminId: 7 });
      assert.equal(response.status, "failed");
      assert.equal(response.errorCode, "MYFORGE_CANCEL_DELIVERY_FAILED");
      assert.equal(value.gateway.online, false);
      assert.equal(value.gateway.closed.at(-1).reason, "cancel_delivery_failed");
      assert.equal(second.status, "queued");
      assert.equal((await value.store.getTask(second.requestId)).queueReason, "agent_offline");
      assert.deepEqual(value.gateway.sent.map((entry) => entry.kind), ["command.execute"]);
    });
  }
});

test("cancel retries the connection mutex when registration and dispatch win the offline race", async () => {
  const value = harness();
  const queued = await value.store.createTask({
    ...requestBody(),
    consumerTargetFile: null,
    renderedPrompt: "queued",
    commandPreview: "queued",
    now: new Date(NOW)
  });
  value.gateway.online = false;
  value.gateway.onMissingConnection = () => {
    value.gateway.onMissingConnection = null;
    value.gateway.online = true;
    Object.assign(queued, {
      status: "dispatched",
      connectionId: CONNECTION_ID,
      executionMode: "codex_exec",
      dispatchedAt: new Date(NOW).toISOString()
    });
  };

  const response = await value.orchestrator.cancelTask(queued.requestId, {}, { adminId: 7 });
  assert.equal(response.status, "dispatched");
  assert.equal(response.cancelRequested, true);
  assert.equal(response.errorCode, undefined);
  assert.equal(value.gateway.sent.length, 1);
  assert.equal(value.gateway.sent[0].kind, "command.cancel");
});

test("active cancel without its owning connection never invents an unnegotiated deadline", async () => {
  const value = harness();
  const created = await value.orchestrator.createFangyuanBlueprint(requestBody());
  value.gateway.online = false;
  const response = await value.orchestrator.cancelTask(created.requestId, {}, { adminId: 7 });
  assert.equal(response.status, "failed");
  assert.equal(response.cancelRequested, false);
  assert.equal(response.cancelDeadlineAt, null);
  assert.equal(response.errorCode, "MYFORGE_AGENT_DISCONNECTED");
});

test("watchdog tick gives cancellation priority, closes timeout connections, and retries idle queues", async () => {
  const value = harness();
  value.store.watchdogs.cancel = [task({
    requestId: "10000000-0000-4000-8000-000000000001",
    status: "failed",
    connectionId: CONNECTION_ID,
    errorCode: "MYFORGE_CANCEL_TIMEOUT"
  })];
  value.store.watchdogs.running = [task({
    requestId: "10000000-0000-4000-8000-000000000002",
    status: "failed",
    connectionId: CONNECTION_ID,
    errorCode: "MYFORGE_COMMAND_TIMEOUT"
  })];
  value.store.watchdogs.dispatched = [task({
    requestId: "10000000-0000-4000-8000-000000000003",
    status: "failed",
    errorCode: "MYFORGE_COMMAND_EXPIRED"
  })];
  value.store.watchdogs.queue = [task({
    requestId: "10000000-0000-4000-8000-000000000004",
    status: "failed",
    errorCode: "MYFORGE_QUEUE_EXPIRED"
  })];
  const dispatched = [];
  value.orchestrator.dispatchNext = async (identity) => { dispatched.push(identity); };

  const result = await value.orchestrator.tick();
  assert.deepEqual(value.store.events.slice(0, 4), [
    "watchdog:cancel",
    "watchdog:running",
    "watchdog:dispatched",
    "watchdog:queue"
  ]);
  assert.deepEqual(result, {
    queueExpired: 1,
    commandExpired: 1,
    commandTimedOut: 1,
    cancelTimedOut: 1
  });
  assert.deepEqual(value.gateway.closed.map((entry) => entry.reason), ["cancel_timeout", "command_timeout"]);
  assert.deepEqual(dispatched, [{ agentId: AGENT_ID, projectId: PROJECT_ID }]);
});

test("periodic scan retries an unexpired queued task even when no timeout transition occurred", async () => {
  const value = harness();
  value.store.queuedAgentIdentities = [{ agentId: AGENT_ID, projectId: PROJECT_ID }];
  const dispatched = [];
  value.orchestrator.dispatchNext = async (identity) => { dispatched.push(identity); };
  await value.orchestrator.tick();
  assert.deepEqual(dispatched, [{ agentId: AGENT_ID, projectId: PROJECT_ID }]);
  assert.equal(value.store.events.at(-1), "watchdog:scan");
});

test("watchdog stop is an idempotent completion barrier for an in-flight tick", async () => {
  let intervalCallback;
  let clearCalls = 0;
  const timer = { unref() {} };
  const value = harness({
    setIntervalFn(callback) {
      intervalCallback = callback;
      return timer;
    },
    clearIntervalFn(received) {
      assert.equal(received, timer);
      clearCalls += 1;
    }
  });
  let releaseTick;
  let markTickStarted;
  const tickStarted = new Promise((resolve) => { markTickStarted = resolve; });
  const tickGate = new Promise((resolve) => { releaseTick = resolve; });
  let tickCount = 0;
  value.store.failExpiredCancellationTasks = async () => {
    tickCount += 1;
    markTickStarted();
    await tickGate;
    return [];
  };

  value.orchestrator.start();
  intervalCallback();
  await tickStarted;

  let stopped = false;
  const first = value.orchestrator.stop();
  const second = value.orchestrator.stop();
  const lifecycle = value.orchestrator.onModuleDestroy();
  first.then(() => { stopped = true; });
  assert.equal(first, second);
  assert.equal(first, lifecycle);
  assert.equal(clearCalls, 1);
  await Promise.resolve();
  assert.equal(stopped, false);

  intervalCallback();
  assert.equal(tickCount, 1);
  releaseTick();
  await first;
  assert.equal(stopped, true);

  intervalCallback();
  await value.orchestrator.tick();
  assert.equal(tickCount, 1);
  assert.equal(clearCalls, 1);
});

test("agent and task HTTP projections omit keys, signatures, connection ids, rendered prompts, and list output", async () => {
  const value = harness();
  const agents = await value.orchestrator.listAgents({});
  assert.equal(agents.items[0].publicKeyFingerprint, undefined);
  assert.equal(agents.items[0].connectionId, undefined);

  const created = await value.orchestrator.createFangyuanBlueprint(requestBody());
  const stored = await value.store.getTask(created.requestId);
  stored.status = "completed_with_errors";
  stored.stdoutPreview = "limited output";
  stored.stderrPreview = "warning";
  stored.audit = { status: "failed" };
  stored.errorCode = "FANGYUAN_BLUEPRINT_AUDIT_FAILED";
  stored.completedAt = new Date(NOW + 5000).toISOString();
  const list = await value.orchestrator.listTasks({ limit: "20", offset: "0" });
  assert.equal(list.items[0].stdoutPreview, undefined);
  assert.equal(list.items[0].renderedPrompt, undefined);
  assert.equal(list.items[0].connectionId, undefined);
  assert.equal(list.items[0].errorCode, "FANGYUAN_BLUEPRINT_AUDIT_FAILED");

  const detail = await value.orchestrator.getTask(created.requestId);
  assert.equal(detail.task.stdoutPreview, "limited output");
  assert.equal(detail.task.audit.status, "failed");
  assert.equal(detail.task.renderedPrompt, undefined);
  assert.equal(detail.task.commandDigest, undefined);
});
