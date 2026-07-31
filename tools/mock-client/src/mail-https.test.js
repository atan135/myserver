import assert from "node:assert/strict";
import test from "node:test";

import { applyDiscoveredServices, httpBaseUrlFromDescriptor } from "./auth.js";
import { parseArgs } from "./args.js";
import {
  resolvePlayerMailBaseUrl,
  runMailClaim,
  runMailGet,
  runMailList,
  runMailRead,
  runMailSend
} from "./scenarios/mail.js";

const GAME_TICKET = `${Buffer.from(JSON.stringify({
  playerId: "plr_current",
  characterId: "chr_current",
  exp: 1_999_999_999
})).toString("base64url")}.signature`;

function playerOptions(overrides = {}) {
  return {
    roomId: "mail-test",
    ticket: GAME_TICKET,
    timeoutMs: 500,
    mailBaseUrl: "http://127.0.0.1:9003",
    mailBaseUrlOverride: "http://127.0.0.1:9003",
    discoveredMailBaseUrl: "",
    mailId: "mail_current",
    mailPlayerId: "",
    mailStatus: "unread",
    limit: 10,
    mailOffset: 0,
    serviceToken: "",
    ...overrides
  };
}

function response(status, payload) {
  return {
    ok: status >= 200 && status < 300,
    status,
    async text() {
      return payload === undefined ? "" : JSON.stringify(payload);
    }
  };
}

async function withFetchStub(responses, action) {
  const originalFetch = globalThis.fetch;
  const calls = [];
  let responseIndex = 0;
  globalThis.fetch = async (url, init = {}) => {
    calls.push({ url: String(url), init });
    const nextResponse = responses[responseIndex++];
    if (!nextResponse) {
      throw new Error("unexpected fetch call");
    }
    return nextResponse;
  };

  try {
    await action(calls);
    assert.equal(responseIndex, responses.length, "all stubbed responses should be consumed");
  } finally {
    globalThis.fetch = originalFetch;
  }
}

async function withoutLogs(action) {
  const originalLog = console.log;
  console.log = () => {};
  try {
    return await action();
  } finally {
    console.log = originalLog;
  }
}

function assertPlayerHeaders(call) {
  assert.equal(call.init.headers["X-Game-Ticket"], GAME_TICKET);
  assert.equal(call.init.headers["x-service-token"], undefined);
  assert.equal(call.init.body, undefined);
  assert.equal(call.init.headers["content-type"], undefined);
}

test("services.mail creates the public HTTPS mail base URL and takes precedence", () => {
  const service = { host: "api.game.zergzerg.cn", port: 443, protocol: "https" };
  assert.equal(httpBaseUrlFromDescriptor(service), "https://api.game.zergzerg.cn");
  assert.equal(
    resolvePlayerMailBaseUrl(
      { mailBaseUrl: "http://127.0.0.1:9003", discoveredMailBaseUrl: "" },
      { services: { mail: service } }
    ),
    "https://api.game.zergzerg.cn"
  );

  const options = parseArgs(["--mail-base-url", "http://127.0.0.1:9003"]);
  applyDiscoveredServices(options, { services: { mail: service } });
  assert.equal(options.mailBaseUrl, "http://127.0.0.1:9003");
  assert.equal(options.mailBaseUrlOverride, "http://127.0.0.1:9003");
  assert.equal(options.discoveredMailBaseUrl, "https://api.game.zergzerg.cn");
  assert.equal(options.serviceToken, "", "player mail scenarios do not read MAIL_SERVICE_TOKEN from the environment");

  const defaultOptions = parseArgs([]);
  applyDiscoveredServices(defaultOptions, { services: { mail: service } });
  assert.equal(defaultOptions.mailBaseUrl, "https://api.game.zergzerg.cn");
  assert.equal(defaultOptions.discoveredMailBaseUrl, "https://api.game.zergzerg.cn");
});

