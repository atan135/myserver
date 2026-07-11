import crypto from "node:crypto";

const ACTIVE_TASK_STATUSES = new Set(["queued", "dispatched", "running"]);
const TERMINAL_TASK_STATUSES = new Set([
  "completed",
  "completed_with_errors",
  "failed",
  "cancelled"
]);
const RESULT_TASK_STATUSES = new Set([
  "completed",
  "completed_with_errors",
  "failed",
  "cancelled"
]);
const QUEUE_REASONS = new Set(["agent_offline", "agent_busy"]);
const EXECUTION_MODES = new Set(["codex_exec", "dry_run"]);
const CANCELLATION_FAILURE_CODES = new Set([
  "MYFORGE_CANCEL_DELIVERY_FAILED",
  "MYFORGE_CANCEL_UNCONFIRMED",
  "MYFORGE_CANCEL_TIMEOUT"
]);

function toIsoString(value) {
  if (value === null || value === undefined) return null;
  if (value instanceof Date) return value.toISOString();
  return String(value);
}

function normalizeJson(value) {
  if (value === null || value === undefined) return null;
  if (typeof value !== "string") return value;
  try {
    return JSON.parse(value);
  } catch {
    return value;
  }
}

function toJsonb(value) {
  return value === null || value === undefined ? null : JSON.stringify(value);
}

function toNumber(value) {
  if (value === null || value === undefined) return null;
  const parsed = Number(value);
  return Number.isSafeInteger(parsed) ? parsed : value;
}

function toAgent(row) {
  if (!row) return null;
  return {
    agentId: row.agent_id,
    projectId: row.project_id,
    label: row.label ?? null,
    publicKeyFingerprint: row.public_key_fingerprint,
    configured: row.configured === true,
    status: row.status,
    hostname: row.hostname ?? null,
    platform: row.platform ?? null,
    agentVersion: row.agent_version ?? null,
    forgeRootSummary: normalizeJson(row.forge_root_summary_json),
    capabilities: normalizeJson(row.capabilities_json),
    limits: normalizeJson(row.limits_json),
    effectiveLimits: normalizeJson(row.effective_limits_json),
    lastRegisteredAt: toIsoString(row.last_registered_at),
    connectedAt: toIsoString(row.connected_at),
    lastSeenAt: toIsoString(row.last_seen_at),
    disconnectedAt: toIsoString(row.disconnected_at),
    createdAt: toIsoString(row.created_at),
    updatedAt: toIsoString(row.updated_at)
  };
}

function toTask(row) {
  if (!row) return null;
  return {
    requestId: row.request_id,
    taskType: row.task_type,
    projectId: row.project_id,
    agentId: row.agent_id,
    status: row.status,
    queueReason: row.queue_reason ?? null,
    executionMode: row.execution_mode ?? null,
    connectionId: row.connection_id ?? null,
    artifactFile: row.artifact_file,
    consumerTargetFile: row.consumer_target_file ?? null,
    rulesFile: row.rules_file,
    prompt: normalizeJson(row.prompt_json),
    renderedPrompt: row.rendered_prompt,
    commandPreview: row.command_preview,
    commandDigest: row.command_digest ?? null,
    commandExpiresAt: toIsoString(row.command_expires_at),
    timeoutMs: toNumber(row.timeout_ms),
    maxOutputBytes: toNumber(row.max_output_bytes),
    stdoutPreview: row.stdout_preview ?? null,
    stderrPreview: row.stderr_preview ?? null,
    stdoutBytes: toNumber(row.stdout_bytes),
    stderrBytes: toNumber(row.stderr_bytes),
    stdoutTruncated: row.stdout_truncated === true,
    stderrTruncated: row.stderr_truncated === true,
    exitCode: toNumber(row.exit_code),
    artifact: normalizeJson(row.artifact_json),
    audit: normalizeJson(row.audit_json),
    resultDigest: row.result_digest ?? null,
    errorCode: row.error_code ?? null,
    errorMessage: row.error_message ?? null,
    createdByAdminId: toNumber(row.created_by_admin_id),
    createdByAdminUsername: row.created_by_admin_username ?? null,
    createdAt: toIsoString(row.created_at),
    queueExpiresAt: toIsoString(row.queue_expires_at),
    dispatchedAt: toIsoString(row.dispatched_at),
    startedAt: toIsoString(row.started_at),
    cancelRequestedAt: toIsoString(row.cancel_requested_at),
    cancelDeadlineAt: toIsoString(row.cancel_deadline_at),
    completedAt: toIsoString(row.completed_at),
    updatedAt: toIsoString(row.updated_at)
  };
}

function pathSummary(value) {
  if (!value) return null;
  const parts = String(value).split("/");
  return {
    name: parts.at(-1),
    sha256: crypto.createHash("sha256").update(String(value), "utf8").digest("hex")
  };
}

function auditDetails(task, extra = {}) {
  return {
    requestId: task.requestId,
    taskType: task.taskType,
    agentId: task.agentId,
    projectId: task.projectId,
    status: task.status,
    paths: {
      artifact: pathSummary(task.artifactFile),
      consumerTarget: pathSummary(task.consumerTargetFile),
      rules: pathSummary(task.rulesFile)
    },
    errorCode: task.errorCode ?? null,
    timing: {
      createdAt: task.createdAt,
      dispatchedAt: task.dispatchedAt,
      startedAt: task.startedAt,
      cancelRequestedAt: task.cancelRequestedAt,
      completedAt: task.completedAt
    },
    ...extra
  };
}

