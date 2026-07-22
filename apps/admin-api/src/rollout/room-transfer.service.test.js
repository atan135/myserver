import assert from "node:assert/strict";
import { register } from "node:module";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../../tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const {
  RoomTransferService,
  downstreamRequestId,
  roomTransferAssertionStage
} = await import("./room-transfer.service.ts");
const { RolloutController } = await import("./rollout.controller.ts");

function transferBody(overrides = {}) {
  return {
    worldId: "local",
    rolloutEpoch: "rollout-test",
    roomId: "room-test",
    oldServerId: "game-server-001",
    newServerId: "game-server-002",
    proxyInstanceId: "game-proxy-001",
    backupReference: "backup-room-test",
    requestId: "room-transfer-request-1",
    reason: "controlled transfer",
    ...overrides
  };
}

test("RoomTransferService rejects malformed input and disabled registry discovery", async () => {
  const service = new RoomTransferService({ registryDiscoveryEnabled: false }, null, {}, {});
  assert.throws(
    () => service.normalizeInput(transferBody({ roomId: "room/invalid" }), 7),
    (error) => error.code === "ROLLOUT_INPUT_INVALID"
  );

  const input = service.normalizeInput(transferBody(), 7);
  await assert.rejects(
    () => service.validate(input),
    (error) => error.code === "SERVICE_DISCOVERY_REQUIRED"
  );
});

test("RoomTransferService derives unique downstream assertion request IDs per protocol operation", () => {
  const root = "room-transfer-request-1";
  const instance = "game-server-001";
  const freeze = downstreamRequestId(root, roomTransferAssertionStage(1601), instance);
  const exported = downstreamRequestId(root, roomTransferAssertionStage(1603), instance);
  const retired = downstreamRequestId(root, roomTransferAssertionStage(1607), instance);

  assert.notEqual(freeze, exported);
  assert.notEqual(exported, retired);
  assert.match(freeze, /^rollout-[a-f0-9]{40}$/);
});

test("RolloutController binds backup evidence and all resolved targets into the high-risk request", async () => {
  let captured = null;
  const roomTransfer = {
    normalizeInput(body, actorId) {
      return { ...transferBody(body), actorId };
    },
    async validate() {
      return {
        old: { instanceId: "game-server-001" },
        new: { instanceId: "game-server-002" },
        proxy: { instanceId: "game-proxy-001" }
      };
    },
    async execute() {
      return { ok: true, stage: "complete", completedStages: ["old_freeze"] };
    }
  };
  const controller = new RolloutController(roomTransfer, {
    async run(value) {
      captured = value;
      return { state: "preflight", response: { ok: true, state: "preflighted" } };
    }
  });
  const body = transferBody({ worldId: "attacker-world" });
  const response = await controller.transferRoom(body, { admin: { sub: 7 } });

  assert.deepEqual(response, { ok: true, state: "preflighted" });
  assert.equal(captured.permission, "game.room.transfer");
  assert.equal(captured.scope.instanceId, "game-server-001");
  assert.equal(captured.targetSummary.proxyInstanceId, "game-proxy-001");
  assert.equal(captured.payload.backupReference, "backup-room-test");
  assert.equal(captured.payload.worldId, "attacker-world");
});
