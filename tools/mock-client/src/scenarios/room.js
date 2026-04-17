import {
  MESSAGE_TYPE
} from "../constants.js";
import {
  encodePingReq,
  encodeRoomJoinReq,
  encodeRoomLeaveReq,
  encodeRoomReadyReq,
  encodeRoomStartReq,
  encodePlayerInputReq,
  encodeRoomEndReq,
  encodeRoomReconnectReq,
  encodeGetRoomDataReq,
  encodeCreateMatchedRoomReq
} from "../messages.js";
import { decodeByMessageType } from "../messages.js";
import { fetchTicket, formatLoginSummary } from "../auth.js";
import { TcpProtocolClient } from "../client.js";

/**
 * Print response and return decoded body
 * @param {string} label
 * @param {{messageType: number, seq: number, body: Buffer}} packet
 * @returns {Object}
 */
export function printResponse(label, packet) {
  const decoded = decodeByMessageType(packet.messageType, packet.body);
  console.log(`${label}:`, JSON.stringify({ messageType: packet.messageType, seq: packet.seq, decoded }, null, 2));
  return decoded;
}

export async function delayBeforeFinalLeave(client, timeoutMs, delayMs = 10000) {
  console.log(`${client.label}.delayBeforeFinalLeave: waiting ${delayMs}ms before final leave`);
  const startedAt = Date.now();
  let pingSeq = 900000;

  while (Date.now() - startedAt < delayMs) {
    const remainingMs = delayMs - (Date.now() - startedAt);
    const sleepMs = Math.min(3000, Math.max(0, remainingMs));
    if (sleepMs > 0) {
      await new Promise((resolve) => setTimeout(resolve, sleepMs));
    }
    if (Date.now() - startedAt >= delayMs) {
      break;
    }

    await client.send(MESSAGE_TYPE.PING_REQ, pingSeq, encodePingReq(Date.now()));
    await client.readUntil(
      timeoutMs,
      (packet) => packet.messageType === MESSAGE_TYPE.PING_RES && packet.seq === pingSeq,
      "delayPing"
    );
    pingSeq += 1;
  }
}

/**
 * Authenticate a client
 */
export async function authenticateClient(
  client,
  options,
  login,
  seq = 1,
  encodeAuthFn,
  authReqType = MESSAGE_TYPE.AUTH_REQ,
  authResType = MESSAGE_TYPE.AUTH_RES
) {
  const { encodeAuthReq } = await import("../messages.js");
  const fn = encodeAuthFn || encodeAuthReq;
  await client.send(authReqType, seq, fn(login.ticket));
  const packet = await client.readNextPacket(options.timeoutMs);
  const auth = printResponse(`${client.label}.auth`, packet);
  if (packet.messageType !== authResType) {
    throw new Error(`${client.label} auth expected messageType ${authResType}, got ${packet.messageType}`);
  }
  if (!auth.ok) {
    throw new Error(`${client.label} auth failed: ${auth.errorCode}`);
  }
}

/**
 * Wait for frame bundle matching expected action
 */
export async function waitForFrameBundle(client, timeoutMs, expectedAction = null) {
  return client.readUntil(
    timeoutMs,
    (packet, decoded) => {
      if (packet.messageType !== MESSAGE_TYPE.FRAME_BUNDLE_PUSH) {
        return false;
      }
      if (decoded.isSilentFrame) {
        return false;
      }
      if (!expectedAction) {
        return true;
      }
      return decoded.inputs.some((input) => input.action === expectedAction);
    },
    "frameBundle"
  );
}

async function waitForMessageType(client, timeoutMs, expectedMessageType, label) {
  return client.readUntil(
    timeoutMs,
    (packet) => packet.messageType === expectedMessageType,
    label
  );
}

async function waitForRoomStartRes(client, timeoutMs, label = "roomStart") {
  return waitForMessageType(client, timeoutMs, MESSAGE_TYPE.ROOM_START_RES, label);
}

async function waitForRoomStatePush(client, timeoutMs, expectedEvent, label = "roomStatePush") {
  return client.readUntil(
    timeoutMs,
    (packet, decoded) => packet.messageType === MESSAGE_TYPE.ROOM_STATE_PUSH && decoded.event === expectedEvent,
    label
  );
}

/**
 * Happy path: login -> join room -> ready -> leave
 */
