import assert from "node:assert/strict";
import { register } from "node:module";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../../tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const { AdminOperationController } = await import("./admin-operation.controller.ts");

function request() {
  return { admin: { sub: 7, username: "approver" } };
}

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
