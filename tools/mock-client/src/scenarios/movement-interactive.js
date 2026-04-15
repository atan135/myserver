import readline from "node:readline/promises";
import process from "node:process";
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
import { authenticateClient, delayBeforeFinalLeave } from "./room.js";
import { decodeByMessageType } from "../messages.js";

// Direction mappings: WASD and arrow keys
const DIRECTION_MAP = {
  w: { x: 0, y: 1 },   // up
  a: { x: -1, y: 0 },  // left
  s: { x: 0, y: -1 },  // down
  d: { x: 1, y: 0 },   // right
  ArrowUp: { x: 0, y: 1 },
  ArrowLeft: { x: -1, y: 0 },
  ArrowDown: { x: 0, y: -1 },
  ArrowRight: { x: 1, y: 0 }
};

function formatSnapshot(label, push) {
  const lines = [];
  lines.push(`${label}: frame=${push.frameId} reason=${push.reason} entities=${push.entities.length}`);
  for (const entity of push.entities) {
    lines.push(
      `  [${entity.playerId.slice(0, 8)}] pos=(${entity.x.toFixed(2)}, ${entity.y.toFixed(2)}) dir=(${entity.dirX.toFixed(1)},${entity.dirY.toFixed(1)}) moving=${entity.moving}`
    );
  }
  return lines.join("\n");
}