export function createMyforgeStoreError(code, message = code, details = {}) {
  const error = new Error(message);
  error.code = code;
  Object.assign(error, details);
  return error;
}

function requireAllowed(value, allowed, code, field) {
  if (!allowed.has(value)) {
    throw createMyforgeStoreError(code, `${field} is invalid`);
  }
}

function sameInstant(left, right) {
  if (!left || !right) return left === right;
  return new Date(left).getTime() === new Date(right).getTime();
}

export class MyforgeStore {
  constructor(pool, config = {}) {
    this.pool = pool;
    this.config = config;
  }

  async withTransaction(callback) {
    const client = typeof this.pool.connect === "function" ? await this.pool.connect() : this.pool;
    const shouldRelease = typeof client.release === "function";
    try {
      await client.query("BEGIN");
      const result = await callback(client);
      await client.query("COMMIT");
      return result;
    } catch (error) {
      try {
        await client.query("ROLLBACK");
      } catch {
        // Preserve the operation failure.
      }
      throw error;
    } finally {
      if (shouldRelease) client.release();
    }
  }

  async appendLifecycleAudit(client, action, task, { actor = null, ip = null, details = {} } = {}) {
    const adminId = actor?.adminId ?? task.createdByAdminId ?? null;
    const adminUsername = actor?.adminUsername ?? task.createdByAdminUsername ?? null;
    await client.query(
      `INSERT INTO admin_audit_logs
         (admin_id, admin_username, action, target_type, target_value, details_json, ip)
       VALUES ($1, $2, $3, 'myforge_task', $4, $5::jsonb, $6)`,
      [
        adminId,
        adminUsername,
        action,
        task.requestId,
        JSON.stringify(auditDetails(task, details)),
        ip
      ]
    );
  }

  async syncKnownAgents(agents) {
    return this.withTransaction((client) => this.syncKnownAgentsInTransaction(client, agents));
  }

  async syncKnownAgentsInTransaction(client, agents) {
    await client.query(
      `UPDATE myforge_agents
       SET configured = false,
           status = 'offline',
           connection_id = NULL,
           disconnected_at = CASE WHEN status = 'online' THEN current_timestamp ELSE disconnected_at END,
           updated_at = current_timestamp`
    );

    for (const agent of agents) {
      await client.query(
        `INSERT INTO myforge_agents
           (agent_id, project_id, label, public_key_fingerprint, configured, status)
         VALUES ($1, $2, $3, $4, true, 'offline')
         ON CONFLICT (agent_id) DO UPDATE
         SET project_id = EXCLUDED.project_id,
             label = EXCLUDED.label,
             public_key_fingerprint = EXCLUDED.public_key_fingerprint,
             configured = true,
             status = 'offline',
             connection_id = NULL,
             disconnected_at = CASE
               WHEN myforge_agents.status = 'online' THEN current_timestamp
               ELSE myforge_agents.disconnected_at
             END,
             updated_at = current_timestamp`,
        [agent.agentId, agent.projectId, agent.label ?? null, agent.publicKeyFingerprint]
      );
    }
  }

  async initializeKnownAgents(agents) {
    return this.withTransaction(async (client) => {
      await this.syncKnownAgentsInTransaction(client, agents);
      const { rows } = await client.query(
        `UPDATE myforge_task_runs
         SET status = 'failed',
             queue_reason = NULL,
             error_code = CASE
               WHEN cancel_requested_at IS NULL THEN 'MYFORGE_SERVER_RESTARTED'
               ELSE 'MYFORGE_CANCEL_UNCONFIRMED'
             END,
             error_message = CASE
               WHEN cancel_requested_at IS NULL THEN 'Admin API restarted before the task completed'
               ELSE 'Cancellation was not confirmed before the Admin API restarted'
             END,
             completed_at = current_timestamp,
             updated_at = current_timestamp
         WHERE status IN ('dispatched', 'running')
         RETURNING *`
      );
      for (const row of rows) {
        const task = toTask(row);
        await this.appendLifecycleAudit(client, "myforge_task_fail", task, {
          details: { reason: "server_restart" }
        });
      }
      return { configuredAgents: agents.length, failedTasks: rows.length };
    });
  }

