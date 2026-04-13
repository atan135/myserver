import { MESSAGE_TYPE } from "../constants.js";
import {
  encodeChatPrivateReq,
  encodeChatGroupReq,
  encodeGroupCreateReq,
  encodeGroupJoinReq,
  encodeGroupLeaveReq,
  encodeGroupDismissReq,
  encodeGroupListReq,
  encodeChatHistoryReq
} from "../messages.js";
import { fetchTicket } from "../auth.js";
import { TcpProtocolClient } from "../client.js";
import { authenticateClient, printResponse } from "./room.js";

/**
 * Connect to chat server
 */
export async function connectToChatServer(options) {
  const chatOptions = { ...options, port: options.chatPort };
  const client = new TcpProtocolClient(chatOptions, "chat");
  await client.connect();
  return client;
}

/**
 * Private chat single client
 */
export async function runChatPrivate(options) {
  const login = await fetchTicket(options);
  console.log("login:", JSON.stringify({ playerId: login.playerId }, null, 2));

  const client = await connectToChatServer(options);
  const { encodeChatAuthReq } = await import("../messages.js");
  await authenticateClient(client, options, login, 1, encodeChatAuthReq);

  const targetId = options.targetId || "target-player-id";
  await client.send(MESSAGE_TYPE.CHAT_PRIVATE_REQ, 2, encodeChatPrivateReq(targetId, options.content));
  const res = printResponse("chat.privateRes", await client.readNextPacket(options.timeoutMs));
  if (!res.ok) {
    throw new Error(`private chat failed: ${res.errorCode}`);
  }
  console.log("private chat sent successfully, msgId:", res.msgId);

  client.close();
}

/**
 * Group chat single client
 */
export async function runChatGroup(options) {
  const login = await fetchTicket(options);
  console.log("login:", JSON.stringify({ playerId: login.playerId }, null, 2));

  const client = await connectToChatServer(options);
  await authenticateClient(client, options, login, 1);

  const groupId = options.groupId || "grp_test";
  await client.send(MESSAGE_TYPE.CHAT_GROUP_REQ, 2, encodeChatGroupReq(groupId, options.content));
  const res = printResponse("chat.groupRes", await client.readNextPacket(options.timeoutMs));
  if (!res.ok) {
    throw new Error(`group chat failed: ${res.errorCode}`);
  }
  console.log("group chat sent successfully, msgId:", res.msgId);

  client.close();
}

/**
 * Create a group
 */
export async function runGroupCreate(options) {
  const login = await fetchTicket(options);
  console.log("login:", JSON.stringify({ playerId: login.playerId }, null, 2));

  const client = await connectToChatServer(options);
  const { encodeChatAuthReq } = await import("../messages.js");
  await authenticateClient(client, options, login, 1, encodeChatAuthReq);

  const groupName = options.groupName || "Test Group";
  await client.send(MESSAGE_TYPE.GROUP_CREATE_REQ, 2, encodeGroupCreateReq(groupName));
  const res = printResponse("chat.groupCreateRes", await client.readNextPacket(options.timeoutMs));
  if (!res.ok) {
    throw new Error(`group create failed: ${res.errorCode}`);
  }
  console.log("group created, groupId:", res.groupId);

  client.close();
  return res.groupId;
}

/**
 * Join a group
 */
export async function runGroupJoin(options, groupId) {
  const login = await fetchTicket(options, { guestId: "joiner-" + options.roomId });
  console.log("login:", JSON.stringify({ playerId: login.playerId }, null, 2));

  const client = await connectToChatServer(options);
  const { encodeChatAuthReq } = await import("../messages.js");
  await authenticateClient(client, options, login, 1, encodeChatAuthReq);

  await client.send(MESSAGE_TYPE.GROUP_JOIN_REQ, 2, encodeGroupJoinReq(groupId));
  const res = printResponse("chat.groupJoinRes", await client.readNextPacket(options.timeoutMs));
  if (!res.ok) {
    throw new Error(`group join failed: ${res.errorCode}`);
  }
  console.log("joined group successfully");

  client.close();
}

