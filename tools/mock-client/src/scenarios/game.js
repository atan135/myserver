import { MESSAGE_TYPE, MOVE_INPUT_TYPE } from "../constants.js";
import {
  encodePingReq,
  encodeRoomJoinReq,
  encodeRoomLeaveReq,
  encodeRoomReadyReq,
  encodeRoomStartReq,
  encodePlayerInputReq,
  encodeMoveInputReq,
  encodeRoomEndReq
} from "../messages.js";
import { fetchTicket } from "../auth.js";
import { TcpProtocolClient } from "../client.js";
import { authenticateClient, printResponse, delayBeforeFinalLeave } from "./room.js";
import { decodeByMessageType } from "../messages.js";

/**
 * Format frame bundle for console display
 */
function formatFrameBundle(label, framePush) {
  const hasSnapshot = !!framePush.snapshot;
  const snapshotInfo = hasSnapshot
    ? ` [SNAPSHOT: frame=${framePush.snapshot.currentFrameId}, state=${framePush.snapshot.state}, members=${framePush.snapshot.members?.length || 0}]`
    : "";

  if (framePush.isSilentFrame) {
    console.log(`${label}: frameId=${framePush.frameId}, fps=${framePush.fps}, SILENT${snapshotInfo}`);
  } else {
    console.log(`${label}: frameId=${framePush.frameId}, fps=${framePush.fps}, inputs=${framePush.inputs.length}${snapshotInfo}`);
    for (const input of framePush.inputs) {
      console.log(`  └─ [${input.playerId}] ${input.action}: ${input.payloadJson}`);
    }
  }
}

/**
 * Read next RoomStatePush with expected event, skipping other packet types
 */
async function waitForRoomStatePush(client, expectedEvent, timeoutMs, label = "roomStatePush") {
  const maxIterations = 100;
  for (let i = 0; i < maxIterations; i++) {
    const packet = await client.readNextPacket(timeoutMs);
    const decoded = decodeByMessageType(packet.messageType, packet.body);

    if (packet.messageType === MESSAGE_TYPE.ROOM_STATE_PUSH) {
      console.log(`${client.label}.${label}:`, JSON.stringify({ event: decoded.event }, null, 2));
      if (decoded.event === expectedEvent) {
        return decoded;
      }
      // Got a different state push, keep waiting
      continue;
    } else if (packet.messageType === MESSAGE_TYPE.FRAME_BUNDLE_PUSH) {
      // Skip frame bundles during cleanup
      formatFrameBundle(`[${client.label} skip frame]`, decoded);
      continue;
    } else {
      // Other packet types, skip
      console.log(`${client.label}.${label}[skip ${packet.messageType}]:`, JSON.stringify(decoded, null, 2));
      continue;
    }
  }
  throw new Error(`Timeout waiting for ${expectedEvent} from ${client.label}`);
}

/**
 * Gameplay roundtrip: two clients join, start game, exchange inputs, end game
 */
