import {
  AsyncMutex,
  MyforgeProtocolError,
  randomBase64Url,
  randomUuidV4,
  serializeMessage,
  signMessage
} from "./protocol.js";
import { validateMessageSchema } from "./schemas.js";

const QUEUE_CAPACITY = 64;

function timeoutError(message) {
  const error = new Error(message);
  error.code = "MYFORGE_WS_WRITE_FAILED";
  return error;
}

function messageExpiredError(message = "outbound message expired before it could be written") {
  return new MyforgeProtocolError("MYFORGE_MESSAGE_EXPIRED", message);
}

function byteLength(value) {
  if (typeof value === "string") return Buffer.byteLength(value, "utf8");
  if (Buffer.isBuffer(value)) return value.length;
  if (value instanceof ArrayBuffer) return value.byteLength;
  if (ArrayBuffer.isView(value)) return value.byteLength;
  return Number.MAX_SAFE_INTEGER;
}

export class MyforgeConnection {
  constructor({ socket, request, gateway, configuredAgent, config, clock = Date.now }) {
    this.socket = socket;
    this.request = request;
    this.gateway = gateway;
    this.config = config;
    this.clock = clock;
    this.configuredAgent = configuredAgent;
    this.agentId = configuredAgent.agentId;
    this.projectId = configuredAgent.projectId;
    this.clientIp = request?.ip ?? request?.socket?.remoteAddress ?? null;
    this.challengeId = randomUuidV4();
    this.challenge = randomBase64Url(32);
    this.connectionId = this.challengeId;
    this.state = "connected";
    this.registered = false;
    this.capabilities = null;
    this.effectiveLimits = null;
    this.inbound = [];
    this.outbound = [];
    this.outboundCapacityWaiters = [];
    this.dispatching = false;
    this.writing = false;
    this.activeWriteAbort = null;
    this.closing = false;
    this.closed = false;
    this.closeReason = null;
    this.operationMutex = new AsyncMutex();
    this.timers = new Set();
    this.finalizePromise = null;

    socket.on("message", (data, isBinary) => this.enqueueInbound(data, isBinary));
    socket.on("close", (code, reason) => {
      void this.finalize(code, reason?.toString?.() || "socket_closed");
    });
    socket.on("error", (error) => {
      if (error?.code === "WS_ERR_UNSUPPORTED_MESSAGE_LENGTH") {
        void this.gateway.handleConnectionError(
          this,
          new MyforgeProtocolError(
            "MYFORGE_OUTPUT_TOO_LARGE",
            "WebSocket frame exceeds the configured limit",
            { safeToRespond: false }
          )
        );
      } else {
        void this.transportFailure(error);
      }
    });
  }

  serverLimits() {
    return {
      authTtlMs: this.config.authTtlMs,
      commandTtlMs: this.config.commandTtlMs,
      clockSkewMs: this.config.clockSkewMs,
      heartbeatIntervalMs: this.config.heartbeatIntervalMs,
      heartbeatTimeoutMs: this.config.heartbeatTimeoutMs,
      commandTimeoutMs: this.config.commandTimeoutMs,
      cancelTimeoutMs: this.config.cancelTimeoutMs,
      maxOutputBytes: this.config.maxOutputBytes,
      wsMaxMessageBytes: this.config.wsMaxMessageBytes
    };
  }

  async start() {
    if (this.closed || this.closing) return;
    const timestampMs = this.clock();
    const challenge = {
      protocolVersion: 1,
      type: "server.challenge",
      challengeId: this.challengeId,
      challenge: this.challenge,
      agentId: this.agentId,
      projectId: this.projectId,
      limits: this.serverLimits(),
      timestampMs,
      expiresAtMs: timestampMs + this.config.authTtlMs,
      nonce: randomBase64Url(16)
    };
    this.state = "challenged";
    this.setDeadline(this.config.authTtlMs + this.config.clockSkewMs, "challenge_timeout");
    await this.sendSigned(challenge);
  }

  setDeadline(delayMs, reason) {
    this.clearTimers();
    const timer = setTimeout(() => {
      this.timers.delete(timer);
      void this.gateway.handleConnectionDeadline(this, reason);
    }, delayMs);
    timer.unref?.();
    this.timers.add(timer);
  }

  armRegisterDeadline() {
    this.setDeadline(10000, "register_timeout");
  }

  armHeartbeatDeadline() {
    this.setDeadline(this.config.heartbeatTimeoutMs, "heartbeat_timeout");
  }