  async registerAgent({
    agentId,
    projectId,
    publicKeyFingerprint,
    hostname,
    platform,
    agentVersion,
    forgeRootSummary,
    capabilities,
    limits,
    effectiveLimits,
    connectionId,
    registeredAt = new Date()
  }) {
    if (!connectionId) {
      throw createMyforgeStoreError("INVALID_REQUEST", "connectionId is required");
    }
    return this.withTransaction(async (client) => {
      const { rows: existingRows } = await client.query(
        `SELECT * FROM myforge_agents WHERE agent_id = $1 FOR UPDATE`,
        [agentId]
      );
      const existing = existingRows[0];
      if (!existing || existing.configured !== true) {
        throw createMyforgeStoreError("MYFORGE_AGENT_UNKNOWN", "Agent is not configured");
      }
      if (existing.project_id !== projectId || existing.public_key_fingerprint !== publicKeyFingerprint) {
        throw createMyforgeStoreError("MYFORGE_IDENTITY_MISMATCH", "Agent identity does not match configuration");
      }
      const replacedConnectionId = existing.connection_id && existing.connection_id !== connectionId
        ? existing.connection_id
        : null;

      const { rows } = await client.query(
        `UPDATE myforge_agents
         SET status = 'online',
             connection_id = $10,
             hostname = $2,
             platform = $3,
             agent_version = $4,
             forge_root_summary_json = $5::jsonb,
             capabilities_json = $6::jsonb,
             limits_json = $7::jsonb,
             effective_limits_json = $8::jsonb,
             last_registered_at = $9,
             connected_at = $9,
             last_seen_at = $9,
             disconnected_at = NULL,
             updated_at = current_timestamp
         WHERE agent_id = $1
         RETURNING *`,
        [
          agentId,
          hostname ?? null,
          platform ?? null,
          agentVersion ?? null,
          toJsonb(forgeRootSummary),
          toJsonb(capabilities),
          toJsonb(limits),
          toJsonb(effectiveLimits),
          registeredAt,
          connectionId
        ]
      );
      return { agent: toAgent(rows[0]), replacedConnectionId };
    });
  }

  async heartbeatAgent({ agentId, projectId, connectionId, seenAt = new Date(), capabilities = undefined }) {
    if (!connectionId) {
      throw createMyforgeStoreError("INVALID_REQUEST", "connectionId is required");
    }
    const params = [agentId, projectId, connectionId, seenAt];
    let capabilitiesUpdate = "";
    if (capabilities !== undefined) {
      params.push(toJsonb(capabilities));
      capabilitiesUpdate = ", capabilities_json = $5::jsonb";
    }
    const { rows } = await this.pool.query(
      `UPDATE myforge_agents
       SET last_seen_at = $4,
           updated_at = current_timestamp
           ${capabilitiesUpdate}
       WHERE agent_id = $1
         AND project_id = $2
         AND connection_id = $3
         AND configured = true
         AND status = 'online'
       RETURNING *`,
      params
    );
    const agent = toAgent(rows[0]);
    return { agent, staleConnection: !agent };
  }

  async markAgentOffline({
    agentId,
    connectionId,
    disconnectedAt = new Date(),
    failureReason = "agent_disconnected",
    deliveryFailure = null
  }) {
    if (!connectionId) {
      throw createMyforgeStoreError("INVALID_REQUEST", "connectionId is required");
    }
    if (!new Set(["agent_disconnected", "server_shutdown"]).has(failureReason)) {
      throw createMyforgeStoreError("INVALID_REQUEST", "failureReason is invalid");
    }
    if (deliveryFailure !== null && (
      !deliveryFailure ||
      typeof deliveryFailure.requestId !== "string" ||
      !new Set(["command.execute", "command.cancel"]).has(deliveryFailure.kind)
    )) {
      throw createMyforgeStoreError("INVALID_REQUEST", "deliveryFailure is invalid");
    }
    return this.withTransaction(async (client) => {
      const { rows: agentRows } = await client.query(
        `UPDATE myforge_agents
         SET status = 'offline',
             connection_id = NULL,
             disconnected_at = $2,
             updated_at = current_timestamp
         WHERE agent_id = $1 AND connection_id = $3
         RETURNING *`,
        [agentId, disconnectedAt, connectionId]
      );

      const params = [
        agentId,
        disconnectedAt,
        connectionId,
        failureReason,
        deliveryFailure?.requestId ?? null,
        deliveryFailure?.kind ?? null
      ];
      const { rows: taskRows } = await client.query(
        `UPDATE myforge_task_runs
         SET status = 'failed',
             queue_reason = NULL,
             error_code = CASE
               WHEN request_id = $5::uuid AND $6 = 'command.execute' THEN 'MYFORGE_DISPATCH_FAILED'
               WHEN request_id = $5::uuid AND $6 = 'command.cancel' AND cancel_requested_at IS NOT NULL
                 THEN 'MYFORGE_CANCEL_DELIVERY_FAILED'
               WHEN cancel_requested_at IS NULL AND $4 = 'server_shutdown' THEN 'MYFORGE_SERVER_RESTARTED'
               WHEN cancel_requested_at IS NULL THEN 'MYFORGE_AGENT_DISCONNECTED'
               ELSE 'MYFORGE_CANCEL_UNCONFIRMED'
             END,
             error_message = CASE
               WHEN request_id = $5::uuid AND $6 = 'command.execute' THEN 'Task command could not be delivered to the agent'
               WHEN request_id = $5::uuid AND $6 = 'command.cancel' AND cancel_requested_at IS NOT NULL
                 THEN 'Cancellation command could not be delivered to the agent'
               WHEN cancel_requested_at IS NULL AND $4 = 'server_shutdown' THEN 'Admin API stopped before the task completed'
               WHEN cancel_requested_at IS NULL THEN 'Agent disconnected before the task completed'
               WHEN $4 = 'server_shutdown' THEN 'Cancellation was not confirmed before the Admin API stopped'
               ELSE 'Cancellation was not confirmed before the agent disconnected'
             END,
             completed_at = $2,
             updated_at = current_timestamp
          WHERE agent_id = $1
            AND status IN ('dispatched', 'running')
            AND connection_id = $3
          RETURNING *`,
        params
      );
      for (const row of taskRows) {
        const task = toTask(row);
        const auditReason = task.errorCode === "MYFORGE_DISPATCH_FAILED"
          ? "dispatch_delivery_failed"
          : task.errorCode === "MYFORGE_CANCEL_DELIVERY_FAILED"
            ? "cancel_delivery_failed"
            : failureReason;
        await this.appendLifecycleAudit(client, "myforge_task_fail", task, {
          details: {
            reason: auditReason
          }
        });
      }
      const agent = toAgent(agentRows[0]);
      return { agent, staleConnection: !agent, failedTasks: taskRows.map(toTask) };
    });
  }

