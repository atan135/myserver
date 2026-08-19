import assert from "node:assert/strict";
import test from "node:test";

import {
  approvalEvidenceSummary,
  approvalStatusType,
  approvalDecisionPayload,
  canDecideApproval,
  isSelfApproval,
  rejectionReason
} from "./operation-approval.js";

test("approval evidence and rejection reasons require non-sensitive summaries", () => {
  assert.deepEqual(approvalEvidenceSummary("verified change window and impact"), {
    summary: "verified change window and impact"
  });
  assert.equal(approvalEvidenceSummary(""), null);
  assert.equal(approvalEvidenceSummary("token: secret-value"), null);
  assert.equal(rejectionReason("target is outside the approved window"), "target is outside the approved window");
  assert.equal(rejectionReason("Authorization: Bearer secret-value-123"), "");
});

test("approval UI blocks self review based on the server-provided requester identity", () => {
  const operation = { requester: { adminId: 17 } };
  assert.equal(isSelfApproval(operation, 17), true);
  assert.equal(isSelfApproval(operation, 18), false);
  assert.equal(approvalStatusType("pending"), "warning");
});

test("approval decisions require a pending operation, an independent approver, and complete safe evidence", () => {
  const pending = { approvalStatus: "pending", requester: { adminId: 17 } };
  assert.equal(canDecideApproval(pending, 18, "checked change request"), true);
  assert.equal(canDecideApproval(pending, 17, "checked change request"), false);
  assert.equal(canDecideApproval({ ...pending, approvalStatus: "approved" }, 18, "checked change request"), false);
  assert.equal(canDecideApproval(pending, 18, "token: should-not-be-accepted"), false);
  assert.equal(canDecideApproval(pending, 18, "checked change request", "", "rejected"), false);
  assert.equal(canDecideApproval(pending, 18, "checked change request", "outside window", "rejected"), true);
  assert.deepEqual(approvalDecisionPayload("approved", "checked change request"), {
    status: "approved",
    evidenceSummary: { summary: "checked change request" }
  });
  assert.deepEqual(approvalDecisionPayload("rejected", "checked change request", "outside window"), {
    status: "rejected",
    evidenceSummary: { summary: "checked change request" },
    rejectionReason: "outside window"
  });
  assert.equal(approvalDecisionPayload("rejected", "checked change request"), null);
});
