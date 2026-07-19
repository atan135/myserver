import assert from "node:assert/strict";
import { register } from "node:module";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../../tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const { AuditController } = await import("./audit.controller.ts");

const FROM = "2026-07-19T00:00:00.000Z";
const TO = "2026-07-19T02:00:00.000Z";

function event(id, createdAt) {
  return {
    id,
    createdAt,
    eventType: "execution_succeeded",
    permissionKey: "players.ban",
    targetSummary: { targetIds: ["player-1"] },
    result: "succeeded"
  };
}

test("operation audit query uses bounded keyset pagination and forwards every supported filter", async () => {
  let captured = null;
  const controller = new AuditController({
    async listAdminOperationAuditEvents(input) {
      captured = input;
      return [
        event(9, "2026-07-19T01:59:00.000Z"),
        event(8, "2026-07-19T01:58:00.000Z"),
        event(7, "2026-07-19T01:57:00.000Z")
      ];
    }
  });
  const response = await controller.operationAuditEvents({
    from: FROM,
    to: TO,
    limit: "2",
    actor_admin_id: "17",
    permission: "players.ban",
    action: "execution_succeeded",
    target: "player-1",
    request_id: "request-1",
    trace_id: "trace-1",
    risk_level: "high",
    result: "succeeded"
  });

  assert.equal(captured.limit, 3);
  assert.equal(captured.actorAdminId, 17);
  assert.equal(captured.permissionKey, "players.ban");
  assert.equal(captured.eventType, "execution_succeeded");
  assert.equal(captured.target, "player-1");
  assert.equal(captured.requestId, "request-1");
  assert.equal(captured.traceId, "trace-1");
  assert.equal(captured.riskLevel, "high");
  assert.equal(captured.result, "succeeded");
  assert.equal(response.events.length, 2);
  assert.ok(response.nextCursor);
  assert.deepEqual(
    JSON.parse(Buffer.from(response.nextCursor, "base64url").toString("utf8")),
    { createdAt: "2026-07-19T01:58:00.000Z", id: 8 }
  );
});

test("operation audit query rejects invalid cursors and unbounded time windows", async () => {
  const controller = new AuditController({ async listAdminOperationAuditEvents() { return []; } });
  await assert.rejects(
    () => controller.operationAuditEvents({ from: FROM, to: "2026-08-20T00:00:00.000Z" }),
    (error) => error.getStatus() === 400 && error.getResponse().error === "AUDIT_TIME_WINDOW_INVALID"
  );
  await assert.rejects(
    () => controller.operationAuditEvents({ from: FROM, to: TO, cursor: "not-a-cursor" }),
    (error) => error.getStatus() === 400 && error.getResponse().error === "AUDIT_CURSOR_INVALID"
  );
});

test("operation audit export keeps audit.read's server route and refuses oversized result sets", async () => {
  const controller = new AuditController({
    async listAdminOperationAuditEvents() {
      return Array.from({ length: 5001 }, (_, index) => event(index + 1, "2026-07-19T01:00:00.000Z"));
    }
  });
  await assert.rejects(
    () => controller.exportOperationAuditEvents({ from: FROM, to: TO }),
    (error) => error.getStatus() === 413 && error.getResponse().error === "AUDIT_EXPORT_LIMIT_EXCEEDED"
  );
  await assert.rejects(
    () => controller.exportOperationAuditEvents({ from: FROM, to: TO, cursor: "eyJpZCI6MX0" }),
    (error) => error.getStatus() === 400 && error.getResponse().error === "AUDIT_CURSOR_INVALID"
  );
});
