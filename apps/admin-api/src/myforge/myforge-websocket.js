import websocket from "@fastify/websocket";

import { log } from "../logger.js";
import { MyforgeConnection } from "./myforge-connection.js";
import {
  MYFORGE_SUBPROTOCOL,
  MyforgeProtocolError,
  ReplayCache,
  assertCanonicalMessageFrame,
  jcsCanonicalize,
  parseStrictJson,
  randomBase64Url,
  semanticDigest,
  serializeMessage,
  signMessage,
  validateMessageTime,
  verifyMessageSignature
} from "./protocol.js";
import { validateMessageSchema } from "./schemas.js";

const ID_PATTERN = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;
const AUTH_MESSAGE_TYPES = new Set([
  "agent.hello",
  "agent.register",
  "agent.heartbeat",
  "command.started",
  "command.result",
  "command.error",
  "protocol.error"
]);
const REGISTERED_MESSAGE_TYPES = new Set([
  "agent.heartbeat",
  "command.started",
  "command.result",
  "command.error",
  "protocol.error"
]);
const WEBSOCKET_ERROR_CODES = new Set([
  "MYFORGE_AGENT_AUTH_FAILED",
  "MYFORGE_AGENT_UNKNOWN",
  "MYFORGE_IDENTITY_MISMATCH",
  "MYFORGE_SERVER_SIGNATURE_INVALID",
  "MYFORGE_AGENT_SIGNATURE_INVALID",
  "MYFORGE_MESSAGE_EXPIRED",
  "MYFORGE_REPLAY_DETECTED",
  "MYFORGE_LIMIT_MISMATCH",
  "MYFORGE_MESSAGE_IJSON_INVALID",
  "MYFORGE_MESSAGE_SCHEMA_INVALID",
  "MYFORGE_PROTOCOL_VERSION_UNSUPPORTED",
  "MYFORGE_PROTOCOL_STATE_INVALID",
  "MYFORGE_DUPLICATE_REQUEST_CONFLICT",
  "MYFORGE_DUPLICATE_RESULT_CONFLICT",
  "MYFORGE_AGENT_BUSY",
  "MYFORGE_AGENT_DISCONNECTED",
  "MYFORGE_SERVER_RESTARTED",
  "MYFORGE_OUTPUT_TOO_LARGE"
]);
const SAFE_ERROR_MESSAGES = Object.freeze({
  MYFORGE_AGENT_AUTH_FAILED: "agent authentication failed",
  MYFORGE_AGENT_UNKNOWN: "agent is not configured",
  MYFORGE_IDENTITY_MISMATCH: "agent identity does not match this connection",
  MYFORGE_SERVER_SIGNATURE_INVALID: "server message signature is invalid",
  MYFORGE_AGENT_SIGNATURE_INVALID: "message signature is invalid",
  MYFORGE_MESSAGE_EXPIRED: "message is outside the accepted time window",
  MYFORGE_REPLAY_DETECTED: "message nonce was already used",
  MYFORGE_LIMIT_MISMATCH: "message limits do not match the connection",
  MYFORGE_MESSAGE_IJSON_INVALID: "message is not valid interoperable JSON",
  MYFORGE_MESSAGE_SCHEMA_INVALID: "message schema is invalid",
  MYFORGE_PROTOCOL_VERSION_UNSUPPORTED: "protocol version is unsupported",
  MYFORGE_PROTOCOL_STATE_INVALID: "message is not valid in the current connection state",
  MYFORGE_DUPLICATE_REQUEST_CONFLICT: "request conflicts with an existing request",
  MYFORGE_DUPLICATE_RESULT_CONFLICT: "result conflicts with the stored terminal result",
  MYFORGE_AGENT_BUSY: "agent protocol capacity is exhausted",
  MYFORGE_SERVER_RESTARTED: "server connection is restarting",
  MYFORGE_OUTPUT_TOO_LARGE: "message exceeds the negotiated size limit"
});

function protocolError(code, message, options = {}) {
  return new MyforgeProtocolError(code, message, options);
}

function asMillis(value) {
  return value === null || value === undefined ? null : new Date(value).getTime();
}

function exactKeys(value, expected, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw protocolError("MYFORGE_MESSAGE_SCHEMA_INVALID", `${label} must be an object`);
  }
  const keys = Object.keys(value);
  if (keys.length !== expected.length || keys.some((key) => !expected.includes(key))) {
    throw protocolError("MYFORGE_MESSAGE_SCHEMA_INVALID", `${label} has an invalid field set`);
  }
}