export async function runGameplayRoundtrip(options) {
  const loginA = await fetchTicket(options, { guestId: `${options.roomId}-owner` });
  const loginB = await fetchTicket(options, { guestId: `${options.roomId}-member` });

  console.log("=".repeat(60));
  console.log("FRAME SYNC TEST - START");
  console.log("=".repeat(60));
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

    console.log("\n--- FRAME SYNC START ---\n");

    // Send multiple inputs and observe frames
    // Wait for PlayerInputRes each time before sending next input
    const inputFrames = [1, 2, 3, 5, 8, 13, 21];
    for (const frameId of inputFrames) {
      const payload = JSON.stringify({ x: frameId * 10, y: frameId * 5, action: `tick-${frameId}` });
      await clientA.send(MESSAGE_TYPE.PLAYER_INPUT_REQ, 4 + frameId, encodePlayerInputReq(frameId, "move", payload));

      // Wait specifically for PlayerInputRes (not FrameBundlePush)
      const maxWait = 50;
      let waited = 0;
      while (waited < maxWait) {
        const packet = await clientA.readNextPacket(100);
        if (!packet) {
          waited++;
          continue;
        }
        if (packet.messageType === MESSAGE_TYPE.PLAYER_INPUT_RES) {
          const decoded = decodeByMessageType(packet.messageType, packet.body);
          if (!decoded.ok) {
            console.log(`[WARN] Input at frame ${frameId} failed: ${decoded.errorCode}`);
          } else {
            console.log(`[CLIENT] Sent input at frame ${frameId}: ${payload}`);
          }
          break;
        } else if (packet.messageType === MESSAGE_TYPE.FRAME_BUNDLE_PUSH) {
          // Frame bundle arrived before input response - skip but don't break
          // Continue waiting for PlayerInputRes
          waited++;
          continue;
        }
        // Other packet types, continue
        waited++;
      }
      if (waited >= maxWait) {
        console.log(`[WARN] Timeout waiting for PlayerInputRes at frame ${frameId}`);
      }
    }

    // Receive and display frame bundles using readUntil for proper decoding
    console.log("\n--- RECEIVING FRAME BUNDLES ---\n");
    const maxFrames = 50;
    let frameCount = 0;

    while (frameCount < maxFrames) {
      // Use readUntil to get proper decoded frame bundles
      const decoded = await clientA.readUntil(
        options.timeoutMs,
        (packet) => packet.messageType === MESSAGE_TYPE.FRAME_BUNDLE_PUSH,
        "frameBundle"
      );

      formatFrameBundle(`[clientA]`, decoded);
      frameCount++;

      // Stop after we see a snapshot frame (indicating cycle)
      if (decoded.snapshot && frameCount > 5) {
        console.log("\n--- Received snapshot frame, ending frame sync test ---\n");
        break;
      }
    }

    console.log(`\nTotal frames received by clientA: ${frameCount}`);

    // End game
    await clientA.send(MESSAGE_TYPE.ROOM_END_REQ, 100, encodeRoomEndReq("round-complete"));
    const endRes = printResponse("clientA.roomEnd", await clientA.readNextPacket(options.timeoutMs));
    if (!endRes.ok) {
      throw new Error(`clientA room end failed: ${endRes.errorCode}`);
    }

    // Wait for game_ended state push, skipping any remaining frame bundles
    const endPushA = await waitForRoomStatePush(clientA, "game_ended", options.timeoutMs * 3, "roomStatePush(gameEnded)");
    const endPushB = await waitForRoomStatePush(clientB, "game_ended", options.timeoutMs * 3, "roomStatePush(gameEnded)");
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
    await waitForRoomStatePush(clientB, "member_left", options.timeoutMs * 2, "roomStatePush(afterOwnerLeave)");

    await delayBeforeFinalLeave(clientB, options.timeoutMs);
    await clientB.send(MESSAGE_TYPE.ROOM_LEAVE_REQ, 7, encodeRoomLeaveReq());
    const leaveB = printResponse("clientB.roomLeave", await clientB.readNextPacket(options.timeoutMs));
    if (!leaveB.ok) {
      throw new Error(`clientB room leave failed: ${leaveB.errorCode}`);
    }

    console.log("\n" + "=".repeat(60));
    console.log("FRAME SYNC TEST - COMPLETE");
    console.log("=".repeat(60));
  } finally {
    clientA.close();
    clientB.close();
  }
}

function formatMovementSnapshot(label, push) {
  console.log(
    `${label}: frameId=${push.frameId}, entities=${push.entities.length}, fullSync=${push.fullSync}, reason=${push.reason}`
  );
  for (const entity of push.entities) {
    console.log(
      `  └─ [${entity.playerId}] entity=${entity.entityId} scene=${entity.sceneId} pos=(${entity.x.toFixed(2)}, ${entity.y.toFixed(2)}) dir=(${entity.dirX.toFixed(2)}, ${entity.dirY.toFixed(2)}) moving=${entity.moving}`
    );
  }
}

