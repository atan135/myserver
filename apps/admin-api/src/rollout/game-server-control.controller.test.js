import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { register } from "node:module";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../../tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const { RolloutController } = await import("./rollout.controller.ts");
const { AdminPolicyGuard } = await import("../auth/admin-policy.guard.ts");
const { AdminPolicyService } = await import("../auth/admin-policy.service.ts");
const { AdminOperationService } = await import("../operations/admin-operation.service.ts");
const { AdminHighRiskOperationService } = await import("../operations/admin-high-risk-operation.service.ts");
const { AdminBreakglassService } = await import("../operations/admin-breakglass.service.ts");
const { AdminOperationController } = await import("../operations/admin-operation.controller.ts");
const { PERMISSIONS_KEY, POLICY_SCOPE_RESOLVER_KEY } = await import("../auth/roles.decorator.ts");

test("admin-api shutdown assertion permission matches the Rust game-server contract", () => {
  const source = readFileSync(
    new URL("../../../game-server/src/admin_server.rs", import.meta.url),
    "utf8"
  );
  const requirement = source.match(/fn admin_write_requirement[\s\S]*?\n}\n\nfn is_emergency_asset_correction/)?.[0] || "";

  assert.match(
    requirement,
    /MessageType::RequestServerShutdownReq\s*=>\s*\("service\.shutdown",\s*"service"\)/
  );
});

const ROOT_SCOPE = {
  world_ids: ["*"],
  service_names: ["*"],
  instance_ids: ["*"],
  field_allowlist: ["*"],
  target_types: ["*"],
  target_ids: ["*"],
  max_targets: 100
};

function body(overrides = {}) {
  return {
    enabled: true,
    reason: "rolling replacement",
    requestId: "control-request-1",
    backupReference: "control-request-1-recovery",
    preflightNonce: "nonce-1",
    preflightSummarySha256: "summary-1",
    ...overrides
  };
}

function createController({ client = {}, run } = {}) {
  let captured;
  const highRisk = {
    async run(input) {
      captured = input;
      if (run) return run(input);
      return { state: "executed", result: await input.execute() };
    }
  };
  return {
    controller: new RolloutController({}, highRisk, client),
    captured: () => captured
  };
}

function routeGuard(handler, request, grants) {
  const permissions = {
    "game.config.write": {
      permission_key: "game.config.write",
      active: true,
      scope_dimensions: ["world_ids", "service_names"]
    },
    "service.shutdown": {
      permission_key: "service.shutdown",
      active: true,
      scope_dimensions: ["service_names", "instance_ids"]
    }
  };
  const store = {
    async findAdminPolicyPermission(permission) { return permissions[permission] || null; },
    async listEffectiveAdminPolicyGrants(_adminId, permission) {
      return (grants[permission] || []).map((scope_json, index) => ({
        ...permissions[permission],
        grant_source: "direct",
        source_id: index + 1,
        scope_json
      }));
    }
  };
  const reflector = {
    getAllAndOverride(key, targets) {
      for (const target of targets) {
        const value = Reflect.getMetadata(key, target);
        if (value !== undefined) return value;
      }
      return undefined;
    }
  };
  const context = {
    getHandler: () => handler,
    getClass: () => RolloutController,
    switchToHttp: () => ({ getRequest: () => request })
  };
  return { guard: new AdminPolicyGuard(reflector, new AdminPolicyService(store), store, {}), context };
}

test("game-server drain control binds registry target, assertion scope, and audit-safe payload", async () => {
  let options;
  const { controller, captured } = createController({
    client: {
      async updateConfig(key, value, inputOptions) {
        assert.equal(key, "drain_mode");
        assert.equal(value, "on");
        options = inputOptions;
        return { ok: true, errorCode: "", instanceId: "game-server-a" };
      }
    }
  });

  const result = await controller.setGameServerDrain(
    "game-server-a",
    body(),
    { admin: { sub: "admin-7" }, body: body() }
  );

  assert.equal(result.ok, true);
  assert.equal(options.targetInstanceId, "game-server-a");
  assert.equal(options.requireRegistryTarget, true);
  assert.equal(options.assertionContext.permission, "game.config.write");
  assert.equal(options.assertionContext.scope.worldId, "*");
  assert.equal(options.assertionContext.scope.instanceId, "game-server-a");
  assert.equal(captured().scope.worldId, "*");
  assert.equal(captured().scope.targetType, "config");
  assert.equal(captured().payload.instanceId, "game-server-a");
  assert.doesNotMatch(JSON.stringify(captured().payload), /token|assertion|host|port|password/i);
});