  clearTimers() {
    for (const timer of this.timers) clearTimeout(timer);
    this.timers.clear();
  }

  enqueueInbound(data, isBinary = false) {
    if (this.closed || this.closing) return;
    if (isBinary) {
      void this.gateway.handleConnectionError(
        this,
        new MyforgeProtocolError(
          "MYFORGE_MESSAGE_IJSON_INVALID",
          "binary WebSocket frames are not accepted",
          { safeToRespond: false }
        )
      );
      return;
    }
    const frameLimit = this.effectiveLimits?.wsMaxMessageBytes ?? this.config.wsMaxMessageBytes;
    if (byteLength(data) > frameLimit) {
      void this.gateway.handleConnectionError(
        this,
        new MyforgeProtocolError(
          this.effectiveLimits ? "MYFORGE_LIMIT_MISMATCH" : "MYFORGE_OUTPUT_TOO_LARGE",
          "WebSocket frame exceeds the accepted limit",
          { safeToRespond: false }
        )
      );
      return;
    }
    if (this.inbound.length >= QUEUE_CAPACITY) {
      void this.gateway.handleConnectionError(
        this,
        new MyforgeProtocolError(
          "MYFORGE_PROTOCOL_STATE_INVALID",
          "inbound WebSocket queue is full",
          { safeToRespond: false }
        )
      );
      return;
    }
    this.inbound.push(data);
    if (!this.dispatching) void this.drainInbound();
  }

  async drainInbound() {
    if (this.dispatching) return;
    this.dispatching = true;
    try {
      while (!this.closed && !this.closing && this.inbound.length > 0) {
        const frame = this.inbound.shift();
        try {
          await this.gateway.handleFrame(this, frame);
        } catch (error) {
          await this.gateway.handleConnectionError(this, error);
          break;
        }
      }
    } finally {
      this.dispatching = false;
    }
  }

  async sendSigned(unsignedMessage) {
    if (this.closed || this.closing) throw timeoutError("connection is closed");
    const signed = signMessage(unsignedMessage, this.config.serverPrivateKey);
    validateMessageSchema(signed);
    const frame = serializeMessage(signed);
    if (Buffer.byteLength(frame, "utf8") > (this.effectiveLimits?.wsMaxMessageBytes ?? this.config.wsMaxMessageBytes)) {
      throw new MyforgeProtocolError("MYFORGE_OUTPUT_TOO_LARGE", "outbound WebSocket frame exceeds the negotiated limit");
    }
    await this.enqueueOutbound(frame, { expiresAtMs: signed.expiresAtMs });
    return signed;
  }

  outboundDeadlineError({ expiresAtMs = null, cancelDeadlineAtMs = null }, nowMs = this.clock()) {
    if ((expiresAtMs !== null && nowMs >= expiresAtMs) ||
        (cancelDeadlineAtMs !== null && nowMs >= cancelDeadlineAtMs)) {
      return messageExpiredError();
    }
    return timeoutError("WebSocket send timed out");
  }

  assertOutboundFresh(item) {
    const nowMs = this.clock();
    if ((item.expiresAtMs !== null && nowMs >= item.expiresAtMs) ||
        (item.cancelDeadlineAtMs !== null && nowMs >= item.cancelDeadlineAtMs)) {
      throw messageExpiredError();
    }
  }

  async waitForOutboundCapacity(deadline, limits) {
    while (!this.closed && !this.closing && this.outbound.length >= QUEUE_CAPACITY) {
      const remaining = deadline - this.clock();
      if (remaining <= 0) throw this.outboundDeadlineError(limits);
      let timer;
      await new Promise((resolve, reject) => {
        const waiter = () => {
          clearTimeout(timer);
          resolve();
        };
        this.outboundCapacityWaiters.push(waiter);
        timer = setTimeout(() => {
          const index = this.outboundCapacityWaiters.indexOf(waiter);
          if (index >= 0) this.outboundCapacityWaiters.splice(index, 1);
          reject(this.outboundDeadlineError(limits));
        }, remaining);
        timer.unref?.();
      });
    }
    if (this.closed || this.closing) throw timeoutError("connection is closed");
  }

  notifyOutboundCapacity() {
    const waiter = this.outboundCapacityWaiters.shift();
    waiter?.();
  }

