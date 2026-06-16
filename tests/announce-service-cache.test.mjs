import assert from "node:assert/strict";
import { mkdir, readFile, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { test } from "node:test";
import { pathToFileURL } from "node:url";
import ts from "typescript";

import "reflect-metadata";
import { AnnouncementStore } from "../apps/announce-service/src/db-store.js";

async function loadAnnouncementsService() {
  const sourcePath = path.resolve(
    "apps/announce-service/src/announcements/announcements.service.ts"
  );
  const outDir = path.resolve("tests/.tmp/announce-service-cache");
  const outPath = path.join(outDir, "announcements.service.mjs");
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

  await rm(outDir, { recursive: true, force: true });
  await mkdir(path.join(outDir, "common"), { recursive: true });
  const outputText = compiled.outputText
    .replaceAll("../common/", "./common/")
    .replaceAll("../logger.js", "./logger.js")
    .replaceAll("../tokens.js", "./tokens.js");

  await writeFile(outPath, outputText, "utf8");
  await writeFile(
    path.join(outDir, "common/http-exception.js"),
    "export * from '../../../../apps/announce-service/src/common/http-exception.ts';\n",
    "utf8"
  );
  await writeFile(
    path.join(outDir, "logger.js"),
    "export function log() {}\n",
    "utf8"
  );
  await writeFile(
    path.join(outDir, "tokens.js"),
    "export * from '../../../apps/announce-service/src/tokens.ts';\n",
    "utf8"
  );

  const imported = await import(`${pathToFileURL(outPath).href}?v=${Date.now()}`);
  return imported.AnnouncementsService;
}

class FakeRedis {
  constructor() {
    this.values = new Map();
    this.failGet = false;
    this.failSet = false;
    this.failScan = false;
  }

  async get(key) {
    if (this.failGet) {
      throw new Error("redis get down");
    }

    return this.values.get(key) ?? null;
  }

  async set(key, value) {
    if (this.failSet) {
      throw new Error("redis set down");
    }

    this.values.set(key, value);
  }

  async scan(cursor, _match, pattern) {
    if (this.failScan) {
      throw new Error("redis scan down");
    }

    const prefix = pattern.replace("*", "");
    const keys = Array.from(this.values.keys()).filter((key) =>
      key.startsWith(prefix)
    );
    return ["0", keys];
  }

  async del(...keys) {
    for (const key of keys) {
      this.values.delete(key);
    }
  }
}

async function createActiveAnnouncement(store, overrides = {}) {
  return store.createAnnouncement({
    announce_id: overrides.announce_id || "ann_001",
    locale: overrides.locale || "zh-CN",
    title: overrides.title || "Title",
    content: overrides.content || "Content",
    priority: overrides.priority ?? 1,
    type: overrides.type || "banner",
    target_group: overrides.target_group || "all",
    start_time: new Date(Date.now() - 1000).toISOString(),
    end_time: new Date(Date.now() + 60_000).toISOString()
  });
}

function spyListAnnouncements(store) {
  let calls = 0;
  const original = store.listAnnouncements.bind(store);
  store.listAnnouncements = async (...args) => {
    calls += 1;
    return original(...args);
  };

  return {
    get calls() {
      return calls;
    }
  };
}

test("AnnouncementsService list uses Redis cache for repeated active queries", async () => {
  const AnnouncementsService = await loadAnnouncementsService();
  const store = new AnnouncementStore(null);
  const redis = new FakeRedis();
  const listSpy = spyListAnnouncements(store);
  await createActiveAnnouncement(store);

  const service = new AnnouncementsService(store, redis, {
    announceCacheTtlSeconds: 10
  });

  const first = await service.list({
    locale: "zh-CN",
    target_group: "all",
    priority: "1",
    limit: "10",
    offset: "0",
    active_only: "true"
  });
  const second = await service.list({
    locale: "zh-CN",
    target_group: "all",
    priority: "1",
    limit: "10",
    offset: "0",
    active_only: "true"
  });

  assert.equal(listSpy.calls, 1);
  assert.equal(first.announcements.length, 1);
  assert.deepEqual(second, first);
});

test("AnnouncementsService write operations invalidate list cache", async () => {
  const AnnouncementsService = await loadAnnouncementsService();
  const store = new AnnouncementStore(null);
  const redis = new FakeRedis();
  const listSpy = spyListAnnouncements(store);
  await createActiveAnnouncement(store);

  const service = new AnnouncementsService(store, redis, {
    announceCacheTtlSeconds: 10
  });

  await service.list({ locale: "zh-CN" });
  await service.create({
    locale: "zh-CN",
    title: "Next",
    content: "Next content",
    duration_seconds: 60
  });
  const afterCreate = await service.list({ locale: "zh-CN" });

  assert.equal(listSpy.calls, 2);
  assert.equal(afterCreate.announcements.length, 2);
});

test("AnnouncementsService falls back to store when Redis operations fail", async () => {
  const AnnouncementsService = await loadAnnouncementsService();
  const store = new AnnouncementStore(null);
  const redis = new FakeRedis();
  redis.failGet = true;
  redis.failSet = true;
  redis.failScan = true;
  const listSpy = spyListAnnouncements(store);
  await createActiveAnnouncement(store);

  const service = new AnnouncementsService(store, redis, {
    announceCacheTtlSeconds: 10
  });

  const first = await service.list({ locale: "zh-CN" });
  await service.update("ann_001", { title: "Updated" });
  const second = await service.list({ locale: "zh-CN" });

  assert.equal(first.ok, true);
  assert.equal(second.ok, true);
  assert.equal(second.announcements[0].title, "Updated");
  assert.equal(listSpy.calls, 2);
});
