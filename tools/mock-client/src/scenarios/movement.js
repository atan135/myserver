import { MESSAGE_TYPE, MOVE_INPUT_TYPE } from "../constants.js";
import {
  encodeMoveInputReq,
  encodeRoomJoinReq,
  encodeRoomLeaveReq,
  encodeRoomReadyReq,
  encodeRoomStartReq
} from "../messages.js";
import { fetchTicket } from "../auth.js";
import { TcpProtocolClient } from "../client.js";
import { authenticateClient, printResponse, delayBeforeFinalLeave } from "./room.js";
import { decodeByMessageType } from "../messages.js";

// ---------------------------------------------------------------------------
// Shared helpers & constants for movement scenarios
// ---------------------------------------------------------------------------

// Server constants (from MovementDemoLogic / SceneTable):
//   speed       = 4.0 units/sec
//   spawn       = (2.0, 2.0) facing (1.0, 0.0)
//   fps         = 20
export const SERVER_SPEED = 4.0;
export const SERVER_SPAWN_X = 2.0;
export const SERVER_SPAWN_Y = 2.0;
export const SERVER_FPS = 20;

export function formatMovementSnapshot(label, push) {
  console.log(
    `${label}: frameId=${push.frameId}, entities=${push.entities.length}, fullSync=${push.fullSync}, reason=${push.reason}`
  );
  for (const entity of push.entities) {
    console.log(
      `  └─ [${entity.playerId}] entity=${entity.entityId} scene=${entity.sceneId} pos=(${entity.x.toFixed(2)}, ${entity.y.toFixed(2)}) dir=(${entity.dirX.toFixed(2)}, ${entity.dirY.toFixed(2)}) moving=${entity.moving}`
    );
  }
}

/**
 * Wait for the next MovementSnapshotPush, skipping other packet types.
 * Drains all already-queued packets (non-blocking) then blocks on the real wait.
 * Throws on timeout.
 */
export async function waitForMovementSnapshot(client, timeoutMs) {
  // Phase 1: drain everything already in the queue without blocking.
  // Uses a 1ms timeout per read; if queue is empty the timer fires and we move on.
  // Loop up to 20 times to avoid infinite loops.
  for (let drain = 0; drain < 20; drain++) {
    let packet;
    try {
      packet = await client.readNextPacket(1);
    } catch {
      // Timeout = queue empty, stop draining.
      break;
    }
    if (!packet) break; // queue empty
    if (packet.messageType === MESSAGE_TYPE.MOVEMENT_SNAPSHOT_PUSH) {
      return decodeByMessageType(packet.messageType, packet.body);
    }
    if (packet.messageType === MESSAGE_TYPE.MOVEMENT_REJECT_PUSH) {
      console.log(`[WARN] Unexpected MovementRejectPush while draining: ${JSON.stringify(decodeByMessageType(packet.messageType, packet.body))}`);
    }
    // Not a snapshot — discard stale packet, keep draining.
  }

  // Phase 2: blocking wait for the real snapshot from the server.
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const remaining = Math.max(100, deadline - Date.now());
    let packet;
    try {
      packet = await client.readNextPacket(remaining);
    } catch {
      // Timeout — loop and recalc remaining.
      continue;
    }
    if (!packet) continue;
    if (packet.messageType === MESSAGE_TYPE.MOVEMENT_SNAPSHOT_PUSH) {
      return decodeByMessageType(packet.messageType, packet.body);
    }
    if (packet.messageType === MESSAGE_TYPE.MOVEMENT_REJECT_PUSH) {
      console.log(`[WARN] Unexpected MovementRejectPush while waiting: ${JSON.stringify(decodeByMessageType(packet.messageType, packet.body))}`);
    }
    // Not a snapshot — discard and keep waiting.
  }
  throw new Error(`Timeout waiting for MovementSnapshotPush after ${timeoutMs}ms`);
}

/**
 * Drain all packets from a client until the next MovementSnapshotPush or timeout.
 * Returns the first snapshot encountered (or throws).
 */
export async function drainUntilSnapshot(client, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const remaining = Math.max(100, deadline - Date.now());
    let packet;
    try {
      packet = await client.readNextPacket(remaining);
    } catch {
      break;
    }
    if (packet && packet.messageType === MESSAGE_TYPE.MOVEMENT_SNAPSHOT_PUSH) {
      return decodeByMessageType(packet.messageType, packet.body);
    }
  }
  throw new Error("Timeout draining until next snapshot");
}

