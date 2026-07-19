import assert from "node:assert/strict";
import test from "node:test";

import {
  createAdminRequestId,
  highRiskState,
  preflightDetails,
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
              expiresAt: "2026-07-19T12:00:00.000Z",
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
  assert.throws(() => preflightDetails({ data: { state: "preflight" } }), /PREFLIGHT_INVALID/);
  assert.match(createAdminRequestId("test"), /^test-/);
});