export function negotiateMyforgeLimits(server, agent) {
  if (agent.heartbeatIntervalMs !== server.heartbeatIntervalMs) {
    throw protocolError("MYFORGE_LIMIT_MISMATCH", "heartbeatIntervalMs differs between server and agent");
  }
  if (agent.authTtlMs < server.authTtlMs) {
    throw protocolError("MYFORGE_LIMIT_MISMATCH", "agent authTtlMs is below the challenge lifetime");
  }
  if (2 * agent.clockSkewMs >= agent.authTtlMs || 2 * agent.clockSkewMs >= agent.commandTtlMs ||
      agent.cancelTimeoutMs > agent.maxCommandTimeoutMs) {
    throw protocolError("MYFORGE_LIMIT_MISMATCH", "agent limits violate protocol invariants");
  }
  const wsMaxMessageBytes = Math.min(server.wsMaxMessageBytes, agent.wsMaxMessageBytes);
  const frameOutputBudget = Math.floor((wsMaxMessageBytes - 262144) / 12);
  const maxOutputBytes = Math.min(server.maxOutputBytes, agent.maxOutputBytes, frameOutputBudget);
  if (maxOutputBytes < 4096) {
    throw protocolError("MYFORGE_LIMIT_MISMATCH", "negotiated output budget is below 4096 bytes");
  }
  return {
    authTtlMs: Math.min(server.authTtlMs, agent.authTtlMs),
    commandTtlMs: Math.min(server.commandTtlMs, agent.commandTtlMs),
    serverClockSkewMs: server.clockSkewMs,
    agentClockSkewMs: agent.clockSkewMs,
    heartbeatIntervalMs: server.heartbeatIntervalMs,
    heartbeatTimeoutMs: server.heartbeatTimeoutMs,
    commandTimeoutMs: Math.min(server.commandTimeoutMs, agent.maxCommandTimeoutMs),
    cancelTimeoutMs: Math.min(server.cancelTimeoutMs, agent.cancelTimeoutMs),
    maxOutputBytes,
    wsMaxMessageBytes
  };
}

function normalizeError(error) {
  if (error instanceof MyforgeProtocolError) return error;
  const code = typeof error?.code === "string" && WEBSOCKET_ERROR_CODES.has(error.code)
    ? error.code
    : "MYFORGE_PROTOCOL_STATE_INVALID";
  return protocolError(code, error?.message || "protocol operation failed");
}

function publicTaskFields(message) {
  return {
    requestId: message.requestId ?? null,
    messageType: message.type ?? null
  };
}

export class MyforgeWebsocketGateway {
  constructor({ config, store, adminStore, clock = Date.now, replayCacheMaxEntries = 65536 }) {
    this.config = config;
    this.store = store;
    this.adminStore = adminStore;
    this.clock = clock;
    this.replayCache = new ReplayCache(replayCacheMaxEntries);
    this.connections = new Set();
    this.connectionsByAgent = new Map();
    this.connectionSettlements = new Map();
    this.shuttingDown = false;
    this.shutdownPromise = null;
    this.taskOrchestrator = null;
  }

  get enabled() {
    return this.config.enabled === true;
  }

  setTaskOrchestrator(orchestrator) {
    this.taskOrchestrator = orchestrator;
  }

  async notifyTaskOrchestrator(method, payload) {
    const callback = this.taskOrchestrator?.[method];
    if (typeof callback !== "function") return;
    try {
      await callback.call(this.taskOrchestrator, payload);
    } catch (error) {
      log("error", "myforge.orchestrator_callback_failed", {
        callback: method,
        errorCode: error?.code ?? error?.name ?? "UNKNOWN_ERROR"
      });
    }
  }

  async audit(code, connection, details = {}, severity = "warning") {
    try {
      await this.adminStore?.appendSecurityAuditLog?.({
        eventType: code,
        targetType: "myforge_agent",
        targetValue: connection?.agentId ?? details.agentId ?? null,
        severity,
        clientIp: connection?.clientIp ?? details.clientIp ?? null,
        details: {
          code,
          projectId: connection?.projectId ?? details.projectId ?? null,
          connectionId: connection?.connectionId ?? null,
          requestId: details.requestId ?? null,
          messageType: details.messageType ?? null,
          connectionState: connection?.state ?? null
        }
      });
    } catch (error) {
      log("warn", "myforge.security_audit_write_failed", {
        eventType: code,
        reason: error?.code ?? error?.name ?? "unknown_error"
      });
    }
  }