export async function runHappyPath(client, options, login) {
  await authenticateClient(client, options, login, 1);

  await client.send(MESSAGE_TYPE.PING_REQ, 2, encodePingReq(Date.now()));
  printResponse(`${client.label}.ping`, await client.readNextPacket(options.timeoutMs));

  await client.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 3, encodeRoomJoinReq(options.roomId));
  const roomJoin = printResponse(`${client.label}.roomJoin`, await client.readNextPacket(options.timeoutMs));
  if (!roomJoin.ok) {
    throw new Error(`room join failed: ${roomJoin.errorCode}`);
  }

  printResponse(`${client.label}.roomStatePush(join)`, await client.readNextPacket(options.timeoutMs));

  await client.send(MESSAGE_TYPE.ROOM_READY_REQ, 4, encodeRoomReadyReq(true));
  const readyRes = printResponse(`${client.label}.roomReady`, await client.readNextPacket(options.timeoutMs));
  if (!readyRes.ok) {
    throw new Error(`room ready failed: ${readyRes.errorCode}`);
  }

  const readyPush = printResponse(`${client.label}.roomStatePush(ready)`, await client.readNextPacket(options.timeoutMs));
  if (readyPush.snapshot?.state !== "ready") {
    throw new Error("expected room state to become ready");
  }

  await delayBeforeFinalLeave(client, options.timeoutMs);
  await client.send(MESSAGE_TYPE.ROOM_LEAVE_REQ, 5, encodeRoomLeaveReq());
  const leaveRes = printResponse(`${client.label}.roomLeave`, await client.readNextPacket(options.timeoutMs));
  if (!leaveRes.ok) {
    throw new Error(`room leave failed: ${leaveRes.errorCode}`);
  }
}

/**
 * Get room data without joining
 */
export async function runGetRoomData(client, options, login) {
  await authenticateClient(client, options, login, 1);

  await client.send(
    MESSAGE_TYPE.GET_ROOM_DATA_REQ,
    2,
    encodeGetRoomDataReq(options.idStart, options.idEnd)
  );
  const response = printResponse(`${client.label}.getRoomData`, await client.readNextPacket(options.timeoutMs));
  if (!response.ok) {
    throw new Error(`get room data failed: ${response.errorCode}`);
  }
  if (response.field0List.length === 0) {
    throw new Error("expected field0List to contain at least one string");
  }

  console.log(`${client.label}.getRoomData.field0List:`, JSON.stringify(response.field0List, null, 2));
}

/**
 * Join room then get room data
 */
export async function runGetRoomDataInRoom(client, options, login) {
  await authenticateClient(client, options, login, 1);

  await client.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(options.roomId));
  const joinRes = printResponse(`${client.label}.roomJoin`, await client.readNextPacket(options.timeoutMs));
  if (!joinRes.ok) {
    throw new Error(`room join failed: ${joinRes.errorCode}`);
  }

  printResponse(`${client.label}.roomStatePush(join)`, await client.readNextPacket(options.timeoutMs));

  await client.send(
    MESSAGE_TYPE.GET_ROOM_DATA_REQ,
    3,
    encodeGetRoomDataReq(options.idStart, options.idEnd)
  );
  const response = printResponse(`${client.label}.getRoomData`, await client.readNextPacket(options.timeoutMs));
  if (!response.ok) {
    throw new Error(`get room data failed: ${response.errorCode}`);
  }
  if (response.field0List.length === 0) {
    throw new Error("expected field0List to contain at least one string");
  }

  console.log(`${client.label}.getRoomData.field0List:`, JSON.stringify(response.field0List, null, 2));

  await delayBeforeFinalLeave(client, options.timeoutMs);
  await client.send(MESSAGE_TYPE.ROOM_LEAVE_REQ, 4, encodeRoomLeaveReq());
  const leaveRes = printResponse(`${client.label}.roomLeave`, await client.readNextPacket(options.timeoutMs));
  if (!leaveRes.ok) {
    throw new Error(`room leave failed: ${leaveRes.errorCode}`);
  }
}

/**
 * Single client start game (should fail - needs 2 players)
 */
export async function runStartGameSingleClient(client, options, login) {
  await authenticateClient(client, options, login, 1);

  await client.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(options.roomId));
  const joinRes = printResponse(`${client.label}.roomJoin`, await client.readNextPacket(options.timeoutMs));
  if (!joinRes.ok) {
    throw new Error(`room join failed: ${joinRes.errorCode}`);
  }
  printResponse(`${client.label}.roomStatePush(join)`, await client.readNextPacket(options.timeoutMs));

  await client.send(MESSAGE_TYPE.ROOM_READY_REQ, 3, encodeRoomReadyReq(true));
  const readyRes = printResponse(`${client.label}.roomReady`, await client.readNextPacket(options.timeoutMs));
  if (!readyRes.ok) {
    throw new Error(`room ready failed: ${readyRes.errorCode}`);
  }
  printResponse(`${client.label}.roomStatePush(ready)`, await client.readNextPacket(options.timeoutMs));

  await client.send(MESSAGE_TYPE.ROOM_START_REQ, 4, encodeRoomStartReq());
  const startRes = printResponse(`${client.label}.roomStart`, await client.readNextPacket(options.timeoutMs));
  if (startRes.ok) {
    throw new Error("expected single-client start game to fail");
  }
  if (startRes.errorCode !== "ROOM_NOT_ENOUGH_PLAYERS") {
    throw new Error(`expected ROOM_NOT_ENOUGH_PLAYERS, got ${startRes.errorCode}`);
  }
}