/**
 * Leave a group
 */
export async function runGroupLeave(options, groupId) {
  const login = await fetchTicket(options);
  console.log("login:", JSON.stringify({ playerId: login.playerId }, null, 2));

  const client = await connectToChatServer(options);
  const { encodeChatAuthReq } = await import("../messages.js");
  await authenticateClient(client, options, login, 1, encodeChatAuthReq);

  await client.send(MESSAGE_TYPE.GROUP_LEAVE_REQ, 2, encodeGroupLeaveReq(groupId));
  const res = printResponse("chat.groupLeaveRes", await client.readNextPacket(options.timeoutMs));
  if (!res.ok) {
    throw new Error(`group leave failed: ${res.errorCode}`);
  }
  console.log("left group successfully");

  client.close();
}

/**
 * Dismiss a group
 */
export async function runGroupDismiss(options, groupId) {
  const login = await fetchTicket(options);
  console.log("login:", JSON.stringify({ playerId: login.playerId }, null, 2));

  const client = await connectToChatServer(options);
  const { encodeChatAuthReq } = await import("../messages.js");
  await authenticateClient(client, options, login, 1, encodeChatAuthReq);

  await client.send(MESSAGE_TYPE.GROUP_DISMISS_REQ, 2, encodeGroupDismissReq(groupId));
  const res = printResponse("chat.groupDismissRes", await client.readNextPacket(options.timeoutMs));
  if (!res.ok) {
    throw new Error(`group dismiss failed: ${res.errorCode}`);
  }
  console.log("group dismissed successfully");

  client.close();
}

/**
 * List groups
 */
export async function runGroupList(options) {
  const login = await fetchTicket(options);
  console.log("login:", JSON.stringify({ playerId: login.playerId }, null, 2));

  const client = await connectToChatServer(options);
  const { encodeChatAuthReq } = await import("../messages.js");
  await authenticateClient(client, options, login, 1, encodeChatAuthReq);

  await client.send(MESSAGE_TYPE.GROUP_LIST_REQ, 2, encodeGroupListReq());
  const res = printResponse("chat.groupListRes", await client.readNextPacket(options.timeoutMs));
  console.log("group list:", JSON.stringify(res.groups, null, 2));

  client.close();
}

/**
 * Query chat history
 */
export async function runChatHistory(options) {
  const login = await fetchTicket(options);
  console.log("login:", JSON.stringify({ playerId: login.playerId }, null, 2));

  const client = await connectToChatServer(options);
  const { encodeChatAuthReq } = await import("../messages.js");
  await authenticateClient(client, options, login, 1, encodeChatAuthReq);

  const chatType = 1; // private
  const targetId = options.targetId || "";
  const beforeTime = options.beforeTime || 0;
  const limit = options.limit || 20;

  await client.send(MESSAGE_TYPE.CHAT_HISTORY_REQ, 2, encodeChatHistoryReq(chatType, targetId, beforeTime, limit));
  const res = printResponse("chat.historyRes", await client.readNextPacket(options.timeoutMs));
  console.log("chat history:", JSON.stringify(res.messages, null, 2));

  client.close();
}

/**
 * Two client group chat: create group, join, send message, receive push
 */