  async preValidateUpgrade(request, reply) {
    const isUpgrade = String(request.headers?.upgrade || "").toLowerCase() === "websocket";
    if (!isUpgrade) return;
    if (!this.enabled || this.shuttingDown) {
      return reply.code(503).send({ ok: false, error: "MYFORGE_DISABLED", message: "myforge agent channel is disabled" });
    }
    const protocols = String(request.headers?.["sec-websocket-protocol"] || "")
      .split(",")
      .map((value) => value.trim())
      .filter(Boolean);
    if (!protocols.includes(MYFORGE_SUBPROTOCOL)) {
      return reply.code(400).send({
        ok: false,
        error: "MYFORGE_PROTOCOL_VERSION_UNSUPPORTED",
        message: "required WebSocket subprotocol is missing"
      });
    }
    const query = request.query;
    const keys = query && typeof query === "object" ? Object.keys(query) : [];
    const agentId = query?.agentId;
    const projectId = query?.projectId;
    if (keys.length !== 2 || !keys.includes("agentId") || !keys.includes("projectId") ||
        typeof agentId !== "string" || typeof projectId !== "string" ||
        !ID_PATTERN.test(agentId) || !ID_PATTERN.test(projectId)) {
      return reply.code(400).send({ ok: false, error: "INVALID_REQUEST", message: "agentId and projectId are required" });
    }
    const configured = this.config.agentsById.get(agentId);
    if (!configured) {
      await this.audit("MYFORGE_AGENT_UNKNOWN", null, { agentId, projectId, clientIp: request.ip });
      return reply.code(404).send({ ok: false, error: "MYFORGE_AGENT_UNKNOWN", message: "agent is not configured" });
    }
    if (configured.projectId !== projectId) {
      await this.audit("MYFORGE_IDENTITY_MISMATCH", null, { agentId, projectId, clientIp: request.ip });
      return reply.code(409).send({ ok: false, error: "MYFORGE_IDENTITY_MISMATCH", message: "agent project does not match" });
    }
  }

  acceptSocket(socket, request) {
    const agentId = request.query.agentId;
    const configuredAgent = this.config.agentsById.get(agentId);
    if (!this.enabled || this.shuttingDown || !configuredAgent || socket.protocol !== MYFORGE_SUBPROTOCOL) {
      socket.close(1008, "myforge_websocket_rejected");
      return null;
    }
    const connection = new MyforgeConnection({
      socket,
      request,
      gateway: this,
      configuredAgent,
      config: this.config,
      clock: this.clock
    });
    this.connections.add(connection);
    connection.trackBackground(
      connection.start().catch((error) => this.handleConnectionError(connection, error))
    );
    return connection;
  }

  verifyFrame(connection, frame) {
    const message = parseStrictJson(frame, connection.config.wsMaxMessageBytes);
    if (!message || typeof message !== "object" || Array.isArray(message)) {
      throw protocolError("MYFORGE_MESSAGE_SCHEMA_INVALID", "message must be an object", { safeToRespond: false });
    }
    verifyMessageSignature(message, connection.configuredAgent.publicKey);
    assertCanonicalMessageFrame(frame, message);
    validateMessageSchema(message);
    return message;
  }

  validateConnectionIdentity(connection, message) {
    if (message.agentId !== connection.agentId || message.projectId !== connection.projectId) {
      throw protocolError("MYFORGE_IDENTITY_MISMATCH", "message identity does not match the connection", {
        requestId: message.requestId ?? null
      });
    }
    if (message.type === "agent.hello") {
      if (message.challengeId !== connection.challengeId || message.challenge !== connection.challenge) {
        throw protocolError("MYFORGE_AGENT_AUTH_FAILED", "challenge response does not match the connection");
      }
    } else if (message.connectionId !== connection.connectionId) {
      throw protocolError("MYFORGE_IDENTITY_MISMATCH", "connectionId does not match the socket", {
        requestId: message.requestId ?? null
      });
    }
  }

  validateState(connection, message) {
    if (message.type === "protocol.error") return;
    if (connection.state === "challenged" && message.type === "agent.hello") return;
    if (connection.state === "authenticated" && message.type === "agent.register") return;
    if (connection.state === "registered" && REGISTERED_MESSAGE_TYPES.has(message.type)) return;
    throw protocolError("MYFORGE_PROTOCOL_STATE_INVALID", "message is not valid in the current connection state", {
      requestId: message.requestId ?? null
    });
  }

  validateTime(connection, message) {
    if (!AUTH_MESSAGE_TYPES.has(message.type)) {
      throw protocolError("MYFORGE_PROTOCOL_STATE_INVALID", "server-bound message type is not accepted");
    }
    const exactLifetimeMs = new Set(["agent.hello", "agent.register"]).has(message.type)
      ? connection.config.authTtlMs
      : null;
    validateMessageTime(message, {
      nowMs: { now: this.clock(), clockSkewMs: connection.config.clockSkewMs },
      ttlMs: connection.config.authTtlMs,
      exactLifetimeMs
    });
  }