/**
 * Two clients: A joins, B joins, B leaves, A leaves (owner transfer)
 */
export async function runTwoClientRoom(options) {
  const loginA = await fetchTicket(options, { guestId: `${options.roomId}-owner` });
  const loginB = await fetchTicket(options, { guestId: `${options.roomId}-member` });

  console.log("clientA.login:", JSON.stringify({ playerId: loginA.playerId }, null, 2));
  console.log("clientB.login:", JSON.stringify({ playerId: loginB.playerId }, null, 2));

  const clientA = new TcpProtocolClient(options, "clientA");
  const clientB = new TcpProtocolClient(options, "clientB");
  await clientA.connect();
  await clientB.connect();

  try {
    await authenticateClient(clientA, options, loginA, 1);
    await authenticateClient(clientB, options, loginB, 1);

    await clientA.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(options.roomId));
    const joinA = printResponse("clientA.roomJoin", await clientA.readNextPacket(options.timeoutMs));
    if (!joinA.ok) {
      throw new Error(`clientA room join failed: ${joinA.errorCode}`);
    }
    const pushA1 = printResponse("clientA.roomStatePush(join1)", await clientA.readNextPacket(options.timeoutMs));
    if (pushA1.snapshot?.ownerPlayerId !== loginA.playerId) {
      throw new Error("clientA should be initial owner");
    }

    await clientB.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(options.roomId));
    const joinB = printResponse("clientB.roomJoin", await clientB.readNextPacket(options.timeoutMs));
    if (!joinB.ok) {
      throw new Error(`clientB room join failed: ${joinB.errorCode}`);
    }

    const pushB1 = printResponse("clientB.roomStatePush(join)", await clientB.readNextPacket(options.timeoutMs));
    const pushA2 = printResponse("clientA.roomStatePush(join2)", await clientA.readNextPacket(options.timeoutMs));
    if (pushA2.snapshot?.members?.length !== 2 || pushB1.snapshot?.members?.length !== 2) {
      throw new Error("expected both clients to observe two room members");
    }
    if (pushA2.snapshot?.ownerPlayerId !== loginA.playerId) {
      throw new Error("owner should remain clientA before leave");
    }

    await clientA.send(MESSAGE_TYPE.ROOM_LEAVE_REQ, 3, encodeRoomLeaveReq());
    const leaveA = printResponse("clientA.roomLeave", await clientA.readNextPacket(options.timeoutMs));
    if (!leaveA.ok) {
      throw new Error(`clientA room leave failed: ${leaveA.errorCode}`);
    }

    const pushB2 = printResponse("clientB.roomStatePush(ownerTransfer)", await clientB.readNextPacket(options.timeoutMs));
    if (pushB2.snapshot?.ownerPlayerId !== loginB.playerId) {
      throw new Error("expected owner to transfer to clientB");
    }
    if (pushB2.snapshot?.members?.length !== 1) {
      throw new Error("expected only one member after owner leave");
    }

    await delayBeforeFinalLeave(clientB, options.timeoutMs);
    await clientB.send(MESSAGE_TYPE.ROOM_LEAVE_REQ, 3, encodeRoomLeaveReq());
    const leaveB = printResponse("clientB.roomLeave", await clientB.readNextPacket(options.timeoutMs));
    if (!leaveB.ok) {
      throw new Error(`clientB room leave failed: ${leaveB.errorCode}`);
    }
  } finally {
    clientA.close();
    clientB.close();
  }
}

/**
 * Start game with both clients ready
 */