  async getAgent(agentId, { configuredOnly = true } = {}) {
    const { rows } = await this.pool.query(
      `SELECT * FROM myforge_agents
       WHERE agent_id = $1
         AND ($2::boolean = false OR configured = true)
       LIMIT 1`,
      [agentId, configuredOnly]
    );
    return toAgent(rows[0]);
  }

  async listAgents({ configuredOnly = true, projectId = null, status = null } = {}) {
    const params = [configuredOnly];
    let query = `SELECT * FROM myforge_agents WHERE ($1::boolean = false OR configured = true)`;
    if (projectId) {
      params.push(projectId);
      query += ` AND project_id = $${params.length}`;
    }
    if (status) {
      requireAllowed(status, new Set(["online", "offline"]), "INVALID_REQUEST", "status");
      params.push(status);
      query += ` AND status = $${params.length}`;
    }
    query += " ORDER BY label NULLS LAST, agent_id";
    const { rows } = await this.pool.query(query, params);
    return rows.map(toAgent);
  }

  async createTask({
    taskType = "fangyuan.blueprint.generate",
    projectId,
    agentId,
    queueReason = undefined,
    artifactFile,
    consumerTargetFile = null,
    rulesFile,
    prompt,
    renderedPrompt,
    commandPreview,
    timeoutMs = this.config.commandTimeoutMs,
    maxOutputBytes = this.config.maxOutputBytes,
    queueTtlMs = this.config.queueTtlMs,
    createdByAdminId = null,
    createdByAdminUsername = null,
    ip = null,
    now = new Date()
  }) {
    return this.withTransaction(async (client) => {
      const requestId = crypto.randomUUID();
      const { rows: agentRows } = await client.query(
        `SELECT agent_id, project_id, status
         FROM myforge_agents
         WHERE agent_id = $1 AND configured = true
         FOR SHARE`,
        [agentId]
      );
      if (agentRows.length === 0) {
        throw createMyforgeStoreError("MYFORGE_AGENT_NOT_FOUND", "Agent is not configured");
      }
      if (agentRows[0].project_id !== projectId) {
        throw createMyforgeStoreError("MYFORGE_AGENT_PROJECT_MISMATCH", "Agent is bound to another project");
      }

      const effectiveQueueReason = queueReason === undefined
        ? (agentRows[0].status === "online" ? null : "agent_offline")
        : queueReason;
      if (effectiveQueueReason !== null) {
        requireAllowed(effectiveQueueReason, QUEUE_REASONS, "INVALID_REQUEST", "queueReason");
      }
      const queueExpiresAt = new Date(now.getTime() + queueTtlMs);
      const { rows } = await client.query(
        `INSERT INTO myforge_task_runs
           (request_id, task_type, project_id, agent_id, status, queue_reason,
            artifact_file, consumer_target_file, rules_file, prompt_json,
            rendered_prompt, command_preview, timeout_ms, max_output_bytes,
            created_by_admin_id, created_by_admin_username, created_at, queue_expires_at, updated_at)
         VALUES
           ($1, $2, $3, $4, 'queued', $5,
            $6, $7, $8, $9::jsonb,
            $10, $11, $12, $13,
            $14, $15, $16, $17, $16)
         RETURNING *`,
        [
          requestId,
          taskType,
          projectId,
          agentId,
          effectiveQueueReason,
          artifactFile,
          consumerTargetFile,
          rulesFile,
          JSON.stringify(prompt),
          renderedPrompt,
          commandPreview,
          timeoutMs,
          maxOutputBytes,
          createdByAdminId,
          createdByAdminUsername,
          now,
          queueExpiresAt
        ]
      );
      const task = toTask(rows[0]);
      await this.appendLifecycleAudit(client, "myforge_task_create", task, {
        actor: { adminId: createdByAdminId, adminUsername: createdByAdminUsername },
        ip
      });
      return task;
    });
  }

  async setQueuedTaskReason(requestId, queueReason) {
    if (queueReason !== null) {
      requireAllowed(queueReason, QUEUE_REASONS, "INVALID_REQUEST", "queueReason");
    }
    const { rows } = await this.pool.query(
      `UPDATE myforge_task_runs
       SET queue_reason = $2, updated_at = current_timestamp
       WHERE request_id = $1 AND status = 'queued'
       RETURNING *`,
      [requestId, queueReason]
    );
    return toTask(rows[0]);
  }