  recordReplay(connection, message) {
    const replayConnectionId = message.type === "agent.hello" ? connection.challengeId : connection.connectionId;
    const replayKey = `${replayConnectionId}\u0000${connection.projectId}\u0000${connection.agentId}\u0000${message.nonce}`;
    this.replayCache.checkAndInsert(
      replayKey,
      message.expiresAtMs + connection.config.clockSkewMs,
      this.clock()
    );
  }

  async handleFrame(connection, frame) {
    const message = this.verifyFrame(connection, frame);
    this.validateTime(connection, message);
    this.validateConnectionIdentity(connection, message);
    this.recordReplay(connection, message);
    this.validateState(connection, message);

    switch (message.type) {
      case "agent.hello":
        connection.state = "authenticated";
        connection.armRegisterDeadline();
        return;
      case "agent.register":
        return this.handleRegister(connection, message);
      case "agent.heartbeat":
        return this.handleHeartbeat(connection, message);
      case "command.started":
        return this.handleStarted(connection, message);
      case "command.result":
        return this.handleResult(connection, message);
      case "command.error":
        return this.handleCommandError(connection, message);
      case "protocol.error":
        return this.handlePeerProtocolError(connection, message);
      default:
        throw protocolError("MYFORGE_PROTOCOL_STATE_INVALID", "message type is not accepted by the server");
    }
  }

  async handleRegister(connection, message) {
    const effectiveLimits = negotiateMyforgeLimits(connection.serverLimits(), message.limits);
    const result = await this.store.registerAgent({
      agentId: connection.agentId,
      projectId: connection.projectId,
      publicKeyFingerprint: connection.configuredAgent.publicKeyFingerprint,
      hostname: message.hostname,
      platform: message.platform,
      agentVersion: message.agentVersion,
      forgeRootSummary: message.forgeRootSummary,
      capabilities: message.capabilities,
      limits: message.limits,
      effectiveLimits,
      connectionId: connection.connectionId,
      registeredAt: new Date(this.clock())
    });

    connection.capabilities = message.capabilities;
    connection.effectiveLimits = effectiveLimits;
    connection.registered = true;
    connection.state = "registered";
    const oldConnection = this.connectionsByAgent.get(connection.agentId);
    this.connectionsByAgent.set(connection.agentId, connection);
    connection.armHeartbeatDeadline();

    if (oldConnection && oldConnection !== connection) {
      await oldConnection.close(1008, "replaced_by_new_connection");
    } else if (result.replacedConnectionId && result.replacedConnectionId !== connection.connectionId) {
      const stale = [...this.connections].find((candidate) => candidate.connectionId === result.replacedConnectionId);
      await stale?.close(1008, "replaced_by_new_connection");
    }
    await this.notifyTaskOrchestrator("onAgentRegistered", {
      agentId: connection.agentId,
      projectId: connection.projectId,
      connectionId: connection.connectionId
    });
  }

  async handleHeartbeat(connection, message) {
    if (message.state === "running") {
      const task = await this.store.getTask(message.activeRequestId);
      if (!task || task.agentId !== connection.agentId || task.projectId !== connection.projectId ||
          task.connectionId !== connection.connectionId || !new Set(["dispatched", "running"]).has(task.status)) {
        throw protocolError("MYFORGE_PROTOCOL_STATE_INVALID", "heartbeat activeRequestId is not active", {
          requestId: message.activeRequestId
        });
      }
    }
    const result = await this.store.heartbeatAgent({
      agentId: connection.agentId,
      projectId: connection.projectId,
      connectionId: connection.connectionId,
      seenAt: new Date(this.clock())
    });
    if (result.staleConnection) {
      throw protocolError("MYFORGE_IDENTITY_MISMATCH", "connection is no longer current");
    }
    connection.armHeartbeatDeadline();
  }

  expectedExecutionMode(connection) {
    return connection.capabilities?.dryRun ? "dry_run" : "codex_exec";
  }

  async handleStarted(connection, message) {
    const now = this.clock();
    const task = await this.store.getTask(message.requestId);
    if (!task) throw protocolError("MYFORGE_PROTOCOL_STATE_INVALID", "task does not exist", { requestId: message.requestId });
    if (message.executionMode !== this.expectedExecutionMode(connection) || message.executionMode !== task.executionMode) {
      throw protocolError("MYFORGE_IDENTITY_MISMATCH", "execution mode does not match the task", { requestId: message.requestId });
    }
    const dispatchedAtMs = asMillis(task.dispatchedAt);
    const commandExpiresAtMs = asMillis(task.commandExpiresAt);
    if (message.startedAtMs < dispatchedAtMs - this.config.clockSkewMs ||
        message.startedAtMs > now + this.config.clockSkewMs ||
        message.startedAtMs > commandExpiresAtMs + this.config.clockSkewMs) {
      throw protocolError("MYFORGE_PROTOCOL_STATE_INVALID", "startedAtMs is outside the task window", { requestId: message.requestId });
    }
    await this.store.markTaskStarted({
      requestId: message.requestId,
      agentId: connection.agentId,
      projectId: connection.projectId,
      connectionId: connection.connectionId,
      executionMode: message.executionMode,
      startedAt: new Date(message.startedAtMs)
    });
  }

