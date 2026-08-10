import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { fileURLToPath } from "node:url";
import test from "node:test";

import { WebSocketServer } from "ws";

import { applyDiscoveredServices, chatWebSocketUrlFromDescriptor } from "../../tools/mock-client/src/auth.js";
import { parseArgs } from "../../tools/mock-client/src/args.js";
import { MESSAGE_TYPE } from "../../tools/mock-client/src/constants.js";
import { decodeByMessageType, encodeChatAuthReq, encodeChatGroupReq, encodeChatHistoryReq, encodeChatPrivateReq } from "../../tools/mock-client/src/messages.js";
import { decodePacketFrame, encodePacket } from "../../tools/mock-client/src/packet.js";
import { authenticateChatClient, connectToChatServer, resolveChatWebSocketUrl } from "../../tools/mock-client/src/scenarios/chat.js";
import { WebSocketProtocolClient } from "../../tools/mock-client/src/websocket-client.js";
import { encodeBoolField, encodeStringField } from "../../tools/mock-client/src/protocol.js";

const MOCK_CLIENT_ENTRY = fileURLToPath(new URL("../../tools/mock-client/src/index.js", import.meta.url));

async function runMockClient(args) {
  const child = spawn(process.execPath, [MOCK_CLIENT_ENTRY, ...args], {
    cwd: fileURLToPath(new URL("../../", import.meta.url)),
    stdio: ["ignore", "pipe", "pipe"]
  });
  let stdout = "";
  let stderr = "";
  child.stdout.setEncoding("utf8");
  child.stderr.setEncoding("utf8");
  child.stdout.on("data", (chunk) => { stdout += chunk; });
  child.stderr.on("data", (chunk) => { stderr += chunk; });
  const [code, signal] = await once(child, "exit");
  return { code, signal, stdout, stderr };
}

async function createWebSocketFixture(onPacket) {
  const server = new WebSocketServer({ port: 0 });
  await once(server, "listening");
  const { port } = server.address();
  const connections = [];
  server.on("connection", (socket) => {
    connections.push(socket);
    socket.on("message", (data, isBinary) => {
      if (!isBinary) {
        socket.close(1003, "binary required");
        return;
      }
      onPacket(socket, decodePacketFrame(data));
    });
  });

  return {
    connections,
    url: `ws://127.0.0.1:${port}/`,
    async close() {
      for (const socket of connections) {
        socket.terminate();
      }
      await new Promise((resolve) => server.close(resolve));
    }
  };
}

function chatAuthRes(ok = true, errorCode = "") {
  return Buffer.concat([
    encodeBoolField(1, ok),
    errorCode ? encodeStringField(2, errorCode) : Buffer.alloc(0)
  ]);
}

function chatSendRes(messageId) {
  return Buffer.concat([encodeBoolField(1, true), encodeStringField(3, messageId)]);
}

function chatPush(messageId) {
  return Buffer.concat([
    encodeStringField(1, messageId),
    encodeStringField(3, "plr_sender"),
    encodeStringField(5, "fixture push")
  ]);
}

function mailNotifyPush(mailId) {
  return Buffer.concat([encodeStringField(1, mailId), encodeStringField(2, "fixture mail")]);
}

test("mock-client keeps TCP chat as the default transport", () => {
  const options = parseArgs([]);
  assert.equal(options.chatTransport, "tcp");
  assert.equal(options.chatWsUrl, "");
  assert.equal(options.chatPort, 0);
});

test("WSS prefers auth services.chat and explicit WebSocket URL wins", () => {
  const descriptor = { host: "chat.bevy.zergzerg.cn", port: 443, protocol: "wss" };
  const options = parseArgs(["--chat-transport", "wss"]);
  const login = { services: { chat: descriptor } };

  applyDiscoveredServices(options, login);
  assert.equal(options.discoveredChatWsUrl, "wss://chat.bevy.zergzerg.cn/");
  assert.equal(resolveChatWebSocketUrl(options, login), "wss://chat.bevy.zergzerg.cn/");

  options.chatWsUrl = "wss://fixture.example.test/";
  assert.equal(resolveChatWebSocketUrl(options, login), "wss://fixture.example.test/");
  assert.equal(chatWebSocketUrlFromDescriptor({ host: "chat.internal", port: 9011, protocol: "ws" }), "ws://chat.internal:9011/");
});

