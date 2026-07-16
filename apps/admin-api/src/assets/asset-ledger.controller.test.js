import assert from "node:assert/strict";
import { register } from "node:module";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../../tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const { AssetLedgerController } = await import("./asset-ledger.controller.ts");
const { AdminStore } = await import("../admin-store.js");

function request() {
  return {
    admin: { sub: 7, username: "ledger-admin", role: "admin" },
    socket: { remoteAddress: "127.0.0.1" },
    headers: {}
  };
}

function storeFixture() {
  return {
    queries: [],
    audits: [],
    async getAssetLedger(query) {
      this.queries.push(query);
      return [{
        id: 1,
        characterId: "chr_1",
        requestId: "reward_1",
        quantityDelta: 3,
        mailId: "mail_1"
      }];
    },
    async countAssetLedger(query) {
      this.queries.push(query);
      return 1;
    },
    async appendAuditLog(entry) {
      this.audits.push(entry);
    }
  };
}

test("asset ledger query accepts supported correlation filters and writes a redacted query audit", async () => {
  const store = storeFixture();
  const controller = new AssetLedgerController({}, store);

  const response = await controller.ledger({
    character_id: "chr_1",
    request_id: "reward_1",
    origin_type: "achievement",
    delivery_id: "delivery_1",
    limit: "20",
    offset: "0"
  }, request());

  assert.equal(response.ok, true);
  assert.equal(response.total, 1);
  assert.equal(store.queries[0].characterId, "chr_1");
  assert.equal(store.queries[0].requestId, "reward_1");
  assert.equal(store.queries[0].originType, "achievement");
  assert.equal(store.queries[0].deliveryId, "delivery_1");
  assert.equal(store.audits.length, 1);
  assert.equal(store.audits[0].action, "asset_ledger_query");
  assert.equal(store.audits[0].details.result, "success");
  assert.equal(store.audits[0].details.resultCount, 1);
  assert.equal(Object.hasOwn(store.audits[0].details, "entries"), false);
});

test("asset ledger query rejects an unbounded read and audits the rejected attempt", async () => {
  const store = storeFixture();
  const controller = new AssetLedgerController({}, store);

  await assert.rejects(
    () => controller.ledger({}, request()),
    (error) => error.getResponse().error === "ASSET_LEDGER_FILTER_REQUIRED"
  );

  assert.equal(store.queries.length, 0);
  assert.equal(store.audits.length, 1);
  assert.equal(store.audits[0].details.result, "failed");
});

test("asset ledger store query keeps limit and offset as distinct parameterized values", async () => {
  const calls = [];
  const gamePool = {
    async query(sql, params) {
      calls.push({ sql, params });
      return { rows: [] };
    }
  };
  const store = new AdminStore(gamePool, null, { characterIdGenerator: {} }, gamePool);

  const entries = await store.getAssetLedger({
    characterId: "chr_1",
    deliveryId: "delivery_1",
    limit: 20,
    offset: 40
  });

  assert.deepEqual(entries, []);
  assert.match(calls[0].sql, /character_id = \$1/);
  assert.match(calls[0].sql, /delivery_id = \$2/);
  assert.match(calls[0].sql, /LIMIT \$3 OFFSET \$4/);
  assert.deepEqual(calls[0].params, ["chr_1", "delivery_1", 20, 40]);
});