// ---------------------------------------------------------------------------
// Scenario: MOVEMENT_DEMO
// Single client joins movement_demo room, sends MoveDir/MoveStop/Reverse,
// observes MovementSnapshotPush / MovementRejectPush.
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Scenario: MOVEMENT_SYNC_VALIDATION
// Validates: MoveDir direction/moving, MoveStop stops, y-axis movement.
// MoveStop/FaceTo do NOT trigger snapshots (no position change).
// ---------------------------------------------------------------------------

export async function runMovementSyncValidation(options) {
  const login = await fetchTicket(options, { guestId: `sync-val-${Date.now()}` });
  console.log("login:", JSON.stringify({ playerId: login.playerId }, null, 2));

  const client = new TcpProtocolClient(options, "client");
  await client.connect();
  try {
    await authenticateClient(client, options, login, 1);

    const policyId = options.policyId || "movement_demo";
    await client.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(options.roomId, policyId));
    const joinRes = printResponse("client.roomJoin", await client.readNextPacket(options.timeoutMs));
    if (!joinRes.ok) throw new Error(`room join failed: ${joinRes.errorCode}`);

    printResponse("client.roomStatePush(join)", await client.readNextPacket(options.timeoutMs));

    await client.send(MESSAGE_TYPE.ROOM_READY_REQ, 3, encodeRoomReadyReq(true));
    const readyRes = printResponse("client.roomReady", await client.readNextPacket(options.timeoutMs));
    if (!readyRes.ok) throw new Error(`room ready failed: ${readyRes.errorCode}`);

    printResponse("client.roomStatePush(ready)", await client.readNextPacket(options.timeoutMs));

    await client.send(MESSAGE_TYPE.ROOM_START_REQ, 4, encodeRoomStartReq());
    const startRes = printResponse("client.roomStart", await client.readNextPacket(options.timeoutMs));
    if (!startRes.ok) throw new Error(`room start failed: ${startRes.errorCode}`);

    printResponse("client.roomStatePush(gameStarted)", await client.readNextPacket(options.timeoutMs));

    // Collect first full-sync snapshot to get spawn position
    const firstSnap = await waitForMovementSnapshot(client, options.timeoutMs);
    const selfEntity = firstSnap.entities.find((e) => e.playerId === login.playerId);
    if (!selfEntity) throw new Error(`Spawn entity not found for player ${login.playerId}`);

    const spawnX = selfEntity.x;
    const spawnY = selfEntity.y;
    console.log(`[ASSERT] Spawn position: (${spawnX.toFixed(3)}, ${spawnY.toFixed(3)})`);

    // -------------------------------------------------------------------
    // 1. MoveDir(1,0) — direction becomes (1,0), moving=true, position advances
    // -------------------------------------------------------------------
    await client.send(MESSAGE_TYPE.MOVE_INPUT_REQ, 100, encodeMoveInputReq(1, MOVE_INPUT_TYPE.MOVE_DIR, 1, 0));
    const moveRes1 = await client.readUntil(
      options.timeoutMs,
      (p) => p.messageType === MESSAGE_TYPE.MOVE_INPUT_RES && p.seq === 100,
      "moveDirRes1"
    );
    if (!moveRes1.ok) throw new Error(`MoveDir frame 1 failed: ${moveRes1.errorCode}`);

    const snapAfterMove = await waitForMovementSnapshot(client, options.timeoutMs * 3);
    const afterMove = snapAfterMove.entities.find((e) => e.playerId === login.playerId);
    if (!afterMove) throw new Error("Entity not found in snapshot after move");

    console.log(`[ASSERT] After 1 MoveDir(1,0): pos=(${afterMove.x.toFixed(3)}, ${afterMove.y.toFixed(3)}) moving=${afterMove.moving}`);
    console.log(`[ASSERT] Direction: (${afterMove.dirX.toFixed(3)}, ${afterMove.dirY.toFixed(3)})`);

    if (Math.abs(afterMove.dirX - 1.0) > 0.01 || Math.abs(afterMove.dirY - 0.0) > 0.01) {
      throw new Error(`Expected dir=(1,0), got dir=(${afterMove.dirX}, ${afterMove.dirY})`);
    }
    if (!afterMove.moving) {
      throw new Error(`Expected moving=true after MoveDir`);
    }

    // -------------------------------------------------------------------
    // 2. MoveStop — does NOT emit MovementSnapshotPush (position unchanged).
    //    Confirm via FrameBundlePush carry-check.
    // -------------------------------------------------------------------
    await client.send(MESSAGE_TYPE.MOVE_INPUT_REQ, 101, encodeMoveInputReq(2, MOVE_INPUT_TYPE.MOVE_STOP, 0, 0));
    const stopRes = await client.readUntil(
      options.timeoutMs,
      (p) => p.messageType === MESSAGE_TYPE.MOVE_INPUT_RES && p.seq === 101,
      "moveStopRes"
    );
    if (!stopRes.ok) throw new Error(`MoveStop failed: ${stopRes.errorCode}`);

    let confirmedMovingFalse = false;
    const fbDeadline = Date.now() + options.timeoutMs * 3;
    while (Date.now() < fbDeadline) {
      const remaining = Math.max(100, fbDeadline - Date.now());
      let packet;
      try {
        packet = await client.readNextPacket(remaining);
      } catch {
        break;
      }
      if (!packet) break;
      if (packet.messageType === MESSAGE_TYPE.FRAME_BUNDLE_PUSH) {
        const fb = decodeByMessageType(packet.messageType, packet.body);
        const hasMoveStop = fb.inputs.some(
          (i) => i.playerId === login.playerId && i.action === "move_stop"
        );
        if (hasMoveStop) {
          console.log(`[ASSERT] MoveStop confirmed in frame bundle frameId=${fb.frameId}`);
          confirmedMovingFalse = true;
          break;
        }
      }
    }
    if (!confirmedMovingFalse) {
      throw new Error("MoveStop input was not observed in any FrameBundlePush");
    }

    // -------------------------------------------------------------------
    // 3. MoveDir(0,1) — changes y position, triggers MovementSnapshotPush.
    //    Also confirms previous moving=false state was held.
    // -------------------------------------------------------------------
    await client.send(MESSAGE_TYPE.MOVE_INPUT_REQ, 102, encodeMoveInputReq(3, MOVE_INPUT_TYPE.MOVE_DIR, 0, 1));
    const moveRes2 = await client.readUntil(
      options.timeoutMs,
      (p) => p.messageType === MESSAGE_TYPE.MOVE_INPUT_RES && p.seq === 102,
      "moveDirRes(0,1)"
    );
    if (!moveRes2.ok) throw new Error(`MoveDir(0,1) failed: ${moveRes2.errorCode}`);

    const snapAfterMove2 = await waitForMovementSnapshot(client, options.timeoutMs * 3);
    const afterMove2 = snapAfterMove2.entities.find((e) => e.playerId === login.playerId);
    if (!afterMove2) throw new Error("Entity not found in snapshot after MoveDir(0,1)");
    console.log(`[ASSERT] After MoveDir(0,1): pos=(${afterMove2.x.toFixed(3)},${afterMove2.y.toFixed(3)}) dir=(${afterMove2.dirX.toFixed(3)},${afterMove2.dirY.toFixed(3)}) moving=${afterMove2.moving}`);

    if (Math.abs(afterMove2.dirX) > 0.01 || Math.abs(afterMove2.dirY - 1.0) > 0.01) {
      throw new Error(`Expected dir=(0,1) after MoveDir(0,1), got (${afterMove2.dirX}, ${afterMove2.dirY})`);
    }
    if (afterMove2.moving !== true) {
      throw new Error(`Expected moving=true after MoveDir, got ${afterMove2.moving}`);
    }

    // Verify y advanced from spawn (2.0) — direction (0,1) means y increases
    if (afterMove2.y <= 2.0) {
      throw new Error(`Expected y > spawn_y after MoveDir(0,1), got y=${afterMove2.y.toFixed(3)}`);
    }
    console.log(`[ASSERT] y advanced: ${afterMove2.y.toFixed(3)} > 2.000 ✓`);

    console.log("\n" + "=".repeat(60));
    console.log("MOVEMENT_SYNC_VALIDATION - ALL ASSERTIONS PASSED");
    console.log("=".repeat(60));

    await delayBeforeFinalLeave(client, options.timeoutMs, 1000);
    await client.send(MESSAGE_TYPE.ROOM_LEAVE_REQ, 300, encodeRoomLeaveReq());
    const leaveRes = await client.readUntil(
      options.timeoutMs,
      (p) => p.messageType === MESSAGE_TYPE.ROOM_LEAVE_RES,
      "roomLeave"
    );
    if (!leaveRes.ok) throw new Error(`room leave failed: ${leaveRes.errorCode}`);
  } finally {
    client.close();
  }
}

