import readline from "node:readline/promises";
import process from "node:process";
import { MESSAGE_TYPE } from "../constants.js";
import { encodeChatPrivateReq } from "../messages.js";
import { fetchTicket } from "../auth.js";
import { connectToChatServer } from "./chat.js";
import { authenticateClient } from "./room.js";
import { decodeByMessageType } from "../messages.js";

/**
 * Interactive chat: two clients, user types messages in terminal
 */
export async function runChatInteractive(options) {
  const loginA = await fetchTicket(options, { guestId: `${options.roomId}-owner` });
  const loginB = await fetchTicket(options, { guestId: `${options.roomId}-member` });

  console.log("clientA.playerId:", loginA.playerId);
  console.log("clientB.playerId:", loginB.playerId);
  console.log("");
  console.log("=== Interactive Chat ===");
  console.log("clientA (you) <---> clientB");
  console.log("Type messages and press Enter to send from clientA to clientB");
  console.log("clientB will auto-reply with your message prefixed with 'B: '");
  console.log("Press Ctrl+C to exit");
  console.log("");

  // Client A connects - this is "us"
  const clientA = await connectToChatServer(options);
  const { encodeChatAuthReq } = await import("../messages.js");
  await authenticateClient(clientA, options, loginA, 1, encodeChatAuthReq);
  console.log("[connected as clientA, waiting for messages...]");

  // Client B connects - this is "other player"
  const clientB = await connectToChatServer(options);
  await authenticateClient(clientB, options, loginB, 1, encodeChatAuthReq);
  console.log("[clientB connected]");

  // Create readline interface for interactive input
  const rl = readline.createInterface({
    input: process.stdin,
    output: process.stdout
  });

  let seq = 2;
  const clientBPlayerId = loginB.playerId;
  let replyingToSeq = 1;

  // Helper to send message from clientA to clientB
  async function sendMessage(content) {
    await clientA.send(MESSAGE_TYPE.CHAT_PRIVATE_REQ, seq, encodeChatPrivateReq(clientBPlayerId, content));
    seq++;
  }

  // Task to handle clientB receiving messages and auto-reply
  const clientBTask = async () => {
    while (true) {
      try {
        const packet = await clientB.readNextPacket(60000);
        const decoded = decodeByMessageType(packet.messageType, packet.body);

        if (packet.messageType === MESSAGE_TYPE.CHAT_PUSH) {
          console.log(`\n[clientB received from ${decoded.senderId}]: ${decoded.content}`);

          // Auto reply
          const replyContent = `B: ${decoded.content}`;
          await clientB.send(MESSAGE_TYPE.CHAT_PRIVATE_REQ, replyingToSeq, encodeChatPrivateReq(decoded.senderId, replyContent));
          replyingToSeq++;
        } else if (packet.messageType === MESSAGE_TYPE.MAIL_NOTIFY_PUSH) {
          console.log(`\n[clientB mail notification]: mailId=${decoded.mailId}, title="${decoded.title}", from=${decoded.fromPlayerId}`);
        }
      } catch (e) {
        // Timeout is normal, just continue
        if (!e.message.includes("Timed out")) {
          console.error("clientB read error:", e.message);
        }
      }
    }
  };

  // Task to handle clientA receiving messages
  const clientATask = async () => {
    while (true) {
      try {
        const packet = await clientA.readNextPacket(60000);
        const decoded = decodeByMessageType(packet.messageType, packet.body);

        if (packet.messageType === MESSAGE_TYPE.CHAT_PUSH) {
          console.log(`\n[received from ${decoded.senderId}]: ${decoded.content}`);
        } else if (packet.messageType === MESSAGE_TYPE.MAIL_NOTIFY_PUSH) {
          console.log(`\n[mail notification]: mailId=${decoded.mailId}, title="${decoded.title}", from=${decoded.fromPlayerId}`);
        }
      } catch (e) {
        // Timeout is normal, just continue
        if (!e.message.includes("Timed out")) {
          console.error("clientA read error:", e.message);
        }
      }
    }
  };

  // Start both tasks in background
  clientBTask();
  clientATask();

  // Main input loop
  const askQuestion = async () => {
    try {
      const answer = await rl.question("> ");
      if (answer.trim()) {
        await sendMessage(answer.trim());
      }
      askQuestion();
    } catch {
      // Input closed, exit
    }
  };

  askQuestion();

  // Keep running until interrupted
  await new Promise(() => {});
}