test("game-server shutdown preserves immediate, armed, and blocked result fields", async (t) => {
  const cases = [
    { ok: true, error_code: "", shutdown_armed: true, connection_count: 0, owned_room_count: 0, migrating_room_count: 0 },
    { ok: false, error_code: "SHUTDOWN_CONNECTIONS_REMAIN", shutdown_armed: true, connection_count: 2, owned_room_count: 0, migrating_room_count: 0 },
    { ok: false, error_code: "SHUTDOWN_DRAIN_MODE_REQUIRED", shutdown_armed: false, connection_count: 0, owned_room_count: 0, migrating_room_count: 0 }
  ];
  for (const expected of cases) {
    await t.test(expected.error_code || "immediate", async () => {
      const { controller } = createController({
        client: { async requestServerShutdown() { return expected; } }
      });
      const result = await controller.shutdownGameServer(
        "game-server-a",
        body(),
        { admin: { sub: "admin-7" }, body: body() }
      );
      assert.deepEqual(result, expected);
    });
  }
});

test("game-server shutdown binds the emergency service permission to the exact registry instance", async () => {
  let options;
  const { controller, captured } = createController({
    client: {
      async requestServerShutdown(_reason, inputOptions) {
        options = inputOptions;
        return { ok: true, error_code: "", shutdown_armed: true };
      }
    }
  });

  await controller.shutdownGameServer(
    "game-server-a",
    body(),
    { admin: { sub: "admin-7" }, body: body() }
  );

  assert.equal(captured().permission, "service.shutdown");
  assert.equal(captured().emergency, true);
  assert.equal(captured().scope.serviceName, "game-server");
  assert.equal(captured().scope.instanceId, "game-server-a");
  assert.equal(captured().scope.worldId, undefined);
  assert.deepEqual(captured().targetSummary, {
    targetType: "service",
    targetIds: ["game-server-a"],
    serviceName: "game-server",
    instanceId: "game-server-a",
    worldId: null
  });
  assert.equal(options.assertionContext.permission, "service.shutdown");
  assert.equal(options.assertionContext.scope.instanceId, "game-server-a");
  assert.equal(options.assertionContext.scope.worldId, undefined);
});

test("formal break-glass activation matches only the exact game-server shutdown target", async () => {
  const grants = [];
  const store = {
    async findAdminPolicyPermission(permission) {
      if (permission === "service.shutdown") {
        return {
          permission_key: permission,
          active: true,
          risk_level: "emergency",
          scope_dimensions: ["service_names", "instance_ids"]
        };
      }
      return null;
    },
    async createAdminBreakglassGrant(input) {
      const grant = { ...input, activatedAt: new Date().toISOString(), revokedAt: null };
      grants.push(grant);
      return { kind: "created", grant };
    },
    async listActiveAdminBreakglassGrants(adminId, permission) {
      return grants.filter((grant) =>
        String(grant.actorAdminId) === String(adminId) && grant.permissionKey === permission
      );
    }
  };
  const breakglass = new AdminBreakglassService({
    async authorize() { return { allowed: true, code: "ALLOWED" }; }
  }, store);
  const operationController = new AdminOperationController({}, breakglass, {});
  await operationController.activateBreakglass({
    requestId: "phase7-breakglass-1",
    permission: "service.shutdown",
    serviceName: "game-server",
    instanceId: "game-server-a",
    targetType: "service",
    targetIds: ["game-server-a"],
    reason: "isolated rollout verification",
    ttlMs: 60000
  }, { admin: { sub: "admin-7" } });

  let operationInput;
  const rollout = new RolloutController({}, {
    async run(input) {
      operationInput = input;
      return { state: "preflight", response: { ok: true, state: "preflighted" } };
    }
  }, {});
  await rollout.shutdownGameServer(
    "game-server-a",
    body(),
    { admin: { sub: "admin-7" }, body: body() }
  );

  const matched = await breakglass.requireActiveGrant({
    actorAdminId: "admin-7",
    permission: operationInput.permission,
    scope: operationInput.scope,
    targetSummary: operationInput.targetSummary
  });
  assert.equal(matched.permissionKey, "service.shutdown");

  const mismatches = [
    {
      scope: { ...operationInput.scope, serviceName: "other-service" },
      targetSummary: { ...operationInput.targetSummary, serviceName: "other-service" }
    },
    {
      scope: { ...operationInput.scope, instanceId: "game-server-b" },
      targetSummary: { ...operationInput.targetSummary, instanceId: "game-server-b" }
    },
    {
      scope: { ...operationInput.scope, worldId: "world-2" },
      targetSummary: { ...operationInput.targetSummary, worldId: "world-2" }
    },
    {
      scope: { ...operationInput.scope, targetIds: ["game-server-b"] },
      targetSummary: { ...operationInput.targetSummary, targetIds: ["game-server-b"] }
    }
  ];
  for (const mismatch of mismatches) {
    await assert.rejects(
      breakglass.requireActiveGrant({
        actorAdminId: "admin-7",
        permission: operationInput.permission,
        ...mismatch
      }),
      (error) => error.code === "ADMIN_BREAKGLASS_GRANT_REQUIRED"
    );
  }
});

