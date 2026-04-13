import { MESSAGE_TYPE } from "../constants.js";
import {
  encodePingReq,
  encodeRoomJoinReq,
  encodeRoomLeaveReq,
  encodeRoomReadyReq,
  encodeRoomStartReq,
  encodePlayerInputReq,
  encodeRoomEndReq
} from "../messages.js";
import { fetchTicket } from "../auth.js";
import { TcpProtocolClient } from "../client.js";
import { authenticateClient, printResponse, waitForFrameBundle, delayBeforeFinalLeave } from "./room.js";

/**
 * Gameplay roundtrip: two clients join, start game, exchange inputs, end game
 */
export async function runGameplayRoundtrip(options) {
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
    printResponse("clientB.roomStatePush(ready2)", await clientB.readNextPacket(options.timeoutMs));
    printResponse("clientA.roomStatePush(ready2)", await clientA.readNextPacket(options.timeoutMs));

    await clientA.send(MESSAGE_TYPE.ROOM_START_REQ, 4, encodeRoomStartReq());
    const startRes = printResponse("clientA.roomStart", await clientA.readNextPacket(options.timeoutMs));
    if (!startRes.ok) {
      throw new Error(`clientA room start failed: ${startRes.errorCode}`);
    }
    printResponse("clientA.roomStatePush(gameStarted)", await clientA.readNextPacket(options.timeoutMs));
    printResponse("clientB.roomStatePush(gameStarted)", await clientB.readNextPacket(options.timeoutMs));

    const payloadJson = JSON.stringify({ x: 4, y: 7, frame: 1 });
    await clientA.send(MESSAGE_TYPE.PLAYER_INPUT_REQ, 5, encodePlayerInputReq(1, "move", payloadJson));
    const inputRes = printResponse("clientA.playerInput", await clientA.readNextPacket(options.timeoutMs));
    if (!inputRes.ok) {
      throw new Error(`clientA player input failed: ${inputRes.errorCode}`);
    }

    const framePushA = await waitForFrameBundle(clientA, options.timeoutMs, "move");
    const framePushB = await waitForFrameBundle(clientB, options.timeoutMs, "move");
    if (framePushA.inputs.length !== 1 || framePushB.inputs.length !== 1) {
      throw new Error("expected one frame input in the first non-silent frame");
    }
    if (framePushA.inputs[0].action !== "move" || framePushB.inputs[0].action !== "move") {
      throw new Error("expected frame bundle action to be move");
    }
    if (framePushA.inputs[0].payloadJson !== payloadJson || framePushB.inputs[0].payloadJson !== payloadJson) {
      throw new Error("expected frame bundle payload to match input payload");
    }
    if (framePushA.inputs[0].playerId !== loginA.playerId || framePushB.inputs[0].playerId !== loginA.playerId) {
      throw new Error("expected frame bundle playerId to be the input sender");
    }

    await clientA.send(MESSAGE_TYPE.ROOM_END_REQ, 6, encodeRoomEndReq("round-complete"));
    const endRes = printResponse("clientA.roomEnd", await clientA.readNextPacket(options.timeoutMs));
    if (!endRes.ok) {
      throw new Error(`clientA room end failed: ${endRes.errorCode}`);
    }

    const endPushA = printResponse("clientA.roomStatePush(gameEnded)", await clientA.readNextPacket(options.timeoutMs));
    const endPushB = printResponse("clientB.roomStatePush(gameEnded)", await clientB.readNextPacket(options.timeoutMs));
    if (endPushA.event !== "game_ended" || endPushB.event !== "game_ended") {
      throw new Error("expected game_ended room state push");
    }
    if (endPushA.snapshot?.state !== "waiting" || endPushB.snapshot?.state !== "waiting") {
      throw new Error("expected room to return to waiting after game end");
    }
    if (endPushA.snapshot?.members?.some((member) => member.ready) || endPushB.snapshot?.members?.some((member) => member.ready)) {
      throw new Error("expected all members ready state to reset after game end");
    }

    await clientA.send(MESSAGE_TYPE.ROOM_LEAVE_REQ, 7, encodeRoomLeaveReq());
    const leaveA = printResponse("clientA.roomLeave", await clientA.readNextPacket(options.timeoutMs));
    if (!leaveA.ok) {
      throw new Error(`clientA room leave failed: ${leaveA.errorCode}`);
    }
    printResponse("clientB.roomStatePush(afterOwnerLeave)", await clientB.readNextPacket(options.timeoutMs));

    await delayBeforeFinalLeave(clientB, options.timeoutMs);
    await clientB.send(MESSAGE_TYPE.ROOM_LEAVE_REQ, 7, encodeRoomLeaveReq());
    const leaveB = printResponse("clientB.roomLeave", await clientB.readNextPacket(options.timeoutMs));
    if (!leaveB.ok) {
      throw new Error(`clientB room leave failed: ${leaveB.errorCode}`);
    }
  } finally {
    clientA.close();
    clientB.close();
  }
}
