import assert from "node:assert/strict";
import test from "node:test";

import {
  approvalEvidenceSummary,
  approvalStatusType,
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