export async function runStartGameReadyRoom(options) {
  const loginA = await fetchTicket(options, { guestId: `${options.roomId}-owner` });
  const loginB = await fetchTicket(options, { guestId: `${options.roomId}-member` });

  console.log("clientA.login:", JSON.stringify({ playerId: loginA.playerId }, null, 2));
  console.log("clientB.login:", JSON.stringify({ playerId: loginB.playerId }, null, 2));

  const clientA = new TcpProtocolClient(options, "clientA");
  const clientB = new TcpProtocolClient(options, "clientB");
  await clientA.connect();
  await clientB.connect();

  try {
    await authenticateClient(clientA, options, loginA, 1);
    await authenticateClient(clientB, options, loginB, 1);

    await clientA.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(options.roomId));
    printResponse("clientA.roomJoin", await clientA.readNextPacket(options.timeoutMs));
    printResponse("clientA.roomStatePush(join1)", await clientA.readNextPacket(options.timeoutMs));

    await clientB.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(options.roomId));
    printResponse("clientB.roomJoin", await clientB.readNextPacket(options.timeoutMs));
    printResponse("clientB.roomStatePush(join)", await clientB.readNextPacket(options.timeoutMs));
    printResponse("clientA.roomStatePush(join2)", await clientA.readNextPacket(options.timeoutMs));

    await clientA.send(MESSAGE_TYPE.ROOM_READY_REQ, 3, encodeRoomReadyReq(true));
    printResponse("clientA.roomReady", await clientA.readNextPacket(options.timeoutMs));
    printResponse("clientA.roomStatePush(ready1)", await clientA.readNextPacket(options.timeoutMs));
    printResponse("clientB.roomStatePush(ready1)", await clientB.readNextPacket(options.timeoutMs));

    await clientB.send(MESSAGE_TYPE.ROOM_READY_REQ, 3, encodeRoomReadyReq(true));
    printResponse("clientB.roomReady", await clientB.readNextPacket(options.timeoutMs));
    const readyPushB = printResponse("clientB.roomStatePush(ready2)", await clientB.readNextPacket(options.timeoutMs));
    const readyPushA = printResponse("clientA.roomStatePush(ready2)", await clientA.readNextPacket(options.timeoutMs));
    if (readyPushA.snapshot?.state !== "ready" || readyPushB.snapshot?.state !== "ready") {
      throw new Error("expected room state to become ready before start");
    }

    await clientA.send(MESSAGE_TYPE.ROOM_START_REQ, 4, encodeRoomStartReq());
    const startRes = printResponse("clientA.roomStart", await clientA.readNextPacket(options.timeoutMs));
    if (!startRes.ok) {
      throw new Error(`clientA room start failed: ${startRes.errorCode}`);
    }

    const startPushA = printResponse("clientA.roomStatePush(gameStarted)", await clientA.readNextPacket(options.timeoutMs));
    const startPushB = printResponse("clientB.roomStatePush(gameStarted)", await clientB.readNextPacket(options.timeoutMs));
    if (startPushA.event !== "game_started" || startPushB.event !== "game_started") {
      throw new Error("expected game_started room state push");
    }
    if (startPushA.snapshot?.state !== "in_game" || startPushB.snapshot?.state !== "in_game") {
      throw new Error("expected room state to become in_game");
    }

    await clientA.send(MESSAGE_TYPE.ROOM_LEAVE_REQ, 5, encodeRoomLeaveReq());
    const leaveA = printResponse("clientA.roomLeave", await clientA.readNextPacket(options.timeoutMs));
    if (!leaveA.ok) {
      throw new Error(`clientA room leave failed: ${leaveA.errorCode}`);
    }
    printResponse("clientB.roomStatePush(afterOwnerLeave)", await clientB.readNextPacket(options.timeoutMs));

    await delayBeforeFinalLeave(clientB, options.timeoutMs);
    await clientB.send(MESSAGE_TYPE.ROOM_LEAVE_REQ, 5, encodeRoomLeaveReq());
    const leaveB = printResponse("clientB.roomLeave", await clientB.readNextPacket(options.timeoutMs));
    if (!leaveB.ok) {
      throw new Error(`clientB room leave failed: ${leaveB.errorCode}`);
    }
  } finally {
    clientA.close();
    clientB.close();
  }
}

/**
 * Invalid ticket test
 */
export async function runInvalidTicket(client, options, login) {
  // Tamper with ticket
  const last = login.ticket.at(-1) === "a" ? "b" : "a";
  const tamperedTicket = `${login.ticket.slice(0, -1)}${last}`;

  const { encodeAuthReq } = await import("../messages.js");
  await client.send(MESSAGE_TYPE.AUTH_REQ, 1, encodeAuthReq(tamperedTicket));
  const auth = printResponse(`${client.label}.auth`, await client.readNextPacket(options.timeoutMs));
  if (auth.ok) {
    throw new Error("expected invalid ticket auth failure");
  }
}

/**
 * Unauthenticated room join test
 */
export async function runUnauthRoomJoin(client, options) {
  await client.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 1, encodeRoomJoinReq(options.roomId));
  await expectErrorPacket(client, options.timeoutMs, "NOT_AUTHENTICATED");
}

/**
 * Unknown message type test
 */
export async function runUnknownMessage(client, options) {
  await client.send(7777, 1, Buffer.alloc(0));
  await expectErrorPacket(client, options.timeoutMs, "UNKNOWN_MESSAGE_TYPE");
}

/**
 * Oversized room ID test
 */
export async function runOversizedRoomJoin(client, options, login) {
  await authenticateClient(client, options, login, 1);
  const oversizedRoomId = "r".repeat(options.maxBodyLen + 64);
  await client.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(oversizedRoomId));
  await expectErrorPacket(client, options.timeoutMs, "BODY_TOO_LARGE");
}

/**
 * Expect an error packet
 */