export async function runMovementInteractive(options) {
  // Generate unique room ID if not provided, to avoid "ROOM_ALREADY_IN_GAME" from previous runs
  const roomId = options.roomId && options.roomId !== "room-default"
    ? options.roomId
    : `room-mov-${Date.now()}`;

  // Determine credentials for A and B
  const loginNameA = options.loginNameA || options.loginName;
  const passwordA = options.passwordA || options.password;
  const loginNameB = options.loginNameB;
  const passwordB = options.passwordB;

  let loginA, loginB;

  // ClientA login: use account if provided, otherwise guest
  if (loginNameA && passwordA) {
    loginA = await fetchTicket(options, { loginName: loginNameA, password: passwordA });
  } else {
    loginA = await fetchTicket(options, { guestId: `mov-a-${Date.now()}` });
  }

  // ClientB login: use account if provided, otherwise guest
  if (loginNameB && passwordB) {
    loginB = await fetchTicket(options, { loginName: loginNameB, password: passwordB });
  } else {
    loginB = await fetchTicket(options, { guestId: `mov-b-${Date.now()}` });
  }

  console.log("=".repeat(60));
  console.log("MOVEMENT INTERACTIVE - Dual Client Movement Sync");
  console.log("=".repeat(60));
  console.log("Room ID:", roomId);
  console.log("clientA.playerId:", loginA.playerId);
  console.log("clientB.playerId:", loginB.playerId);
  console.log("");
  console.log("Controls:");
  console.log("  w / ArrowUp    - Move up");
  console.log("  a / ArrowLeft  - Move left");
  console.log("  s / ArrowDown  - Move down");
  console.log("  d / ArrowRight - Move right");
  console.log("  space          - Stop moving");
  console.log("  q              - Quit");
  console.log("");
  console.log("Both clients will see each other's positions update in real-time.");
  console.log("=".repeat(60));

  const clientA = new TcpProtocolClient(options, "clientA");
  const clientB = new TcpProtocolClient(options, "clientB");
  await clientA.connect();
  await clientB.connect();

  try {
    await authenticateClient(clientA, options, loginA, 1);
    await authenticateClient(clientB, options, loginB, 1);

    const policyId = options.policyId || "movement_demo";

    // Both clients join the room
    await clientA.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(roomId, policyId));
    await clientB.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(roomId, policyId));

    const joinA = await clientA.readNextPacket(options.timeoutMs);
    const joinB = await clientB.readNextPacket(options.timeoutMs);

    const decodedJoinA = decodeByMessageType(joinA.messageType, joinA.body);
    const decodedJoinB = decodeByMessageType(joinB.messageType, joinB.body);

    if (!decodedJoinA.ok) throw new Error(`clientA join failed: ${decodedJoinA.errorCode}`);
    if (!decodedJoinB.ok) throw new Error(`clientB join failed: ${decodedJoinB.errorCode}`);

    console.log("Both clients joined room:", roomId);

    // Drain state pushes
    for (let i = 0; i < 3; i++) {
      clientA.readNextPacket(100).catch(() => {});
      clientB.readNextPacket(100).catch(() => {});
    }

    // Both ready up
    await clientA.send(MESSAGE_TYPE.ROOM_READY_REQ, 3, encodeRoomReadyReq(true));
    await clientB.send(MESSAGE_TYPE.ROOM_READY_REQ, 3, encodeRoomReadyReq(true));

    await clientA.readNextPacket(options.timeoutMs);
    await clientB.readNextPacket(options.timeoutMs);
    await clientA.readNextPacket(options.timeoutMs);
    await clientB.readNextPacket(options.timeoutMs);

    // Start game
    await clientA.send(MESSAGE_TYPE.ROOM_START_REQ, 4, encodeRoomStartReq());
    const startRes = await clientA.readNextPacket(options.timeoutMs);
    const decodedStart = decodeByMessageType(startRes.messageType, startRes.body);
    if (!decodedStart.ok) throw new Error(`room start failed: ${decodedStart.errorCode}`);

    // Drain game started pushes
    await clientA.readNextPacket(options.timeoutMs);
    await clientB.readNextPacket(options.timeoutMs);

    console.log("Game started! Waiting for initial snapshots...");

    // Wait for initial snapshots from both clients
    let snapA = null, snapB = null;
    const initDeadline = Date.now() + 5000;
    while (Date.now() < initDeadline && (!snapA || !snapB)) {
      try {
        const pktA = await clientA.readNextPacket(500);
        if (pktA && pktA.messageType === MESSAGE_TYPE.MOVEMENT_SNAPSHOT_PUSH) {
          snapA = decodeByMessageType(pktA.messageType, pktA.body);
        }
      } catch {}
      try {
        const pktB = await clientB.readNextPacket(500);
        if (pktB && pktB.messageType === MESSAGE_TYPE.MOVEMENT_SNAPSHOT_PUSH) {
          snapB = decodeByMessageType(pktB.messageType, pktB.body);
        }
      } catch {}
    }

    if (snapA) console.log("\n" + formatSnapshot("[clientA.snapshot]", snapA));
    if (snapB) console.log("\n" + formatSnapshot("[clientB.snapshot]", snapB));

    // Track frame ID for movement inputs
    let nextFrameId = 1;
    let inputSeq = 100;

    // Helper: send movement from clientA
    async function sendMoveFromA(dirX, dirY) {
      await clientA.send(
        MESSAGE_TYPE.MOVE_INPUT_REQ,
        inputSeq++,
        encodeMoveInputReq(nextFrameId++, MOVE_INPUT_TYPE.MOVE_DIR, dirX, dirY)
      );
      try {
        await clientA.readUntil(options.timeoutMs,
          (p) => p.messageType === MESSAGE_TYPE.MOVE_INPUT_RES && p.seq === inputSeq - 1,
          "moveRes"
        );
      } catch {}
    }

    // Helper: send stop from clientA
    async function sendStopFromA() {
      await clientA.send(
        MESSAGE_TYPE.MOVE_INPUT_REQ,
        inputSeq++,
        encodeMoveInputReq(nextFrameId++, MOVE_INPUT_TYPE.MOVE_STOP, 0, 0)
      );
      try {
        await clientA.readUntil(options.timeoutMs,
          (p) => p.messageType === MESSAGE_TYPE.MOVE_INPUT_RES && p.seq === inputSeq - 1,
          "stopRes"
        );
      } catch {}
    }

    // Background task: read snapshots from clientA
    const clientATask = async () => {
      while (true) {
        try {
          const packet = await clientA.readNextPacket(1000);
          if (!packet) continue;
          if (packet.messageType === MESSAGE_TYPE.MOVEMENT_SNAPSHOT_PUSH) {
            const snap = decodeByMessageType(packet.messageType, packet.body);
            console.log("\n" + formatSnapshot("[clientA received snapshot]", snap));
          }
        } catch {
          // timeout normal, continue
        }
      }
    };

    // Background task: read snapshots from clientB
    const clientBTask = async () => {
      while (true) {
        try {
          const packet = await clientB.readNextPacket(1000);
          if (!packet) continue;
          if (packet.messageType === MESSAGE_TYPE.MOVEMENT_SNAPSHOT_PUSH) {
            const snap = decodeByMessageType(packet.messageType, packet.body);
            console.log("\n" + formatSnapshot("[clientB received snapshot]", snap));
          }
        } catch {
          // timeout normal, continue
        }
      }
    };

    // Start background tasks
    clientATask();
    clientBTask();

    // Interactive input loop
    const rl = readline.createInterface({
      input: process.stdin,
      output: process.stdout
    });

    console.log("\n[Ready for input - type direction and press Enter]");
    console.log("[clientA is the one you control, clientB will auto-move periodically]");

    // Auto-move clientB periodically for demonstration
    let autoMoveCount = 0;
    const autoMoveTask = async () => {
      while (true) {
        await new Promise((resolve) => setTimeout(resolve, 3000));
        if (autoMoveCount < 3) {
          // ClientB does a simple move right
          await clientB.send(
            MESSAGE_TYPE.MOVE_INPUT_REQ,
            200 + autoMoveCount,
            encodeMoveInputReq(autoMoveCount + 1, MOVE_INPUT_TYPE.MOVE_DIR, 1, 0)
          );
          try {
            await clientB.readUntil(options.timeoutMs,
              (p) => p.messageType === MESSAGE_TYPE.MOVE_INPUT_RES,
              "autoMoveRes"
            );
          } catch {}
          autoMoveCount++;
        } else if (autoMoveCount === 3) {
          // Stop clientB
          await clientB.send(
            MESSAGE_TYPE.MOVE_INPUT_REQ,
            210,
            encodeMoveInputReq(10, MOVE_INPUT_TYPE.MOVE_STOP, 0, 0)
          );
          autoMoveCount++;
        }
      }
    };

    autoMoveTask();

    const askQuestion = async () => {
      try {
        const answer = await rl.question("> ");
        const trimmed = answer.trim().toLowerCase();

        if (trimmed === "q" || trimmed === "quit" || trimmed === "exit") {
          console.log("Quitting...");
          rl.close();
          return;
        }

        if (trimmed === " " || trimmed === "space" || trimmed === "stop") {
          await sendStopFromA();
          console.log("[clientA sent: STOP]");
        } else if (DIRECTION_MAP[answer.trim()]) {
          const dir = DIRECTION_MAP[answer.trim()];
          await sendMoveFromA(dir.x, dir.y);
          console.log(`[clientA sent: MOVE (${dir.x}, ${dir.y})]`);
        } else if (trimmed) {
          console.log("Unknown command. Use: w/a/s/d or arrow keys to move, space to stop, q to quit");
        }

        askQuestion();
      } catch {
        // Input closed
      }
    };

    askQuestion();

    // Keep running until interrupted
    await new Promise(() => {});
  } finally {
    clientA.close();
    clientB.close();
  }
}
