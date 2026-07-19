import assert from "node:assert/strict";
import { register } from "node:module";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../../tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const { AdminOperationService } = await import("./admin-operation.service.ts");
const { AdminBreakglassService } = await import("./admin-breakglass.service.ts");

const ROOT_SCOPE = {
  worldIds: ["*"],
  serviceNames: ["*"],
  instanceIds: ["*"],
  fieldAllowlist: ["*"],
  targetTypes: ["*"],
  targetIds: ["*"],
  maxTargets: 10000
};

const PERMISSIONS = {
  "gm.broadcast": { permission_key: "gm.broadcast", active: true, risk_level: "high", scope_dimensions: ["world_ids"] },
  "gm.send_item": { permission_key: "gm.send_item", active: true, risk_level: "high", scope_dimensions: ["world_ids", "target_ids"] },
  "gm.asset_correction.emergency": { permission_key: "gm.asset_correction.emergency", active: true, risk_level: "emergency", scope_dimensions: ["world_ids", "target_ids"] },
  "service.shutdown": { permission_key: "service.shutdown", active: true, risk_level: "emergency", scope_dimensions: ["service_names", "instance_ids"] },
  "breakglass.activate": { permission_key: "breakglass.activate", active: true, risk_level: "emergency", scope_dimensions: ["service_names", "target_ids"] }
};

class MemoryOperationStore {
  constructor() {
    this.operations = new Map();
    this.breakglass = new Map();
    this.audit = [];
  }

  async findAdminPolicyPermission(permission) {
    return PERMISSIONS[permission] || null;
  }

  async reserveAdminOperationPreflight(input) {
    const existing = this.operations.get(input.requestId);
    if (existing) return { kind: existing.semanticSha256 === input.semanticSha256 ? "existing" : "conflict", operation: existing };
    const operation = {
      operationId: input.operationId,
      requestId: input.requestId,
      actorAdminId: input.actorAdminId,
      actorSubject: input.actorSubject,
      permissionKey: input.permissionKey,
      riskLevel: input.riskLevel,
      authorizationScope: input.authorizationScope,
      requestedScope: input.requestedScope,
      scopeSha256: input.scopeSha256,
      targetSummary: input.targetSummary,
      targetSha256: input.targetSha256,
      payloadSha256: input.payloadSha256,
      semanticSha256: input.semanticSha256,
      reason: input.reason,
      traceId: input.traceId,
      status: "preflighted",
      approvalStatus: input.approvalStatus,
      preview: {
        previewId: input.preview.previewId,
        summarySha256: input.preview.summarySha256,
        expiresAt: input.preview.expiresAt,
        consumedAt: null,
        nonceSha256: input.preview.nonceSha256
      }
    };
    this.operations.set(input.requestId, operation);
    this.audit.push({ event: "preflight_created", requestId: input.requestId, payloadSha256: input.payloadSha256 });
    return { kind: "created", operation };
  }

  async claimAdminOperationExecution({ requestId, semanticSha256, nonceSha256, summarySha256 }) {
    const operation = this.operations.get(requestId);
    if (!operation) return { kind: "not_found" };
    if (operation.semanticSha256 !== semanticSha256) return { kind: "conflict", operation };
    if (["succeeded", "failed", "execution_uncertain", "cancelled"].includes(operation.status)) return { kind: "terminal", operation };
    if (operation.status === "executing") return { kind: "in_progress", operation };
    if (operation.approvalStatus === "pending") return { kind: "approval_pending", operation };
    if (operation.approvalStatus === "rejected") return { kind: "approval_rejected", operation };
    if (new Date(operation.preview.expiresAt).getTime() <= Date.now()) return { kind: "preview_expired", operation };
    if (operation.preview.consumedAt) return { kind: "nonce_replayed", operation };
    if (operation.preview.nonceSha256 !== nonceSha256 || operation.preview.summarySha256 !== summarySha256) return { kind: "preview_mismatch", operation };
    operation.preview.consumedAt = new Date().toISOString();
    operation.status = "executing";
    this.audit.push({ event: "execution_claimed", requestId });
    return { kind: "claimed", operation };
  }

