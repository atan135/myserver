import { log } from "../logger.js";
import { AsyncMutex, isUuidV4 } from "./protocol.js";
import {
  assertEmptyAgentQuery,
  assertEmptyCancelBody,
  buildCommandPreview,
  normalizeFangyuanBlueprintRequest,
  normalizeTaskListQuery
} from "./myforge-task-input.js";

const TERMINAL_STATUSES = new Set(["completed", "completed_with_errors", "failed", "cancelled"]);
const NON_CANCELLABLE_STATUSES = new Set(["completed", "completed_with_errors", "failed"]);
const WATCHDOG_INTERVAL_MS = 1000;

export class MyforgeOrchestrationError extends Error {
  constructor(code, message, statusCode) {
    super(message);
    this.name = "MyforgeOrchestrationError";
    this.code = code;
    this.statusCode = statusCode;
  }
}

function fail(code, message, statusCode) {
  throw new MyforgeOrchestrationError(code, message, statusCode);
}

function asMillis(value) {
  if (value === null || value === undefined) return null;
  const result = new Date(value).getTime();
  return Number.isFinite(result) ? result : null;
}

function durationMs(task) {
  const completedAt = asMillis(task.completedAt);
  const startedAt = asMillis(task.startedAt ?? task.dispatchedAt ?? task.createdAt);
  if (completedAt === null || startedAt === null) return null;
  return Math.max(0, completedAt - startedAt);
}

function publicAgent(agent) {
  return {
    agentId: agent.agentId,
    projectId: agent.projectId,
    label: agent.label,
    configured: agent.configured,
    status: agent.status,
    hostname: agent.hostname,
    platform: agent.platform,
    agentVersion: agent.agentVersion,
    forgeRootSummary: agent.forgeRootSummary,
    capabilities: agent.capabilities,
    limits: agent.limits,
    effectiveLimits: agent.effectiveLimits,
    lastSeenAt: agent.lastSeenAt
  };
}

function createdBy(task) {
  return {
    adminId: task.createdByAdminId,
    username: task.createdByAdminUsername
  };
}

function publicTaskListItem(task) {
  return {
    requestId: task.requestId,
    taskType: task.taskType,
    projectId: task.projectId,
    agentId: task.agentId,
    status: task.status,
    queueReason: task.queueReason,
    executionMode: task.executionMode,
    dangerFullAccess: task.dangerFullAccess,
    artifactFile: task.artifactFile,
    consumerTargetFile: task.consumerTargetFile,
    createdBy: createdBy(task),
    createdAt: task.createdAt,
    dispatchedAt: task.dispatchedAt,
    startedAt: task.startedAt,
    cancelRequestedAt: task.cancelRequestedAt,
    cancelDeadlineAt: task.cancelDeadlineAt,
    completedAt: task.completedAt,
    durationMs: durationMs(task),
    errorCode: task.errorCode
  };
}

function publicTaskDetail(task) {
  return {
    requestId: task.requestId,
    taskType: task.taskType,
    projectId: task.projectId,
    agentId: task.agentId,
    status: task.status,
    queueReason: task.queueReason,
    executionMode: task.executionMode,
    dangerFullAccess: task.dangerFullAccess,
    artifactFile: task.artifactFile,
    consumerTargetFile: task.consumerTargetFile,
    rulesFile: task.rulesFile,
    prompt: task.prompt,
    commandPreview: task.commandPreview,
    stdoutPreview: task.stdoutPreview,
    stderrPreview: task.stderrPreview,
    stdoutBytes: task.stdoutBytes,
    stderrBytes: task.stderrBytes,
    stdoutTruncated: task.stdoutTruncated,
    stderrTruncated: task.stderrTruncated,
    exitCode: task.exitCode,
    artifact: task.artifact,
    audit: task.audit,
    errorCode: task.errorCode,
    errorMessage: task.errorMessage,
    createdBy: createdBy(task),
    createdAt: task.createdAt,
    dispatchedAt: task.dispatchedAt,
    startedAt: task.startedAt,
    cancelRequestedAt: task.cancelRequestedAt,
    cancelDeadlineAt: task.cancelDeadlineAt,
    completedAt: task.completedAt
  };
}

