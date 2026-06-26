import { MESSAGE_TYPE } from "../constants.js";
import {
  encodePlayerInputReq,
  encodeRoomJoinReq,
  encodeRoomLeaveReq,
  encodeRoomReadyReq,
  encodeRoomStartReq
} from "../messages.js";
import { decodeByMessageType } from "../messages.js";
import { fetchTicket, resolveMultiClientLoginOverrides } from "../auth.js";
import { TcpProtocolClient } from "../client.js";
import { authenticateClient, delayBeforeFinalLeave } from "./room.js";

export const ROBOT_SYNC_POLICY_ID = "robot_sync_room";
export const ROBOT_MOVE_ACTION = "robot_move";

function resolveRobotSyncRoomId(options) {
  return options.roomId && options.roomId !== "room-default"
    ? options.roomId
    : `room-robot-sync-${Date.now()}`;
}

export function buildRobotMovePayload({
  version = 1,
  seq,
  botTick,
  dirX,
  dirY,
  speed
}) {
  return JSON.stringify({
    version,
    seq,
    botTick,
    dirX,
    dirY,
    speed
  });
}

function summarizeFrameBundle(bundle) {
  return {
    frameId: bundle.frameId,
    fps: bundle.fps,
    inputCount: bundle.inputs.length,
    isSilentFrame: bundle.isSilentFrame,
    inputs: bundle.inputs.map((input) => ({
      frameId: input.frameId,
      characterId: input.characterId,
      action: input.action,
      payloadJson: input.payloadJson
    }))
  };
}

async function waitForMessageType(client, timeoutMs, expectedMessageType, expectedSeq, label) {
  return client.readUntil(
    timeoutMs,
    (packet) =>
      packet.messageType === expectedMessageType &&
      (expectedSeq === null || packet.seq === expectedSeq),
    label
  );
}

async function waitForRoomStatePush(client, timeoutMs, expectedEvent, label) {
  return client.readUntil(
    timeoutMs,
    (packet, decoded) =>
      packet.messageType === MESSAGE_TYPE.ROOM_STATE_PUSH && decoded.event === expectedEvent,
    label
  );
}

async function joinRoom(client, options, roomId, policyId, seq) {
  await client.send(MESSAGE_TYPE.ROOM_JOIN_REQ, seq, encodeRoomJoinReq(roomId, policyId));
  const joinRes = await waitForMessageType(
    client,
    options.timeoutMs,
    MESSAGE_TYPE.ROOM_JOIN_RES,
    seq,
    "roomJoin"
  );
  if (!joinRes.ok) {
    throw new Error(`${client.label} room join failed: ${joinRes.errorCode}`);
  }
  await waitForRoomStatePush(client, options.timeoutMs, "member_joined", "roomStatePush(join)");
  return joinRes;
}

async function readyRoom(client, options, seq) {
  await client.send(MESSAGE_TYPE.ROOM_READY_REQ, seq, encodeRoomReadyReq(true));
  const readyRes = await waitForMessageType(
    client,
    options.timeoutMs,
    MESSAGE_TYPE.ROOM_READY_RES,
    seq,
    "roomReady"
  );
  if (!readyRes.ok) {
    throw new Error(`${client.label} room ready failed: ${readyRes.errorCode}`);
  }
  return readyRes;
}

async function startRoom(ownerClient, options, seq) {
  await ownerClient.send(MESSAGE_TYPE.ROOM_START_REQ, seq, encodeRoomStartReq());
  const startRes = await waitForMessageType(
    ownerClient,
    options.timeoutMs,
    MESSAGE_TYPE.ROOM_START_RES,
    seq,
    "roomStart"
  );
  if (!startRes.ok) {
    throw new Error(`${ownerClient.label} room start failed: ${startRes.errorCode}`);
  }
  return startRes;
}