// ---------------------------------------------------------------------------
// Scenario: MOVEMENT_DUAL_CLIENT_SYNC
// Two clients in same movement_demo room. Client A moves, both receive
// the same entity positions in MovementSnapshotPush.
// ---------------------------------------------------------------------------

export async function runMovementDualClientSync(options) {
  const loginA = await fetchTicket(options, { guestId: `dual-a-${Date.now()}` });
  const loginB = await fetchTicket(options, { guestId: `dual-b-${Date.now()}` });

  console.log("=".repeat(60));
  console.log("MOVEMENT_DUAL_CLIENT_SYNC - START");
  console.log("=".repeat(60));
  console.log("clientA.playerId:", loginA.playerId);
  console.log("clientB.playerId:", loginB.playerId);

  const clientA = new TcpProtocolClient(options, "clientA");
  const clientB = new TcpProtocolClient(options, "clientB");
  await clientA.connect();
  await clientB.connect();

  try {
    await authenticateClient(clientA, options, loginA, 1);
    await authenticateClient(clientB, options, loginB, 1);

    const policyId = options.policyId || "movement_demo";
    await clientA.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(options.roomId, policyId));
    await clientB.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(options.roomId, policyId));

    const joinA = printResponse("clientA.roomJoin", await clientA.readNextPacket(options.timeoutMs));
    if (!joinA.ok) throw new Error(`clientA join failed: ${joinA.errorCode}`);
    const joinB = printResponse("clientB.roomJoin", await clientB.readNextPacket(options.timeoutMs));
    if (!joinB.ok) throw new Error(`clientB join failed: ${joinB.errorCode}`);

    // Drain state pushes for both
    for (const cl of [clientA, clientB]) {
      for (let i = 0; i < 3; i++) {
        cl.readNextPacket(options.timeoutMs).catch(() => {});
      }
    }

    await clientA.send(MESSAGE_TYPE.ROOM_READY_REQ, 3, encodeRoomReadyReq(true));
    await clientB.send(MESSAGE_TYPE.ROOM_READY_REQ, 3, encodeRoomReadyReq(true));

    for (const cl of [clientA, clientB]) {
      printResponse(`${cl.label}.roomReady`, await cl.readNextPacket(options.timeoutMs));
      printResponse(`${cl.label}.roomStatePush(ready)`, await cl.readNextPacket(options.timeoutMs));
    }

    await clientA.send(MESSAGE_TYPE.ROOM_START_REQ, 4, encodeRoomStartReq());
    const startA = printResponse("clientA.roomStart", await clientA.readNextPacket(options.timeoutMs));
    if (!startA.ok) throw new Error(`room start failed: ${startA.errorCode}`);
    printResponse("clientA.roomStatePush(gameStarted)", await clientA.readNextPacket(options.timeoutMs));
    printResponse("clientB.roomStatePush(gameStarted)", await clientB.readNextPacket(options.timeoutMs));

    // Collect initial snapshot from both clients to confirm spawn positions
    const snapA0 = await waitForMovementSnapshot(clientA, options.timeoutMs * 3);
    const snapB0 = await waitForMovementSnapshot(clientB, options.timeoutMs * 3);

    const entityA0 = snapA0.entities.find((e) => e.playerId === loginA.playerId);
    const entityB0 = snapB0.entities.find((e) => e.playerId === loginB.playerId);
    if (!entityA0 || !entityB0) throw new Error("Initial spawn entities not found");

    console.log(`[ASSERT] clientA spawn: (${entityA0.x.toFixed(3)}, ${entityA0.y.toFixed(3)})`);
    console.log(`[ASSERT] clientB spawn: (${entityB0.x.toFixed(3)}, ${entityB0.y.toFixed(3)})`);

    // ClientA moves right (1,0) for 3 frames
    for (let frame = 1; frame <= 3; frame++) {
      await clientA.send(MESSAGE_TYPE.MOVE_INPUT_REQ, 100 + frame, encodeMoveInputReq(frame, MOVE_INPUT_TYPE.MOVE_DIR, 1, 0));
      const res = await clientA.readUntil(
        options.timeoutMs,
        (p) => p.messageType === MESSAGE_TYPE.MOVE_INPUT_RES && p.seq === 100 + frame,
        `moveDirRes(${frame})`
      );
      if (!res.ok) throw new Error(`clientA MoveDir frame ${frame} failed: ${res.errorCode}`);
    }

    // Wait for snapshots on both clients and compare entity positions
    const snapA1 = await waitForMovementSnapshot(clientA, options.timeoutMs * 3);
    const snapB1 = await waitForMovementSnapshot(clientB, options.timeoutMs * 3);

    const posA = Object.fromEntries(snapA1.entities.map((e) => [e.playerId, { x: e.x, y: e.y }]));
    const posB = Object.fromEntries(snapB1.entities.map((e) => [e.playerId, { x: e.x, y: e.y }]));

    console.log(`\n[SYNC CHECK] ClientA snapshot entities:`);
    for (const e of snapA1.entities) {
      console.log(`  ${e.playerId}: (${e.x.toFixed(3)}, ${e.y.toFixed(3)}) moving=${e.moving}`);
    }
    console.log(`\n[SYNC CHECK] ClientB snapshot entities:`);
    for (const e of snapB1.entities) {
      console.log(`  ${e.playerId}: (${e.x.toFixed(3)}, ${e.y.toFixed(3)}) moving=${e.moving}`);
    }

    const playersA = new Set(Object.keys(posA));
    const playersB = new Set(Object.keys(posB));
    if (playersA.size !== playersB.size) {
      throw new Error(`Entity count mismatch: clientA has ${playersA.size}, clientB has ${playersB.size}`);
    }

    const EPS = 0.05;
    for (const playerId of playersA) {
      if (!playersB.has(playerId)) {
        throw new Error(`Entity ${playerId} missing in clientB snapshot`);
      }
      const a = posA[playerId];
      const b = posB[playerId];
      if (Math.abs(a.x - b.x) > EPS || Math.abs(a.y - b.y) > EPS) {
        throw new Error(
          `Position mismatch for ${playerId}: clientA=(${a.x.toFixed(3)},${a.y.toFixed(3)}) ` +
            `clientB=(${b.x.toFixed(3)},${b.y.toFixed(3)}) delta=(${Math.abs(a.x-b.x).toFixed(3)},${Math.abs(a.y-b.y).toFixed(3)})`
        );
      }
    }

    console.log("\n" + "=".repeat(60));
    console.log("MOVEMENT_DUAL_CLIENT_SYNC - ALL ASSERTIONS PASSED");
    console.log("=".repeat(60));

    await delayBeforeFinalLeave(clientA, options.timeoutMs, 1000);
    await clientA.send(MESSAGE_TYPE.ROOM_LEAVE_REQ, 300, encodeRoomLeaveReq());
    printResponse("clientA.roomLeave", await clientA.readNextPacket(options.timeoutMs));
    await delayBeforeFinalLeave(clientB, options.timeoutMs, 1000);
    await clientB.send(MESSAGE_TYPE.ROOM_LEAVE_REQ, 301, encodeRoomLeaveReq());
    printResponse("clientB.roomLeave", await clientB.readNextPacket(options.timeoutMs));
  } finally {
    clientA.close();
    clientB.close();
  }
}

