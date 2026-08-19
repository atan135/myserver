import assert from "node:assert/strict";
import { register } from "node:module";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../../tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const { AdminOperationController } = await import("./admin-operation.controller.ts");
const { POLICY_SCOPE_RESOLVER_KEY } = await import("../auth/roles.decorator.ts");

function request() {
  return { admin: { sub: 7, username: "approver" } };
}

test("operation detail and approval routes derive requestId scope for narrow grants", () => {
  const detailResolver = Reflect.getMetadata(POLICY_SCOPE_RESOLVER_KEY, AdminOperationController.prototype.getOperation);
  const approvalResolver = Reflect.getMetadata(POLICY_SCOPE_RESOLVER_KEY, AdminOperationController.prototype.decideApproval);
  assert.equal(typeof detailResolver, "function");
  assert.equal(typeof approvalResolver, "function");
  assert.deepEqual(detailResolver({ params: { requestId: "request-a" } }, "admin.permissions.manage"), {
    targetIds: ["request-a"],
    targetCount: 1
  });
  assert.deepEqual(approvalResolver({ params: { requestId: "request-b" } }, "admin.permissions.manage"), {
    targetIds: ["request-b"],
    targetCount: 1
  });
});

test("approval uses an independent actor and rejects self-approval before state mutation", async () => {
  let decisions = 0;
  const operations = {
    async decideApproval(input) {
      decisions += 1;
      assert.equal(input.actor.adminId, 7);
      assert.equal(input.actor.subject, "admin:7");
      return { kind: "approved", operation: { operationId: "op-1", requestId: input.requestId, status: "approved", approvalStatus: "approved" } };
    }
  };
  const controller = new AdminOperationController(operations, {}, {
    async getAdminOperationByRequestId() { return { actorAdminId: 3 }; }
  });
  const approved = await controller.decideApproval("request-1", { status: "approved", evidenceSummary: { ticket: "INC-1" } }, request());
  assert.equal(approved.decision, "approved");
  assert.equal(decisions, 1);

  const selfController = new AdminOperationController(operations, {}, {
    async getAdminOperationByRequestId() { return { actorAdminId: 7 }; }
  });
  await assert.rejects(
    () => selfController.decideApproval("request-1", { status: "approved" }, request()),
    (error) => error.getStatus() === 403 && error.getResponse().error === "ADMIN_OPERATION_SELF_APPROVAL_FORBIDDEN"
  );
  assert.equal(decisions, 1);
});

test("approval read models return only redacted summaries and require evidence for every decision", async () => {
  const operation = {
    operationId: "op-1",
    requestId: "request-1",
    actorAdminId: 3,
    actorSubject: "admin:3",
    permissionKey: "game.config.write",
    riskLevel: "high",
    status: "preflighted",
    approvalStatus: "pending",
    reason: "graceful replacement",
    targetSummary: { instanceId: "game-server-a", endpoint: { host: "10.0.0.1", port: 7500 } },
    preview: {
      nonce: "must-not-leak",
      summarySha256: "must-not-leak",
      impactSummary: { connectionCount: 2 },
      expiresAt: "2026-07-19T12:00:00.000Z",
      consumedAt: null
    },
    payload: { token: "must-not-leak" },
    approval: { status: "pending", evidenceSummary: {} }
  };
  const controller = new AdminOperationController({
    async decideApproval() { throw new Error("should not decide"); }
  }, {}, {
    async listPendingAdminOperations() { return [operation]; },
    async getAdminOperationByRequestId() { return operation; }
  }, {
    async authorize() { return { allowed: true }; }
  });
  const listed = await controller.listPendingApprovals({ limit: "10" }, request());
  const detail = await controller.getOperation("request-1");
  assert.equal(listed.operations.length, 1);
  assert.equal(detail.operation.preview.expiresAt, "2026-07-19T12:00:00.000Z");
  assert.doesNotMatch(JSON.stringify({ listed, detail }), /must-not-leak|10\.0\.0\.1|7500|nonce|payload|token/i);
  await assert.rejects(
    () => controller.decideApproval("request-1", { status: "approved" }, request()),
    (error) => error.getStatus() === 400 && error.getResponse().error === "ADMIN_OPERATION_APPROVAL_EVIDENCE_REQUIRED"
  );
  await assert.rejects(
    () => controller.decideApproval("request-1", { status: "approved", evidenceSummary: { token: "must-not-leak" } }, request()),
    (error) => error.getStatus() === 400 && error.getResponse().error === "ADMIN_OPERATION_APPROVAL_EVIDENCE_INVALID"
  );
});

test("pending approval list filters each operation against the approver's narrow server-side grant", async () => {
  const controller = new AdminOperationController({}, {}, {
    async listPendingAdminOperations() {
      return [
        { requestId: "request-a", actorAdminId: 3, actorSubject: "admin:3", status: "preflighted", approvalStatus: "pending" },
        { requestId: "request-b", actorAdminId: 4, actorSubject: "admin:4", status: "preflighted", approvalStatus: "pending" }
      ];
    }
  }, {
    async authorize(adminId, permission, scope) {
      assert.equal(adminId, 7);
      assert.equal(permission, "admin.permissions.manage");
      return { allowed: scope.targetIds[0] === "request-a" };
    }
  });
  const result = await controller.listPendingApprovals({}, request());
  assert.deepEqual(result.operations.map((operation) => operation.requestId), ["request-a"]);
});
test("break-glass activation derives actor and normalized target scope from the endpoint input", async () => {
  let activation = null;
  const controller = new AdminOperationController({}, {
    async activate(input) {
      activation = input;
      return { kind: "created", grant: { grantId: "grant-1", permissionKey: input.permission, expiresAt: "2026-07-19T12:00:00.000Z" } };
    },
    async revoke(input) {
      return { grantId: input.grantId, revokedAt: "2026-07-19T11:05:00.000Z" };
    }
  }, {});
  const created = await controller.activateBreakglass({
    requestId: "breakglass-request-1",
    permission: "gm.asset_correction.emergency",
    serviceName: "game-server",
    worldId: "world-1",
    targetType: "character",
    targetId: "chr_1",
    ttlMs: 60000,
    reason: "asset correction incident"
  }, request());
  assert.equal(created.state, "created");
  assert.equal(activation.actor.adminId, 7);
  assert.deepEqual(activation.scope, {
    worldId: "world-1",
    serviceName: "game-server",
    instanceId: undefined,
    targetType: "character",
    targetIds: ["chr_1"],
    targetCount: 1
  });
  assert.deepEqual(activation.targetSummary.targetIds, ["chr_1"]);
  assert.equal(activation.permission, "gm.asset_correction.emergency");

  const revoked = await controller.revokeBreakglass("grant-1", { reason: "incident resolved" }, request());
  assert.deepEqual(revoked, { ok: true, grantId: "grant-1", revokedAt: "2026-07-19T11:05:00.000Z" });
});