  async completeAdminOperation({ operationId, status, resultSummary, errorSummary }) {
    const operation = [...this.operations.values()].find((entry) => entry.operationId === operationId);
    if (!operation) throw new Error("not found");
    if (["succeeded", "failed", "execution_uncertain", "cancelled"].includes(operation.status)) return { kind: "terminal", operation };
    if (operation.status !== "executing") return { kind: "state_conflict", operation };
    operation.status = status;
    operation.resultSummary = resultSummary;
    operation.errorSummary = errorSummary;
    this.audit.push({ event: `execution_${status}`, requestId: operation.requestId });
    return { kind: "completed", operation };
  }

  async markAdminOperationExecutionUncertain({ operationId, errorSummary }) {
    const operation = [...this.operations.values()].find((entry) => entry.operationId === operationId);
    if (!operation) throw new Error("not found");
    if (operation.status === "executing") {
      operation.status = "execution_uncertain";
      operation.errorSummary = errorSummary;
      return { kind: "marked_uncertain", operation };
    }
    return { kind: "terminal_or_conflict", operation };
  }

  async createAdminBreakglassGrant(input) {
    const existing = this.breakglass.get(input.activationRequestId);
    if (existing) return { kind: existing.semanticSha256 === input.semanticSha256 ? "existing" : "conflict", grant: existing };
    const grant = {
      ...input,
      grantId: input.grantId,
      expiresAt: input.expiresAt,
      revokedAt: null
    };
    this.breakglass.set(input.activationRequestId, grant);
    this.audit.push({ event: "breakglass_activated", requestId: input.activationRequestId });
    return { kind: "created", grant };
  }

  async listActiveAdminBreakglassGrants(adminId, permission) {
    return [...this.breakglass.values()].filter((grant) =>
      String(grant.actorAdminId) === String(adminId) &&
      grant.permissionKey === permission &&
      !grant.revokedAt && new Date(grant.expiresAt).getTime() > Date.now()
    );
  }

  async revokeAdminBreakglassGrant({ grantId, revokedByAdminId, revokedBySubject, reason }) {
    const grant = [...this.breakglass.values()].find((entry) => entry.grantId === grantId && !entry.revokedAt);
    if (!grant) {
      const error = new Error("not active");
      error.code = "ADMIN_BREAKGLASS_GRANT_NOT_ACTIVE";
      throw error;
    }
    grant.revokedAt = new Date().toISOString();
    grant.revokedByAdminId = revokedByAdminId;
    grant.revokedBySubject = revokedBySubject;
    grant.revocationReason = reason;
    this.audit.push({ event: "breakglass_revoked", requestId: grant.activationRequestId });
    return grant;
  }
}

class Policy {
  constructor({ allowBreakglass = true } = {}) {
    this.allowBreakglass = allowBreakglass;
  }

  async authorize(_actorId, permission) {
    if (permission === "breakglass.activate" && !this.allowBreakglass) {
      return { allowed: false, code: "PERMISSION_DENIED" };
    }
    return { allowed: true, code: "ALLOWED", matchedGrant: { scope: ROOT_SCOPE } };
  }
}

function services({ allowBreakglass = true } = {}) {
  const store = new MemoryOperationStore();
  const policy = new Policy({ allowBreakglass });
  return {
    store,
    operations: new AdminOperationService({ adminOperationPreflightTtlMs: 120000 }, policy, store),
    breakglass: new AdminBreakglassService(policy, store)
  };
}

const ACTOR = { adminId: 7, subject: "admin:operator-7" };
const HIGH_SCOPE = { worldId: "world-1", serviceName: "game-server", targetType: "player", targetIds: ["player-1"] };

function preflightInput(overrides = {}) {
  return {
    actor: ACTOR,
    permission: "gm.broadcast",
    scope: HIGH_SCOPE,
    requestId: "request-0001",
    reason: "incident response",
    targetSummary: { worldId: "world-1", targetCount: 1 },
    payload: { message: "maintenance" },
    impactSummary: { affectedPlayers: 12, publicMessage: true },
    ...overrides
  };
}

