import assert from "node:assert/strict";
import { register } from "node:module";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../../tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const {
  AdminPolicyService,
  adminPolicyScopeToDatabase,
  normalizeAdminPolicyScope
} = await import("./admin-policy.service.ts");

const ROOT_SCOPE = {
  world_ids: ["*"],
  service_names: ["*"],
  instance_ids: ["*"],
  field_allowlist: ["*"],
  target_types: ["*"],
  target_ids: ["*"],
  max_targets: 100
};

function store({ permission, grants = [] } = {}) {
  return {
    async findAdminPolicyPermission(key) {
      return key === permission?.permission_key ? permission : null;
    },
    async listEffectiveAdminPolicyGrants(_adminId, key) {
      return grants.filter((grant) => grant.permission_key === key);
    }
  };
}

const SEND_ITEM = {
  permission_key: "gm.send_item",
  active: true,
  scope_dimensions: ["world_ids", "target_ids"]
};

test("policy scopes require every dimension, reject malformed values, and retain an explicit root scope", () => {
  assert.deepEqual(normalizeAdminPolicyScope(ROOT_SCOPE), {
    worldIds: ["*"],
    serviceNames: ["*"],
    instanceIds: ["*"],
    fieldAllowlist: ["*"],
    targetTypes: ["*"],
    targetIds: ["*"],
    maxTargets: 100
  });
  assert.deepEqual(adminPolicyScopeToDatabase(normalizeAdminPolicyScope(ROOT_SCOPE)), ROOT_SCOPE);
  assert.throws(() => normalizeAdminPolicyScope({ ...ROOT_SCOPE, target_ids: ["*", "player-1"] }), /cannot combine wildcard/);
  assert.throws(() => normalizeAdminPolicyScope({ ...ROOT_SCOPE, max_targets: 0 }), /between 1/);
  assert.throws(() => normalizeAdminPolicyScope({ ...ROOT_SCOPE, unexpected: true }), /unknown key/);
});

test("policy service defaults to deny for unknown and inactive permissions", async () => {
  let policy = new AdminPolicyService(store());
  assert.deepEqual(await policy.authorize(7, "unknown.write"), {
    allowed: false,
    code: "UNKNOWN_PERMISSION",
    permissionKey: "unknown.write"
  });

  policy = new AdminPolicyService(store({ permission: { ...SEND_ITEM, active: false } }));
  assert.deepEqual(await policy.authorize(7, "gm.send_item", { worldId: "world-1", targetIds: ["player-1"] }), {
    allowed: false,
    code: "PERMISSION_INACTIVE",
    permissionKey: "gm.send_item"
  });
});

test("policy service requires a catalog-declared scope and accepts only a matching effective grant", async () => {
  const policy = new AdminPolicyService(store({
    permission: SEND_ITEM,
    grants: [{
      ...SEND_ITEM,
      grant_source: "direct",
      source_id: 22,
      scope_json: {
        ...ROOT_SCOPE,
        world_ids: ["world-1"],
        target_ids: ["player-1", "player-2"],
        max_targets: 2
      }
    }]
  }));

  assert.equal((await policy.authorize(7, "gm.send_item", { targetIds: ["player-1"] })).code, "SCOPE_REQUIRED");
  assert.equal((await policy.authorize(7, "gm.send_item", { worldId: "world-2", targetIds: ["player-1"] })).code, "SCOPE_DENIED");
  assert.equal((await policy.authorize(7, "gm.send_item", { worldId: "world-1", targetIds: ["player-1", "player-2", "player-3"] })).code, "SCOPE_DENIED");

  const allowed = await policy.authorize(7, "gm.send_item", { worldId: "world-1", targetIds: ["player-1", "player-2"] });
  assert.equal(allowed.allowed, true);
  assert.equal(allowed.matchedGrant.source, "direct");
  assert.equal(allowed.matchedGrant.sourceId, 22);
});

test("malformed persisted scopes do not become capabilities", async () => {
  const policy = new AdminPolicyService(store({
    permission: SEND_ITEM,
    grants: [{
      ...SEND_ITEM,
      grant_source: "role",
      source_id: 3,
      scope_json: { world_ids: ["world-1"] }
    }]
  }));

  assert.equal((await policy.authorize(7, "gm.send_item", { worldId: "world-1", targetIds: ["player-1"] })).code, "SCOPE_DENIED");
  assert.deepEqual([...await policy.effectiveCapabilities(7)], []);
});