  async setQueuedTasksReasonForAgent({ agentId, projectId, queueReason }) {
    requireAllowed(queueReason, QUEUE_REASONS, "INVALID_REQUEST", "queueReason");
    const { rows } = await this.pool.query(
      `UPDATE myforge_task_runs
       SET queue_reason = $3, updated_at = current_timestamp
       WHERE agent_id = $1 AND project_id = $2 AND status = 'queued'
       RETURNING *`,
      [agentId, projectId, queueReason]
    );
    return rows.map(toTask);
  }

  async findNextQueuedTask({ agentId, projectId, now = new Date() }) {
    const { rows } = await this.pool.query(
      `SELECT * FROM myforge_task_runs
       WHERE agent_id = $1
         AND project_id = $2
         AND status = 'queued'
         AND queue_expires_at > $3
       ORDER BY created_at ASC, request_id ASC
       LIMIT 1`,
      [agentId, projectId, now]
    );
    return toTask(rows[0]);
  }

  async listQueuedAgentIdentities(now = new Date()) {
    const { rows } = await this.pool.query(
      `SELECT DISTINCT task.agent_id, task.project_id
       FROM myforge_task_runs task
       INNER JOIN myforge_agents agent
         ON agent.agent_id = task.agent_id
        AND agent.project_id = task.project_id
        AND agent.configured = true
        AND agent.status = 'online'
       WHERE task.status = 'queued' AND task.queue_expires_at > $1
         AND NOT EXISTS (
           SELECT 1 FROM myforge_task_runs active
           WHERE active.agent_id = task.agent_id
             AND active.project_id = task.project_id
             AND active.status IN ('dispatched', 'running')
         )
       ORDER BY task.agent_id, task.project_id`,
      [now]
    );
    return rows.map((row) => ({ agentId: row.agent_id, projectId: row.project_id }));
  }

  async claimTaskDispatched({
    requestId,
    agentId,
    projectId,
    connectionId,
    executionMode,
    commandDigest,
    commandExpiresAt,
    timeoutMs,
    maxOutputBytes,
    dispatchedAt = new Date()
  }) {
    requireAllowed(executionMode, EXECUTION_MODES, "INVALID_REQUEST", "executionMode");
    return this.withTransaction(async (client) => {
      const { rows: agentRows } = await client.query(
        `SELECT agent_id
         FROM myforge_agents
         WHERE agent_id = $1
           AND project_id = $2
           AND connection_id = $3
           AND configured = true
           AND status = 'online'
         FOR SHARE`,
        [agentId, projectId, connectionId]
      );
      if (agentRows.length === 0) return null;

      const { rows } = await client.query(
        `UPDATE myforge_task_runs task
         SET status = 'dispatched',
             queue_reason = NULL,
             connection_id = $4,
             execution_mode = $5,
             command_digest = $6,
             command_expires_at = $7,
             timeout_ms = $8,
             max_output_bytes = $9,
             dispatched_at = $10,
             updated_at = current_timestamp
         WHERE request_id = $1
           AND agent_id = $2
           AND project_id = $3
           AND status = 'queued'
           AND queue_expires_at > $10
           AND EXISTS (
             SELECT 1 FROM myforge_agents current_agent
             WHERE current_agent.agent_id = task.agent_id
               AND current_agent.project_id = task.project_id
               AND current_agent.connection_id = $4
               AND current_agent.configured = true
               AND current_agent.status = 'online'
           )
           AND NOT EXISTS (
             SELECT 1 FROM myforge_task_runs active
             WHERE active.agent_id = task.agent_id
               AND active.status IN ('dispatched', 'running')
           )
         RETURNING task.*`,
        [
          requestId,
          agentId,
          projectId,
          connectionId,
          executionMode,
          commandDigest,
          commandExpiresAt,
          timeoutMs,
          maxOutputBytes,
          dispatchedAt
        ]
      );
      const task = toTask(rows[0]);
      if (!task) return null;
      await this.appendLifecycleAudit(client, "myforge_task_dispatch", task);
      return task;
    });
  }

  assertTaskIdentity(task, identity = {}) {
    if (identity.agentId && task.agentId !== identity.agentId) {
      throw createMyforgeStoreError("MYFORGE_IDENTITY_MISMATCH", "Task agent identity does not match");
    }
    if (identity.projectId && task.projectId !== identity.projectId) {
      throw createMyforgeStoreError("MYFORGE_IDENTITY_MISMATCH", "Task project identity does not match");
    }
    if (identity.connectionId && task.connectionId !== identity.connectionId) {
      throw createMyforgeStoreError("MYFORGE_IDENTITY_MISMATCH", "Task connection identity does not match");
    }
    if (identity.executionMode && task.executionMode !== identity.executionMode) {
      throw createMyforgeStoreError("MYFORGE_IDENTITY_MISMATCH", "Task execution mode does not match");
    }
  }

  async lockTask(client, requestId) {
    const { rows } = await client.query(
      `SELECT * FROM myforge_task_runs WHERE request_id = $1 FOR UPDATE`,
      [requestId]
    );
    if (rows.length === 0) {
      throw createMyforgeStoreError("MYFORGE_TASK_NOT_FOUND", "Task was not found");
    }
    return toTask(rows[0]);
  }