function createResponse(task) {
  return {
    ok: true,
    requestId: task.requestId,
    status: task.status,
    queueReason: task.queueReason,
    executionMode: task.executionMode,
    createdAt: task.createdAt,
    queueExpiresAt: task.queueExpiresAt,
    ...(task.errorCode ? { errorCode: task.errorCode } : {})
  };
}

function cancellationResponse(task) {
  return {
    ok: true,
    requestId: task.requestId,
    status: task.status,
    cancelRequested: task.cancelRequestedAt !== null,
    cancelDeadlineAt: task.cancelDeadlineAt,
    ...(task.errorCode && task.status === "failed" ? { errorCode: task.errorCode } : {})
  };
}

function safeFailureMessage(code) {
  const messages = {
    MYFORGE_DISPATCH_FAILED: "Task command could not be delivered to the agent",
    MYFORGE_CANCEL_DELIVERY_FAILED: "Cancellation command could not be delivered to the agent",
    MYFORGE_QUEUE_EXPIRED: "Task expired while waiting for an agent"
  };
  return messages[code] ?? "Myforge task orchestration failed";
}

export class MyforgeOrchestrator {
  constructor({
    config,
    store,
    gateway,
    clock = Date.now,
    watchdogIntervalMs = WATCHDOG_INTERVAL_MS,
    setIntervalFn = setInterval,
    clearIntervalFn = clearInterval
  }) {
    this.config = config;
    this.store = store;
    this.gateway = gateway;
    this.clock = clock;
    this.watchdogIntervalMs = watchdogIntervalMs;
    this.setIntervalFn = setIntervalFn;
    this.clearIntervalFn = clearIntervalFn;
    this.watchdogMutex = new AsyncMutex();
    this.watchdogTimer = null;
    this.watchdogTickPending = false;
    this.watchdogStopping = false;
    this.watchdogStopPromise = null;
  }

  get enabled() {
    return this.config.enabled === true;
  }

  assertEnabled() {
    if (!this.enabled) fail("MYFORGE_DISABLED", "myforge task orchestration is disabled", 503);
  }

  now() {
    return new Date(this.clock());
  }

  start() {
    if (!this.enabled || this.watchdogTimer || this.watchdogStopping) return;
    this.watchdogTimer = this.setIntervalFn(() => {
      if (this.watchdogStopping || this.watchdogTickPending) return;
      this.watchdogTickPending = true;
      void this.tick()
        .catch((error) => {
          log("error", "myforge.watchdog_failed", {
            errorCode: error?.code ?? error?.name ?? "UNKNOWN_ERROR"
          });
        })
        .finally(() => {
          this.watchdogTickPending = false;
        });
    }, this.watchdogIntervalMs);
    this.watchdogTimer.unref?.();
  }

  stop() {
    if (this.watchdogStopPromise) return this.watchdogStopPromise;
    this.watchdogStopping = true;
    if (this.watchdogTimer) {
      this.clearIntervalFn(this.watchdogTimer);
      this.watchdogTimer = null;
    }
    this.watchdogStopPromise = this.watchdogMutex.runExclusive(() => undefined);
    return this.watchdogStopPromise;
  }

  onModuleDestroy() {
    return this.stop();
  }

  async listAgents(query = {}) {
    this.assertEnabled();
    assertEmptyAgentQuery(query);
    const agents = await this.store.listAgents({ configuredOnly: true });
    return {
      ok: true,
      items: agents.map(publicAgent),
      total: agents.length
    };
  }

  async listTasks(query = {}) {
    this.assertEnabled();
    const filters = normalizeTaskListQuery(query);
    const [items, total] = await Promise.all([
      this.store.listTasks(filters),
      this.store.countTasks(filters)
    ]);
    return {
      ok: true,
      items: items.map(publicTaskListItem),
      total,
      limit: filters.limit,
      offset: filters.offset
    };
  }

  async getTask(requestId) {
    this.assertEnabled();
    this.assertRequestId(requestId);
    const task = await this.store.getTask(requestId);
    if (!task) fail("MYFORGE_TASK_NOT_FOUND", "Task was not found", 404);
    return { ok: true, task: publicTaskDetail(task) };
  }