export async function expectErrorPacket(client, timeoutMs, expectedErrorCode, label = "error") {
  const packet = await client.readNextPacket(timeoutMs);
  const decoded = printResponse(`${client.label}.${label}`, packet);
  if (packet.messageType !== MESSAGE_TYPE.ERROR_RES) {
    throw new Error(`expected ERROR_RES, got ${packet.messageType}`);
  }
  if (decoded.errorCode !== expectedErrorCode) {
    throw new Error(`expected ${expectedErrorCode}, got ${decoded.errorCode}`);
  }
}

/**
 * Reconnect scenario: clientA joins, clientB joins, game starts,
 * clientA disconnects without sending ROOM_LEAVE, then reconnects
 */
export async function runReconnect(options) {
  const loginA = await fetchTicket(options, { guestId: `${options.roomId}-owner-reconnect` });
  const loginB = await fetchTicket(options, { guestId: `${options.roomId}-member-reconnect` });
  let clientA2 = null;

  console.log("clientA.login:", JSON.stringify({ playerId: loginA.playerId }, null, 2));
  console.log("clientB.login:", JSON.stringify({ playerId: loginB.playerId }, null, 2));

  // ClientA connects and joins room
  const clientA = new TcpProtocolClient(options, "clientA");
  const clientB = new TcpProtocolClient(options, "clientB");
  await clientA.connect();
  await clientB.connect();

  try {
    await authenticateClient(clientA, options, loginA, 1);

    await clientA.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(options.roomId));
    const joinA = printResponse("clientA.roomJoin", await clientA.readNextPacket(options.timeoutMs));
    if (!joinA.ok) {
      throw new Error(`clientA room join failed: ${joinA.errorCode}`);
    }
    const pushA1 = printResponse("clientA.roomStatePush(join)", await clientA.readNextPacket(options.timeoutMs));
    console.log("clientA joined room, owner:", pushA1.snapshot?.ownerPlayerId);

    // ClientB connects and joins same room
    await authenticateClient(clientB, options, loginB, 1);

    await clientB.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(options.roomId));
    const joinB = printResponse("clientB.roomJoin", await clientB.readNextPacket(options.timeoutMs));
    if (!joinB.ok) {
      throw new Error(`clientB room join failed: ${joinB.errorCode}`);
    }

    // Both should receive state push
    printResponse("clientB.roomStatePush(join)", await clientB.readNextPacket(options.timeoutMs));
    const pushA2 = printResponse("clientA.roomStatePush(member joined)", await clientA.readNextPacket(options.timeoutMs));
    console.log("Both clients in room, member count:", pushA2.snapshot?.members?.length);

    // Both ready up
    await clientA.send(MESSAGE_TYPE.ROOM_READY_REQ, 3, encodeRoomReadyReq(true));
    const readyA = printResponse("clientA.ready", await clientA.readNextPacket(options.timeoutMs));
    if (!readyA.ok) {
      throw new Error(`clientA ready failed: ${readyA.errorCode}`);
    }
    await waitForRoomStatePush(clientA, options.timeoutMs, "ready_changed", "roomStatePush(ready1)");
    await waitForRoomStatePush(clientB, options.timeoutMs, "ready_changed", "roomStatePush(ready1)");

    await clientB.send(MESSAGE_TYPE.ROOM_READY_REQ, 4, encodeRoomReadyReq(true));
    const readyB = printResponse("clientB.ready", await clientB.readNextPacket(options.timeoutMs));
    if (!readyB.ok) {
      throw new Error(`clientB ready failed: ${readyB.errorCode}`);
    }
    await waitForRoomStatePush(clientB, options.timeoutMs, "ready_changed", "roomStatePush(ready2)");
    await waitForRoomStatePush(clientA, options.timeoutMs, "ready_changed", "roomStatePush(ready2)");

    // Owner starts game
    await clientA.send(MESSAGE_TYPE.ROOM_START_REQ, 5, encodeRoomStartReq());

    const startA = await waitForRoomStartRes(clientA, options.timeoutMs, "roomStart");
    if (!startA.ok) {
      throw new Error(`clientA start failed: ${startA.errorCode}`);
    }
    await waitForRoomStatePush(clientA, options.timeoutMs, "game_started", "roomStatePush(gameStarted)");
    await waitForRoomStatePush(clientB, options.timeoutMs, "game_started", "roomStatePush(gameStarted)");
    console.log("Game started!");

    // Simulate player input from clientB
    await clientB.send(MESSAGE_TYPE.PLAYER_INPUT_REQ, 5, encodePlayerInputReq(1, "move", '{"direction":"right"}'));
    const inputRes = await waitForMessageType(clientB, options.timeoutMs, MESSAGE_TYPE.PLAYER_INPUT_RES, "playerInput");
    if (!inputRes.ok) {
      throw new Error(`clientB player input failed: ${inputRes.errorCode}`);
    }

    // Simulate disconnect: close clientA connection without sending ROOM_LEAVE
    console.log("Simulating disconnect for clientA (close without ROOM_LEAVE)...");
    clientA.close();

    // Wait a bit for disconnect to be detected
    await new Promise((resolve) => setTimeout(resolve, 1000));

    // ClientB should see member disconnected
    const pushDisconnected = await waitForRoomStatePush(
      clientB,
      options.timeoutMs * 2,
      "member_disconnected",
      "roomStatePush(disconnected)"
    );
    console.log("ClientB saw member disconnected, offline members:", pushDisconnected.snapshot?.members?.filter(m => m.offline)?.length);

    // ClientA reconnects with new connection
    console.log("ClientA reconnecting...");
    clientA2 = new TcpProtocolClient(options, "clientA2");
    await clientA2.connect();
    await authenticateClient(clientA2, options, loginA, 6);

    // Send reconnect request
    await clientA2.send(MESSAGE_TYPE.ROOM_RECONNECT_REQ, 7, encodeRoomReconnectReq(loginA.playerId));
    const reconnectRes = printResponse("clientA2.roomReconnect", await clientA2.readNextPacket(options.timeoutMs));

    if (!reconnectRes.ok) {
      throw new Error(`clientA reconnect failed: ${reconnectRes.errorCode}`);
    }

    console.log("ClientA reconnected successfully!");
    console.log("Reconnected room_id:", reconnectRes.roomId);
    console.log("Snapshot state:", reconnectRes.snapshot?.state);
    console.log("Snapshot member count:", reconnectRes.snapshot?.members?.length);
    console.log("Offline members:", reconnectRes.snapshot?.members?.filter(m => m.offline)?.length);

    // Reconnect should succeed even if ownership was transferred while clientA was offline.
    if (reconnectRes.roomId !== options.roomId) {
      throw new Error(`clientA reconnect returned unexpected room: ${reconnectRes.roomId}`);
    }
    if (reconnectRes.snapshot?.state !== "in_game") {
      throw new Error(`clientA reconnect expected in_game snapshot, got ${reconnectRes.snapshot?.state}`);
    }
    if (![loginA.playerId, loginB.playerId].includes(reconnectRes.snapshot?.ownerPlayerId)) {
      throw new Error(`unexpected owner after reconnect: ${reconnectRes.snapshot?.ownerPlayerId}`);
    }
    if (reconnectRes.snapshot?.ownerPlayerId !== loginA.playerId) {
      console.log("Owner transferred while clientA was offline:", reconnectRes.snapshot?.ownerPlayerId);
    }

    // Cleanup
    await delayBeforeFinalLeave(clientB, options.timeoutMs);
    await clientB.send(MESSAGE_TYPE.ROOM_LEAVE_REQ, 8, encodeRoomLeaveReq());
    const leaveB = await waitForMessageType(clientB, options.timeoutMs, MESSAGE_TYPE.ROOM_LEAVE_RES, "roomLeave");
    if (!leaveB.ok) {
      throw new Error(`clientB room leave failed: ${leaveB.errorCode}`);
    }

    console.log("Reconnect scenario completed successfully!");
  } finally {
    clientA.close();
    clientB.close();
    clientA2?.close();
  }
}

