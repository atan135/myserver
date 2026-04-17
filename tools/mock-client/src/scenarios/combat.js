import { MESSAGE_TYPE } from "../constants.js";
import {
  encodePlayerInputReq,
  encodeRoomEndReq,
  encodeRoomJoinReq,
  encodeRoomLeaveReq,
  encodeRoomReadyReq,
  encodeRoomStartReq
} from "../messages.js";
import { decodeByMessageType } from "../messages.js";
import { fetchTicket, resolveMultiClientLoginOverrides } from "../auth.js";
import { TcpProtocolClient } from "../client.js";
import { authenticateClient, delayBeforeFinalLeave, printResponse } from "./room.js";

function resolveCombatRoomId(options) {
  return options.roomId && options.roomId !== "room-default"
    ? options.roomId
    : `room-combat-${Date.now()}`;
}

function parseCombatPayload(decoded, label) {
  try {
    return decoded.payloadJson ? JSON.parse(decoded.payloadJson) : {};
  } catch (error) {
    throw new Error(`${label} failed to parse combat payload JSON: ${error.message}`);
  }
}

function formatCombatPush(client, label, decoded, payload) {
  console.log(
    `${client.label}.${label}:`,
    JSON.stringify(
      {
        action: decoded.action,
        playerId: decoded.playerId,
        payload
      },
      null,
      2
    )
  );
}

function findCombatEntity(snapshotEnvelope, playerId) {
  return snapshotEnvelope?.snapshot?.entities?.find((entity) => entity.player_id === playerId) || null;
}

async function waitForRoomStatePush(client, expectedEvent, timeoutMs, label) {
  return client.readUntil(
    timeoutMs,
    (packet, decoded) =>
      packet.messageType === MESSAGE_TYPE.ROOM_STATE_PUSH && decoded.event === expectedEvent,
    label
  );
}

async function waitForCombatPush(client, timeoutMs, predicate, label) {
  const deadline = Date.now() + timeoutMs;

  while (Date.now() < deadline) {
    const remainingMs = Math.max(100, deadline - Date.now());
    const packet = await client.readNextPacket(remainingMs);
    const decoded = decodeByMessageType(packet.messageType, packet.body);

    if (packet.messageType === MESSAGE_TYPE.GAME_MESSAGE_PUSH && decoded.event === "combat") {
      const payload = parseCombatPayload(decoded, `${client.label}.${label}`);
      formatCombatPush(client, label, decoded, payload);

      if (decoded.action === "input_reject") {
        throw new Error(
          `${client.label} combat input rejected: ${payload.error_code || payload.errorCode || "UNKNOWN"}`
        );
      }

      if (decoded.action === "rejected") {
        throw new Error(
          `${client.label} combat event rejected: ${payload.detail || "UNKNOWN_REJECT_REASON"}`
        );
      }

      if (predicate(decoded, payload)) {
        return { decoded, payload };
      }
      continue;
    }

    if (packet.messageType === MESSAGE_TYPE.ROOM_STATE_PUSH) {
      console.log(
        `${client.label}.${label}[roomState]:`,
        JSON.stringify({ event: decoded.event }, null, 2)
      );
      continue;
    }

    if (packet.messageType === MESSAGE_TYPE.FRAME_BUNDLE_PUSH) {
      console.log(
        `${client.label}.${label}[frameBundle]:`,
        JSON.stringify(
          {
            frameId: decoded.frameId,
            fps: decoded.fps,
            inputCount: decoded.inputs.length,
            isSilentFrame: decoded.isSilentFrame
          },
          null,
          2
        )
      );
      continue;
    }

    console.log(
      `${client.label}.${label}[skip]:`,
      JSON.stringify(
        { messageType: packet.messageType, seq: packet.seq, decoded },
        null,
        2
      )
    );
  }

  throw new Error(`${client.label} timed out waiting for ${label}`);
}

async function createCombatLogins(options, roomId) {
  const loginA = await fetchTicket(
    options,
    resolveMultiClientLoginOverrides(options, "A", `${roomId}-combat-a`)
  );
  const loginB = await fetchTicket(
    options,
    resolveMultiClientLoginOverrides(options, "B", `${roomId}-combat-b`)
  );

  return { loginA, loginB };
}