  async createFangyuanBlueprint(body, actor = {}) {
    this.assertEnabled();
    const normalized = normalizeFangyuanBlueprintRequest(body);
    const task = await this.store.createTask({
      projectId: normalized.projectId,
      agentId: normalized.agentId,
      artifactFile: normalized.artifactFile,
      consumerTargetFile: normalized.consumerTargetFile,
      rulesFile: normalized.rulesFile,
      prompt: normalized.prompt,
      renderedPrompt: normalized.renderedPrompt,
      commandPreview: normalized.commandPreview,
      createdByAdminId: actor.adminId ?? null,
      createdByAdminUsername: actor.adminUsername ?? null,
      ip: actor.ip ?? null,
      now: this.now()
    });

    await this.dispatchBestEffort(task, "task_create");
    return createResponse(await this.store.getTask(task.requestId));
  }

  async cancelTask(requestId, body, actor = {}) {
    this.assertEnabled();
    this.assertRequestId(requestId);
    assertEmptyCancelBody(body);
    const task = await this.store.getTask(requestId);
    if (!task) fail("MYFORGE_TASK_NOT_FOUND", "Task was not found", 404);
    if (NON_CANCELLABLE_STATUSES.has(task.status)) {
      fail("MYFORGE_TASK_NOT_CANCELLABLE", "Task is already complete", 409);
    }

    if (task.status === "cancelled") {
      const duplicate = await this.store.requestTaskCancellation({
        requestId,
        adminId: actor.adminId ?? null,
        adminUsername: actor.adminUsername ?? null,
        ip: actor.ip ?? null,
        requestedAt: this.now()
      });
      const response = cancellationResponse(duplicate.task);
      await this.dispatchBestEffort(task, "cancelled_task_retry");
      return response;
    }
    if (task.cancelRequestedAt && asMillis(task.cancelDeadlineAt) <= this.clock()) {
      return cancellationResponse(task);
    }

    try {
      const response = await this.gateway.withConnectionOperation(
        { agentId: task.agentId, projectId: task.projectId },
        (operation) => this.cancelWithinConnection(operation, requestId, actor)
      );
      if (response.status === "cancelled") {
        await this.dispatchBestEffort(task, "queued_cancel");
      }
      return response;
    } catch (error) {
      if (error?.code !== "MYFORGE_AGENT_DISCONNECTED" && error?.code !== "MYFORGE_IDENTITY_MISMATCH") {
        throw error;
      }
      const response = await this.cancelAfterConnectionMiss(task, requestId, actor);
      if (response.status === "cancelled") {
        await this.dispatchBestEffort(task, "offline_queued_cancel");
      }
      return response;
    }
  }

  async pauseTask(requestId, body, actor = {}) {
    this.assertEnabled();
    this.assertRequestId(requestId);
    assertEmptyCancelBody(body);
    const paused = await this.store.pauseTask({
      requestId,
      adminId: actor.adminId ?? null,
      adminUsername: actor.adminUsername ?? null,
      ip: actor.ip ?? null,
      pausedAt: this.now()
    });
    return { ok: true, requestId, status: paused.task.status, paused: paused.task.status === "paused" };
  }

  async resumeTask(requestId, body, actor = {}) {
    this.assertEnabled();
    this.assertRequestId(requestId);
    assertEmptyCancelBody(body);
    const resumed = await this.store.resumeTask({
      requestId,
      adminId: actor.adminId ?? null,
      adminUsername: actor.adminUsername ?? null,
      ip: actor.ip ?? null,
      resumedAt: this.now()
    });
    // dispatchNext handles an offline agent as a queued state, but propagates a
    // control-plane/store failure instead of reporting a successful resume.
    await this.dispatchNext({ agentId: resumed.task.agentId, projectId: resumed.task.projectId });
    const current = await this.store.getTask(requestId);
    return { ok: true, requestId, status: current?.status ?? resumed.task.status, paused: false };
  }

  async dispatchBestEffort(task, trigger) {
    try {
      return await this.dispatchNext({ agentId: task.agentId, projectId: task.projectId });
    } catch (error) {
      log("error", "myforge.dispatch_trigger_failed", {
        trigger,
        requestId: task.requestId,
        agentId: task.agentId,
        errorCode: error?.code ?? error?.name ?? "UNKNOWN_ERROR"
      });
      return null;
    }
  }

