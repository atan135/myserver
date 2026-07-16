import assert from "node:assert/strict";
import test from "node:test";

import {
  analyzeErrorCodes,
  analyzeMockClient,
  analyzePacketRouting,
  analyzeRpcs
} from "../../tools/check-protocol-routing-consistency.js";

const routingConfig = {
  mockClient: { nonGameMessageIdFloor: 20000 },
  packetRoutes: {
    dispatches: [{ function: "dispatch_packet", name: "player" }],
    preDispatchControls: []
  },
  rpcConsumers: {
    Example: {
      implementation: { path: "implementation.rs", trait: "Example" },
      client: { path: "client.rs", receiver: "inner" }
    }
  }
};

const canonicalSource = `
  pub enum MessageType { AuthReq = 1001, AuthRes = 1002, ServerPush = 1201, }
  impl MessageType {
    pub fn from_u16(value: u16) -> Option<Self> {
      match value { 1001 => Some(Self::AuthReq), 1002 => Some(Self::AuthRes), 1201 => Some(Self::ServerPush), _ => None, }
    }
  }
`;

const playerDispatch = `
  async fn dispatch_packet() {
    match packet.message_type() {
      Some(MessageType::AuthReq) => handle_auth(),
      None => handle_unknown(),
    }
  }
`;

test("packet route analysis catches duplicate ids, missing dispatches, unknown branches, and orphan pushes", () => {
  const duplicateId = canonicalSource.replace("AuthRes = 1002", "AuthRes = 1001");
  const brokenDispatch = playerDispatch.replace("AuthReq", "UnknownReq");
  const result = analyzePacketRouting({
    canonicalSource: duplicateId,
    config: routingConfig,
    dispatchSources: new Map([["player", brokenDispatch]]),
    producerSources: [`connection.queue_message(MessageType::AuthRes, 1, response);`]
  });
  const rules = new Set(result.diagnostics.map((diagnostic) => diagnostic.rule));
  assert.ok(rules.has("MESSAGE_TYPE_ID_DUPLICATE"));
  assert.ok(rules.has("DISPATCH_MESSAGE_UNKNOWN"));
  assert.ok(rules.has("PACKET_REQUEST_WITHOUT_CONSUMER"));
  assert.ok(rules.has("PACKET_OUTBOUND_ORPHAN"));
});

test("mock-client analysis ties sent constants to encoders and outgoing decoders without requiring internal codecs", () => {
  const result = analyzeMockClient({
    canonicalEntries: [
      { id: 1001, name: "AuthReq" },
      { id: 1002, name: "AuthRes" },
      { id: 1601, name: "InternalReq" }
    ],
    config: routingConfig,
    constantsSource: `export const MESSAGE_TYPE = { AUTH_REQ: 1001, AUTH_RES: 1002, INTERNAL_REQ: 1601, CHAT_PUSH: 20105, };`,
    messagesSource: `
      export function encodeAuthReq() {}
      export function decodeByMessageType(messageType) { switch (messageType) { case MESSAGE_TYPE.AUTH_RES: return {}; } }
    `,
    sourceFiles: ["scenario.js"],
    routesByMessage: new Map([["AuthReq", new Set(["player"])]]),
    readSource: () => `
      await client.send(MESSAGE_TYPE.AUTH_REQ, 1, encodeAuthReq());
      if (packet.messageType === MESSAGE_TYPE.AUTH_RES) {}
    `
  });
  assert.equal(result.diagnostics.filter((diagnostic) => diagnostic.rule === "MOCK_CLIENT_CONSTANT_UNKNOWN").length, 0);
  assert.equal(result.decoders.has("AUTH_RES"), true);
  assert.deepEqual(result.diagnostics, []);
});