  async handleResult(connection, message) {
    const now = this.clock();
    const task = await this.store.getTask(message.requestId);
    if (!task) throw protocolError("MYFORGE_PROTOCOL_STATE_INVALID", "task does not exist", { requestId: message.requestId });
    if (message.executionMode !== this.expectedExecutionMode(connection) || message.executionMode !== task.executionMode) {
      throw protocolError("MYFORGE_IDENTITY_MISMATCH", "execution mode does not match the task", { requestId: message.requestId });
    }
    const maxOutputBytes = connection.effectiveLimits.maxOutputBytes;
    if (Buffer.byteLength(message.stdoutPreview, "utf8") > maxOutputBytes ||
        Buffer.byteLength(message.stderrPreview, "utf8") > maxOutputBytes) {
      throw protocolError("MYFORGE_OUTPUT_TOO_LARGE", "result preview exceeds negotiated output limits", { requestId: message.requestId });
    }
    const resultDigest = semanticDigest(message);
    const terminalTask = new Set(["completed", "completed_with_errors", "failed", "cancelled"]).has(task.status);
    if (!terminalTask) {
      const storedStartedAtMs = asMillis(task.startedAt);
      const preStartCancellation = message.status === "cancelled" && task.cancelRequestedAt &&
        storedStartedAtMs === null && message.startedAtMs === null;
      if (message.startedAtMs !== storedStartedAtMs) {
        if (!preStartCancellation) {
          throw protocolError("MYFORGE_PROTOCOL_STATE_INVALID", "startedAtMs does not match the task", { requestId: message.requestId });
        }
      }
      if (message.completedAtMs > now + this.config.clockSkewMs ||
          (message.startedAtMs !== null && message.completedAtMs < message.startedAtMs)) {
        throw protocolError("MYFORGE_PROTOCOL_STATE_INVALID", "completedAtMs is outside the task window", { requestId: message.requestId });
      }
      if (preStartCancellation) {
        const dispatchedAtMs = asMillis(task.dispatchedAt);
        const cancelRequestedAtMs = asMillis(task.cancelRequestedAt);
        const cancelDeadlineAtMs = asMillis(task.cancelDeadlineAt);
        if (![dispatchedAtMs, cancelRequestedAtMs, cancelDeadlineAtMs].every(Number.isSafeInteger) ||
            message.completedAtMs < Math.max(dispatchedAtMs, cancelRequestedAtMs) - this.config.clockSkewMs ||
            message.completedAtMs > cancelDeadlineAtMs + this.config.clockSkewMs) {
          throw protocolError("MYFORGE_PROTOCOL_STATE_INVALID", "pre-start cancellation completedAtMs is outside the task window", {
            requestId: message.requestId
          });
        }
      }
      if (task.cancelRequestedAt) {
        const deadline = asMillis(task.cancelDeadlineAt);
        if (message.status !== "cancelled" || now > deadline + this.config.clockSkewMs) {
          throw protocolError("MYFORGE_DUPLICATE_RESULT_CONFLICT", "cancellation has priority over this result", { requestId: message.requestId });
        }
      }
    }
    const result = await this.store.recordTaskResult({
      requestId: message.requestId,
      agentId: connection.agentId,
      projectId: connection.projectId,
      connectionId: connection.connectionId,
      executionMode: message.executionMode,
      status: message.status,
      resultDigest,
      stdoutPreview: message.stdoutPreview,
      stderrPreview: message.stderrPreview,
      stdoutBytes: message.stdoutBytes,
      stderrBytes: message.stderrBytes,
      stdoutTruncated: message.stdoutTruncated,
      stderrTruncated: message.stderrTruncated,
      exitCode: message.exitCode,
      artifactFile: message.artifactFile,
      consumerTargetFile: message.consumerTargetFile,
      artifact: message.artifact,
      audit: message.audit,
      errorCode: message.errorCode,
      errorMessage: message.errorMessage,
      completedAt: new Date(message.completedAtMs)
    });
    await this.notifyTaskOrchestrator("onTaskTerminal", result.task);
  }