export async function runCombatDualClient(options) {
  const roomId = resolveCombatRoomId(options);
  const policyId = options.policyId || "combat_demo";
  const skillId = options.combatSkillId || 2;
  const { loginA, loginB } = await createCombatLogins(options, roomId);

  console.log("=".repeat(60));
  console.log("COMBAT_DUAL_CLIENT - START");
  console.log("=".repeat(60));
  console.log(
    "scenario:",
    JSON.stringify(
      {
        roomId,
        policyId,
        skillId,
        clientA: loginA.playerId,
        clientB: loginB.playerId
      },
      null,
      2
    )
  );

  const clientA = new TcpProtocolClient(options, "clientA");
  const clientB = new TcpProtocolClient(options, "clientB");
  await clientA.connect();
  await clientB.connect();

  try {
    await authenticateClient(clientA, options, loginA, 1);
    await authenticateClient(clientB, options, loginB, 1);

    await clientA.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(roomId, policyId));
    const joinA = printResponse("clientA.roomJoin", await clientA.readNextPacket(options.timeoutMs));
    if (!joinA.ok) {
      throw new Error(`clientA room join failed: ${joinA.errorCode}`);
    }
    printResponse("clientA.roomStatePush(join1)", await clientA.readNextPacket(options.timeoutMs));

    await clientB.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(roomId, policyId));
    const joinB = printResponse("clientB.roomJoin", await clientB.readNextPacket(options.timeoutMs));
    if (!joinB.ok) {
      throw new Error(`clientB room join failed: ${joinB.errorCode}`);
    }
    printResponse("clientB.roomStatePush(join)", await clientB.readNextPacket(options.timeoutMs));
    printResponse("clientA.roomStatePush(join2)", await clientA.readNextPacket(options.timeoutMs));

    await clientA.send(MESSAGE_TYPE.ROOM_READY_REQ, 3, encodeRoomReadyReq(true));
    const readyA = printResponse("clientA.roomReady", await clientA.readNextPacket(options.timeoutMs));
    if (!readyA.ok) {
      throw new Error(`clientA ready failed: ${readyA.errorCode}`);
    }
    printResponse("clientA.roomStatePush(ready1)", await clientA.readNextPacket(options.timeoutMs));
    printResponse("clientB.roomStatePush(ready1)", await clientB.readNextPacket(options.timeoutMs));

    await clientB.send(MESSAGE_TYPE.ROOM_READY_REQ, 3, encodeRoomReadyReq(true));
    const readyB = printResponse("clientB.roomReady", await clientB.readNextPacket(options.timeoutMs));
    if (!readyB.ok) {
      throw new Error(`clientB ready failed: ${readyB.errorCode}`);
    }
    printResponse("clientB.roomStatePush(ready2)", await clientB.readNextPacket(options.timeoutMs));
    printResponse("clientA.roomStatePush(ready2)", await clientA.readNextPacket(options.timeoutMs));

    await clientA.send(MESSAGE_TYPE.ROOM_START_REQ, 4, encodeRoomStartReq());
    const startA = printResponse("clientA.roomStart", await clientA.readNextPacket(options.timeoutMs));
    if (!startA.ok) {
      throw new Error(`clientA room start failed: ${startA.errorCode}`);
    }
    await waitForRoomStatePush(clientA, "game_started", options.timeoutMs * 2, "roomStatePush(gameStarted)");
    await waitForRoomStatePush(clientB, "game_started", options.timeoutMs * 2, "roomStatePush(gameStarted)");

    const initialSnapshotA = await waitForCombatPush(
      clientA,
      options.timeoutMs * 4,
      (decoded, payload) => decoded.action === "snapshot" && payload.reason === "game_started",
      "combatSnapshot(initial)"
    );
    const initialSnapshotB = await waitForCombatPush(
      clientB,
      options.timeoutMs * 4,
      (decoded, payload) => decoded.action === "snapshot" && payload.reason === "game_started",
      "combatSnapshot(initial)"
    );

    const attackerA = findCombatEntity(initialSnapshotA.payload, loginA.playerId);
    const targetA = findCombatEntity(initialSnapshotA.payload, loginB.playerId);
    const attackerB = findCombatEntity(initialSnapshotB.payload, loginA.playerId);
    const targetB = findCombatEntity(initialSnapshotB.payload, loginB.playerId);

    if (!attackerA || !targetA || !attackerB || !targetB) {
      throw new Error("failed to resolve combat entity ids from initial snapshot");
    }

    const castFrameId = Number(initialSnapshotA.payload.frame_id || 1) + 1;
    const castPayload = JSON.stringify({
      skillId,
      targetPlayerId: loginB.playerId
    });

    console.log(
      "combat.castRequest:",
      JSON.stringify(
        {
          castFrameId,
          skillId,
          sourcePlayerId: loginA.playerId,
          targetPlayerId: loginB.playerId,
          targetEntityId: targetA.entity_id
        },
        null,
        2
      )
    );

    await clientA.send(
      MESSAGE_TYPE.PLAYER_INPUT_REQ,
      100,
      encodePlayerInputReq(castFrameId, "combat_cast_skill", castPayload)
    );
    const inputRes = await clientA.readUntil(
      options.timeoutMs,
      (packet, decoded) => packet.messageType === MESSAGE_TYPE.PLAYER_INPUT_RES && packet.seq === 100,
      "playerInput(combatCast)"
    );
    if (!inputRes.ok) {
      throw new Error(`combat cast input failed: ${inputRes.errorCode}`);
    }

    const castPushA = await waitForCombatPush(
      clientA,
      options.timeoutMs * 4,
      (decoded, payload) =>
        decoded.action === "skill_cast" &&
        payload.source_entity === attackerA.entity_id &&
        payload.target_entity === targetA.entity_id &&
        payload.skill_id === skillId,
      "combatEvent(skillCast)"
    );
    const castPushB = await waitForCombatPush(
      clientB,
      options.timeoutMs * 4,
      (decoded, payload) =>
        decoded.action === "skill_cast" &&
        payload.source_entity === attackerB.entity_id &&
        payload.target_entity === targetB.entity_id &&
        payload.skill_id === skillId,
      "combatEvent(skillCast)"
    );

    const damagePushA = await waitForCombatPush(
      clientA,
      options.timeoutMs * 4,
      (decoded, payload) =>
        decoded.action === "damage" &&
        payload.target_entity === targetA.entity_id &&
        payload.skill_id === skillId &&
        payload.value > 0,
      "combatEvent(damage)"
    );
    const damagePushB = await waitForCombatPush(
      clientB,
      options.timeoutMs * 4,
      (decoded, payload) =>
        decoded.action === "damage" &&
        payload.target_entity === targetB.entity_id &&
        payload.skill_id === skillId &&
        payload.value > 0,
      "combatEvent(damage)"
    );

    const snapshotAfterDamage = await waitForCombatPush(
      clientB,
      options.timeoutMs * 4,
      (decoded, payload) => {
        if (decoded.action !== "snapshot" || !payload.snapshot?.entities?.length) {
          return false;
        }
        const targetEntity = findCombatEntity(payload, loginB.playerId);
        return Boolean(targetEntity && targetEntity.hp < targetEntity.max_hp);
      },
      "combatSnapshot(afterDamage)"
    );

    const targetAfterDamage = findCombatEntity(snapshotAfterDamage.payload, loginB.playerId);
    if (!targetAfterDamage) {
      throw new Error("target entity missing from post-damage snapshot");
    }

    console.log(
      "combat.assertions:",
      JSON.stringify(
        {
          castObservedByClientA: castPushA.payload.skill_id,
          castObservedByClientB: castPushB.payload.skill_id,
          damageObservedByClientA: damagePushA.payload.value,
          damageObservedByClientB: damagePushB.payload.value,
          targetHpBefore: targetB.hp,
          targetHpAfter: targetAfterDamage.hp,
          targetMaxHp: targetAfterDamage.max_hp
        },
        null,
        2
      )
    );

    if (targetAfterDamage.hp >= targetAfterDamage.max_hp) {
      throw new Error("expected target HP to drop after damage");
    }

    await clientA.send(MESSAGE_TYPE.ROOM_END_REQ, 101, encodeRoomEndReq("combat-dual-client-complete"));
    const endRes = await clientA.readUntil(
      options.timeoutMs,
      (packet) => packet.messageType === MESSAGE_TYPE.ROOM_END_RES && packet.seq === 101,
      "roomEnd"
    );
    if (!endRes.ok) {
      throw new Error(`clientA room end failed: ${endRes.errorCode}`);
    }
    await waitForRoomStatePush(clientA, "game_ended", options.timeoutMs * 2, "roomStatePush(gameEnded)");
    await waitForRoomStatePush(clientB, "game_ended", options.timeoutMs * 2, "roomStatePush(gameEnded)");

    await clientA.send(MESSAGE_TYPE.ROOM_LEAVE_REQ, 102, encodeRoomLeaveReq());
    const leaveA = await clientA.readUntil(
      options.timeoutMs,
      (packet) => packet.messageType === MESSAGE_TYPE.ROOM_LEAVE_RES && packet.seq === 102,
      "roomLeave"
    );
    if (!leaveA.ok) {
      throw new Error(`clientA room leave failed: ${leaveA.errorCode}`);
    }
    await waitForRoomStatePush(clientB, "member_left", options.timeoutMs * 2, "roomStatePush(memberLeft)");

    await delayBeforeFinalLeave(clientB, options.timeoutMs, 1000);
    await clientB.send(MESSAGE_TYPE.ROOM_LEAVE_REQ, 103, encodeRoomLeaveReq());
    const leaveB = await clientB.readUntil(
      options.timeoutMs,
      (packet) => packet.messageType === MESSAGE_TYPE.ROOM_LEAVE_RES && packet.seq === 103,
      "roomLeave"
    );
    if (!leaveB.ok) {
      throw new Error(`clientB room leave failed: ${leaveB.errorCode}`);
    }

    console.log("=".repeat(60));
    console.log("COMBAT_DUAL_CLIENT - COMPLETE");
    console.log("=".repeat(60));
  } finally {
    clientA.close();
    clientB.close();
  }
}