/**
 * Reconnect-all-disconnected scenario:
 * clientA/clientB join -> start game -> both disconnect without ROOM_LEAVE ->
 * both reconnect within offline TTL and verify room stays in_game.
 */
export async function runReconnectAllDisconnected(options) {
  const loginA = await fetchTicket(options, { guestId: `${options.roomId}-all-offline-a` });
  const loginB = await fetchTicket(options, { guestId: `${options.roomId}-all-offline-b` });
  let clientA2 = null;
  let clientB2 = null;

  console.log("clientA.login:", JSON.stringify({ playerId: loginA.playerId }, null, 2));
  console.log("clientB.login:", JSON.stringify({ playerId: loginB.playerId }, null, 2));

  const clientA = new TcpProtocolClient(options, "clientA");
  const clientB = new TcpProtocolClient(options, "clientB");
  await clientA.connect();
  await clientB.connect();

  try {
    await authenticateClient(clientA, options, loginA, 1);
    await authenticateClient(clientB, options, loginB, 1);

    await clientA.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(options.roomId));
    const joinA = printResponse("clientA.roomJoin", await clientA.readNextPacket(options.timeoutMs));
    if (!joinA.ok) {
      throw new Error(`clientA room join failed: ${joinA.errorCode}`);
    }
    printResponse("clientA.roomStatePush(join)", await clientA.readNextPacket(options.timeoutMs));

    await clientB.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(options.roomId));
    const joinB = printResponse("clientB.roomJoin", await clientB.readNextPacket(options.timeoutMs));
    if (!joinB.ok) {
      throw new Error(`clientB room join failed: ${joinB.errorCode}`);
    }
    printResponse("clientB.roomStatePush(join)", await clientB.readNextPacket(options.timeoutMs));
    printResponse("clientA.roomStatePush(join2)", await clientA.readNextPacket(options.timeoutMs));

    await clientA.send(MESSAGE_TYPE.ROOM_READY_REQ, 3, encodeRoomReadyReq(true));
    const readyA = printResponse("clientA.ready", await clientA.readNextPacket(options.timeoutMs));
    if (!readyA.ok) {
      throw new Error(`clientA ready failed: ${readyA.errorCode}`);
    }

    await clientB.send(MESSAGE_TYPE.ROOM_READY_REQ, 4, encodeRoomReadyReq(true));
    const readyB = printResponse("clientB.ready", await clientB.readNextPacket(options.timeoutMs));
    if (!readyB.ok) {
      throw new Error(`clientB ready failed: ${readyB.errorCode}`);
    }

    await clientA.send(MESSAGE_TYPE.ROOM_START_REQ, 5, encodeRoomStartReq());
    const startRes = await waitForRoomStartRes(clientA, options.timeoutMs, "roomStart");
    if (!startRes.ok) {
      throw new Error(`clientA start failed: ${startRes.errorCode}`);
    }
    console.log("Game started!");

    console.log("Simulating disconnect for clientA and clientB (close without ROOM_LEAVE)...");
    clientA.close();
    clientB.close();

    await new Promise((resolve) => setTimeout(resolve, 1500));

    console.log("ClientA reconnecting after global disconnect...");
    clientA2 = new TcpProtocolClient(options, "clientA2");
    await clientA2.connect();
    await authenticateClient(clientA2, options, loginA, 6);
    await clientA2.send(MESSAGE_TYPE.ROOM_RECONNECT_REQ, 7, encodeRoomReconnectReq(loginA.playerId));
    const reconnectA = printResponse("clientA2.roomReconnect", await clientA2.readNextPacket(options.timeoutMs));
    if (!reconnectA.ok) {
      throw new Error(`clientA reconnect failed: ${reconnectA.errorCode}`);
    }
    if (reconnectA.snapshot?.state !== "in_game") {
      throw new Error(`clientA reconnect expected in_game snapshot, got ${reconnectA.snapshot?.state}`);
    }

    console.log("ClientB reconnecting after global disconnect...");
    clientB2 = new TcpProtocolClient(options, "clientB2");
    await clientB2.connect();
    await authenticateClient(clientB2, options, loginB, 8);
    await clientB2.send(MESSAGE_TYPE.ROOM_RECONNECT_REQ, 9, encodeRoomReconnectReq(loginB.playerId));
    const reconnectB = printResponse("clientB2.roomReconnect", await clientB2.readNextPacket(options.timeoutMs));
    if (!reconnectB.ok) {
      throw new Error(`clientB reconnect failed: ${reconnectB.errorCode}`);
    }
    if (reconnectB.snapshot?.state !== "in_game") {
      throw new Error(`clientB reconnect expected in_game snapshot, got ${reconnectB.snapshot?.state}`);
    }

    console.log("Reconnect-all-disconnected summary:", JSON.stringify({
      roomId: options.roomId,
      reconnectA: {
        currentFrameId: reconnectA.currentFrameId,
        waitingFrameId: reconnectA.waitingFrameId,
        inputDelayFrames: reconnectA.inputDelayFrames,
        memberCount: reconnectA.snapshot?.members?.length,
        offlineMembers: reconnectA.snapshot?.members?.filter((member) => member.offline)?.length ?? 0
      },
      reconnectB: {
        currentFrameId: reconnectB.currentFrameId,
        waitingFrameId: reconnectB.waitingFrameId,
        inputDelayFrames: reconnectB.inputDelayFrames,
        memberCount: reconnectB.snapshot?.members?.length,
        offlineMembers: reconnectB.snapshot?.members?.filter((member) => member.offline)?.length ?? 0
      }
    }, null, 2));

    clientA2.close();
    clientB2.close();
    console.log("Reconnect-all-disconnected scenario completed successfully!");
  } catch (error) {
    clientA.close();
    clientB.close();
    clientA2?.close();
    clientB2?.close();
    throw error;
  }
}