export async function runMovementDemo(client, options, login) {
  await authenticateClient(client, options, login, 1);

  const policyId = options.policyId || "movement_demo";
  await client.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(options.roomId, policyId));
  const roomJoin = printResponse(`${client.label}.roomJoin`, await client.readNextPacket(options.timeoutMs));
  if (!roomJoin.ok) {
    throw new Error(`movement demo room join failed: ${roomJoin.errorCode}`);
  }

  printResponse(`${client.label}.roomStatePush(join)`, await client.readNextPacket(options.timeoutMs));

  await client.send(MESSAGE_TYPE.ROOM_READY_REQ, 3, encodeRoomReadyReq(true));
  const readyRes = printResponse(`${client.label}.roomReady`, await client.readNextPacket(options.timeoutMs));
  if (!readyRes.ok) {
    throw new Error(`movement demo room ready failed: ${readyRes.errorCode}`);
  }
  printResponse(`${client.label}.roomStatePush(ready)`, await client.readNextPacket(options.timeoutMs));

  await client.send(MESSAGE_TYPE.ROOM_START_REQ, 4, encodeRoomStartReq());
  const startRes = printResponse(`${client.label}.roomStart`, await client.readNextPacket(options.timeoutMs));
  if (!startRes.ok) {
    throw new Error(`movement demo room start failed: ${startRes.errorCode}`);
  }
  printResponse(`${client.label}.roomStatePush(gameStarted)`, await client.readNextPacket(options.timeoutMs));

  let sawSnapshot = false;
  let sawReject = false;
  const frames = options.moveFrames?.length ? options.moveFrames : [1, 2, 3, 4, 5];

  for (const frameId of frames) {
    const reqSeq = 100 + frameId;
    await client.send(
      MESSAGE_TYPE.MOVE_INPUT_REQ,
      reqSeq,
      encodeMoveInputReq(frameId, MOVE_INPUT_TYPE.MOVE_DIR, 1, 0)
    );
    const moveRes = await client.readUntil(
      options.timeoutMs,
      (packet) => packet.messageType === MESSAGE_TYPE.MOVE_INPUT_RES && packet.seq === reqSeq,
      `moveInputRes(${frameId})`
    );
    if (!moveRes.ok) {
      throw new Error(`movement demo move input failed at frame ${frameId}: ${moveRes.errorCode}`);
    }
  }

  await client.send(
    MESSAGE_TYPE.MOVE_INPUT_REQ,
    200,
    encodeMoveInputReq(frames.at(-1) + 1, MOVE_INPUT_TYPE.MOVE_STOP, 0, 0)
  );
  const stopRes = await client.readUntil(
    options.timeoutMs,
    (packet) => packet.messageType === MESSAGE_TYPE.MOVE_INPUT_RES && packet.seq === 200,
    "moveStopRes"
  );
  if (!stopRes.ok) {
    throw new Error(`movement demo move stop failed: ${stopRes.errorCode}`);
  }

  await client.send(
    MESSAGE_TYPE.MOVE_INPUT_REQ,
    201,
    encodeMoveInputReq(frames.at(-1) + 2, MOVE_INPUT_TYPE.MOVE_DIR, -1, 0)
  );
  const reverseRes = await client.readUntil(
    options.timeoutMs,
    (packet) => packet.messageType === MESSAGE_TYPE.MOVE_INPUT_RES && packet.seq === 201,
    "moveReverseRes"
  );
  if (!reverseRes.ok) {
    throw new Error(`movement demo reverse move failed: ${reverseRes.errorCode}`);
  }

  const startedAt = Date.now();
  while (Date.now() - startedAt < options.timeoutMs * 4) {
    const packet = await client.readNextPacket(options.timeoutMs);
    const decoded = decodeByMessageType(packet.messageType, packet.body);

    if (packet.messageType === MESSAGE_TYPE.MOVEMENT_SNAPSHOT_PUSH) {
      formatMovementSnapshot(`[${client.label}.movementSnapshot]`, decoded);
      sawSnapshot = true;
      if (decoded.entities.length > 0 && decoded.entities.some((entity) => entity.moving === false)) {
        break;
      }
      continue;
    }

    if (packet.messageType === MESSAGE_TYPE.MOVEMENT_REJECT_PUSH) {
      printResponse(`${client.label}.movementReject`, packet);
      sawReject = true;
      break;
    }

    if (packet.messageType === MESSAGE_TYPE.FRAME_BUNDLE_PUSH) {
      console.log(`${client.label}.frameBundle:`, JSON.stringify(decoded, null, 2));
      continue;
    }

    printResponse(`${client.label}.misc`, packet);
  }

  if (!sawSnapshot && !sawReject) {
    throw new Error(
      "movement demo did not receive MovementSnapshotPush or MovementRejectPush (may have been consumed while waiting for MoveInputRes)"
    );
  }

  console.log(`${client.label}.movementDemo.summary:`, JSON.stringify({ sawSnapshot, sawReject }, null, 2));

  await delayBeforeFinalLeave(client, options.timeoutMs, 1000);
  await client.send(MESSAGE_TYPE.ROOM_LEAVE_REQ, 300, encodeRoomLeaveReq());
  const leaveRes = await client.readUntil(
    options.timeoutMs,
    (packet) => packet.messageType === MESSAGE_TYPE.ROOM_LEAVE_RES,
    "roomLeave"
  );
  if (!leaveRes.ok) {
    throw new Error(`movement demo room leave failed: ${leaveRes.errorCode}`);
  }
}
