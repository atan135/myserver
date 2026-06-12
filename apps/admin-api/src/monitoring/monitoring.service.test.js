import assert from "node:assert/strict";
import http from "node:http";
import { register } from "node:module";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../../tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const { MonitoringService } = await import("./monitoring.service.ts");

function makeService(config = {}) {
  const redis = {};
  const mysqlPool = {};
  return new MonitoringService(
    {
      gameProxyAdminHost: "127.0.0.1",
      gameProxyAdminPort: 0,
      gameProxyAdminToken: "write-token",
      gameProxyAdminReadToken: "read-token",
      gameProxyAdminRequestTimeoutMs: 500,
      gameProxyAdminMaxResponseBytes: 4096,
      ...config
    },
    redis,
    mysqlPool
  );
}

async function withHttpServer(handler, fn) {
  const server = http.createServer(handler);
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));

  try {
    await fn(server.address().port);
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
}

test("rolloutDrain returns drained rollout warning snapshot", async () => {
  await withHttpServer((req, res) => {
    assert.equal(req.url, "/rollout");
    assert.equal(req.headers.authorization, "Bearer read-token");
    res.setHeader("content-type", "application/json");
    res.end(
      JSON.stringify({
        ok: true,
        rollout_session: {
          rollout_epoch: "epoch-1",
          old_server_id: "old-1",
          new_server_id: "new-1",
          state: "Active",
          started_at_ms: 1713000000000
        },
        drain_evaluation: {
          status: "Drained",
          rollout_epoch: "epoch-1",
          old_server_id: "old-1",
          new_server_id: "new-1",
          blocked_room_count: 0,
          blocked_player_count: 0,
          stale_room_route_count: 1,
          stale_player_route_count: 2,
          blocked_room_samples: [],
          blocked_player_samples: []
        }
      })
    );
  }, async (port) => {
    const service = makeService({ gameProxyAdminPort: port });
    const result = await service.rolloutDrain();

    assert.equal(result.ok, true);
    assert.equal(result.active, true);
    assert.equal(result.status, "drained");
    assert.equal(result.alert_level, "warning");
    assert.equal(result.rollout.epoch, "epoch-1");
    assert.equal(result.rollout.old_server, "old-1");
    assert.equal(result.rollout.new_server, "new-1");
    assert.equal(result.blockers.stale_room_route_count, 1);
    assert.equal(result.blockers.stale_player_route_count, 2);
  });
});

test("rolloutDrain returns blocked samples and does not overexpose sample lists", async () => {
  await withHttpServer((req, res) => {
    res.setHeader("content-type", "application/json");
    res.end(
      JSON.stringify({
        ok: true,
        rollout_session: {
          rollout_epoch: "epoch-2",
          old_server_id: "old-2",
          new_server_id: "new-2",
          state: "Active",
          started_at_ms: 1713000000000
        },
        drain_evaluation: {
          status: "Blocked",
          blocked_room_count: 6,
          blocked_player_count: 1,
          blocked_room_samples: ["r1", "r2", "r3", "r4", "r5", "r6"],
          blocked_player_samples: ["p1"]
        }
      })
    );
  }, async (port) => {
    const service = makeService({ gameProxyAdminPort: port });
    const result = await service.rolloutDrain();

    assert.equal(result.status, "blocked");
    assert.equal(result.alert_message, "仍有旧服房间/玩家/迁移中阻塞");
    assert.equal(result.blockers.blocked_room_count, 6);
    assert.deepEqual(result.blockers.blocked_room_samples, ["r1", "r2", "r3", "r4", "r5"]);
    assert.deepEqual(result.blockers.blocked_player_samples, ["p1"]);
  });
});

test("rolloutDrain returns displayable critical state when proxy admin is unavailable", async () => {
  const service = makeService({ gameProxyAdminPort: 9 });
  const result = await service.rolloutDrain();

  assert.equal(result.ok, false);
  assert.equal(result.status, "error");
  assert.equal(result.alert_level, "critical");
  assert.equal(result.alert_message, "控制面不可达");
  assert.equal(result.rollout, null);
});
