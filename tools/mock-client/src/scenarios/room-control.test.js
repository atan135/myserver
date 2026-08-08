import assert from "node:assert/strict";
import http from "node:http";
import { afterEach, test } from "node:test";

import { parseArgs } from "../args.js";
import { runGameServerControlOperation } from "./room.js";

const servers = [];

afterEach(async () => {
  await Promise.all(servers.splice(0).map((server) => new Promise((resolve) => server.close(resolve))));
});

async function startControlServer(requests) {
  const server = http.createServer(async (req, res) => {
    const chunks = [];
    for await (const chunk of req) chunks.push(chunk);
    const body = JSON.parse(Buffer.concat(chunks).toString("utf8"));
    requests.push({
      authorization: req.headers.authorization,
      url: req.url,
      body
    });
    res.setHeader("content-type", "application/json");
    if (requests.length % 2 === 1) {
      res.end(JSON.stringify({
        ok: true,
        preflight: { nonce: "preflight-nonce", summarySha256: "preflight-summary" }
      }));
      return;
    }
    res.end(JSON.stringify({ ok: true }));
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  servers.push(server);
  return `http://127.0.0.1:${server.address().port}`;
}

test("mock-client control uses explicit recovery reference identically in preflight and execute", async () => {
  const requests = [];
  const adminBaseUrl = await startControlServer(requests);

  await runGameServerControlOperation({
    adminBaseUrl,
    adminToken: "admin-token-must-not-enter-recovery-reference",
    gameServerInstanceId: "game-server-a",
    backupReference: "release-phase7-backup-001"
  }, "drain", { enabled: true, reason: "rolling replacement" });

  assert.equal(requests.length, 2);
  assert.equal(requests[0].authorization, "Bearer admin-token-must-not-enter-recovery-reference");
  assert.equal(requests[0].url, "/api/v1/rollouts/game-server/game-server-a/drain");
  assert.equal(requests[0].body.backupReference, "release-phase7-backup-001");
  assert.equal(requests[1].body.backupReference, requests[0].body.backupReference);
  assert.equal(requests[1].body.requestId, requests[0].body.requestId);
  assert.equal(requests[1].body.preflightNonce, "preflight-nonce");
  assert.equal(requests[1].body.preflightSummarySha256, "preflight-summary");
});

test("mock-client generated recovery reference is request-bound and secret-free", async () => {
  const requests = [];
  const adminBaseUrl = await startControlServer(requests);

  await runGameServerControlOperation({
    adminBaseUrl,
    adminToken: "secret-admin-token",
    gameServerInstanceId: "game-server-a"
  }, "shutdown", { reason: "rolling replacement" });

  const { requestId, backupReference } = requests[0].body;
  assert.equal(backupReference, `${requestId}-recovery`);
  assert.match(backupReference, /^[A-Za-z0-9][A-Za-z0-9._:@-]{0,127}$/);
  assert.equal(backupReference.includes("secret-admin-token"), false);
  assert.equal(requests[1].body.backupReference, backupReference);
});

test("mock-client parses explicit control recovery reference", () => {
  assert.equal(
    parseArgs(["--backup-reference", "release-phase7-backup-002"]).backupReference,
    "release-phase7-backup-002"
  );
});