test("game-server controls pass real policy and high-risk preflight with least-privilege scopes", async () => {
  const permissions = {
    "game.config.write": {
      permission_key: "game.config.write",
      active: true,
      risk_level: "high",
      scope_dimensions: ["world_ids", "service_names"]
    },
    "service.shutdown": {
      permission_key: "service.shutdown",
      active: true,
      risk_level: "emergency",
      scope_dimensions: ["service_names", "instance_ids"]
    }
  };
  const reservations = [];
  const store = {
    async findAdminPolicyPermission(permission) {
      return permissions[permission] || null;
    },
    async listEffectiveAdminPolicyGrants(_adminId, permission) {
      return permissions[permission]
        ? [{ ...permissions[permission], grant_source: "role", source_id: 1, scope_json: ROOT_SCOPE }]
        : [];
    },
    async reserveAdminOperationPreflight(input) {
      reservations.push(input);
      return {
        kind: "created",
        operation: {
          operationId: input.operationId,
          requestId: input.requestId,
          status: "preflighted",
          approvalStatus: input.approvalStatus
        }
      };
    }
  };
  const policy = new AdminPolicyService(store);
  const operations = new AdminOperationService({ adminOperationPreflightTtlMs: 120000 }, policy, store);
  const highRisk = new AdminHighRiskOperationService(operations, {}, {});
  const controller = new RolloutController({}, highRisk, {
    async updateConfig() { throw new Error("preflight must not call game-server"); },
    async requestServerShutdown() { throw new Error("preflight must not call game-server"); }
  });
  const preflightBody = body({ preflightNonce: undefined, preflightSummarySha256: undefined });

  const drain = await controller.setGameServerDrain(
    "game-server-a",
    preflightBody,
    { admin: { sub: "admin-7" }, body: preflightBody }
  );
  const shutdownBody = { ...preflightBody, requestId: "control-request-2" };
  const shutdown = await controller.shutdownGameServer(
    "game-server-a",
    shutdownBody,
    { admin: { sub: "admin-7" }, body: shutdownBody }
  );

  assert.equal(drain.state, "preflighted");
  assert.equal(shutdown.state, "preflighted");
  assert.deepEqual(reservations.map((entry) => entry.permissionKey), ["game.config.write", "service.shutdown"]);
  assert.equal(reservations[0].requestedScope.worldId, "*");
  assert.equal(reservations[0].requestedScope.serviceName, "game-server");
  assert.equal(reservations[1].requestedScope.worldId, null);
  assert.equal(reservations[1].requestedScope.instanceId, "game-server-a");
  assert.deepEqual(reservations.map((entry) => entry.approvalStatus), ["pending", "pending"]);
});