  async cancelWithinConnection(operation, requestId, actor) {
    const reservation = operation.reserveDelivery({ requestId, kind: "command.cancel" });
    const requestedAt = this.now();
    let cancellation;
    try {
      cancellation = await this.store.requestTaskCancellation({
        requestId,
        agentId: operation.connection.agentId,
        projectId: operation.connection.projectId,
        connectionId: operation.connection.connectionId,
        adminId: actor.adminId ?? null,
        adminUsername: actor.adminUsername ?? null,
        ip: actor.ip ?? null,
        requestedAt,
        cancelTimeoutMs: operation.connection.effectiveLimits.cancelTimeoutMs
      });
    } catch (error) {
      operation.releaseDelivery(reservation);
      throw error;
    }
    if (!cancellation.sendCancel) {
      operation.releaseDelivery(reservation);
      return cancellationResponse(cancellation.task);
    }
    if (cancellation.outcome === "duplicate" && asMillis(cancellation.task.cancelDeadlineAt) <= this.clock()) {
      operation.releaseDelivery(reservation);
      return cancellationResponse(cancellation.task);
    }

    try {
      const prepared = operation.prepareCancel({
        requestId,
        cancelRequestedAtMs: asMillis(cancellation.task.cancelRequestedAt),
        cancelDeadlineAtMs: asMillis(cancellation.task.cancelDeadlineAt)
      });
      await operation.send(prepared);
      return cancellationResponse(cancellation.task);
    } catch {
      let failed;
      let failureError;
      try {
        failed = await this.store.failTask({
          requestId,
          expectedStatuses: ["dispatched", "running"],
          errorCode: "MYFORGE_CANCEL_DELIVERY_FAILED",
          errorMessage: safeFailureMessage("MYFORGE_CANCEL_DELIVERY_FAILED"),
          completedAt: this.now(),
          adminId: actor.adminId ?? null,
          adminUsername: actor.adminUsername ?? null,
          ip: actor.ip ?? null
        });
      } catch (error) {
        failureError = error;
      }
      try {
        await operation.close(1011, "cancel_delivery_failed");
      } catch (error) {
        log("error", "myforge.cancel_delivery_connection_close_failed", {
          requestId,
          agentId: operation.connection.agentId,
          errorCode: error?.code ?? error?.name ?? "UNKNOWN_ERROR"
        });
      } finally {
        operation.releaseDelivery(reservation);
      }
      if (failureError) throw failureError;
      return cancellationResponse(failed.task);
    }
  }

  async cancelAfterConnectionMiss(task, requestId, actor) {
    const queuedAttempt = await this.store.requestTaskCancellation({
      requestId,
      adminId: actor.adminId ?? null,
      adminUsername: actor.adminUsername ?? null,
      ip: actor.ip ?? null,
      requestedAt: this.now(),
      queuedOnly: true
    });
    if (queuedAttempt.outcome !== "requires_connection") {
      return cancellationResponse(queuedAttempt.task);
    }

    try {
      return await this.gateway.withConnectionOperation(
        { agentId: task.agentId, projectId: task.projectId },
        (operation) => this.cancelWithinConnection(operation, requestId, actor)
      );
    } catch (error) {
      if (error?.code !== "MYFORGE_AGENT_DISCONNECTED" && error?.code !== "MYFORGE_IDENTITY_MISMATCH") {
        throw error;
      }
      return this.finishCancellationAfterSettlement(task, requestId, actor);
    }
  }

  async finishCancellationAfterSettlement(task, requestId, actor) {
    await this.gateway.waitForConnectionSettlement?.(task.connectionId);
    const latest = await this.store.getTask(requestId);
    if (!latest) fail("MYFORGE_TASK_NOT_FOUND", "Task was not found", 404);
    if (latest.status === "cancelled") return cancellationResponse(latest);
    if (NON_CANCELLABLE_STATUSES.has(latest.status)) {
      fail("MYFORGE_TASK_NOT_CANCELLABLE", "Task is already complete", 409);
    }
    if (latest.status === "queued") {
      const cancellation = await this.store.requestTaskCancellation({
        requestId,
        adminId: actor.adminId ?? null,
        adminUsername: actor.adminUsername ?? null,
        ip: actor.ip ?? null,
        requestedAt: this.now(),
        queuedOnly: true
      });
      return cancellationResponse(cancellation.task);
    }

    const current = this.gateway.getRegisteredConnection(latest.agentId, latest.projectId);
    if (current?.connectionId === latest.connectionId) {
      try {
        return await this.gateway.withConnectionOperation(
          { agentId: latest.agentId, projectId: latest.projectId },
          (operation) => this.cancelWithinConnection(operation, requestId, actor)
        );
      } catch (error) {
        if (error?.code !== "MYFORGE_AGENT_DISCONNECTED" && error?.code !== "MYFORGE_IDENTITY_MISMATCH") {
          throw error;
        }
      }
    }

    const errorCode = latest.cancelRequestedAt
      ? "MYFORGE_CANCEL_UNCONFIRMED"
      : "MYFORGE_AGENT_DISCONNECTED";
    const failed = await this.store.failTask({
      requestId,
      expectedStatuses: ["dispatched", "running"],
      errorCode,
      errorMessage: latest.cancelRequestedAt
        ? "Cancellation could not be confirmed after the agent disconnected"
        : "Agent disconnected before cancellation could be requested",
      completedAt: this.now(),
      adminId: actor.adminId ?? null,
      adminUsername: actor.adminUsername ?? null,
      ip: actor.ip ?? null
    });
    return cancellationResponse(failed.task);
  }

