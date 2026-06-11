import assert from "node:assert/strict";
import { register } from "node:module";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../../tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const {
  PERMISSIONS_KEY,
  ROLES_KEY,
  ROLE_PERMISSIONS,
  roleHasPermission,
  roleHasAllPermissions
} = await import("./roles.decorator.ts");
const { RolesGuard } = await import("./roles.guard.ts");

function makeContext({ role, permissions, roles, method = "POST", url = "/api/v1/gm/ban-player" }) {
  function handler() {}

  const request = {
    method,
    url,
    headers: {},
    socket: {
      remoteAddress: "198.51.100.10"
    },
    admin: {
      sub: 7,
      username: "worker",
      role
    }
  };

  return {
    reflector: {
      getAllAndOverride(key) {
        if (key === PERMISSIONS_KEY) return permissions;
        if (key === ROLES_KEY) return roles;
        return undefined;
      }
    },
    context: {
      getHandler: () => handler,
      getClass: () => class TestController {},
      switchToHttp: () => ({
        getRequest: () => request
      })
    },
    request
  };
}

function assertForbidden(error) {
  assert.equal(error.getStatus(), 403);
  assert.deepEqual(error.getResponse(), {
    ok: false,
    error: "INSUFFICIENT_PERMISSION",
    message: "Insufficient permission"
  });
  return true;
}

test("admin-api role permission matrix covers read, player, GM, maintenance, monitoring, and admin management", () => {
  assert.deepEqual(ROLE_PERMISSIONS.viewer, [
    "audit.read",
    "security.read",
    "players.read",
    "maintenance.read",
    "monitoring.read"
  ]);

  assert.equal(roleHasPermission("viewer", "players.read"), true);
  assert.equal(roleHasPermission("viewer", "gm.kick_player"), false);
  assert.equal(roleHasPermission("operator", "players.status.update"), true);
  assert.equal(roleHasPermission("operator", "gm.kick_player"), true);
  assert.equal(roleHasPermission("operator", "gm.ban_player"), false);
  assert.equal(roleHasPermission("operator", "maintenance.write"), false);
  assert.equal(roleHasPermission("admin", "admins.reset_password"), true);
  assert.equal(roleHasPermission("super_admin", "monitoring.archive"), true);
  assert.equal(roleHasPermission("unknown", "players.read"), false);
});

test("RolesGuard allows typical read/write operations by permission", async () => {
  let fixture = makeContext({ role: "viewer", permissions: ["players.read"] });
  assert.equal(await new RolesGuard(fixture.reflector).canActivate(fixture.context), true);

  fixture = makeContext({ role: "operator", permissions: ["gm.kick_player"] });
  assert.equal(await new RolesGuard(fixture.reflector).canActivate(fixture.context), true);

  fixture = makeContext({ role: "admin", permissions: ["admins.revoke_tokens"] });
  assert.equal(await new RolesGuard(fixture.reflector).canActivate(fixture.context), true);

  fixture = makeContext({ role: "super_admin", permissions: ["gm.ban_player"] });
  assert.equal(await new RolesGuard(fixture.reflector).canActivate(fixture.context), true);
});

test("RolesGuard rejects unauthorized write operations with 403", async () => {
  const fixture = makeContext({ role: "viewer", permissions: ["gm.broadcast"] });

  await assert.rejects(() => new RolesGuard(fixture.reflector).canActivate(fixture.context), assertForbidden);
});

test("RolesGuard requires every declared permission", async () => {
  assert.equal(roleHasAllPermissions("operator", ["players.read", "players.status.update"]), true);
  assert.equal(roleHasAllPermissions("operator", ["players.read", "players.ban"]), false);

  const fixture = makeContext({
    role: "operator",
    permissions: ["players.status.update", "players.ban"],
    url: "/api/v1/players/player-1/status"
  });

  await assert.rejects(() => new RolesGuard(fixture.reflector).canActivate(fixture.context), assertForbidden);
});

test("RolesGuard keeps legacy role metadata compatible when no permission metadata is present", async () => {
  let fixture = makeContext({ role: "operator", roles: ["operator", "admin"] });
  assert.equal(await new RolesGuard(fixture.reflector).canActivate(fixture.context), true);

  fixture = makeContext({ role: "viewer", roles: ["operator", "admin"] });
  await assert.rejects(() => new RolesGuard(fixture.reflector).canActivate(fixture.context), assertForbidden);
});

test("RolesGuard writes security audit on permission denial", async () => {
  const fixture = makeContext({ role: "viewer", permissions: ["gm.broadcast"] });
  const audits = [];
  const store = {
    async appendSecurityAuditLog(entry) {
      audits.push(entry);
    }
  };

  await assert.rejects(
    () => new RolesGuard(fixture.reflector, store, {}).canActivate(fixture.context),
    assertForbidden
  );

  assert.equal(audits.length, 1);
  assert.equal(audits[0].eventType, "admin_permission_denied");
  assert.equal(audits[0].targetValue, "worker");
  assert.equal(audits[0].severity, "critical");
  assert.equal(audits[0].clientIp, "198.51.100.10");
  assert.deepEqual(audits[0].details.requiredPermissions, ["gm.broadcast"]);
  assert.equal(audits[0].details.adminId, 7);
});

test("RolesGuard audit write failure does not change 403 result", async () => {
  const fixture = makeContext({ role: "viewer", permissions: ["gm.broadcast"] });
  const store = {
    async appendSecurityAuditLog() {
      throw new Error("mysql unavailable");
    }
  };

  await assert.rejects(
    () => new RolesGuard(fixture.reflector, store, {}).canActivate(fixture.context),
    assertForbidden
  );
});