test("mock-client analysis catches a deliberately drifted encoder route", () => {
  const result = analyzeMockClient({
    canonicalEntries: [{ id: 1001, name: "AuthReq" }],
    config: routingConfig,
    constantsSource: `export const MESSAGE_TYPE = { AUTH_REQ: 1001, };`,
    messagesSource: `export function encodeWrongReq() {} export function decodeByMessageType(messageType) { switch (messageType) { default: return {}; } }`,
    sourceFiles: ["scenario.js"],
    routesByMessage: new Map([["AuthReq", new Set(["player"])]]),
    readSource: () => `await client.send(MESSAGE_TYPE.AUTH_REQ, 1, encodeWrongReq());`
  });
  assert.ok(result.diagnostics.some((diagnostic) => diagnostic.rule === "MOCK_CLIENT_ENCODER_ROUTE_DRIFT"));
});

test("mock-client analysis rejects a request that only has an internal route", () => {
  const result = analyzeMockClient({
    canonicalEntries: [{ id: 1601, name: "InternalReq" }],
    config: routingConfig,
    constantsSource: `export const MESSAGE_TYPE = { INTERNAL_REQ: 1601, };`,
    messagesSource: `export function encodeInternalReq() {} export function decodeByMessageType(messageType) { switch (messageType) { default: return {}; } }`,
    sourceFiles: ["scenario.js"],
    routesByMessage: new Map([["InternalReq", new Set(["internal"])]]),
    readSource: () => `await client.send(MESSAGE_TYPE.INTERNAL_REQ, 1, encodeInternalReq());`
  });
  assert.ok(result.diagnostics.some((diagnostic) => diagnostic.rule === "MOCK_CLIENT_SEND_WITHOUT_PLAYER_ROUTE"));
});

test("RPC analysis reports a configured RPC that has no implementation or client consumer", () => {
  const result = analyzeRpcs({
    config: routingConfig,
    matchProtoSource: `syntax = "proto3"; service Example { rpc DoWork(WorkReq) returns (WorkRes); }`,
    readSource: (file) => file === "implementation.rs"
      ? `impl Example for Service { async fn another_method(&self) {} }`
      : `impl Client { async fn nothing(&self) {} }`
  });
  assert.ok(result.diagnostics.some((diagnostic) => diagnostic.rule === "RPC_WITHOUT_CONSUMER"));
});

test("RPC analysis recognizes a formatted generated-client call", () => {
  const result = analyzeRpcs({
    config: routingConfig,
    matchProtoSource: `syntax = "proto3"; service Example { rpc DoWork(WorkReq) returns (WorkRes); }`,
    readSource: (file) => file === "implementation.rs"
      ? `impl Example for Service { async fn do_work(&self) {} }`
      : `self\n  .inner\n  .do_work(request)\n  .await`
  });
  assert.deepEqual(result.diagnostics, []);
  assert.deepEqual(result.report, [{ client: true, implemented: true, method: "do_work", rpc: "DoWork", service: "Example" }]);
});

test("error-code analysis records shared fields and explicit dynamic-source metadata", () => {
  const result = analyzeErrorCodes({
    config: {
      errorCodes: {
        definitionMode: "implementation_literals",
        staticCodes: ["KNOWN_CODE", "UNUSED_CODE"],
        dynamicSources: [{ path: "service.rs", reason: "Result values are forwarded" }]
      }
    },
    implementationSources: {
      "service.rs": `fn fail() { queue_error(1, "KNOWN_CODE", "message"); }`
    },
    protoSources: {
      "packages/proto/example.proto": `message Reply { string error_code = 1; }`
    }
  });
  assert.equal(result.fields.length, 1);
  assert.equal(result.literalCodes.has("KNOWN_CODE"), true);
  assert.equal(result.dynamicSources.length, 1);
  assert.ok(result.diagnostics.some((diagnostic) => diagnostic.rule === "ERROR_CODE_UNUSED"));
});

test("error-code analysis rejects an implementation literal absent from the static catalog", () => {
  const result = analyzeErrorCodes({
    config: { errorCodes: { definitionMode: "implementation_literals", staticCodes: [] } },
    implementationSources: { "service.rs": `fn fail() { queue_error(1, "UNKNOWN_CODE", "message"); }` },
    protoSources: { "packages/proto/example.proto": `message Reply { string error_code = 1; }` }
  });
  assert.ok(result.diagnostics.some((diagnostic) => diagnostic.rule === "ERROR_CODE_UNDEFINED"));
});