test("mail list uses only X-Game-Ticket and allowed list query fields", async () => {
  await withoutLogs(() => withFetchStub([
    response(200, { ok: true, mails: [], unread_count: 0 })
  ], async (calls) => {
    await runMailList(playerOptions());
    assert.equal(calls.length, 1);
    assert.equal(calls[0].url, "http://127.0.0.1:9003/api/v1/mails?status=unread&limit=10&offset=0");
    assertPlayerHeaders(calls[0]);
    assert.equal(calls[0].init.method, undefined);
  }));
});

test("mail detail and read send the ticket without caller-owned identity fields", async () => {
  await withoutLogs(() => withFetchStub([
    response(200, { ok: true, mail: { id: "mail_current" } }),
    response(200, { ok: true, updated: true })
  ], async (calls) => {
    await runMailGet(playerOptions());
    await runMailRead(playerOptions());
    assert.equal(calls[0].url, "http://127.0.0.1:9003/api/v1/mails/mail_current");
    assertPlayerHeaders(calls[0]);
    assert.equal(calls[1].url, "http://127.0.0.1:9003/api/v1/mails/mail_current/read");
    assert.equal(calls[1].init.method, "PUT");
    assertPlayerHeaders(calls[1]);
  }));
});

test("mail claim supports a repeat response and reconciles an unknown result without retrying", async () => {
  await withoutLogs(() => withFetchStub([
    response(200, { ok: true, claimed: true, already_claimed: false, status: "claimed" }),
    response(200, { ok: true, claimed: false, already_claimed: true, status: "claimed" }),
    response(202, { ok: true, status: "claiming" }),
    response(200, { ok: true, mail: { id: "mail_current", status: "claimed" } })
  ], async (calls) => {
    await runMailClaim(playerOptions());
    await runMailClaim(playerOptions());
    await runMailClaim(playerOptions());

    assert.equal(calls[0].init.method, "POST");
    assert.equal(calls[1].init.method, "POST");
    assert.equal(calls[2].init.method, "POST");
    assert.equal(calls[3].init.method, undefined);
    assert.equal(calls[3].url, "http://127.0.0.1:9003/api/v1/mails/mail_current");
    for (const call of calls) assertPlayerHeaders(call);
  }));
});

test("expired ticket, ownership denial, and rate limits preserve public response status", async () => {
  await withoutLogs(() => withFetchStub([
    response(401, { ok: false, message: "Player ticket is invalid" })
  ], async () => {
    await assert.rejects(runMailList(playerOptions()), /mail\.list failed \(401\): Player ticket is invalid/);
  }));

  await withoutLogs(() => withFetchStub([
    response(404, { ok: false, message: "Mail not found" })
  ], async () => {
    await assert.rejects(runMailGet(playerOptions()), /mail\.get failed \(404\): Mail not found/);
  }));

  await withoutLogs(() => withFetchStub([
    response(429, { ok: false, message: "Too many mail requests" })
  ], async () => {
    await assert.rejects(runMailList(playerOptions()), /mail\.list failed \(429\): Too many mail requests/);
  }));
});

test("player scenarios reject legacy player identity and service credentials before any request", async () => {
  await withoutLogs(async () => {
    await assert.rejects(
      runMailList(playerOptions({ mailPlayerId: "plr_other" })),
      /--mail-player-id is not accepted/
    );
    await assert.rejects(
      runMailList(playerOptions({ serviceToken: "internal-service-token" })),
      /--service-token is only valid/
    );
  });
});

test("system mail send requires explicit internal URL and service token", async () => {
  await withoutLogs(async () => {
    await assert.rejects(
      runMailSend(playerOptions({ mailBaseUrlOverride: "", serviceToken: "internal-service-token" })),
      /explicit internal --mail-base-url/
    );
    await assert.rejects(
      runMailSend(playerOptions({ serviceToken: "" })),
      /explicit --service-token/
    );
    await assert.rejects(
      runMailSend(playerOptions({ mailBaseUrlOverride: "https://api.game.zergzerg.cn", serviceToken: "internal-service-token" })),
      /only allow an HTTP internal --mail-base-url/
    );
  });
});