/**
 * Create matched room scenario: login -> create matched room
 */
export async function runCreateMatchedRoom(client, options, login) {
  await authenticateClient(client, options, login, 1);

  const matchId = options.matchId || `match-${Date.now()}`;
  const roomId = options.roomId || "room-matched-001";
  // Ensure authenticated player is always in player_ids
  const playerIds = (options.playerIds && options.playerIds.length > 0)
    ? [...new Set([login.playerId, ...options.playerIds])]  // Merge and dedupe
    : [login.playerId];
  const mode = options.mode || "1v1";

  console.log(`${client.label}.createMatchedRoom:`, JSON.stringify({ matchId, roomId, playerIds, mode }, null, 2));

  await client.send(
    MESSAGE_TYPE.CREATE_MATCHED_ROOM_REQ,
    2,
    encodeCreateMatchedRoomReq(matchId, roomId, playerIds, mode)
  );

  const res = printResponse(`${client.label}.createMatchedRoomRes`, await client.readNextPacket(options.timeoutMs));
  if (!res.ok) {
    throw new Error(`create matched room failed: ${res.errorCode}`);
  }

  console.log(`${client.label}.matchedRoom:`, JSON.stringify({
    roomId: res.roomId,
    snapshot: res.snapshot ? {
      state: res.snapshot.state,
      memberCount: res.snapshot.members?.length,
      owner: res.snapshot.ownerPlayerId
    } : null
  }, null, 2));

  console.log(`${client.label}.create matched room success!`);
  console.log(`  Note: In real flow, players receive MatchEventStream and join via RoomJoinReq`);
}

