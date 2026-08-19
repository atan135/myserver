import assert from "node:assert/strict";
import test from "node:test";

import {
  createAdminRequestId,
  classifyHighRiskError,
  highRiskState,
  normalizeHighRiskError,
  preflightDetails,
  resumeHighRiskOperation,
  runHighRiskOperation
} from "./high-risk.js";

test("high-risk helper preflights, confirms and reuses request id with nonce binding", async () => {
  const calls = [];
  const result = await runHighRiskOperation({
    requestId: "admin-web-test-1",
    payload: { reason: "repair missing item" },
    invoke: async (body) => {
      calls.push(body);
      if (!body.preflightNonce) {
        return {
          data: {
            ok: true,
            state: "preflighted",
            operation: { requestId: body.requestId },
            preflight: {
              nonce: "signed-nonce",
              summarySha256: "a".repeat(64),
              expiresAt: "2099-07-19T12:00:00.000Z",
              impactSummary: { targetCount: 1 },
              approvalStatus: "not_required"
            }
          }
        };
      }
      return { data: { ok: true, state: "terminal", operation: { status: "succeeded" } } };
    },
    confirm: async (preflight) => {
      assert.equal(preflight.operation.requestId, "admin-web-test-1");
      assert.equal(preflight.impactSummary.targetCount, 1);
      return true;
    }
  });

  assert.equal(result.phase, "terminal");
  assert.equal(calls.length, 2);
  assert.equal(calls[0].requestId, calls[1].requestId);
  assert.equal(calls[1].preflightNonce, "signed-nonce");
  assert.equal(calls[1].preflightSummarySha256, "a".repeat(64));
});

test("high-risk helper stops after preview when the operator cancels", async () => {
  let calls = 0;
  const result = await runHighRiskOperation({
    requestId: "admin-web-test-cancel",
    payload: { reason: "operator cancelled" },
    invoke: async () => {
      calls += 1;
      return {
        data: {
          state: "preflight",
          preflight: { nonce: "nonce", summarySha256: "b".repeat(64) }
        }
      };
    },
    confirm: async () => false
  });

  assert.equal(result.phase, "cancelled");
  assert.equal(calls, 1);
});

test("high-risk helpers expose in-progress and reject malformed previews", () => {
  assert.equal(highRiskState({ data: { state: "in_progress" } }), "in_progress");
  assert.equal(highRiskState({ data: { operation: { status: "execution_uncertain" } } }), "execution_uncertain");
  assert.equal(highRiskState({ data: { state: "terminal", operation: { status: "execution_uncertain" } } }), "execution_uncertain");
  assert.throws(() => preflightDetails({ data: { state: "preflight" } }), /PREFLIGHT_INVALID/);
  assert.match(createAdminRequestId("test"), /^test-/);
});

test("high-risk error classification preserves safe next steps for control-plane states", () => {
  const approvalRequired = {
    response: {
      status: 409,
      data: { error: "ADMIN_OPERATION_APPROVAL_REQUIRED", message: "High-risk operation rejected" }
    }
  };
  assert.equal(classifyHighRiskError(approvalRequired), "approval_required");
  assert.deepEqual(normalizeHighRiskError(approvalRequired), {
    kind: "approval_required",
    code: "ADMIN_OPERATION_APPROVAL_REQUIRED",
    title: "等待独立审批",
    description: "该高风险操作需要其他管理员审批后才能执行。",
    serverMessage: "High-risk operation rejected",
    status: 409
  });
  assert.equal(classifyHighRiskError({ response: { status: 403, data: {} } }), "permission_denied");
  assert.equal(
    classifyHighRiskError({ response: { status: 400, data: { error: "ADMIN_OPERATION_PREVIEW_EXPIRED" } } }),
    "preflight_expired"
  );
  assert.equal(
    classifyHighRiskError({ response: { status: 503, data: { error: "ADMIN_OPERATION_PERSISTENCE_FAILED" } } }),
    "execution_uncertain"
  );
});

test("high-risk helper does not execute an already expired local preview", async () => {
  let calls = 0;
  const result = await runHighRiskOperation({
    requestId: "admin-web-expired-preview",
    payload: { reason: "drain expired preview" },
    invoke: async () => {
      calls += 1;
      return {
        data: {
          state: "preflighted",
          preflight: {
            nonce: "expired-nonce",
            summarySha256: "c".repeat(64),
            expiresAt: "2000-01-01T00:00:00.000Z"
          }
        }
      };
    },
    confirm: async () => true
  });

  assert.equal(result.phase, "expired");
  assert.equal(calls, 1);
});

test("high-risk helper preserves the original preflight when approval is pending and resumes it later", async () => {
  const calls = [];
  const preflight = {
    operation: { requestId: "approval-request-1" },
    nonce: "n".repeat(32),
    summarySha256: "a".repeat(64),
    approvalStatus: "pending"
  };
  const approvalError = { response: { status: 409, data: { error: "ADMIN_OPERATION_APPROVAL_REQUIRED" } } };
  const first = await runHighRiskOperation({
    invoke: async (payload) => {
      calls.push(payload);
      if (calls.length === 1) return { data: { state: "preflighted", preflight } };
      throw approvalError;
    },
    payload: { enabled: true },
    requestId: "approval-request-1",
    confirm: async () => true
  });
  assert.equal(first.phase, "approval_required");
  const resumed = await resumeHighRiskOperation({
    invoke: async (payload) => {
      calls.push(payload);
      return { data: { state: "terminal", operation: { status: "succeeded" } } };
    },
    payload: { enabled: true },
    requestId: first.requestId,
    preflight: first.preflight
  });
  assert.equal(resumed.phase, "terminal");
  assert.equal(calls[1].requestId, "approval-request-1");
  assert.equal(calls[2].requestId, "approval-request-1");
  assert.equal(calls[2].preflightNonce, preflight.nonce);
  assert.equal(calls[2].preflightSummarySha256, preflight.summarySha256);
});