test("high-risk preflight persists only payload hash, returns a short-lived nonce, and rejects sensitive summaries", async () => {
  const { store, operations } = services();
  const created = await operations.preflight(preflightInput());
  assert.equal(created.state, "preflighted");
  assert.match(created.preflight.nonce, /^[A-Za-z0-9_-]{32,128}$/);
  assert.match(created.preflight.summarySha256, /^[0-9a-f]{64}$/);
  const stored = store.operations.get("request-0001");
  assert.match(stored.payloadSha256, /^[0-9a-f]{64}$/);
  assert.equal(JSON.stringify(stored).includes("maintenance"), false);
  assert.equal(JSON.stringify(stored).includes(created.preflight.nonce), false);
  assert.equal(stored.approvalStatus, "not_required");
  await assert.rejects(
    () => operations.preflight(preflightInput({ requestId: "request-0002", impactSummary: { token: "must-not-persist" } })),
    (error) => error.code === "ADMIN_OPERATION_SENSITIVE_SUMMARY"
  );
});

test("operation reasons reject explicit credentials but allow ordinary token rotation text", async () => {
  const { store, operations } = services();
  await assert.rejects(
    () => operations.preflight(preflightInput({ reason: "Authorization: Bearer secret-value-123" })),
    (error) => error.code === "ADMIN_OPERATION_SENSITIVE_REASON"
  );
  assert.equal(store.operations.size, 0);

  const allowed = await operations.preflight(preflightInput({
    requestId: "request-token-rotation",
    reason: "token rotation completed"
  }));
  assert.equal(allowed.state, "preflighted");
  assert.equal(store.operations.get("request-token-rotation").reason, "token rotation completed");
});
test("request semantics, payload and preview binding fail closed while duplicate execution reports in-progress", async () => {
  const { store, operations } = services();
  const created = await operations.preflight(preflightInput());
  await assert.rejects(
    () => operations.preflight(preflightInput({ payload: { message: "different" } })),
    (error) => error.code === "ADMIN_OPERATION_REQUEST_CONFLICT"
  );
  await assert.rejects(
    () => operations.claimExecution({
      ...preflightInput(),
      payload: { message: "tampered" },
      nonce: created.preflight.nonce,
      preflightSummarySha256: created.preflight.summarySha256
    }),
    (error) => error.code === "ADMIN_OPERATION_REQUEST_CONFLICT"
  );
  const claimed = await operations.claimExecution({
    ...preflightInput(),
    nonce: created.preflight.nonce,
    preflightSummarySha256: created.preflight.summarySha256
  });
  assert.equal(claimed.state, "claimed");
  const retried = await operations.claimExecution({
    ...preflightInput(),
    nonce: created.preflight.nonce,
    preflightSummarySha256: created.preflight.summarySha256
  });
  assert.equal(retried.state, "in_progress");
  store.operations.get("request-0001").status = "preflighted";
  await assert.rejects(
    () => operations.claimExecution({
      ...preflightInput(),
      nonce: created.preflight.nonce,
      preflightSummarySha256: created.preflight.summarySha256
    }),
    (error) => error.code === "ADMIN_OPERATION_NONCE_REPLAYED"
  );
});

test("execution uncertainty marker persists recovery state without depending on another audit append", async () => {
  const { store, operations } = services();
  const created = await operations.preflight(preflightInput());
  await operations.claimExecution({
    ...preflightInput(),
    nonce: created.preflight.nonce,
    preflightSummarySha256: created.preflight.summarySha256
  });
  const auditCount = store.audit.length;
  const marked = await operations.markExecutionUncertain({
    operationId: store.operations.get("request-0001").operationId,
    errorSummary: { code: "ADMIN_OPERATION_RESULT_PERSISTENCE_FAILED" }
  });
  assert.equal(marked.kind, "marked_uncertain");
  assert.equal(store.operations.get("request-0001").status, "execution_uncertain");
  assert.equal(store.audit.length, auditCount);
});

