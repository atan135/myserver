import assert from "node:assert/strict";
import { register } from "node:module";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../../tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const { PERMISSIONS_KEY } = await import("../auth/roles.decorator.ts");
const { MyforgeController } = await import("./myforge.controller.ts");
const { HTTP_CODE_METADATA } = await import("@nestjs/common/constants.js");

function request() {
  return {
    admin: { sub: 7, username: "admin", role: "admin" },
    headers: {},
    socket: { remoteAddress: "127.0.0.1" }
  };
}

function codedError(code, message, statusCode) {
  const error = new Error(message);
  error.code = code;
  error.statusCode = statusCode;
  return error;
}

const highRiskOperations = {
  async run(input) {
    return { state: "executed", result: await input.execute() };
  }
};

test("myforge controller declares one exact permission per HTTP operation", () => {
  assert.deepEqual(Reflect.getMetadata(PERMISSIONS_KEY, MyforgeController.prototype.listAgents), ["myforge.agent.read"]);
  assert.deepEqual(Reflect.getMetadata(PERMISSIONS_KEY, MyforgeController.prototype.listTasks), ["myforge.task.read"]);
  assert.deepEqual(Reflect.getMetadata(PERMISSIONS_KEY, MyforgeController.prototype.getTask), ["myforge.task.read"]);
  assert.deepEqual(Reflect.getMetadata(PERMISSIONS_KEY, MyforgeController.prototype.createFangyuanBlueprint), ["myforge.task.create"]);
  assert.deepEqual(Reflect.getMetadata(PERMISSIONS_KEY, MyforgeController.prototype.cancelTask), ["myforge.task.cancel"]);
  assert.equal(Reflect.getMetadata(HTTP_CODE_METADATA, MyforgeController.prototype.createFangyuanBlueprint), 202);
  assert.equal(Reflect.getMetadata(HTTP_CODE_METADATA, MyforgeController.prototype.cancelTask), 200);
});

test("myforge controller passes typed create actor context and cancel body to the orchestrator", async () => {
  const calls = [];
  const orchestrator = {
    async createFangyuanBlueprint(body, actor) {
      calls.push({ method: "create", body, actor });
      return { ok: true, requestId: "11111111-1111-4111-8111-111111111111", status: "queued" };
    },
    async cancelTask(requestId, body, actor) {
      calls.push({ method: "cancel", requestId, body, actor });
      return { ok: true, requestId, status: "cancelled" };
    }
  };
  const controller = new MyforgeController({}, orchestrator, highRiskOperations);
  const body = { agentId: "dev-pc-001", reason: "operator requested blueprint" };
  await controller.createFangyuanBlueprint(body, request());
  await controller.cancelTask("11111111-1111-4111-8111-111111111111", {}, request());

  assert.deepEqual(calls[0], {
    method: "create",
    body: { agentId: "dev-pc-001" },
    actor: { adminId: 7, adminUsername: "admin", ip: "127.0.0.1" }
  });
  assert.equal(calls[1].method, "cancel");
  assert.deepEqual(calls[1].body, {});
  assert.equal(calls[1].actor.adminUsername, "admin");
});

test("myforge controller supports list and repeated detail polling without response rewriting", async () => {
  let detailCalls = 0;
  const orchestrator = {
    async listAgents(query) { return { ok: true, items: [], total: 0, query }; },
    async listTasks(query) { return { ok: true, items: [], total: 0, limit: 20, offset: 0, query }; },
    async getTask(requestId) {
      detailCalls += 1;
      return {
        ok: true,
        task: {
          requestId,
          status: detailCalls === 1 ? "running" : "completed",
          executionMode: "codex_exec"
        }
      };
    }
  };
  const controller = new MyforgeController({}, orchestrator, highRiskOperations);
  assert.equal((await controller.listAgents({})).total, 0);
  assert.equal((await controller.listTasks({ status: "running" })).limit, 20);
  const first = await controller.getTask("11111111-1111-4111-8111-111111111111");
  const second = await controller.getTask("11111111-1111-4111-8111-111111111111");
  assert.equal(first.task.status, "running");
  assert.equal(second.task.status, "completed");
});

test("myforge controller maps explicit validation, conflict, not-found, size, and disabled errors", async () => {
  const scenarios = [
    ["MYFORGE_TARGET_PATH_INVALID", 400],
    ["MYFORGE_TASK_NOT_FOUND", 404],
    ["MYFORGE_AGENT_PROJECT_MISMATCH", 409],
    ["MYFORGE_PROMPT_TOO_LARGE", 413],
    ["MYFORGE_DISABLED", 503]
  ];
  for (const [code, status] of scenarios) {
    const controller = new MyforgeController({}, {
      async getTask() { throw codedError(code, `${code} message`); }
    });
    await assert.rejects(
      controller.getTask("11111111-1111-4111-8111-111111111111"),
      (error) => {
        assert.equal(error.getStatus(), status);
        assert.deepEqual(error.getResponse(), {
          ok: false,
          error: code,
          message: `${code} message`
        });
        return true;
      }
    );
  }
});