  async enqueueOutbound(frame, { expiresAtMs = null, cancelDeadlineAtMs = null } = {}) {
    const nowMs = this.clock();
    const limits = { expiresAtMs, cancelDeadlineAtMs };
    const deadline = Math.min(
      nowMs + this.config.wsWriteTimeoutMs,
      expiresAtMs ?? Number.MAX_SAFE_INTEGER,
      cancelDeadlineAtMs ?? Number.MAX_SAFE_INTEGER
    );
    if (deadline <= nowMs) throw this.outboundDeadlineError(limits, nowMs);
    try {
      await this.waitForOutboundCapacity(deadline, limits);
    } catch (error) {
      await this.transportFailure(error);
      throw error;
    }
    return new Promise((resolve, reject) => {
      const item = {
        frame,
        resolve,
        reject,
        deadline,
        expiresAtMs,
        cancelDeadlineAtMs,
        queueTimer: null
      };
      item.queueTimer = setTimeout(() => {
        const index = this.outbound.indexOf(item);
        if (index < 0) return;
        this.outbound.splice(index, 1);
        const error = this.outboundDeadlineError(item);
        item.reject(error);
        void this.transportFailure(error);
      }, deadline - this.clock());
      item.queueTimer.unref?.();
      this.outbound.push(item);
      if (!this.writing) void this.drainOutbound();
    });
  }

  async drainOutbound() {
    if (this.writing) return;
    this.writing = true;
    try {
      while (!this.closed && this.outbound.length > 0) {
        const item = this.outbound.shift();
        clearTimeout(item.queueTimer);
        this.notifyOutboundCapacity();
        try {
          this.assertOutboundFresh(item);
          await this.writeFrame(item);
          item.resolve();
        } catch (error) {
          item.reject(error);
          await this.transportFailure(error);
          break;
        }
      }
    } finally {
      this.writing = false;
    }
  }

  writeFrame(item) {
    try {
      this.assertOutboundFresh(item);
    } catch (error) {
      return Promise.reject(error);
    }
    const remaining = item.deadline - this.clock();
    if (remaining <= 0) return Promise.reject(this.outboundDeadlineError(item));
    return new Promise((resolve, reject) => {
      let settled = false;
      const timer = setTimeout(() => {
        if (settled) return;
        settled = true;
        this.activeWriteAbort = null;
        reject(this.outboundDeadlineError(item));
      }, remaining);
      timer.unref?.();
      const complete = (error) => {
        if (settled) return;
        if (!error && this.clock() >= item.deadline) {
          error = this.outboundDeadlineError(item);
        }
        settled = true;
        clearTimeout(timer);
        this.activeWriteAbort = null;
        if (error) reject(error);
        else resolve();
      };
      this.activeWriteAbort = (error) => complete(error);
      try {
        this.assertOutboundFresh(item);
        this.socket.send(item.frame, complete);
      } catch (error) {
        complete(error);
      }
    });
  }

  rejectPending(error) {
    for (const item of this.outbound.splice(0)) {
      clearTimeout(item.queueTimer);
      item.reject(error);
    }
    for (const waiter of this.outboundCapacityWaiters.splice(0)) waiter();
  }

  async transportFailure(error) {
    if (this.closed) return this.finalizePromise;
    this.rejectPending(error);
    return this.close(1011, "websocket_transport_failure");
  }

  async close(code = 1008, reason = "policy_violation") {
    if (this.closed) return this.finalizePromise;
    if (!this.closing) {
      this.closing = true;
      this.closeReason = reason;
      try {
        this.socket.close(code, reason.slice(0, 123));
      } catch {
        try {
          this.socket.terminate?.();
        } catch {
          // The connection is already unusable.
        }
      }
    }
    return this.finalize(code, reason);
  }

  async finalize(code = 1006, reason = "socket_closed") {
    if (this.finalizePromise) return this.finalizePromise;
    this.finalizePromise = (async () => {
      this.closed = true;
      this.closing = true;
      this.state = "closed";
      this.closeReason = this.closeReason ?? reason;
      this.clearTimers();
      this.inbound.length = 0;
      const closeError = timeoutError("connection closed before send completed");
      this.activeWriteAbort?.(closeError);
      this.rejectPending(closeError);
      await this.gateway.onConnectionClosed(this, { code, reason: this.closeReason });
    })();
    return this.finalizePromise;
  }
}

export const MYFORGE_CONNECTION_QUEUE_CAPACITY = QUEUE_CAPACITY;
