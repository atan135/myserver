import assert from "node:assert/strict";
import { register } from "node:module";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../../tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const {
  AdminPolicyGuard,
  extractAdminPolicyScope
} = await import("./admin-policy.guard.ts");
const {
  PERMISSIONS_KEY,
  POLICY_PERMISSION_RESOLVER_KEY
} = await import("./roles.decorator.ts");

function makeContext({ request = {}, permissions, resolver } = {}) {
  function handler() {}
  const req = {
    method: "POST",
    url: "/api/v1/gm/send-item",
    headers: {},
    params: {},
    query: {},
    body: {},
    admin: { sub: 7, username: "operator", role: "viewer" },
    ...request
  };
  return {
    request: req,
    reflector: {
      getAllAndOverride(key) {
        if (key === PERMISSIONS_KEY) return permissions;
        if (key === POLICY_PERMISSION_RESOLVER_KEY) return resolver;
        return undefined;
      }
    },
    context: {
      getHandler: () => handler,
      getClass: () => class TestController {},
      switchToHttp: () => ({ getRequest: () => req })
    }
  };
}

function makeGuard(fixture, { decision = { allowed: true, code: "ALLOWED" }, store = {} } = {}) {
  const calls = [];
  const policy = {
    async authorize(adminId, permission, scope) {
      calls.push({ adminId, permission, scope });
      return typeof decision === "function" ? decision(adminId, permission, scope) : decision;
    }
  };
  return {
    guard: new AdminPolicyGuard(fixture.reflector, policy, store, {}),
    calls
  };
}

function assertHttpError(status, error) {
  return (exception) => {
    assert.equal(exception.getStatus(), status);
    assert.equal(exception.getResponse().error, error);
    return true;
  };
}

test("AdminPolicyGuard authenticates an admin identity without using its legacy role", async () => {
  const fixture = makeContext({
    permissions: ["gm.kick_player"],
    request: { body: { playerId: "player-1", scope: { targetIds: ["different-player"] }, permission: "gm.ban_player" } }
  });
  const { guard, calls } = makeGuard(fixture, { decision: { allowed: true, code: "ALLOWED" } });

  assert.equal(await guard.canActivate(fixture.context), true);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].adminId, 7);
  assert.equal(calls[0].permission, "gm.kick_player");
  assert.deepEqual(calls[0].scope.targetIds, ["player-1"]);
  assert.deepEqual(calls[0].scope.worldId, "*");
});

test("AdminPolicyGuard returns a stable unauthenticated response", async () => {
  const fixture = makeContext({ permissions: ["players.read"], request: { admin: null } });
  const { guard } = makeGuard(fixture);
  await assert.rejects(() => guard.canActivate(fixture.context), assertHttpError(401, "UNAUTHORIZED"));
});

test("AdminPolicyGuard rejects route handlers without permission metadata", async () => {
  const fixture = makeContext();
  const { guard } = makeGuard(fixture);
  await assert.rejects(() => guard.canActivate(fixture.context), assertHttpError(403, "ADMIN_PERMISSION_NOT_DECLARED"));
});

test("AdminPolicyGuard maps unknown permission, missing grant, and scope denial to stable errors", async () => {
  const fixture = makeContext({ permissions: ["gm.kick_player"], request: { body: { playerId: "player-1" } } });

  for (const [decision, error] of [
    [{ allowed: false, code: "UNKNOWN_PERMISSION" }, "ADMIN_PERMISSION_UNAVAILABLE"],
    [{ allowed: false, code: "PERMISSION_DENIED" }, "ADMIN_PERMISSION_DENIED"],
    [{ allowed: false, code: "SCOPE_DENIED" }, "ADMIN_SCOPE_DENIED"],
    [{ allowed: false, code: "SCOPE_REQUIRED" }, "ADMIN_SCOPE_REQUIRED"]
  ]) {
    const { guard } = makeGuard(fixture, { decision });
    await assert.rejects(() => guard.canActivate(fixture.context), assertHttpError(403, error));
  }
});

test("AdminPolicyGuard sends every batch target and derives count instead of trusting client scope", async () => {
  const fixture = makeContext({
    permissions: ["gm.kick_player"],
    request: {
      body: {
        playerIds: ["player-1", "player-2"],
        targetCount: 1,
        scope: { targetIds: ["player-1"], maxTargets: 9999 }
      }
    }
  });
  const { guard, calls } = makeGuard(fixture, { decision: { allowed: false, code: "SCOPE_DENIED" } });

  await assert.rejects(() => guard.canActivate(fixture.context), assertHttpError(403, "ADMIN_SCOPE_DENIED"));
  assert.deepEqual(calls[0].scope.targetIds, ["player-1", "player-2"]);
  assert.equal(calls[0].scope.targetCount, 2);
});

test("AdminPolicyGuard resolves a GM character world from the server-side target record", async () => {
  const fixture = makeContext({
    permissions: ["gm.send_item"],
    request: { body: { characterId: "chr_123", worldId: "forged-world" } }
  });
  const { guard, calls } = makeGuard(fixture, {
    store: {
      async findCharacterById(characterId) {
        assert.equal(characterId, "chr_123");
        return { worldId: 42 };
      }
    }
  });

  assert.equal(await guard.canActivate(fixture.context), true);
  assert.equal(calls[0].scope.worldId, "42");
  assert.deepEqual(calls[0].scope.targetIds, ["chr_123"]);
});

test("extractAdminPolicyScope marks a targetless management request as global", async () => {
  const scope = await extractAdminPolicyScope({ params: {}, body: {}, query: {} }, "gm.broadcast");
  assert.equal(scope.worldId, "*");
  assert.deepEqual(scope.targetIds, ["*"]);
  assert.equal(scope.targetCount, 1);
});

test("AdminPolicyGuard supports server-defined conditional permissions", async () => {
  const fixture = makeContext({
    resolver: (request) => request.body.status === "banned"
      ? ["players.status.update", "players.ban"]
      : ["players.status.update"],
    request: { params: { playerId: "player-1" }, body: { status: "banned" } }
  });
  const { guard, calls } = makeGuard(fixture);

  assert.equal(await guard.canActivate(fixture.context), true);
  assert.deepEqual(calls.map((call) => call.permission), ["players.status.update", "players.ban"]);
});