test("expired, altered summary and approval-pending previews never claim execution", async () => {
  const { store, operations } = services();
  const created = await operations.preflight(preflightInput());
  store.operations.get("request-0001").preview.expiresAt = new Date(Date.now() - 1).toISOString();
  await assert.rejects(
    () => operations.claimExecution({ ...preflightInput(), nonce: created.preflight.nonce, preflightSummarySha256: created.preflight.summarySha256 }),
    (error) => error.code === "ADMIN_OPERATION_PREVIEW_EXPIRED"
  );

  const approval = await operations.preflight(preflightInput({
    requestId: "request-approval-1",
    permission: "gm.send_item",
    impactSummary: { affectedPlayers: 1, assetDelta: "item grant" }
  }));
  assert.equal(approval.preflight.approvalStatus, "pending");
  await assert.rejects(
    () => operations.claimExecution({
      ...preflightInput({ requestId: "request-approval-1", permission: "gm.send_item", impactSummary: undefined }),
      nonce: approval.preflight.nonce,
      preflightSummarySha256: approval.preflight.summarySha256
    }),
    (error) => error.code === "ADMIN_OPERATION_APPROVAL_REQUIRED"
  );
  const operation = store.operations.get("request-approval-1");
  operation.approvalStatus = "approved";
  operation.status = "approved";
  await assert.rejects(
    () => operations.claimExecution({
      ...preflightInput({ requestId: "request-approval-1", permission: "gm.send_item", impactSummary: undefined }),
      nonce: approval.preflight.nonce,
      preflightSummarySha256: "0".repeat(64)
    }),
    (error) => error.code === "ADMIN_OPERATION_PREVIEW_MISMATCH"
  );
});

test("break-glass requires its own permission, binds an emergency action and target, caps TTL, audits activation and revocation", async () => {
  const denied = services({ allowBreakglass: false });
  await assert.rejects(
    () => denied.breakglass.activate({
      actor: ACTOR,
      requestId: "breakglass-request-1",
      permission: "gm.asset_correction.emergency",
      scope: HIGH_SCOPE,
      targetSummary: { playerId: "player-1", correction: "ledger reconciliation" },
      reason: "production asset incident",
      ttlMs: 60000
    }),
    (error) => error.code === "ADMIN_BREAKGLASS_ACTIVATE_DENIED"
  );

  const { store, breakglass } = services();
  await assert.rejects(
    () => breakglass.activate({
      actor: ACTOR,
      requestId: "breakglass-sensitive-reason",
      permission: "gm.asset_correction.emergency",
      scope: HIGH_SCOPE,
      targetSummary: { playerId: "player-1" },
      reason: "cookie=session-secret-value",
      ttlMs: 60000
    }),
    (error) => error.code === "ADMIN_BREAKGLASS_SENSITIVE_REASON"
  );
  assert.equal(store.breakglass.size, 0);
  await assert.rejects(
    () => breakglass.activate({
      actor: ACTOR,
      requestId: "breakglass-request-1",
      permission: "gm.asset_correction.emergency",
      scope: HIGH_SCOPE,
      targetSummary: { playerId: "player-1" },
      reason: "production asset incident",
      ttlMs: 900001
    }),
    (error) => error.code === "ADMIN_BREAKGLASS_TTL_INVALID"
  );
  const activated = await breakglass.activate({
    actor: ACTOR,
    requestId: "breakglass-request-1",
    permission: "gm.asset_correction.emergency",
    scope: HIGH_SCOPE,
    targetSummary: { playerId: "player-1", correction: "ledger reconciliation" },
    reason: "production asset incident",
    ttlMs: 60000
  });
  assert.equal(activated.kind, "created");
  await breakglass.requireActiveGrant({
    actorAdminId: 7,
    permission: "gm.asset_correction.emergency",
    scope: HIGH_SCOPE,
    targetSummary: { playerId: "player-1", correction: "ledger reconciliation" }
  });
  await assert.rejects(
    () => breakglass.requireActiveGrant({
      actorAdminId: 7,
      permission: "gm.asset_correction.emergency",
      scope: HIGH_SCOPE,
      targetSummary: { playerId: "player-2", correction: "ledger reconciliation" }
    }),
    (error) => error.code === "ADMIN_BREAKGLASS_GRANT_REQUIRED"
  );
  await assert.rejects(
    () => breakglass.revoke({
      grantId: activated.grant.grantId,
      actor: ACTOR,
      reason: "private_key=never-store-this-value"
    }),
    (error) => error.code === "ADMIN_BREAKGLASS_SENSITIVE_REASON"
  );
  assert.equal(activated.grant.revokedAt, null);
  await breakglass.revoke({ grantId: activated.grant.grantId, actor: ACTOR, reason: "incident resolved" });
  assert.deepEqual(store.audit.map((event) => event.event), ["breakglass_activated", "breakglass_revoked"]);
});
