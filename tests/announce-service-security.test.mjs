import assert from "node:assert/strict";
import { mkdir, readFile, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { test } from "node:test";
import { pathToFileURL } from "node:url";
import ts from "typescript";

import "reflect-metadata";

async function transpileTypeScriptModule(sourcePath, outPath, replacements = {}) {
  const source = await readFile(sourcePath, "utf8");
  const compiled = ts.transpileModule(source, {
    compilerOptions: {
      target: ts.ScriptTarget.ES2022,
      module: ts.ModuleKind.ES2022,
      moduleResolution: ts.ModuleResolutionKind.NodeNext,
      experimentalDecorators: true,
      emitDecoratorMetadata: true
    },
    fileName: sourcePath
  });

  let outputText = compiled.outputText;
  for (const [from, to] of Object.entries(replacements)) {
    outputText = outputText.replaceAll(from, to);
  }

  await writeFile(outPath, outputText, "utf8");
}

async function loadAnnouncementsController() {
  const sourcePath = path.resolve(
    "apps/announce-service/src/announcements/announcements.controller.ts"
  );
  const outDir = path.resolve("tests/.tmp/announce-service-security");
  const outPath = path.join(outDir, "announcements.controller.mjs");

  await rm(outDir, { recursive: true, force: true });
  await mkdir(path.join(outDir, "common"), { recursive: true });
  await transpileTypeScriptModule(sourcePath, outPath, {
    "../common/": "./common/",
    "../tokens.js": "./tokens.js",
    "./announcements.service.js": "./announcements.service.js"
  });
  await writeFile(
    path.join(outDir, "common/http-exception.js"),
    "export * from '../../../../apps/announce-service/src/common/http-exception.ts';\n",
    "utf8"
  );
  await writeFile(
    path.join(outDir, "tokens.js"),
    "export * from '../../../apps/announce-service/src/tokens.ts';\n",
    "utf8"
  );
  await writeFile(
    path.join(outDir, "announcements.service.js"),
    "export class AnnouncementsService {}\n",
    "utf8"
  );

  const imported = await import(`${pathToFileURL(outPath).href}?v=${Date.now()}`);
  return imported.AnnouncementsController;
}

function createServiceStub() {
  const calls = [];
  return {
    calls,
    list(query) {
      calls.push(["list", query]);
      return { ok: true, announcements: [] };
    },
    get(announceId) {
      calls.push(["get", announceId]);
      return { ok: true, announcement: { announce_id: announceId } };
    },
    create(body) {
      calls.push(["create", body]);
      return { ok: true, announcement: body };
    },
    update(announceId, body) {
      calls.push(["update", announceId, body]);
      return { ok: true, announcement: { announce_id: announceId, ...body } };
    },
    delete(announceId) {
      calls.push(["delete", announceId]);
      return { ok: true, deleted: true };
    }
  };
}

function createController(ControllerClass) {
  const service = createServiceStub();
  const controller = new ControllerClass(service, {
    announceAdminToken: "test-announce-admin-token"
  });
  return { controller, service };
}

function assertApiError(error, statusCode, code) {
  assert.equal(error.getStatus?.(), statusCode);
  assert.equal(error.getResponse?.().ok, false);
  assert.equal(error.getResponse?.().error, code);
}

function withEnv(overrides, fn) {
  const previousEnv = new Map(Object.entries(process.env));
  try {
    for (const [key, value] of Object.entries(overrides)) {
      if (value === undefined) {
        delete process.env[key];
      } else {
        process.env[key] = value;
      }
    }
    return fn();
  } finally {
    for (const key of Object.keys(process.env)) {
      if (!previousEnv.has(key)) {
        delete process.env[key];
      }
    }
    for (const [key, value] of previousEnv.entries()) {
      process.env[key] = value;
    }
  }
}

test("announce write APIs reject missing admin token", async () => {
  const AnnouncementsController = await loadAnnouncementsController();
  const { controller, service } = createController(AnnouncementsController);

  assert.throws(
    () => controller.create({}, { title: "Title" }),
    (error) => {
      assertApiError(error, 401, "ANNOUNCE_ADMIN_TOKEN_REQUIRED");
      return true;
    }
  );
  assert.deepEqual(service.calls, []);
});

test("announce write APIs do not accept token outside supported headers", async () => {
  const AnnouncementsController = await loadAnnouncementsController();
  const { controller, service } = createController(AnnouncementsController);

  assert.throws(
    () =>
      controller.create(
        {},
        {
          title: "Title",
          admin_token: "test-announce-admin-token",
          announce_admin_token: "test-announce-admin-token"
        }
      ),
    (error) => {
      assertApiError(error, 401, "ANNOUNCE_ADMIN_TOKEN_REQUIRED");
      return true;
    }
  );
  assert.deepEqual(service.calls, []);
});

test("announce write APIs reject invalid admin token", async () => {
  const AnnouncementsController = await loadAnnouncementsController();
  const { controller, service } = createController(AnnouncementsController);

  assert.throws(
    () =>
      controller.update(
        "ann_001",
        { authorization: "Bearer wrong-token" },
        { title: "Updated" }
      ),
    (error) => {
      assertApiError(error, 403, "ANNOUNCE_ADMIN_TOKEN_INVALID");
      return true;
    }
  );
  assert.deepEqual(service.calls, []);
});

test("announce write APIs accept Bearer and X-Admin-Token headers", async () => {
  const AnnouncementsController = await loadAnnouncementsController();
  const { controller, service } = createController(AnnouncementsController);

  const created = controller.create(
    { authorization: "Bearer test-announce-admin-token" },
    { title: "Title" }
  );
  const deleted = controller.delete("ann_001", {
    "X-Admin-Token": "test-announce-admin-token"
  });

  assert.equal(created.ok, true);
  assert.equal(deleted.ok, true);
  assert.deepEqual(service.calls, [
    ["create", { title: "Title" }],
    ["delete", "ann_001"]
  ]);
});

test("announce read APIs do not require admin token", async () => {
  const AnnouncementsController = await loadAnnouncementsController();
  const { controller, service } = createController(AnnouncementsController);

  assert.equal(controller.list({ locale: "zh-CN" }).ok, true);
  assert.equal(controller.get("ann_001").ok, true);
  assert.deepEqual(service.calls, [
    ["list", { locale: "zh-CN" }],
    ["get", "ann_001"]
  ]);
});

test("announce-service production config rejects default admin token", async () => {
  const { getConfig } = await import(
    `../apps/announce-service/src/config.js?v=${Date.now()}`
  );

  withEnv(
    {
      NODE_ENV: "production",
      APP_ENV: undefined,
      ANNOUNCE_ADMIN_TOKEN: undefined
    },
    () => {
      assert.throws(
        () => getConfig(),
        /ANNOUNCE_ADMIN_TOKEN must be set to a non-default value in production/
      );
    }
  );

  withEnv(
    {
      NODE_ENV: "development",
      APP_ENV: "production",
      ANNOUNCE_ADMIN_TOKEN: "dev-only-change-this-announce-admin-token"
    },
    () => {
      assert.throws(
        () => getConfig(),
        /ANNOUNCE_ADMIN_TOKEN must be set to a non-default value in production/
      );
    }
  );

  withEnv(
    {
      NODE_ENV: "production",
      APP_ENV: undefined,
      ANNOUNCE_ADMIN_TOKEN: "prod-announce-admin-token"
    },
    () => {
      const config = getConfig();
      assert.equal(config.env, "production");
      assert.equal(config.announceAdminToken, "prod-announce-admin-token");
    }
  );
});