test("WebSocket chat authenticates and carries private, group, history, and mail packets unchanged", async (t) => {
  const requests = [];
  const fixture = await createWebSocketFixture((socket, packet) => {
    requests.push(packet);
    switch (packet.messageType) {
      case MESSAGE_TYPE.CHAT_AUTH_REQ:
        socket.send(encodePacket(MESSAGE_TYPE.CHAT_AUTH_RES, packet.seq, chatAuthRes()));
        break;
      case MESSAGE_TYPE.CHAT_PRIVATE_REQ:
        socket.send(encodePacket(MESSAGE_TYPE.CHAT_PRIVATE_RES, packet.seq, chatSendRes("msg-private")));
        socket.send(encodePacket(MESSAGE_TYPE.CHAT_PUSH, 0, chatPush("push-private")));
        break;
      case MESSAGE_TYPE.CHAT_GROUP_REQ:
        socket.send(encodePacket(MESSAGE_TYPE.CHAT_GROUP_RES, packet.seq, chatSendRes("msg-group")));
        break;
      case MESSAGE_TYPE.CHAT_HISTORY_REQ:
        socket.send(encodePacket(MESSAGE_TYPE.CHAT_HISTORY_RES, packet.seq, Buffer.alloc(0)));
        socket.send(encodePacket(MESSAGE_TYPE.MAIL_NOTIFY_PUSH, 0, mailNotifyPush("mail-1")));
        break;
      default:
        socket.close(1002, "unexpected packet");
    }
  });
  t.after(() => fixture.close());

  const options = parseArgs(["--chat-transport", "ws", "--chat-ws-url", fixture.url, "--timeout-ms", "1000"]);
  const login = { playerId: "plr_client", ticket: "ticket", ticketExpiresAt: null, services: {} };
  const client = await connectToChatServer(options, login);
  t.after(() => client.close());

  await authenticateChatClient(client, options, login, 1);

  await client.send(MESSAGE_TYPE.CHAT_PRIVATE_REQ, 2, encodeChatPrivateReq("plr_target", "private"));
  const privateRes = await client.readNextPacket(1000);
  assert.equal(privateRes.messageType, MESSAGE_TYPE.CHAT_PRIVATE_RES);
  assert.equal(privateRes.seq, 2);
  assert.equal(decodeByMessageType(privateRes.messageType, privateRes.body).msgId, "msg-private");

  const privatePush = await client.readNextPacket(1000);
  assert.equal(privatePush.messageType, MESSAGE_TYPE.CHAT_PUSH);
  assert.equal(privatePush.seq, 0);

  await client.send(MESSAGE_TYPE.CHAT_GROUP_REQ, 3, encodeChatGroupReq("grp_1", "group"));
  const groupRes = await client.readNextPacket(1000);
  assert.equal(groupRes.messageType, MESSAGE_TYPE.CHAT_GROUP_RES);
  assert.equal(groupRes.seq, 3);
  assert.equal(decodeByMessageType(groupRes.messageType, groupRes.body).msgId, "msg-group");

  await client.send(MESSAGE_TYPE.CHAT_HISTORY_REQ, 4, encodeChatHistoryReq(1, "plr_target", 0, 20));
  const historyRes = await client.readNextPacket(1000);
  assert.equal(historyRes.messageType, MESSAGE_TYPE.CHAT_HISTORY_RES);
  assert.equal(historyRes.seq, 4);
  const notify = await client.readNextPacket(1000);
  assert.equal(notify.messageType, MESSAGE_TYPE.MAIL_NOTIFY_PUSH);
  assert.equal(decodeByMessageType(notify.messageType, notify.body).mailId, "mail-1");

  assert.deepEqual(requests.map((packet) => packet.messageType), [
    MESSAGE_TYPE.CHAT_AUTH_REQ,
    MESSAGE_TYPE.CHAT_PRIVATE_REQ,
    MESSAGE_TYPE.CHAT_GROUP_REQ,
    MESSAGE_TYPE.CHAT_HISTORY_REQ
  ]);
  assert.deepEqual(requests[0].body, encodeChatAuthReq("ticket"));
});

test("chat CLI scenarios do not open an unrelated game TCP connection", async (t) => {
  const fixture = await createWebSocketFixture((socket, packet) => {
    if (packet.messageType === MESSAGE_TYPE.CHAT_AUTH_REQ) {
      socket.send(encodePacket(MESSAGE_TYPE.CHAT_AUTH_RES, packet.seq, chatAuthRes()));
    } else if (packet.messageType === MESSAGE_TYPE.CHAT_PRIVATE_REQ) {
      socket.send(encodePacket(MESSAGE_TYPE.CHAT_PRIVATE_RES, packet.seq, chatSendRes("msg-cli")));
    }
  });
  t.after(() => fixture.close());

  const result = await runMockClient([
    "--scenario", "chat-private",
    "--ticket", "payload.signature",
    "--chat-transport", "ws",
    "--chat-ws-url", fixture.url,
    "--target-id", "plr_target",
    "--host", "127.0.0.1",
    "--port", "1",
    "--timeout-ms", "1000"
  ]);

  assert.equal(result.signal, null);
  assert.equal(result.code, 0, result.stderr || result.stdout);
  assert.match(result.stdout, /scenario completed: chat-private/);
  assert.doesNotMatch(result.stderr, /ECONNREFUSED/);
});

test("WebSocket client rejects malformed logical binary messages and text messages", async (t) => {
  const fixture = await createWebSocketFixture((socket, packet) => {
    if (packet.seq === 1) {
      socket.send("unexpected text frame");
    }
  });
  t.after(() => fixture.close());

  const client = new WebSocketProtocolClient({ url: fixture.url, maxBodyLen: 64 }, "fixture");
  t.after(() => client.close());
  await client.connect();
  await client.send(MESSAGE_TYPE.CHAT_AUTH_REQ, 1, encodeChatAuthReq("ticket"));
  await assert.rejects(() => client.readNextPacket(1000), /text WebSocket message/);

  assert.throws(
    () => decodePacketFrame(Buffer.concat([encodePacket(MESSAGE_TYPE.CHAT_AUTH_RES, 1, Buffer.alloc(0)), Buffer.from([0])])),
    /frame length mismatch/
  );
});

test("WebSocket client can reconnect without changing packet encoding", async (t) => {
  const fixture = await createWebSocketFixture((socket, packet) => {
    socket.send(encodePacket(MESSAGE_TYPE.CHAT_AUTH_RES, packet.seq, chatAuthRes()));
  });
  t.after(() => fixture.close());

  const client = new WebSocketProtocolClient({ url: fixture.url, maxBodyLen: 64 }, "fixture");
  t.after(() => client.close());
  await client.connect();
  await client.send(MESSAGE_TYPE.CHAT_AUTH_REQ, 1, encodeChatAuthReq("ticket"));
  assert.equal((await client.readNextPacket(1000)).seq, 1);

  await client.reconnect();
  await client.send(MESSAGE_TYPE.CHAT_AUTH_REQ, 2, encodeChatAuthReq("ticket"));
  assert.equal((await client.readNextPacket(1000)).seq, 2);
  assert.equal(fixture.connections.length, 2);
});