  async dispatchNext({ agentId, projectId }) {
    if (!this.enabled) return null;
    try {
      return await this.gateway.withConnectionOperation({ agentId, projectId }, async (operation) => {
        for (;;) {
          const task = await this.store.findNextQueuedTask({ agentId, projectId, now: this.now() });
          if (!task) return null;
          const reservation = operation.reserveDelivery({
            requestId: task.requestId,
            kind: "command.execute"
          });

          let prepared;
          try {
            prepared = operation.prepareExecute({
              requestId: task.requestId,
              taskType: task.taskType,
              profile: "codex_exec",
              input: {
                artifactFile: task.artifactFile,
                consumerTargetFile: task.consumerTargetFile,
                rulesFile: task.rulesFile,
                prompt: task.prompt,
                renderedPrompt: task.renderedPrompt
              },
              timeoutMs: operation.connection.effectiveLimits.commandTimeoutMs,
              maxOutputBytes: operation.connection.effectiveLimits.maxOutputBytes
            });
          } catch (error) {
            operation.releaseDelivery(reservation);
            if (error?.code === "MYFORGE_AGENT_DISCONNECTED") throw error;
            const failed = await this.store.failTask({
              requestId: task.requestId,
              expectedStatuses: ["queued"],
              errorCode: "MYFORGE_DISPATCH_FAILED",
              errorMessage: safeFailureMessage("MYFORGE_DISPATCH_FAILED"),
              completedAt: this.now()
            });
            return failed.task;
          }

          let claimed;
          try {
            claimed = await this.store.claimTaskDispatched({
              requestId: task.requestId,
              agentId,
              projectId,
              connectionId: operation.connection.connectionId,
              executionMode: operation.connection.capabilities.dryRun ? "dry_run" : "codex_exec",
              dangerFullAccess: operation.connection.capabilities.dangerFullAccess,
              commandPreview: buildCommandPreview(
                task.renderedPrompt,
                operation.connection.capabilities.dangerFullAccess
              ),
              commandDigest: prepared.semanticDigest,
              commandExpiresAt: prepared.expiresAt,
              timeoutMs: operation.connection.effectiveLimits.commandTimeoutMs,
              maxOutputBytes: operation.connection.effectiveLimits.maxOutputBytes,
              dispatchedAt: new Date(prepared.message.timestampMs)
            });
          } catch (error) {
            operation.releaseDelivery(reservation);
            throw error;
          }
          if (!claimed) {
            operation.releaseDelivery(reservation);
            const current = await this.store.getTask(task.requestId);
            if (!current || TERMINAL_STATUSES.has(current.status)) continue;
            if (asMillis(current.queueExpiresAt) <= this.clock()) {
              await this.store.failTask({
                requestId: task.requestId,
                expectedStatuses: ["queued"],
                errorCode: "MYFORGE_QUEUE_EXPIRED",
                errorMessage: safeFailureMessage("MYFORGE_QUEUE_EXPIRED"),
                completedAt: this.now()
              });
              continue;
            }
            operation.assertCurrent();
            await this.store.setQueuedTasksReasonForAgent({ agentId, projectId, queueReason: "agent_busy" });
            operation.assertCurrent();
            return current;
          }

          try {
            await operation.send(prepared);
          } catch {
            operation.releaseDelivery(reservation);
            const failed = await this.store.failTask({
              requestId: task.requestId,
              expectedStatuses: ["dispatched"],
              errorCode: "MYFORGE_DISPATCH_FAILED",
              errorMessage: safeFailureMessage("MYFORGE_DISPATCH_FAILED"),
              completedAt: this.now()
            });
            return failed.task;
          }
          await this.store.setQueuedTasksReasonForAgent({ agentId, projectId, queueReason: "agent_busy" });
          operation.assertCurrent();
          return claimed;
        }
      });
    } catch (error) {
      if (error?.code !== "MYFORGE_AGENT_DISCONNECTED") throw error;
      await this.store.setQueuedTasksReasonForAgent({ agentId, projectId, queueReason: "agent_offline" });
      return null;
    }
  }