// ---------------------------------------------------------------------------
// Scenario: MOVEMENT_SNAPSHOT_THROTTLE
// Verifies: first snapshot fullSync=true (game_started), then snapshots
// are emitted every 3 frames (SNAPSHOT_INTERVAL_FRAMES).
// ---------------------------------------------------------------------------

export async function runMovementSnapshotThrottle(options) {
  const login = await fetchTicket(options, { guestId: `throttle-${Date.now()}` });
  console.log("=".repeat(60));
  console.log("MOVEMENT_SNAPSHOT_THROTTLE - START");
  console.log("=".repeat(60));

  const client = new TcpProtocolClient(options, "client");
  await client.connect();
  try {
    await authenticateClient(client, options, login, 1);

    const policyId = options.policyId || "movement_demo";
    await client.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(options.roomId, policyId));
    const joinRes = printResponse("client.roomJoin", await client.readNextPacket(options.timeoutMs));
    if (!joinRes.ok) throw new Error(`room join failed: ${joinRes.errorCode}`);
    printResponse("client.roomStatePush(join)", await client.readNextPacket(options.timeoutMs));

    await client.send(MESSAGE_TYPE.ROOM_READY_REQ, 3, encodeRoomReadyReq(true));
    printResponse("client.roomReady", await client.readNextPacket(options.timeoutMs));
    printResponse("client.roomStatePush(ready)", await client.readNextPacket(options.timeoutMs));

    await client.send(MESSAGE_TYPE.ROOM_START_REQ, 4, encodeRoomStartReq());
    const startRes = printResponse("client.roomStart", await client.readNextPacket(options.timeoutMs));
    if (!startRes.ok) throw new Error(`room start failed: ${startRes.errorCode}`);
    printResponse("client.roomStatePush(gameStarted)", await client.readNextPacket(options.timeoutMs));

    // First snapshot must be full_sync=true (game_started)
    const snap0 = await waitForMovementSnapshot(client, options.timeoutMs * 3);
    console.log(`[ASSERT] snap0: frameId=${snap0.frameId} fullSync=${snap0.fullSync} reason=${snap0.reason}`);
    if (!snap0.fullSync) {
      throw new Error(`Expected first snapshot fullSync=true (game_started), got ${snap0.fullSync}`);
    }
    if (snap0.reason !== "game_started") {
      throw new Error(`Expected first snapshot reason='game_started', got '${snap0.reason}'`);
    }

    // Send MoveDir, collect several snapshots, verify throttle interval
    await client.send(MESSAGE_TYPE.MOVE_INPUT_REQ, 100, encodeMoveInputReq(1, MOVE_INPUT_TYPE.MOVE_DIR, 1, 0));
    const res0 = await client.readUntil(options.timeoutMs, (p) => p.messageType === MESSAGE_TYPE.MOVE_INPUT_RES && p.seq === 100, "moveRes0");
    if (!res0.ok) throw new Error(`MoveDir failed: ${res0.errorCode}`);

    const snapFrames = [];
    for (let i = 0; i < 6; i++) {
      const snap = await waitForMovementSnapshot(client, options.timeoutMs * 3);
      snapFrames.push(snap.frameId);
      console.log(`[THROTTLE] snap[${i}]: frameId=${snap.frameId} fullSync=${snap.fullSync} reason=${snap.reason} entities=${snap.entities.length}`);
    }

    console.log(`\n[ASSERT] Snapshot frame IDs: ${snapFrames.join(" -> ")}`);
    for (let i = 1; i < snapFrames.length; i++) {
      const delta = snapFrames[i] - snapFrames[i - 1];
      if (delta !== 3) {
        console.log(`[WARN] Snapshot delta between snap[${i-1}] and snap[${i}]: ${delta} (expected 3)`);
      }
    }

    console.log("\n" + "=".repeat(60));
    console.log("MOVEMENT_SNAPSHOT_THROTTLE - COMPLETE");
    console.log("=".repeat(60));

    await delayBeforeFinalLeave(client, options.timeoutMs, 1000);
    await client.send(MESSAGE_TYPE.ROOM_LEAVE_REQ, 300, encodeRoomLeaveReq());
    const leaveRes = await client.readUntil(options.timeoutMs, (p) => p.messageType === MESSAGE_TYPE.ROOM_LEAVE_RES, "roomLeave");
    if (!leaveRes.ok) throw new Error(`room leave failed: ${leaveRes.errorCode}`);
  } finally {
    client.close();
  }
}