  async markTaskStarted({ requestId, startedAt, ...identity }) {
    return this.withTransaction(async (client) => {
      const existing = await this.lockTask(client, requestId);
      this.assertTaskIdentity(existing, identity);
      if (existing.status === "running" && sameInstant(existing.startedAt, startedAt)) {
        return { outcome: "duplicate", task: existing };
      }
      if (existing.status !== "dispatched" || existing.cancelRequestedAt) {
        throw createMyforgeStoreError("MYFORGE_PROTOCOL_STATE_INVALID", "Task cannot transition to running");
      }
      const { rows } = await client.query(
        `UPDATE myforge_task_runs
         SET status = 'running', started_at = $2, updated_at = current_timestamp
         WHERE request_id = $1 AND status = 'dispatched'
         RETURNING *`,
        [requestId, startedAt]
      );
      const task = toTask(rows[0]);
      await this.appendLifecycleAudit(client, "myforge_task_started", task);
      return { outcome: "updated", task };
    });
  }

  async recordTaskResult({
    requestId,
    status,
    resultDigest,
    stdoutPreview = null,
    stderrPreview = null,
    stdoutBytes = null,
    stderrBytes = null,
    stdoutTruncated = false,
    stderrTruncated = false,
    exitCode = null,
    artifact = null,
    audit = null,
    errorCode = null,
    errorMessage = null,
    completedAt = new Date(),
    artifactFile = undefined,
    consumerTargetFile = undefined,
    ...identity
  }) {
    requireAllowed(status, RESULT_TASK_STATUSES, "MYFORGE_MESSAGE_SCHEMA_INVALID", "status");
    return this.withTransaction(async (client) => {
      const existing = await this.lockTask(client, requestId);
      this.assertTaskIdentity(existing, identity);
      if (artifactFile !== undefined && artifactFile !== existing.artifactFile) {
        throw createMyforgeStoreError("MYFORGE_IDENTITY_MISMATCH", "Artifact path does not match task");
      }
      if (consumerTargetFile !== undefined && consumerTargetFile !== existing.consumerTargetFile) {
        throw createMyforgeStoreError("MYFORGE_IDENTITY_MISMATCH", "Consumer target path does not match task");
      }
      if (TERMINAL_TASK_STATUSES.has(existing.status)) {
        if (existing.resultDigest && existing.resultDigest === resultDigest) {
          return { outcome: "duplicate", task: existing };
        }
        throw createMyforgeStoreError("MYFORGE_DUPLICATE_RESULT_CONFLICT", "Task already has a different terminal result");
      }
      if (existing.cancelRequestedAt && status !== "cancelled") {
        throw createMyforgeStoreError("MYFORGE_DUPLICATE_RESULT_CONFLICT", "Cancellation has priority over task result");
      }
      if (status === "cancelled") {
        if (!existing.cancelRequestedAt || !["dispatched", "running"].includes(existing.status)) {
          throw createMyforgeStoreError("MYFORGE_PROTOCOL_STATE_INVALID", "Task cannot transition to cancelled");
        }
      } else if (existing.status !== "running") {
        throw createMyforgeStoreError("MYFORGE_PROTOCOL_STATE_INVALID", "Task result requires a running task");
      }

      const { rows } = await client.query(
        `UPDATE myforge_task_runs
         SET status = $2,
             queue_reason = NULL,
             stdout_preview = $3,
             stderr_preview = $4,
             stdout_bytes = $5,
             stderr_bytes = $6,
             stdout_truncated = $7,
             stderr_truncated = $8,
             exit_code = $9,
             artifact_json = $10::jsonb,
             audit_json = $11::jsonb,
             result_digest = $12,
             error_code = $13,
             error_message = $14,
             completed_at = $15,
             updated_at = current_timestamp
         WHERE request_id = $1
         RETURNING *`,
        [
          requestId,
          status,
          stdoutPreview,
          stderrPreview,
          stdoutBytes,
          stderrBytes,
          stdoutTruncated,
          stderrTruncated,
          exitCode,
          toJsonb(artifact),
          toJsonb(audit),
          resultDigest,
          errorCode,
          errorMessage,
          completedAt
        ]
      );
      const task = toTask(rows[0]);
      const action = status === "cancelled"
        ? "myforge_task_cancelled"
        : status === "failed" ? "myforge_task_fail" : "myforge_task_complete";
      await this.appendLifecycleAudit(client, action, task);
      return { outcome: "updated", task };
    });
  }

  async recordTaskError({ requestId, errorCode, errorMessage, completedAt = new Date(), ...identity }) {
    return this.withTransaction(async (client) => {
      const existing = await this.lockTask(client, requestId);
      this.assertTaskIdentity(existing, identity);
      if (TERMINAL_TASK_STATUSES.has(existing.status)) {
        if (existing.status === "failed" && existing.errorCode === errorCode) {
          return { outcome: "duplicate", task: existing };
        }
        throw createMyforgeStoreError("MYFORGE_DUPLICATE_RESULT_CONFLICT", "Task already reached a terminal state");
      }
      if (existing.status !== "dispatched" || existing.cancelRequestedAt) {
        throw createMyforgeStoreError("MYFORGE_PROTOCOL_STATE_INVALID", "command.error requires a dispatched task");
      }
      const { rows } = await client.query(
        `UPDATE myforge_task_runs
         SET status = 'failed',
             queue_reason = NULL,
             error_code = $2,
             error_message = $3,
             completed_at = $4,
             updated_at = current_timestamp
         WHERE request_id = $1
         RETURNING *`,
        [requestId, errorCode, errorMessage, completedAt]
      );
      const task = toTask(rows[0]);
      await this.appendLifecycleAudit(client, "myforge_task_fail", task);
      return { outcome: "updated", task };
    });
  }

