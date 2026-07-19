import assert from "node:assert/strict";
import { register } from "node:module";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../../tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const { AuthService } = await import("./auth.service.ts");

test("auth me returns server-derived effective permissions and scope summaries", async () => {
  const admin = {
    id: 7,
    username: "bootstrap-admin",
    displayName: "Bootstrap Admin",
    role: "viewer"
  };
  const policy = {
    async effectiveCapabilities(adminId) {
      assert.equal(adminId, 7);
      return new Map([
        ["audit.read", [{ scope: { targetIds: ["*"] } }]],
        ["gm.send_item", [{ scope: { worldIds: ["world-1"], targetIds: ["character-1"] } }]]
      ]);
    }
  };
  const service = new AuthService(
    {},
    {},
    { async findAdminByUsername() { return admin; } },
    {},
    policy
  );

  const response = await service.me({ admin: { username: admin.username } });

  assert.deepEqual(response.admin.permissions, ["audit.read", "gm.send_item"]);
  assert.deepEqual(response.admin.permissionScopes, {
    "audit.read": [{ targetIds: ["*"] }],
    "gm.send_item": [{ worldIds: ["world-1"], targetIds: ["character-1"] }]
  });
  assert.equal(response.admin.role, "viewer");
});