test("game-server route guards accept only matching narrow grants and ignore forged body scope", async () => {
  const drainScope = { ...ROOT_SCOPE, service_names: ["game-server"] };
  const shutdownScope = {
    ...ROOT_SCOPE,
    service_names: ["game-server"],
    instance_ids: ["game-server-a"]
  };
  const forgedBody = {
    worldId: "forged-world",
    serviceName: "forged-service",
    instanceId: "forged-instance",
    targetIds: ["forged-target"]
  };
  const drainRequest = {
    admin: { sub: "admin-7", username: "operator" },
    params: { instanceId: "game-server-a" },
    query: {},
    body: forgedBody,
    headers: {},
    method: "POST",
    url: "/api/v1/rollouts/game-server/game-server-a/drain"
  };
  const shutdownRequest = {
    ...drainRequest,
    url: "/api/v1/rollouts/game-server/game-server-a/shutdown"
  };

  let fixture = routeGuard(RolloutController.prototype.setGameServerDrain, drainRequest, {
    "game.config.write": [drainScope]
  });
  assert.equal(await fixture.guard.canActivate(fixture.context), true);

  fixture = routeGuard(RolloutController.prototype.shutdownGameServer, shutdownRequest, {
    "service.shutdown": [shutdownScope]
  });
  assert.equal(await fixture.guard.canActivate(fixture.context), true);

  for (const scope of [
    { ...shutdownScope, service_names: ["other-service"] },
    { ...shutdownScope, instance_ids: ["game-server-b"] }
  ]) {
    fixture = routeGuard(RolloutController.prototype.shutdownGameServer, shutdownRequest, {
      "service.shutdown": [scope]
    });
    await assert.rejects(
      fixture.guard.canActivate(fixture.context),
      (error) => error.getStatus() === 403 && error.getResponse().error === "ADMIN_SCOPE_DENIED"
    );
  }
});

test("game-server route guard rejects an invalid instance before policy evaluation", async () => {
  const request = {
    admin: { sub: "admin-7", username: "operator" },
    params: { instanceId: "../other" },
    query: {},
    body: { serviceName: "game-server", instanceId: "game-server-a" },
    headers: {},
    method: "POST",
    url: "/api/v1/rollouts/game-server/../other/shutdown"
  };
  const fixture = routeGuard(RolloutController.prototype.shutdownGameServer, request, {
    "service.shutdown": [{ ...ROOT_SCOPE }]
  });

  await assert.rejects(
    fixture.guard.canActivate(fixture.context),
    (error) => error.getStatus() === 403 && error.getResponse().error === "ADMIN_SCOPE_DENIED"
  );
});

test("game-server control maps unknown targets and downstream timeouts", async (t) => {
  for (const [code, status] of [
    ["GAME_SERVER_ADMIN_TARGET_NOT_FOUND", 404],
    ["GAME_ADMIN_READ_TIMEOUT", 502]
  ]) {
    await t.test(code, async () => {
      const error = Object.assign(new Error(code), { code });
      const { controller } = createController({
        client: { async requestServerShutdown() { throw error; } }
      });
      await assert.rejects(
        controller.shutdownGameServer(
          "game-server-a",
          body(),
          { admin: { sub: "admin-7" }, body: body() }
        ),
        (thrown) => thrown.getStatus() === status
      );
    });
  }
});

test("game-server control rejects invalid targets before downstream calls", async () => {
  const { controller } = createController({ client: {} });
  await assert.rejects(
    controller.setGameServerDrain("../other", body(), { admin: { sub: "admin-7" }, body: body() }),
    (error) => error.getStatus() === 400
  );
});

test("game-server control endpoints require action-specific permissions", () => {
  const drainPermission = Reflect.getMetadata(PERMISSIONS_KEY, RolloutController.prototype.setGameServerDrain);
  const shutdownPermission = Reflect.getMetadata(PERMISSIONS_KEY, RolloutController.prototype.shutdownGameServer);
  assert.deepEqual(drainPermission, ["game.config.write"]);
  assert.deepEqual(shutdownPermission, ["service.shutdown"]);
  assert.equal(typeof Reflect.getMetadata(POLICY_SCOPE_RESOLVER_KEY, RolloutController.prototype.setGameServerDrain), "function");
  assert.equal(typeof Reflect.getMetadata(POLICY_SCOPE_RESOLVER_KEY, RolloutController.prototype.shutdownGameServer), "function");
});