export async function runChatTwoClient(options) {
  const loginA = await fetchTicket(options, { guestId: `${options.roomId}-owner` });
  const loginB = await fetchTicket(options, { guestId: `${options.roomId}-member` });

  console.log("clientA.login:", JSON.stringify({ playerId: loginA.playerId }, null, 2));
  console.log("clientB.login:", JSON.stringify({ playerId: loginB.playerId }, null, 2));

  // Create a group first
  const clientA = await connectToChatServer(options);
  const { encodeChatAuthReq } = await import("../messages.js");
  await authenticateClient(clientA, options, loginA, 1, encodeChatAuthReq);

  const groupName = options.groupName || "Test Group";
  await clientA.send(MESSAGE_TYPE.GROUP_CREATE_REQ, 2, encodeGroupCreateReq(groupName));
  const createRes = printResponse("clientA.groupCreate", await clientA.readNextPacket(options.timeoutMs));
  if (!createRes.ok) {
    throw new Error(`group create failed: ${createRes.errorCode}`);
  }
  const groupId = createRes.groupId;
  console.log("group created:", groupId);

  // Client B joins
  const clientB = await connectToChatServer(options);
  await authenticateClient(clientB, options, loginB, 1, encodeChatAuthReq);

  await clientB.send(MESSAGE_TYPE.GROUP_JOIN_REQ, 3, encodeGroupJoinReq(groupId));
  const joinRes = printResponse("clientB.groupJoin", await clientB.readNextPacket(options.timeoutMs));
  if (!joinRes.ok) {
    throw new Error(`group join failed: ${joinRes.errorCode}`);
  }
  console.log("clientB joined group");

  // Client A sends a group message
  await clientA.send(MESSAGE_TYPE.CHAT_GROUP_REQ, 3, encodeChatGroupReq(groupId, options.content));
  const chatRes = printResponse("clientA.groupChat", await clientA.readNextPacket(options.timeoutMs));
  if (!chatRes.ok) {
    throw new Error(`group chat failed: ${chatRes.errorCode}`);
  }
  console.log("clientA sent group message");

  // Client B receives push
  const push = await clientB.readUntil(
    options.timeoutMs,
    (packet) => packet.messageType === MESSAGE_TYPE.CHAT_PUSH,
    "chatPush"
  );
  console.log("clientB received push:", JSON.stringify(push, null, 2));

  clientA.close();
  clientB.close();
}

/**
 * Two client private chat: mutual messages
 */
export async function runChatPrivateTwoClient(options) {
  const loginA = await fetchTicket(options, { guestId: `${options.roomId}-owner` });
  const loginB = await fetchTicket(options, { guestId: `${options.roomId}-member` });

  console.log("clientA.login:", JSON.stringify({ playerId: loginA.playerId }, null, 2));
  console.log("clientB.login:", JSON.stringify({ playerId: loginB.playerId }, null, 2));

  // Client A connects and waits for messages
  const clientA = await connectToChatServer(options);
  const { encodeChatAuthReq } = await import("../messages.js");
  await authenticateClient(clientA, options, loginA, 1, encodeChatAuthReq);
  console.log("clientA connected, waiting for private message...");

  // Client B connects and sends private message to A
  const clientB = await connectToChatServer(options);
  await authenticateClient(clientB, options, loginB, 1, encodeChatAuthReq);

  await clientB.send(MESSAGE_TYPE.CHAT_PRIVATE_REQ, 2, encodeChatPrivateReq(loginA.playerId, options.content));
  const chatRes = printResponse("clientB.privateChat", await clientB.readNextPacket(options.timeoutMs));
  if (!chatRes.ok) {
    throw new Error(`private chat failed: ${chatRes.errorCode}`);
  }
  console.log("clientB sent private message to clientA");

  // Client A receives push
  const push = await clientA.readUntil(
    options.timeoutMs,
    (packet) => packet.messageType === MESSAGE_TYPE.CHAT_PUSH,
    "chatPush"
  );
  console.log("clientA received push:", JSON.stringify(push, null, 2));

  // Now A replies to B
  await clientA.send(MESSAGE_TYPE.CHAT_PRIVATE_REQ, 2, encodeChatPrivateReq(loginB.playerId, "Reply: " + options.content));
  const replyRes = printResponse("clientA.privateChat", await clientA.readNextPacket(options.timeoutMs));
  if (!replyRes.ok) {
    throw new Error(`reply chat failed: ${replyRes.errorCode}`);
  }
  console.log("clientA replied to clientB");

  // Client B receives A's reply
  const replyPush = await clientB.readUntil(
    options.timeoutMs,
    (packet) => packet.messageType === MESSAGE_TYPE.CHAT_PUSH,
    "chatPush"
  );
  console.log("clientB received reply push:", JSON.stringify(replyPush, null, 2));

  clientA.close();
  clientB.close();
}