  async failTask({
    requestId,
    expectedStatuses = ["queued", "dispatched", "running"],
    errorCode,
    errorMessage,
    completedAt = new Date(),
    adminId = null,
    adminUsername = null,
    ip = null
  }) {
    for (const status of expectedStatuses) {
      requireAllowed(status, ACTIVE_TASK_STATUSES, "INVALID_REQUEST", "expectedStatus");
    }
    return this.withTransaction(async (client) => {
      const existing = await this.lockTask(client, requestId);
      if (TERMINAL_TASK_STATUSES.has(existing.status)) {
        return { outcome: "terminal", task: existing };
      }
      if (!expectedStatuses.includes(existing.status)) {
        return { outcome: "not_applicable", task: existing };
      }
      if (existing.cancelRequestedAt && !CANCELLATION_FAILURE_CODES.has(errorCode)) {
        throw createMyforgeStoreError(
          "MYFORGE_PROTOCOL_STATE_INVALID",
          "A cancellation-pending task requires a cancellation failure code"
        );
      }
      if (!existing.cancelRequestedAt && CANCELLATION_FAILURE_CODES.has(errorCode)) {
        throw createMyforgeStoreError(
          "MYFORGE_PROTOCOL_STATE_INVALID",
          "A cancellation failure code requires a cancellation-pending task"
        );
      }
      const { rows } = await client.query(
        `UPDATE myforge_task_runs
         SET status = 'failed',
             queue_reason = NULL,
             error_code = $2,
             error_message = $3,
             completed_at = $4,
             updated_at = current_timestamp
         WHERE request_id = $1
         RETURNING *`,
        [requestId, errorCode, errorMessage, completedAt]
      );
      const task = toTask(rows[0]);
      await this.appendLifecycleAudit(client, "myforge_task_fail", task, {
        actor: { adminId, adminUsername },
        ip
      });
      return { outcome: "updated", task };
    });
  }

  async failExpiredQueuedTasks(now = new Date()) {
    return this.withTransaction(async (client) => {
      const { rows } = await client.query(
        `UPDATE myforge_task_runs
         SET status = 'failed',
             queue_reason = NULL,
             error_code = 'MYFORGE_QUEUE_EXPIRED',
             error_message = 'Task expired while waiting for an agent',
             completed_at = $1,
             updated_at = current_timestamp
         WHERE status = 'queued' AND queue_expires_at <= $1
         RETURNING *`,
        [now]
      );
      const tasks = rows.map(toTask);
      for (const task of tasks) {
        await this.appendLifecycleAudit(client, "myforge_task_fail", task);
      }
      return tasks;
    });
  }

  async failExpiredDispatchedTasks({ now = new Date(), clockSkewMs = this.config.clockSkewMs ?? 0 } = {}) {
    return this.withTransaction(async (client) => {
      const { rows } = await client.query(
        `UPDATE myforge_task_runs
         SET status = 'failed',
             queue_reason = NULL,
             error_code = 'MYFORGE_COMMAND_EXPIRED',
             error_message = 'Agent did not acknowledge the command before it expired',
             completed_at = $1,
             updated_at = current_timestamp
         WHERE status = 'dispatched'
           AND cancel_requested_at IS NULL
           AND command_expires_at + ($2::bigint * interval '1 millisecond') <= $1
         RETURNING *`,
        [now, clockSkewMs]
      );
      const tasks = rows.map(toTask);
      for (const task of tasks) {
        await this.appendLifecycleAudit(client, "myforge_task_fail", task, {
          details: { reason: "command_expired" }
        });
      }
      return tasks;
    });
  }

  async failTimedOutRunningTasks({ now = new Date(), clockSkewMs = this.config.clockSkewMs ?? 0 } = {}) {
    return this.withTransaction(async (client) => {
      const { rows } = await client.query(
        `UPDATE myforge_task_runs
         SET status = 'failed',
             queue_reason = NULL,
             error_code = 'MYFORGE_COMMAND_TIMEOUT',
             error_message = 'Task exceeded the negotiated command timeout',
             completed_at = $1,
             updated_at = current_timestamp
         WHERE status = 'running'
           AND cancel_requested_at IS NULL
           AND started_at + ((timeout_ms + $2::bigint) * interval '1 millisecond') <= $1
         RETURNING *`,
        [now, clockSkewMs]
      );
      const tasks = rows.map(toTask);
      for (const task of tasks) {
        await this.appendLifecycleAudit(client, "myforge_task_fail", task, {
          details: { reason: "command_timeout" }
        });
      }
      return tasks;
    });
  }

  async failExpiredCancellationTasks({ now = new Date(), clockSkewMs = this.config.clockSkewMs ?? 0 } = {}) {
    return this.withTransaction(async (client) => {
      const { rows } = await client.query(
        `UPDATE myforge_task_runs
         SET status = 'failed',
             queue_reason = NULL,
             error_code = 'MYFORGE_CANCEL_TIMEOUT',
             error_message = 'Agent did not confirm cancellation before the deadline',
             completed_at = $1,
             updated_at = current_timestamp
         WHERE status IN ('dispatched', 'running')
           AND cancel_requested_at IS NOT NULL
           AND cancel_deadline_at + ($2::bigint * interval '1 millisecond') <= $1
         RETURNING *`,
        [now, clockSkewMs]
      );
      const tasks = rows.map(toTask);
      for (const task of tasks) {
        await this.appendLifecycleAudit(client, "myforge_task_fail", task, {
          details: { reason: "cancel_timeout" }
        });
      }
      return tasks;
    });
  }