async function waitForLatestFrameBundle(client, timeoutMs, label) {
  const deadline = Date.now() + timeoutMs;
  let latest = null;

  while (Date.now() < deadline) {
    const remainingMs = latest ? 1 : Math.max(100, deadline - Date.now());
    let packet;
    try {
      packet = await client.readNextPacket(remainingMs);
    } catch (error) {
      if (latest) {
        return latest;
      }
      throw error;
    }

    const decoded = decodeByMessageType(packet.messageType, packet.body);
    if (packet.messageType === MESSAGE_TYPE.FRAME_BUNDLE_PUSH) {
      latest = decoded;
      console.log(`${client.label}.${label}:`, JSON.stringify(summarizeFrameBundle(decoded), null, 2));
      continue;
    }

    if (packet.messageType === MESSAGE_TYPE.MOVEMENT_SNAPSHOT_PUSH) {
      throw new Error(`${client.label} received unexpected MovementSnapshotPush in robot_sync_room`);
    }

    if (packet.messageType === MESSAGE_TYPE.ERROR_RES) {
      throw new Error(`${client.label} received ERROR_RES while waiting for frame bundle: ${decoded.errorCode}`);
    }

    console.log(
      `${client.label}.${label}[skip]:`,
      JSON.stringify({ messageType: packet.messageType, seq: packet.seq, decoded }, null, 2)
    );
  }

  if (latest) {
    return latest;
  }
  throw new Error(`${client.label} timed out waiting for latest FrameBundlePush`);
}

function findExpectedRobotInputs(bundle, expectedInputs) {
  return expectedInputs.map((expected) =>
    bundle.inputs.find(
      (input) =>
        input.characterId === expected.characterId &&
        input.action === ROBOT_MOVE_ACTION &&
        input.payloadJson === expected.payloadJson
    ) || null
  );
}

export async function waitForRobotMoveFrameBundle(client, timeoutMs, expectedInputs, label = "robotMoveFrameBundle") {
  const deadline = Date.now() + timeoutMs;

  while (Date.now() < deadline) {
    const remainingMs = Math.max(100, deadline - Date.now());
    const packet = await client.readNextPacket(remainingMs);
    const decoded = decodeByMessageType(packet.messageType, packet.body);

    if (packet.messageType === MESSAGE_TYPE.FRAME_BUNDLE_PUSH) {
      console.log(`${client.label}.${label}:`, JSON.stringify(summarizeFrameBundle(decoded), null, 2));
      const matches = findExpectedRobotInputs(decoded, expectedInputs);
      if (matches.every(Boolean)) {
        return decoded;
      }
      continue;
    }

    if (packet.messageType === MESSAGE_TYPE.MOVEMENT_SNAPSHOT_PUSH) {
      throw new Error(`${client.label} received unexpected MovementSnapshotPush in robot_sync_room`);
    }

    if (packet.messageType === MESSAGE_TYPE.ERROR_RES) {
      throw new Error(`${client.label} received ERROR_RES while waiting for robot move bundle: ${decoded.errorCode}`);
    }

    console.log(
      `${client.label}.${label}[skip]:`,
      JSON.stringify({ messageType: packet.messageType, seq: packet.seq, decoded }, null, 2)
    );
  }

  throw new Error(`${client.label} timed out waiting for FrameBundlePush containing both robot_move inputs`);
}

async function waitForRobotMoveAcceptedAndBundle(
  client,
  timeoutMs,
  responseSeq,
  expectedInputs,
  label = "robotMoveAcceptedAndBundle"
) {
  const deadline = Date.now() + timeoutMs;
  let inputAccepted = false;
  let matchedBundle = null;

  while (Date.now() < deadline) {
    const remainingMs = Math.max(100, deadline - Date.now());
    const packet = await client.readNextPacket(remainingMs);
    const decoded = decodeByMessageType(packet.messageType, packet.body);

    if (packet.messageType === MESSAGE_TYPE.PLAYER_INPUT_RES && packet.seq === responseSeq) {
      console.log(`${client.label}.${label}.playerInputRes:`, JSON.stringify(decoded, null, 2));
      if (!decoded.ok) {
        throw new Error(`${client.label} robot_move input failed: ${decoded.errorCode}`);
      }
      inputAccepted = true;
      if (matchedBundle) {
        return { inputRes: decoded, bundle: matchedBundle };
      }
      continue;
    }

    if (packet.messageType === MESSAGE_TYPE.FRAME_BUNDLE_PUSH) {
      console.log(`${client.label}.${label}.frameBundle:`, JSON.stringify(summarizeFrameBundle(decoded), null, 2));
      const matches = findExpectedRobotInputs(decoded, expectedInputs);
      if (matches.every(Boolean)) {
        matchedBundle = decoded;
        if (inputAccepted) {
          return { inputRes: null, bundle: matchedBundle };
        }
      }
      continue;
    }

    if (packet.messageType === MESSAGE_TYPE.MOVEMENT_SNAPSHOT_PUSH) {
      throw new Error(`${client.label} received unexpected MovementSnapshotPush in robot_sync_room`);
    }

    if (packet.messageType === MESSAGE_TYPE.ERROR_RES) {
      throw new Error(`${client.label} received ERROR_RES while waiting for robot move: ${decoded.errorCode}`);
    }

    console.log(
      `${client.label}.${label}[skip]:`,
      JSON.stringify({ messageType: packet.messageType, seq: packet.seq, decoded }, null, 2)
    );
  }

  throw new Error(`${client.label} timed out waiting for accepted robot_move input and matching FrameBundlePush`);
}