  async onAgentRegistered({ agentId, projectId }) {
    await this.dispatchNext({ agentId, projectId });
  }

  async onAgentDisconnected({ agentId, projectId }) {
    if (this.gateway.getRegisteredConnection(agentId, projectId)) return;
    await this.store.setQueuedTasksReasonForAgent({ agentId, projectId, queueReason: "agent_offline" });
  }

  async onTaskTerminal(task) {
    if (!task || !TERMINAL_STATUSES.has(task.status)) return;
    await this.dispatchNext({ agentId: task.agentId, projectId: task.projectId });
  }

  async tick() {
    if (!this.enabled || this.watchdogStopping) {
      return { queueExpired: 0, commandExpired: 0, commandTimedOut: 0, cancelTimedOut: 0 };
    }
    return this.watchdogMutex.runExclusive(async () => {
      const now = this.now();
      const attemptedAgents = new Set();
      const cancelTimedOut = await this.store.failExpiredCancellationTasks({
        now,
        clockSkewMs: this.config.clockSkewMs
      });
      await this.closeTimedOutTasks(cancelTimedOut);
      const commandTimedOut = await this.store.failTimedOutRunningTasks({
        now,
        clockSkewMs: this.config.clockSkewMs
      });
      await this.closeTimedOutTasks(commandTimedOut);
      const commandExpired = await this.store.failExpiredDispatchedTasks({
        now,
        clockSkewMs: this.config.clockSkewMs
      });
      await this.dispatchAffectedTasks(commandExpired, "command_expired", attemptedAgents);
      const queueExpired = await this.store.failExpiredQueuedTasks(now);
      await this.dispatchAffectedTasks(queueExpired, "queue_expired", attemptedAgents);
      const queuedAgents = await this.store.listQueuedAgentIdentities(now);
      for (const identity of queuedAgents) {
        const key = `${identity.agentId}\n${identity.projectId}`;
        if (attemptedAgents.has(key)) continue;
        attemptedAgents.add(key);
        await this.dispatchBestEffort({ ...identity, requestId: null }, "periodic_scan");
      }
      return {
        queueExpired: queueExpired.length,
        commandExpired: commandExpired.length,
        commandTimedOut: commandTimedOut.length,
        cancelTimedOut: cancelTimedOut.length
      };
    });
  }

  async closeTimedOutTasks(tasks) {
    for (const task of tasks) {
      try {
        await this.gateway.closeTaskConnection({
          agentId: task.agentId,
          projectId: task.projectId,
          connectionId: task.connectionId,
          reason: task.errorCode === "MYFORGE_CANCEL_TIMEOUT" ? "cancel_timeout" : "command_timeout"
        });
      } catch (error) {
        log("error", "myforge.watchdog_connection_close_failed", {
          requestId: task.requestId,
          agentId: task.agentId,
          errorCode: error?.code ?? error?.name ?? "UNKNOWN_ERROR"
        });
      }
    }
  }

  async dispatchAffectedTasks(tasks, trigger, attemptedAgents = new Set()) {
    const identities = new Map();
    for (const task of tasks) {
      identities.set(`${task.agentId}\n${task.projectId}`, task);
    }
    for (const task of identities.values()) {
      const key = `${task.agentId}\n${task.projectId}`;
      if (attemptedAgents.has(key)) continue;
      attemptedAgents.add(key);
      await this.dispatchBestEffort(task, trigger);
    }
  }

  assertRequestId(requestId) {
    if (!isUuidV4(requestId)) fail("INVALID_REQUEST", "requestId must be a lowercase UUID v4", 400);
  }
}

export {
  cancellationResponse,
  createResponse,
  publicAgent,
  publicTaskDetail,
  publicTaskListItem
};