  async handleCommandError(connection, message) {
    const task = await this.store.getTask(message.requestId);
    if (!task || task.status !== "dispatched") {
      throw protocolError("MYFORGE_PROTOCOL_STATE_INVALID", "command.error requires a dispatched task", {
        requestId: message.requestId
      });
    }
    if (task.cancelRequestedAt) {
      throw protocolError("MYFORGE_DUPLICATE_RESULT_CONFLICT", "cancellation has priority over command.error", {
        requestId: message.requestId
      });
    }
    const result = await this.store.recordTaskError({
      requestId: message.requestId,
      agentId: connection.agentId,
      projectId: connection.projectId,
      connectionId: connection.connectionId,
      executionMode: this.expectedExecutionMode(connection),
      errorCode: message.errorCode,
      errorMessage: message.errorMessage,
      completedAt: new Date(this.clock())
    });
    await this.notifyTaskOrchestrator("onTaskTerminal", result.task);
  }

  async handlePeerProtocolError(connection, message) {
    await this.audit(message.errorCode, connection, publicTaskFields(message));
    if (message.fatal) await connection.close(1008, "peer_protocol_error");
  }

  async handleConnectionDeadline(connection, reason) {
    const code = reason === "heartbeat_timeout"
      ? "MYFORGE_AGENT_DISCONNECTED"
      : "MYFORGE_PROTOCOL_STATE_INVALID";
    await this.audit(code, connection, { messageType: reason });
    await connection.close(1008, reason);
  }

  async handleConnectionError(connection, rawError) {
    if (connection.closed) return;
    const error = normalizeError(rawError);
    const severity = error.code.includes("SIGNATURE") || error.code.includes("REPLAY") ? "critical" : "warning";
    await this.audit(error.code, connection, {
      requestId: error.requestId ?? null,
      messageType: null
    }, severity);

    if (error.safeToRespond && this.enabled && connection.state !== "connected" && !connection.closing) {
      const timestampMs = this.clock();
      const ttlMs = connection.effectiveLimits?.authTtlMs ?? connection.config.authTtlMs;
      const response = {
        protocolVersion: 1,
        type: "protocol.error",
        connectionId: connection.challengeId ?? null,
        agentId: connection.agentId,
        projectId: connection.projectId,
        requestId: error.requestId ?? null,
        errorCode: error.code,
        errorMessage: SAFE_ERROR_MESSAGES[error.code] ?? "protocol message was rejected",
        fatal: true,
        timestampMs,
        expiresAtMs: timestampMs + ttlMs,
        nonce: randomBase64Url(16)
      };
      try {
        await connection.sendSigned(response);
      } catch {
        // The transport is already failed; close below.
      }
    }
    await connection.close(1008, "protocol_error");
  }

  async onConnectionClosed(connection, { reason = "socket_closed" } = {}) {
    let resolveSettlement;
    const settlement = new Promise((resolve) => { resolveSettlement = resolve; });
    this.connectionSettlements.set(connection.connectionId, settlement);
    this.connections.delete(connection);
    if (this.connectionsByAgent.get(connection.agentId) === connection) {
      this.connectionsByAgent.delete(connection.agentId);
    }
    try {
      if (connection.registered) {
        try {
          const deliveryFailure = reason === "server_shutdown" ? null : connection.deliveryInProgress;
          const result = await this.store.markAgentOffline({
            agentId: connection.agentId,
            connectionId: connection.connectionId,
            disconnectedAt: new Date(this.clock()),
            failureReason: reason === "server_shutdown" ? "server_shutdown" : "agent_disconnected",
            ...(deliveryFailure ? { deliveryFailure } : {})
          });
          await this.notifyTaskOrchestrator("onAgentDisconnected", {
            agentId: connection.agentId,
            projectId: connection.projectId,
            connectionId: connection.connectionId,
            staleConnection: result.staleConnection
          });
        } catch (error) {
          await this.audit("MYFORGE_PROTOCOL_STATE_INVALID", connection, { messageType: "connection.close" });
        }
      }
    } finally {
      resolveSettlement();
      if (this.connectionSettlements.get(connection.connectionId) === settlement) {
        this.connectionSettlements.delete(connection.connectionId);
      }
    }
  }

  async waitForConnectionSettlement(connectionId) {
    const settlement = this.connectionSettlements.get(connectionId);
    if (settlement) await settlement;
  }

  getRegisteredConnection(agentId, projectId) {
    const connection = this.connectionsByAgent.get(agentId);
    if (!connection || connection.closed || !connection.acceptingOperations ||
        connection.state !== "registered" || connection.projectId !== projectId) {
      return null;
    }
    return connection;
  }

  snapshotConnection(connection) {
    return Object.freeze({
      agentId: connection.agentId,
      projectId: connection.projectId,
      connectionId: connection.connectionId,
      capabilities: structuredClone(connection.capabilities),
      effectiveLimits: structuredClone(connection.effectiveLimits)
    });
  }