/**
 * Create matched room and have all players join it - tests full flow including player_joined callbacks
 */
export async function runCreateMatchedRoomAndJoin(options) {
  // Create guest IDs for each player
  const hostGuestId = `host-${Date.now()}`;
  const guest1Id = `guest1-${Date.now()}`;
  const guest2Id = `guest2-${Date.now()}`;

  const matchId = options.matchId || `match-${Date.now()}`;
  const roomId = options.roomId || "room-matched-001";
  const mode = options.mode || "1v1";

  console.log("=== Create Matched Room And Join ===");
  console.log("Room:", JSON.stringify({ matchId, roomId, mode }, null, 2));

  // Step 1: All players get tickets (with delay to ensure unique guestIds)
  console.log("\n--- Getting tickets for all players ---");
  const loginHost = await fetchTicket(options, { guestId: hostGuestId });
  await new Promise((resolve) => setTimeout(resolve, 10));
  const login1 = await fetchTicket(options, { guestId: guest1Id });
  await new Promise((resolve) => setTimeout(resolve, 10));
  const login2 = await fetchTicket(options, { guestId: guest2Id });

  console.log("Host:", JSON.stringify({ playerId: loginHost.playerId }, null, 2));
  console.log("Player1:", JSON.stringify({ playerId: login1.playerId }, null, 2));
  console.log("Player2:", JSON.stringify({ playerId: login2.playerId }, null, 2));

  // Build player_ids from actual authenticated player IDs
  const playerIds = [loginHost.playerId, login1.playerId, login2.playerId];
  console.log("Player IDs for match:", playerIds);

  // Step 2: Host creates matched room
  const clientHost = new TcpProtocolClient(options, "clientHost");
  await clientHost.connect();
  await authenticateClient(clientHost, options, loginHost, 1);

  console.log("\n--- Host creating matched room ---");
  await clientHost.send(
    MESSAGE_TYPE.CREATE_MATCHED_ROOM_REQ,
    2,
    encodeCreateMatchedRoomReq(matchId, roomId, playerIds, mode)
  );

  const res = printResponse("clientHost.createMatchedRoomRes", await clientHost.readNextPacket(options.timeoutMs));
  if (!res.ok) {
    throw new Error(`create matched room failed: ${res.errorCode}`);
  }

  console.log("Matched room created:", JSON.stringify({
    roomId: res.roomId,
    snapshot: res.snapshot ? {
      state: res.snapshot.state,
      memberCount: res.snapshot.members?.length,
      owner: res.snapshot.ownerPlayerId
    } : null
  }, null, 2));

  // Step 3: All players join the room
  console.log("\n--- All players joining room ---");

  const players = [
    { login: login1, label: "Player1" },
    { login: login2, label: "Player2" },
  ];

  for (const { login, label } of players) {
    console.log(`[${label}] ${login.playerId} joining...`);

    const client = new TcpProtocolClient(options, `client_${label}`);
    await client.connect();
    await authenticateClient(client, options, login, 3);

    await client.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 4, encodeRoomJoinReq(roomId));
    const joinRes = printResponse(`client_${label}.roomJoin`, await client.readNextPacket(options.timeoutMs));

    if (!joinRes.ok) {
      throw new Error(`player ${label} room join failed: ${joinRes.errorCode}`);
    }

    // Wait for room state push
    const pushRes = printResponse(`client_${label}.roomStatePush`, await client.readNextPacket(options.timeoutMs));
    console.log(`[${label}] ${login.playerId} joined room, members:`, pushRes.snapshot?.members?.length);
  }

  console.log("\n--- All players joined successfully ---");
  console.log("Match flow complete!");
  console.log("  match_id:", matchId);
  console.log("  room_id:", roomId);
  console.log("  player_count:", playerIds.length);

  // Keep host connection open briefly to see the room state
  await new Promise((resolve) => setTimeout(resolve, 500));
  clientHost.close();
}
