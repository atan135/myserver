import assert from "node:assert/strict";
import { register } from "node:module";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../../tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const { AdminHighRiskOperationService } = await import("./admin-high-risk-operation.service.ts");
const { GmController } = await import("../gm/gm.controller.ts");

function request(body = {}) {
  return {
    admin: { sub: 7, username: "operator" },
    body,
    headers: {},
    socket: { remoteAddress: "127.0.0.1" }
  };
}

function input(overrides = {}) {
  return {
    request: request({ requestId: "operation-request-1", reason: "incident response" }),
    permission: "gm.send_item",
    scope: { worldId: "world-1", targetType: "character", targetIds: ["chr_1"], targetCount: 1 },
    targetSummary: { targetType: "character", targetIds: ["chr_1"] },
    payload: { characterId: "chr_1", itemId: "item-1", itemCount: 1 },
    impactSummary: { targetCount: 1, action: "item_grant" },
    reason: "incident response",
    execute: async () => ({ ok: true, sideEffect: true }),
    resultSummary: () => ({ action: "gm.send_item", outcome: "succeeded" }),
    ...overrides
  };
}

test("high-risk protocol preflight never calls the handler and returns only the preview", async () => {
  let sideEffects = 0;
  const calls = [];
  const service = new AdminHighRiskOperationService({
    async preflight(value) {
      calls.push(value);
      return {
        state: "preflighted",
        operation: { operationId: "op-1", requestId: value.requestId, status: "preflighted", approvalStatus: "not_required" },
        preflight: { nonce: "not-persisted-here", summarySha256: "a".repeat(64), expiresAt: "2026-07-19T12:00:00.000Z" }
      };
    }
  }, {});

  const result = await service.run(input({ execute: async () => { sideEffects += 1; return { ok: true }; } }));
  assert.equal(result.state, "preflight");
  assert.equal(sideEffects, 0);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].permission, "gm.send_item");
  assert.deepEqual(calls[0].scope.targetIds, ["chr_1"]);
});

test("claimed execution runs exactly once and duplicate execution returns its stored terminal state", async () => {
  let sideEffects = 0;
  const completions = [];
  let claimCount = 0;
  const operations = {
    async claimExecution() {
      claimCount += 1;
      return claimCount === 1
        ? { state: "claimed", operation: { operationId: "op-1", requestId: "operation-request-1", status: "executing" } }
        : { state: "terminal", operation: { operationId: "op-1", requestId: "operation-request-1", status: "succeeded", resultSummary: { action: "gm.send_item" } } };
    },
    async completeExecution(value) {
      completions.push(value);
      return { kind: "completed" };
    }
  };
  const service = new AdminHighRiskOperationService(operations, {});
  const executeRequest = request({
    requestId: "operation-request-1",
    reason: "incident response",
    preflightNonce: "a".repeat(32),
    preflightSummarySha256: "b".repeat(64)
  });
  const first = await service.run(input({ request: executeRequest, execute: async () => { sideEffects += 1; return { ok: true }; } }));
  const duplicate = await service.run(input({ request: executeRequest, execute: async () => { sideEffects += 1; return { ok: true }; } }));

  assert.equal(first.state, "executed");
  assert.equal(duplicate.state, "terminal");
  assert.equal(sideEffects, 1);
  assert.equal(completions.length, 1);
  assert.equal(completions[0].status, "succeeded");
});

test("tamper, expiry, replay and pending approval are stable protocol rejections", async () => {
  const cases = [
    ["ADMIN_OPERATION_REQUEST_CONFLICT", 409],
    ["ADMIN_OPERATION_PREVIEW_EXPIRED", 400],
    ["ADMIN_OPERATION_NONCE_REPLAYED", 409],
    ["ADMIN_OPERATION_APPROVAL_REQUIRED", 409]
  ];
  for (const [code, status] of cases) {
    const operations = {
      async claimExecution() {
        const error = new Error(code);
        error.code = code;
        throw error;
      }
    };
    const service = new AdminHighRiskOperationService(operations, {});
    await assert.rejects(
      () => service.run(input({ request: request({
        requestId: "operation-request-1",
        reason: "incident response",
        preflightNonce: "a".repeat(32),
        preflightSummarySha256: "b".repeat(64)
      }) })),
      (error) => error.getStatus() === status && error.getResponse().error === code
    );
  }
});

test("a handler-reported partial failure is persisted as execution_uncertain", async () => {
  const completions = [];
  const service = new AdminHighRiskOperationService({
    async claimExecution() {
      return { state: "claimed", operation: { operationId: "op-partial", requestId: "operation-request-1", status: "executing" } };
    },
    async completeExecution(value) {
      completions.push(value);
      return { kind: "completed" };
    }
  }, {});
  const outcome = await service.run(input({
    request: request({
      requestId: "operation-request-1",
      reason: "incident response",
      preflightNonce: "a".repeat(32),
      preflightSummarySha256: "b".repeat(64)
    }),
    execute: async () => ({ ok: false, error: "SESSION_KICK_PUBLISH_FAILED" })
  }));
  assert.equal(outcome.state, "executed");
  assert.equal(completions[0].status, "execution_uncertain");
  assert.deepEqual(completions[0].errorSummary, { code: "SESSION_KICK_PUBLISH_FAILED" });
});

test("emergency execution requires a matching active break-glass grant before claim", async () => {
  let claims = 0;
  const service = new AdminHighRiskOperationService({
    async claimExecution() { claims += 1; return { state: "claimed" }; }
  }, {
    async requireActiveGrant() {
      const error = new Error("missing grant");
      error.code = "ADMIN_BREAKGLASS_GRANT_REQUIRED";
      throw error;
    }
  });
  await assert.rejects(
    () => service.run(input({
      permission: "gm.asset_correction.emergency",
      emergency: true,
      request: request({
        requestId: "operation-request-1",
        reason: "incident response",
        preflightNonce: "a".repeat(32),
        preflightSummarySha256: "b".repeat(64)
      })
    })),
    (error) => error.getStatus() === 403 && error.getResponse().error === "ADMIN_BREAKGLASS_GRANT_REQUIRED"
  );
  assert.equal(claims, 0);
});

test("GM send-item protocol scope and target come from the resolved character, not client world fields", async () => {
  let captured = null;
  let gameCalls = 0;
  const highRiskOperations = {
    async run(value) {
      captured = value;
      return { state: "preflight", response: { ok: true, state: "preflighted" } };
    }
  };
  const adminStore = {
    async findCharacterById() { return { characterId: "chr_server", worldId: "world-server" }; },
    async appendAuditLog() {}
  };
  const controller = new GmController({}, adminStore, { async publishJson() {} }, {
    async sendItem() { gameCalls += 1; return { ok: true }; }
  }, highRiskOperations);
  const body = {
    characterId: "chr_server",
    itemId: "item-1",
    itemCount: 1,
    reason: "incident response",
    requestId: "operation-request-1",
    worldId: "attacker-world",
    targetIds: ["attacker-target"]
  };
  const response = await controller.sendItem(body, request(body));

  assert.deepEqual(response, { ok: true, state: "preflighted" });
  assert.equal(gameCalls, 0);
  assert.equal(captured.scope.worldId, "world-server");
  assert.deepEqual(captured.scope.targetIds, ["chr_server"]);
  assert.deepEqual(captured.targetSummary.targetIds, ["chr_server"]);
});