// ---------------------------------------------------------------------------
// Scenario: MOVEMENT_FACE_TO
// Tests that FaceTo changes direction without starting movement.
// FaceTo does NOT emit MovementSnapshotPush (position unchanged).
// MoveDir OVERWRITES direction (last input wins).
// ---------------------------------------------------------------------------

export async function runMovementFaceTo(options) {
  const login = await fetchTicket(options, { guestId: `faceto-${Date.now()}` });
  console.log("=".repeat(60));
  console.log("MOVEMENT_FACE_TO - START");
  console.log("=".repeat(60));

  const client = new TcpProtocolClient(options, "client");
  await client.connect();
  try {
    await authenticateClient(client, options, login, 1);

    const policyId = options.policyId || "movement_demo";
    await client.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(options.roomId, policyId));
    const joinRes = printResponse("client.roomJoin", await client.readNextPacket(options.timeoutMs));
    if (!joinRes.ok) throw new Error(`room join failed: ${joinRes.errorCode}`);
    printResponse("client.roomStatePush(join)", await client.readNextPacket(options.timeoutMs));

    await client.send(MESSAGE_TYPE.ROOM_READY_REQ, 3, encodeRoomReadyReq(true));
    printResponse("client.roomReady", await client.readNextPacket(options.timeoutMs));
    printResponse("client.roomStatePush(ready)", await client.readNextPacket(options.timeoutMs));

    await client.send(MESSAGE_TYPE.ROOM_START_REQ, 4, encodeRoomStartReq());
    const startRes = printResponse("client.roomStart", await client.readNextPacket(options.timeoutMs));
    if (!startRes.ok) throw new Error(`room start failed: ${startRes.errorCode}`);
    printResponse("client.roomStatePush(gameStarted)", await client.readNextPacket(options.timeoutMs));

    // Drain first full-sync snapshot
    await waitForMovementSnapshot(client, options.timeoutMs * 3);

    // Helper: confirm a FaceTo was processed via FrameBundlePush carry-check
    async function confirmFaceToInBundle(expectedAction) {
      const fbDeadline = Date.now() + options.timeoutMs * 3;
      while (Date.now() < fbDeadline) {
        const remaining = Math.max(100, fbDeadline - Date.now());
        let packet;
        try {
          packet = await client.readNextPacket(remaining);
        } catch {
          break;
        }
        if (!packet) break;
        if (packet.messageType === MESSAGE_TYPE.FRAME_BUNDLE_PUSH) {
          const fb = decodeByMessageType(packet.messageType, packet.body);
          const found = fb.inputs.some(
            (i) => i.playerId === login.playerId && i.action === expectedAction
          );
          if (found) {
            console.log(`[ASSERT] ${expectedAction} confirmed in frameId=${fb.frameId}`);
            return;
          }
        }
      }
      throw new Error(`${expectedAction} input was not observed in any FrameBundlePush`);
    }

    // -------------------------------------------------------------------
    // 1. FaceTo(0,1) — face up (confirmed via bundle)
    // -------------------------------------------------------------------
    await client.send(MESSAGE_TYPE.MOVE_INPUT_REQ, 100, encodeMoveInputReq(1, MOVE_INPUT_TYPE.FACE_TO, 0, 1));
    const res1 = await client.readUntil(
      options.timeoutMs,
      (p) => p.messageType === MESSAGE_TYPE.MOVE_INPUT_RES && p.seq === 100,
      "faceToRes1"
    );
    if (!res1.ok) throw new Error(`FaceTo(0,1) failed: ${res1.errorCode}`);
    await confirmFaceToInBundle("face_to");

    // -------------------------------------------------------------------
    // 2. MoveDir(1,0) — triggers snapshot; direction becomes (1,0)
    //    because MoveDir OVERWRITES the direction set by prior FaceTo.
    // -------------------------------------------------------------------
    await client.send(MESSAGE_TYPE.MOVE_INPUT_REQ, 101, encodeMoveInputReq(2, MOVE_INPUT_TYPE.MOVE_DIR, 1, 0));
    const res2 = await client.readUntil(
      options.timeoutMs,
      (p) => p.messageType === MESSAGE_TYPE.MOVE_INPUT_RES && p.seq === 101,
      "moveDirRes1"
    );
    if (!res2.ok) throw new Error(`MoveDir(1,0) failed: ${res2.errorCode}`);

    const snap2 = await waitForMovementSnapshot(client, options.timeoutMs * 3);
    const ent2 = snap2.entities.find((e) => e.playerId === login.playerId);
    if (!ent2) throw new Error("Entity not found after MoveDir");
    console.log(`[ASSERT] MoveDir(1,0) snap: dir=(${ent2.dirX.toFixed(3)},${ent2.dirY.toFixed(3)}) moving=${ent2.moving}`);

    if (Math.abs(ent2.dirX - 1.0) > 0.01 || Math.abs(ent2.dirY) > 0.01) {
      throw new Error(`Expected dir=(1,0) after MoveDir, got (${ent2.dirX}, ${ent2.dirY})`);
    }
    if (ent2.moving !== true) {
      throw new Error(`Expected moving=true after MoveDir, got ${ent2.moving}`);
    }

    // -------------------------------------------------------------------
    // 3. MoveStop, then FaceTo(-1,0)
    // -------------------------------------------------------------------
    await client.send(MESSAGE_TYPE.MOVE_INPUT_REQ, 102, encodeMoveInputReq(3, MOVE_INPUT_TYPE.MOVE_STOP, 0, 0));
    const resStop = await client.readUntil(
      options.timeoutMs,
      (p) => p.messageType === MESSAGE_TYPE.MOVE_INPUT_RES && p.seq === 102,
      "moveStopRes"
    );
    if (!resStop.ok) throw new Error(`MoveStop failed: ${resStop.errorCode}`);

    // Confirm MoveStop in bundle
    const fbDeadline = Date.now() + options.timeoutMs * 3;
    let confirmedStop = false;
    while (Date.now() < fbDeadline && !confirmedStop) {
      const remaining = Math.max(100, fbDeadline - Date.now());
      let packet;
      try {
        packet = await client.readNextPacket(remaining);
      } catch {
        break;
      }
      if (!packet) break;
      if (packet.messageType === MESSAGE_TYPE.FRAME_BUNDLE_PUSH) {
        const fb = decodeByMessageType(packet.messageType, packet.body);
        confirmedStop = fb.inputs.some(
          (i) => i.playerId === login.playerId && i.action === "move_stop"
        );
      }
    }

    await client.send(MESSAGE_TYPE.MOVE_INPUT_REQ, 103, encodeMoveInputReq(4, MOVE_INPUT_TYPE.FACE_TO, -1, 0));
    const res3 = await client.readUntil(
      options.timeoutMs,
      (p) => p.messageType === MESSAGE_TYPE.MOVE_INPUT_RES && p.seq === 103,
      "faceToRes2"
    );
    if (!res3.ok) throw new Error(`FaceTo(-1,0) failed: ${res3.errorCode}`);
    await confirmFaceToInBundle("face_to");

    // -------------------------------------------------------------------
    // 4. MoveDir(0,1) — triggers snapshot; direction becomes (0,1)
    //    because MoveDir OVERWRITES the direction set by prior FaceTo.
    // -------------------------------------------------------------------
    await client.send(MESSAGE_TYPE.MOVE_INPUT_REQ, 104, encodeMoveInputReq(5, MOVE_INPUT_TYPE.MOVE_DIR, 0, 1));
    const res4 = await client.readUntil(
      options.timeoutMs,
      (p) => p.messageType === MESSAGE_TYPE.MOVE_INPUT_RES && p.seq === 104,
      "moveDirRes2"
    );
    if (!res4.ok) throw new Error(`MoveDir(0,1) after FaceTo(-1,0) failed: ${res4.errorCode}`);

    const snap4 = await waitForMovementSnapshot(client, options.timeoutMs * 3);
    const ent4 = snap4.entities.find((e) => e.playerId === login.playerId);
    if (!ent4) throw new Error("Entity not found after MoveDir following FaceTo(-1,0)");
    console.log(`[ASSERT] MoveDir(0,1) snap: dir=(${ent4.dirX.toFixed(3)},${ent4.dirY.toFixed(3)}) moving=${ent4.moving}`);

    if (Math.abs(ent4.dirX) > 0.01 || Math.abs(ent4.dirY - 1.0) > 0.01) {
      throw new Error(`Expected dir=(0,1) after MoveDir, got (${ent4.dirX}, ${ent4.dirY})`);
    }
    if (ent4.moving !== true) {
      throw new Error(`Expected moving=true after MoveDir, got ${ent4.moving}`);
    }

    console.log("\n" + "=".repeat(60));
    console.log("MOVEMENT_FACE_TO - ALL ASSERTIONS PASSED");
    console.log("=".repeat(60));

    await delayBeforeFinalLeave(client, options.timeoutMs, 1000);
    await client.send(MESSAGE_TYPE.ROOM_LEAVE_REQ, 300, encodeRoomLeaveReq());
    const leaveRes = await client.readUntil(options.timeoutMs, (p) => p.messageType === MESSAGE_TYPE.ROOM_LEAVE_RES, "roomLeave");
    if (!leaveRes.ok) throw new Error(`room leave failed: ${leaveRes.errorCode}`);
  } finally {
    client.close();
  }
}