  assertCurrentConnection(connection) {
    if (!connection || this.connectionsByAgent.get(connection.agentId) !== connection ||
        connection.closed || !connection.acceptingOperations || connection.state !== "registered") {
      throw protocolError("MYFORGE_AGENT_DISCONNECTED", "agent connection is not available");
    }
  }

  reserveCommandDelivery(connection, { requestId, kind }) {
    this.assertCurrentConnection(connection);
    if (typeof requestId !== "string" || !new Set(["command.execute", "command.cancel"]).has(kind)) {
      throw protocolError("MYFORGE_PROTOCOL_STATE_INVALID", "delivery intent is invalid");
    }
    if (connection.deliveryInProgress) {
      throw protocolError("MYFORGE_PROTOCOL_STATE_INVALID", "another command delivery is already in progress");
    }
    const reservation = Object.freeze({ requestId, kind });
    connection.deliveryInProgress = reservation;
    return reservation;
  }

  releaseCommandDelivery(connection, reservation) {
    if (connection.deliveryInProgress === reservation) {
      connection.deliveryInProgress = null;
    }
  }

  async withConnectionOperation({ agentId, projectId }, callback) {
    const connection = this.getRegisteredConnection(agentId, projectId);
    if (!connection) throw protocolError("MYFORGE_AGENT_DISCONNECTED", "agent connection is not available");
    return connection.operationMutex.runExclusive(async () => {
      this.assertCurrentConnection(connection);
      return callback({
        connection: this.snapshotConnection(connection),
        prepareExecute: (payload) => this.prepareCommandExecute(connection, payload),
        prepareCancel: (payload) => this.prepareCommandCancel(connection, payload),
        reserveDelivery: (intent) => this.reserveCommandDelivery(connection, intent),
        releaseDelivery: (reservation) => this.releaseCommandDelivery(connection, reservation),
        assertCurrent: () => this.assertCurrentConnection(connection),
        close: (code, reason) => connection.close(code, reason),
        send: (prepared) => this.sendPreparedCommand(connection, prepared)
      });
    });
  }

  prepareCommandExecute(connection, payload) {
    this.assertCurrentConnection(connection);
    exactKeys(payload, ["requestId", "taskType", "profile", "input", "timeoutMs", "maxOutputBytes"], "execute payload");
    if (payload.timeoutMs !== connection.effectiveLimits.commandTimeoutMs ||
        payload.maxOutputBytes !== connection.effectiveLimits.maxOutputBytes) {
      throw protocolError("MYFORGE_LIMIT_MISMATCH", "execute limits do not match the connection");
    }
    const timestampMs = this.clock();
    const unsigned = {
      protocolVersion: 1,
      type: "command.execute",
      connectionId: connection.connectionId,
      requestId: payload.requestId,
      taskType: payload.taskType,
      agentId: connection.agentId,
      projectId: connection.projectId,
      profile: payload.profile,
      input: payload.input,
      timeoutMs: payload.timeoutMs,
      maxOutputBytes: payload.maxOutputBytes,
      timestampMs,
      expiresAtMs: timestampMs + connection.effectiveLimits.commandTtlMs,
      nonce: randomBase64Url(16)
    };
    const message = signMessage(unsigned, this.config.serverPrivateKey);
    validateMessageSchema(message, "command.execute");
    return Object.freeze({
      kind: "command.execute",
      connectionId: connection.connectionId,
      message,
      frame: serializeMessage(message),
      semanticDigest: semanticDigest(message),
      expiresAt: new Date(message.expiresAtMs)
    });
  }

  prepareCommandCancel(connection, payload) {
    this.assertCurrentConnection(connection);
    exactKeys(payload, ["requestId", "cancelRequestedAtMs", "cancelDeadlineAtMs"], "cancel payload");
    if (payload.cancelDeadlineAtMs - payload.cancelRequestedAtMs !== connection.effectiveLimits.cancelTimeoutMs) {
      throw protocolError("MYFORGE_LIMIT_MISMATCH", "cancel deadline does not match the connection");
    }
    const timestampMs = Math.max(this.clock(), payload.cancelRequestedAtMs);
    if (timestampMs >= payload.cancelDeadlineAtMs) {
      throw protocolError("MYFORGE_MESSAGE_EXPIRED", "cancel deadline has already passed");
    }
    const unsigned = {
      protocolVersion: 1,
      type: "command.cancel",
      connectionId: connection.connectionId,
      requestId: payload.requestId,
      agentId: connection.agentId,
      projectId: connection.projectId,
      reasonCode: "ADMIN_CANCELLED",
      cancelRequestedAtMs: payload.cancelRequestedAtMs,
      cancelDeadlineAtMs: payload.cancelDeadlineAtMs,
      timestampMs,
      expiresAtMs: Math.min(timestampMs + connection.effectiveLimits.commandTtlMs, payload.cancelDeadlineAtMs),
      nonce: randomBase64Url(16)
    };
    const message = signMessage(unsigned, this.config.serverPrivateKey);
    validateMessageSchema(message, "command.cancel");
    return Object.freeze({
      kind: "command.cancel",
      connectionId: connection.connectionId,
      message,
      frame: serializeMessage(message),
      semanticDigest: semanticDigest(message),
      expiresAt: new Date(message.expiresAtMs)
    });
  }