export async function expectPlayerInputRejected(
  client,
  options,
  {
    seq,
    frameId,
    action = ROBOT_MOVE_ACTION,
    payloadJson,
    expectedErrorCode,
    label
  }
) {
  await client.send(
    MESSAGE_TYPE.PLAYER_INPUT_REQ,
    seq,
    encodePlayerInputReq(frameId, action, payloadJson)
  );
  const inputRes = await waitForMessageType(
    client,
    options.timeoutMs,
    MESSAGE_TYPE.PLAYER_INPUT_RES,
    seq,
    label || `playerInputRejected(${expectedErrorCode})`
  );
  if (inputRes.ok) {
    throw new Error(`${client.label} expected ${expectedErrorCode}, got ok response`);
  }
  if (inputRes.errorCode !== expectedErrorCode) {
    throw new Error(`${client.label} expected ${expectedErrorCode}, got ${inputRes.errorCode}`);
  }

  console.log(
    `${client.label}.inputRejected:`,
    JSON.stringify({ expectedErrorCode, actualErrorCode: inputRes.errorCode }, null, 2)
  );
  return inputRes;
}

async function leaveRoom(client, options, seq) {
  try {
    await delayBeforeFinalLeave(client, options.timeoutMs, 500);
    await client.send(MESSAGE_TYPE.ROOM_LEAVE_REQ, seq, encodeRoomLeaveReq());
    const leaveRes = await waitForMessageType(
      client,
      options.timeoutMs,
      MESSAGE_TYPE.ROOM_LEAVE_RES,
      seq,
      "roomLeave"
    );
    if (!leaveRes.ok) {
      throw new Error(`${client.label} room leave failed: ${leaveRes.errorCode}`);
    }
  } catch (error) {
    console.warn(`${client.label}.roomLeave cleanup skipped: ${error.message}`);
  }
}

