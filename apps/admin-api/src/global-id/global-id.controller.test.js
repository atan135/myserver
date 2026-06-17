import assert from "node:assert/strict";
import { register } from "node:module";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../../tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const { GlobalIdController } = await import("./global-id.controller.ts");

function storeFixture() {
  return {
    calls: [],
    async findIdOrigin(originId) {
      this.calls.push(["findIdOrigin", originId]);
      return {
        origin_id: 2,
        origin_key: "cn-s002",
        created_at: "2026-01-01T00:00:00.000Z",
        retired_at: null
      };
    },
    async findCurrentWorldMembership(originId) {
      this.calls.push(["findCurrentWorldMembership", originId]);
      return {
        world_id: 10001,
        world_key: "cn-s001-s002",
        origin_id: originId,
        origin_key: "cn-s002",
        active_origin_id: 1,
        active_origin_key: "cn-s001",
        joined_at: "2026-02-01T00:00:00.000Z",
        left_at: null
      };
    },
    async findWorldMembershipAt(input) {
      this.calls.push(["findWorldMembershipAt", input]);
      return {
        world_id: 2,
        world_key: "cn-s002",
        origin_id: input.originId,
        origin_key: "cn-s002",
        active_origin_id: 2,
        active_origin_key: "cn-s002",
        joined_at: "2026-01-01T00:00:00.000Z",
        left_at: "2026-02-01T00:00:00.000Z"
      };
    },
    async findMergeContext(input) {
      this.calls.push(["findMergeContext", input]);
      return {
        merge_id: "90001",
        target_world_id: 10001,
        target_world_key: "cn-s001-s002",
        active_origin_id: 1,
        active_origin_key: "cn-s001",
        source_world_ids: [1, 2],
        source_world_keys: ["cn-s001", "cn-s002"],
        source_origin_ids: [1, 2],
        source_origin_keys: ["cn-s001", "cn-s002"],
        merged_at: "2026-02-01T00:00:00.000Z",
        operator: "ops",
        details_json: null
      };
    },
    async findIdOrigins(input) {
      this.calls.push(["findIdOrigins", input]);
      return [{ origin_id: 1, origin_key: "cn-s001" }];
    },
    async countIdOrigins(input) {
      this.calls.push(["countIdOrigins", input]);
      return 1;
    },
    async findWorlds(input) {
      this.calls.push(["findWorlds", input]);
      return [{ world_id: 1, world_key: "cn-s001", active_origin_id: 1 }];
    },
    async countWorlds(input) {
      this.calls.push(["countWorlds", input]);
      return 1;
    },
    async findWorldMergeEvents(input) {
      this.calls.push(["findWorldMergeEvents", input]);
      return [{ merge_id: 1, target_world_id: 10001 }];
    },
    async countWorldMergeEvents(input) {
      this.calls.push(["countWorldMergeEvents", input]);
      return 1;
    }
  };
}

test("GlobalIdController decodes ID and enriches metadata", async () => {
  const store = storeFixture();
  const controller = new GlobalIdController(store);
  controller.decodeGlobalIdInput = async (id) => ({
    raw_id: id,
    normalized_id: "plr_abc",
    id_kind: "player",
    numeric_id: "1900000000000",
    created_at: "2026-01-15T00:00:00.000Z",
    origin_id: 2,
    worker_id: 3,
    sequence: 4
  });

  const response = await controller.decode(" plr_abc ");

  assert.equal(response.ok, true);
  assert.equal(response.decoded.raw_id, "plr_abc");
  assert.equal(response.decoded.origin_id, 2);
  assert.equal(response.decoded.origin_key, "cn-s002");
  assert.equal(response.decoded.world_at_create.world_id, 2);
  assert.equal(response.decoded.current_world.world_id, 10001);
  assert.equal(response.decoded.merge_context.merge_id, "90001");
  assert.deepEqual(store.calls.map((call) => call[0]), [
    "findIdOrigin",
    "findCurrentWorldMembership",
    "findWorldMembershipAt",
    "findMergeContext"
  ]);
});

test("GlobalIdController rejects missing decode id", async () => {
  const controller = new GlobalIdController(storeFixture());

  await assert.rejects(
    () => controller.decode(" "),
    (error) => {
      assert.equal(error.getStatus(), 400);
      assert.equal(error.getResponse().error, "INVALID_GLOBAL_ID");
      return true;
    }
  );
});

test("GlobalIdController lists origins, worlds, and merge events with filters", async () => {
  const store = storeFixture();
  const controller = new GlobalIdController(store);

  assert.deepEqual(await controller.origins({
    origin_id: "1",
    origin_key: "cn",
    limit: "20",
    offset: "40"
  }), {
    ok: true,
    origins: [{ origin_id: 1, origin_key: "cn-s001" }],
    total: 1,
    limit: 20,
    offset: 40
  });

  assert.deepEqual(await controller.worlds({
    world_id: "10001",
    world_key: "cn",
    origin_id: "1"
  }), {
    ok: true,
    worlds: [{ world_id: 1, world_key: "cn-s001", active_origin_id: 1 }],
    total: 1,
    limit: 50,
    offset: 0
  });

  assert.deepEqual(await controller.mergeEvents({
    world_id: "10001",
    origin_id: "1"
  }), {
    ok: true,
    mergeEvents: [{ merge_id: 1, target_world_id: 10001 }],
    total: 1,
    limit: 50,
    offset: 0
  });
});