  async sendPreparedCommand(connection, prepared) {
    if (!prepared || prepared.connectionId !== connection.connectionId ||
        !new Set(["command.execute", "command.cancel"]).has(prepared.kind)) {
      throw protocolError("MYFORGE_IDENTITY_MISMATCH", "prepared command belongs to another connection");
    }
    let reservation = connection.deliveryInProgress;
    if (reservation && (reservation.requestId !== prepared.message?.requestId || reservation.kind !== prepared.kind)) {
      this.releaseCommandDelivery(connection, reservation);
      throw protocolError("MYFORGE_PROTOCOL_STATE_INVALID", "prepared command does not match the delivery intent");
    }
    if (!reservation) {
      reservation = this.reserveCommandDelivery(connection, {
        requestId: prepared.message?.requestId,
        kind: prepared.kind
      });
    }
    try {
      this.assertCurrentConnection(connection);
      const parsed = parseStrictJson(prepared.frame, connection.effectiveLimits.wsMaxMessageBytes);
      validateMessageSchema(parsed, prepared.kind);
      verifyMessageSignature(parsed, this.config.serverPublicKey);
      if (jcsCanonicalize(parsed) !== prepared.frame) {
        throw protocolError("MYFORGE_MESSAGE_IJSON_INVALID", "prepared command is not canonical JSON");
      }
      if (parsed.requestId !== reservation.requestId || parsed.type !== reservation.kind) {
        throw protocolError("MYFORGE_PROTOCOL_STATE_INVALID", "prepared command does not match the delivery intent");
      }
      const nowMs = this.clock();
      const cancelDeadlineAtMs = parsed.type === "command.cancel" ? parsed.cancelDeadlineAtMs : null;
      if (nowMs >= parsed.expiresAtMs || (cancelDeadlineAtMs !== null && nowMs >= cancelDeadlineAtMs)) {
        throw protocolError("MYFORGE_MESSAGE_EXPIRED", "prepared command expired before send");
      }
      await connection.enqueueOutbound(prepared.frame, {
        expiresAtMs: parsed.expiresAtMs,
        cancelDeadlineAtMs
      });
      return prepared.message;
    } finally {
      this.releaseCommandDelivery(connection, reservation);
    }
  }

  async closeTaskConnection({ agentId, projectId, connectionId, reason }) {
    const connection = this.getRegisteredConnection(agentId, projectId);
    if (!connection || connection.connectionId !== connectionId) return false;
    connection.acceptingOperations = false;
    await connection.operationMutex.runExclusive(() => connection.close(1008, reason));
    return true;
  }

  shutdown() {
    if (this.shutdownPromise) return this.shutdownPromise;
    this.shuttingDown = true;
    const connections = [...this.connections];
    for (const connection of connections) connection.stopAcceptingWork();
    this.shutdownPromise = (async () => {
      await this.taskOrchestrator?.stop?.();
      await Promise.allSettled(connections.map((connection) => connection.close(1001, "server_shutdown")));
      await Promise.allSettled(connections.map((connection) => connection.waitForQuiescence()));
    })();
    return this.shutdownPromise;
  }
}

export async function registerMyforgeWebsocket(fastify, gateway, config) {
  await fastify.register(websocket, {
    options: {
      maxPayload: config.myforge.wsMaxMessageBytes,
      perMessageDeflate: false,
      handleProtocols(protocols) {
        return protocols.has(MYFORGE_SUBPROTOCOL) ? MYFORGE_SUBPROTOCOL : false;
      }
    }
  });

  fastify.route({
    method: "GET",
    url: "/api/v1/myforge/ws",
    preValidation: (request, reply) => gateway.preValidateUpgrade(request, reply),
    handler: (_request, reply) => reply.code(gateway.enabled ? 426 : 503).send({
      ok: false,
      error: gateway.enabled ? "WEBSOCKET_UPGRADE_REQUIRED" : "MYFORGE_DISABLED",
      message: gateway.enabled ? "WebSocket upgrade is required" : "myforge agent channel is disabled"
    }),
    wsHandler: (socket, request) => {
      gateway.acceptSocket(socket, request);
    }
  });
}