  async requestTaskCancellation({
    requestId,
    agentId = null,
    projectId = null,
    connectionId = null,
    adminId = null,
    adminUsername = null,
    ip = null,
    requestedAt = new Date(),
    cancelTimeoutMs = this.config.cancelTimeoutMs,
    queuedOnly = false
  }) {
    return this.withTransaction(async (client) => {
      const existing = await this.lockTask(client, requestId);
      const actor = { adminId, adminUsername };
      if (existing.status === "cancelled") {
        return { outcome: "duplicate", task: existing, sendCancel: false };
      }
      if (["completed", "completed_with_errors", "failed"].includes(existing.status)) {
        throw createMyforgeStoreError("MYFORGE_TASK_NOT_CANCELLABLE", "Task is already complete");
      }
      if (existing.status === "queued") {
        const { rows } = await client.query(
          `UPDATE myforge_task_runs
           SET status = 'cancelled',
               queue_reason = NULL,
               error_code = 'MYFORGE_COMMAND_CANCELLED',
               error_message = 'Task was cancelled before dispatch',
               completed_at = $2,
               updated_at = current_timestamp
           WHERE request_id = $1 AND status = 'queued'
           RETURNING *`,
          [requestId, requestedAt]
        );
        const task = toTask(rows[0]);
        await this.appendLifecycleAudit(client, "myforge_task_cancel_request", task, { actor, ip });
        await this.appendLifecycleAudit(client, "myforge_task_cancelled", task, { actor, ip });
        return { outcome: "cancelled", task, sendCancel: false };
      }

      if (queuedOnly) {
        return { outcome: "requires_connection", task: existing, sendCancel: true };
      }
      if (existing.cancelRequestedAt) {
        this.assertTaskIdentity(existing, { agentId, projectId, connectionId });
        return { outcome: "duplicate", task: existing, sendCancel: true };
      }

      this.assertTaskIdentity(existing, { agentId, projectId, connectionId });
      const deadlineAt = new Date(requestedAt.getTime() + cancelTimeoutMs);
      const { rows } = await client.query(
        `UPDATE myforge_task_runs
         SET cancel_requested_at = $2,
             cancel_deadline_at = $3,
             updated_at = current_timestamp
         WHERE request_id = $1 AND status IN ('dispatched', 'running')
         RETURNING *`,
        [requestId, requestedAt, deadlineAt]
      );
      const task = toTask(rows[0]);
      await this.appendLifecycleAudit(client, "myforge_task_cancel_request", task, { actor, ip });
      return { outcome: "requested", task, sendCancel: true };
    });
  }

  async getTask(requestId) {
    const { rows } = await this.pool.query(
      `SELECT * FROM myforge_task_runs WHERE request_id = $1 LIMIT 1`,
      [requestId]
    );
    return toTask(rows[0]);
  }

  buildTaskFilters({ agentId = null, projectId = null, status = null } = {}) {
    const params = [];
    const clauses = [];
    if (agentId) {
      params.push(agentId);
      clauses.push(`agent_id = $${params.length}`);
    }
    if (projectId) {
      params.push(projectId);
      clauses.push(`project_id = $${params.length}`);
    }
    if (status) {
      const statuses = Array.isArray(status) ? status : [status];
      for (const value of statuses) {
        if (!ACTIVE_TASK_STATUSES.has(value) && !TERMINAL_TASK_STATUSES.has(value)) {
          throw createMyforgeStoreError("INVALID_REQUEST", "status is invalid");
        }
      }
      params.push(statuses);
      clauses.push(`status = ANY($${params.length}::varchar[])`);
    }
    return { params, where: clauses.length > 0 ? `WHERE ${clauses.join(" AND ")}` : "" };
  }

  async listTasks({ limit = 50, offset = 0, ...filters } = {}) {
    const built = this.buildTaskFilters(filters);
    built.params.push(limit);
    const limitParam = built.params.length;
    built.params.push(offset);
    const offsetParam = built.params.length;
    const { rows } = await this.pool.query(
      `SELECT * FROM myforge_task_runs
       ${built.where}
       ORDER BY created_at DESC, request_id DESC
       LIMIT $${limitParam} OFFSET $${offsetParam}`,
      built.params
    );
    return rows.map(toTask);
  }

  async countTasks(filters = {}) {
    const built = this.buildTaskFilters(filters);
    const { rows } = await this.pool.query(
      `SELECT COUNT(*) AS total FROM myforge_task_runs ${built.where}`,
      built.params
    );
    return Number.parseInt(String(rows[0]?.total ?? "0"), 10);
  }
}

export {
  ACTIVE_TASK_STATUSES,
  CANCELLATION_FAILURE_CODES,
  EXECUTION_MODES,
  QUEUE_REASONS,
  TERMINAL_TASK_STATUSES,
  auditDetails,
  toAgent,
  toTask
};