export async function runRobotSyncRoom(options) {
  const roomId = resolveRobotSyncRoomId(options);
  const policyId = options.policyId || ROBOT_SYNC_POLICY_ID;
  const loginA = await fetchTicket(
    options,
    resolveMultiClientLoginOverrides(options, "A", `${roomId}-robot-a`)
  );
  const loginB = await fetchTicket(
    options,
    resolveMultiClientLoginOverrides(options, "B", `${roomId}-robot-b`)
  );

  console.log("=".repeat(60));
  console.log("ROBOT_SYNC_ROOM - START");
  console.log("=".repeat(60));
  console.log(
    "scenario:",
    JSON.stringify(
      {
        roomId,
        policyId,
        clientA: {
          accountPlayerId: loginA.playerId,
          characterId: loginA.characterId
        },
        clientB: {
          accountPlayerId: loginB.playerId,
          characterId: loginB.characterId
        }
      },
      null,
      2
    )
  );

  const clientA = new TcpProtocolClient(options, "clientA");
  const clientB = new TcpProtocolClient(options, "clientB");

  try {
    await clientA.connect();
    await clientB.connect();

    await authenticateClient(clientA, options, loginA, 1);
    await authenticateClient(clientB, options, loginB, 1);

    await joinRoom(clientA, options, roomId, policyId, 2);
    await joinRoom(clientB, options, roomId, policyId, 2);
    await waitForRoomStatePush(clientA, options.timeoutMs, "member_joined", "roomStatePush(join2)");

    await readyRoom(clientA, options, 3);
    await waitForRoomStatePush(clientA, options.timeoutMs, "ready_changed", "roomStatePush(readyA)");
    await waitForRoomStatePush(clientB, options.timeoutMs, "ready_changed", "roomStatePush(readyA)");

    await readyRoom(clientB, options, 3);
    await waitForRoomStatePush(clientB, options.timeoutMs, "ready_changed", "roomStatePush(readyB)");
    await waitForRoomStatePush(clientA, options.timeoutMs, "ready_changed", "roomStatePush(readyB)");

    await startRoom(clientA, options, 4);
    await waitForRoomStatePush(clientA, options.timeoutMs * 2, "game_started", "roomStatePush(gameStarted)");
    await waitForRoomStatePush(clientB, options.timeoutMs * 2, "game_started", "roomStatePush(gameStarted)");

    const latestBundle = await waitForLatestFrameBundle(clientA, options.timeoutMs * 2, "latestFrameBundle");
    const targetFrameId = latestBundle.frameId + 2;
    const payloadA = buildRobotMovePayload({
      seq: 1,
      botTick: targetFrameId,
      dirX: 1000,
      dirY: 0,
      speed: 7500
    });
    const payloadB = buildRobotMovePayload({
      seq: 2,
      botTick: targetFrameId,
      dirX: -500,
      dirY: 250,
      speed: 3200
    });

    console.log(
      "robotMove.inputs:",
      JSON.stringify(
        {
          targetFrameId,
          clientA: { characterId: loginA.characterId, payloadJson: payloadA },
          clientB: { characterId: loginB.characterId, payloadJson: payloadB }
        },
        null,
        2
      )
    );

    const expectedInputs = [
      { characterId: loginA.characterId, payloadJson: payloadA },
      { characterId: loginB.characterId, payloadJson: payloadB }
    ];
    await Promise.all([
      clientA.send(
        MESSAGE_TYPE.PLAYER_INPUT_REQ,
        100,
        encodePlayerInputReq(targetFrameId, ROBOT_MOVE_ACTION, payloadA)
      ),
      clientB.send(
        MESSAGE_TYPE.PLAYER_INPUT_REQ,
        100,
        encodePlayerInputReq(targetFrameId, ROBOT_MOVE_ACTION, payloadB)
      )
    ]);
    const [{ bundle: bundleA }, { bundle: bundleB }] = await Promise.all([
      waitForRobotMoveAcceptedAndBundle(
        clientA,
        options.timeoutMs * 4,
        100,
        expectedInputs,
        "robotMoveA"
      ),
      waitForRobotMoveAcceptedAndBundle(
        clientB,
        options.timeoutMs * 4,
        100,
        expectedInputs,
        "robotMoveB"
      )
    ]);

    if (bundleA.frameId !== targetFrameId || bundleB.frameId !== targetFrameId) {
      throw new Error(
        `expected robot move bundle frame ${targetFrameId}, got clientA=${bundleA.frameId}, clientB=${bundleB.frameId}`
      );
    }

    console.log(
      "robotMove.assertions:",
      JSON.stringify(
        {
          frameId: targetFrameId,
          observedByClientA: findExpectedRobotInputs(bundleA, expectedInputs).filter(Boolean).length,
          observedByClientB: findExpectedRobotInputs(bundleB, expectedInputs).filter(Boolean).length
        },
        null,
        2
      )
    );

    const invalidFrameId = targetFrameId + 1;
    const validPayload = buildRobotMovePayload({
      seq: 10,
      botTick: invalidFrameId,
      dirX: 0,
      dirY: 0,
      speed: 1
    });

    await expectPlayerInputRejected(clientA, options, {
      seq: 200,
      frameId: invalidFrameId,
      action: "move",
      payloadJson: validPayload,
      expectedErrorCode: "INVALID_ROBOT_MOVE_ACTION",
      label: "playerInputRejected(invalidAction)"
    });
    await expectPlayerInputRejected(clientA, options, {
      seq: 201,
      frameId: invalidFrameId,
      payloadJson: "{",
      expectedErrorCode: "INVALID_ROBOT_MOVE_JSON",
      label: "playerInputRejected(invalidJson)"
    });
    await expectPlayerInputRejected(clientA, options, {
      seq: 202,
      frameId: invalidFrameId,
      payloadJson: buildRobotMovePayload({
        seq: 11,
        botTick: invalidFrameId,
        dirX: 1001,
        dirY: 0,
        speed: 1
      }),
      expectedErrorCode: "ROBOT_MOVE_DIR_OUT_OF_RANGE",
      label: "playerInputRejected(dirOutOfRange)"
    });
    await expectPlayerInputRejected(clientA, options, {
      seq: 203,
      frameId: invalidFrameId,
      payloadJson: buildRobotMovePayload({
        seq: 12,
        botTick: invalidFrameId,
        dirX: 0,
        dirY: 0,
        speed: 10001
      }),
      expectedErrorCode: "ROBOT_MOVE_SPEED_OUT_OF_RANGE",
      label: "playerInputRejected(speedOutOfRange)"
    });

    console.log("=".repeat(60));
    console.log("ROBOT_SYNC_ROOM - COMPLETE");
    console.log("=".repeat(60));
  } finally {
    await leaveRoom(clientA, options, 300);
    await leaveRoom(clientB, options, 300);
    clientA.close();
    clientB.close();
  }
}
