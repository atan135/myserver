import { SCENARIO } from "./constants.js";

function collectOptionValue(argv, startIndex) {
  const valueIndex = startIndex + 1;
  return {
    nextIndex: valueIndex,
    value: argv[valueIndex] || ""
  };
}

function collectJsonLikeOptionValue(argv, startIndex) {
  const valueIndex = startIndex + 1;
  let nextIndex = valueIndex;
  let value = argv[valueIndex] || "";

  while (nextIndex + 1 < argv.length && !argv[nextIndex + 1].startsWith("--")) {
    value += argv[nextIndex + 1];
    nextIndex += 1;
  }

  return {
    nextIndex,
    value
  };
}

const DEFAULT_OPTIONS = {
  host: "127.0.0.1",
  port: 7000,
  chatPort: 9001,
  httpBaseUrl: "http://127.0.0.1:3000",
  announceBaseUrl: "http://127.0.0.1:9004",
  mailBaseUrl: "http://127.0.0.1:9003",
  roomId: "room-default",
  guestId: "",
  loginName: "",
  password: "",
  loginNameA: "",
  passwordA: "",
  loginNameB: "",
  passwordB: "",
  ticket: "",
  timeoutMs: 5000,
  scenario: SCENARIO.HAPPY,
  maxBodyLen: 4096,
  idStart: 1000,
  idEnd: 1000,
  targetId: "",
  groupId: "",
  content: "Hello from mock-client!",
  groupName: "",
  limit: 20,
  beforeTime: 0,
  matchId: "",
  playerIds: [],
  mode: "1v1",
  policyId: "",
  moveFrames: [1, 2, 3, 4, 5],
  combatSkillId: 2,
  // Mail parameters
  mailId: "",
  mailPlayerId: "",
  mailToPlayerId: "",
  mailStatus: "",
  mailOffset: 0,
  mailTitle: "Mock mail from mock-client",
  mailContent: "Hello from mock-client mail!",
  mailType: "system",
  senderType: "system",
  senderId: "system",
  senderName: "系统",
  createdByType: "script",
  createdById: "mock-client",
  createdByName: "mock-client",
  attachmentsJson: "",
  mailWatchSeconds: 15,
  // Announcement parameters
  announceId: "",
  announceLocale: "",
  announcePriority: "",
  announceType: "",
  announceTargetGroup: "",
  announceOffset: 0,
  announceTitle: "",
  announceContent: "",
  announceStartTime: "",
  announceEndTime: "",
  announceDurationSeconds: "",
  announceActiveOnly: true,
  // Inventory parameters
  itemUid: 0,
  equipSlot: "",
  useItemUid: 0,
  discardUid: 0,
  discardCount: 0,
  depositUid: 0,
  depositCount: 0,
  warehouseAction: "deposit",
  addItemId: 0,
  addCount: 1,
  addBinded: false
};

/**
 * Parse command line arguments into options object
 * @param {string[]} argv - Process.argv slice
 * @returns {Object} Parsed options
 */
export function parseArgs(argv) {
  const result = { ...DEFAULT_OPTIONS };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];

    switch (arg) {
      case "--host":
        ({ value: result.host, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--port":
        result.port = Number.parseInt(collectOptionValue(argv, index).value, 10);
        index = collectOptionValue(argv, index).nextIndex;
        break;
      case "--chat-port":
        result.chatPort = Number.parseInt(collectOptionValue(argv, index).value, 10);
        index = collectOptionValue(argv, index).nextIndex;
        break;
      case "--http-base-url":
        ({ value: result.httpBaseUrl, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--announce-base-url":
        ({ value: result.announceBaseUrl, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--mail-base-url":
        ({ value: result.mailBaseUrl, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--room-id":
        ({ value: result.roomId, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--guest-id":
        ({ value: result.guestId, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--login-name":
        ({ value: result.loginName, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--password":
        ({ value: result.password, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--login-name-a":
        ({ value: result.loginNameA, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--password-a":
        ({ value: result.passwordA, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--login-name-b":
        ({ value: result.loginNameB, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--password-b":
        ({ value: result.passwordB, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--ticket":
        ({ value: result.ticket, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--timeout-ms":
        result.timeoutMs = Number.parseInt(collectOptionValue(argv, index).value, 10);
        index = collectOptionValue(argv, index).nextIndex;
        break;
      case "--scenario":
        ({ value: result.scenario, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--max-body-len":
        result.maxBodyLen = Number.parseInt(collectOptionValue(argv, index).value, 10);
        index = collectOptionValue(argv, index).nextIndex;
        break;
      case "--id-start":
        result.idStart = Number.parseInt(collectOptionValue(argv, index).value, 10);
        index = collectOptionValue(argv, index).nextIndex;
        break;
      case "--id-end":
        result.idEnd = Number.parseInt(collectOptionValue(argv, index).value, 10);
        index = collectOptionValue(argv, index).nextIndex;
        break;
      case "--target-id":
        ({ value: result.targetId, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--group-id":
        ({ value: result.groupId, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--content":
        ({ value: result.content, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--group-name":
        ({ value: result.groupName, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--limit":
        result.limit = Number.parseInt(collectOptionValue(argv, index).value, 10);
        index = collectOptionValue(argv, index).nextIndex;
        break;
      case "--before-time":
        result.beforeTime = Number.parseInt(collectOptionValue(argv, index).value, 10);
        index = collectOptionValue(argv, index).nextIndex;
        break;
      case "--match-id":
        ({ value: result.matchId, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--player-ids":
        result.playerIds = collectOptionValue(argv, index).value.split(",");
        index = collectOptionValue(argv, index).nextIndex;
        break;
      case "--mode":
        ({ value: result.mode, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--policy-id":
        ({ value: result.policyId, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--move-frames":
        result.moveFrames = collectOptionValue(argv, index).value
          .split(",")
          .map((value) => Number.parseInt(value, 10))
          .filter((value) => Number.isFinite(value) && value > 0);
        index = collectOptionValue(argv, index).nextIndex;
        break;
      case "--combat-skill-id":
        result.combatSkillId = Number.parseInt(collectOptionValue(argv, index).value, 10);
        index = collectOptionValue(argv, index).nextIndex;
        break;
      // Mail arguments
      case "--mail-id":
        ({ value: result.mailId, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--mail-player-id":
        ({ value: result.mailPlayerId, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--mail-to-player-id":
        ({ value: result.mailToPlayerId, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--mail-status":
        ({ value: result.mailStatus, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--mail-offset":
        result.mailOffset = Number.parseInt(collectOptionValue(argv, index).value, 10);
        index = collectOptionValue(argv, index).nextIndex;
        break;
      case "--mail-title":
        ({ value: result.mailTitle, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--mail-content":
        ({ value: result.mailContent, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--mail-type":
        ({ value: result.mailType, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--sender-type":
        ({ value: result.senderType, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--sender-id":
        ({ value: result.senderId, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--sender-name":
        ({ value: result.senderName, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--created-by-type":
        ({ value: result.createdByType, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--created-by-id":
        ({ value: result.createdById, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--created-by-name":
        ({ value: result.createdByName, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--attachments-json":
        ({ value: result.attachmentsJson, nextIndex: index } = collectJsonLikeOptionValue(argv, index));
        break;
      case "--mail-watch-seconds":
        result.mailWatchSeconds = Number.parseInt(collectOptionValue(argv, index).value, 10);
        index = collectOptionValue(argv, index).nextIndex;
        break;
      // Announcement arguments
      case "--announce-id":
        ({ value: result.announceId, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--announce-locale":
        ({ value: result.announceLocale, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--announce-priority":
        ({ value: result.announcePriority, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--announce-type":
        ({ value: result.announceType, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--announce-target-group":
        ({ value: result.announceTargetGroup, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--announce-offset":
        result.announceOffset = Number.parseInt(collectOptionValue(argv, index).value, 10);
        index = collectOptionValue(argv, index).nextIndex;
        break;
      case "--announce-title":
        ({ value: result.announceTitle, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--announce-content":
        ({ value: result.announceContent, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--announce-start-time":
        ({ value: result.announceStartTime, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--announce-end-time":
        ({ value: result.announceEndTime, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--announce-duration-seconds":
        ({ value: result.announceDurationSeconds, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--announce-active-only":
        result.announceActiveOnly =
          collectOptionValue(argv, index).value !== "false" &&
          collectOptionValue(argv, index).value !== "0";
        index = collectOptionValue(argv, index).nextIndex;
        break;
      // Inventory arguments
      case "--item-uid":
        result.itemUid = Number.parseInt(collectOptionValue(argv, index).value, 10);
        index = collectOptionValue(argv, index).nextIndex;
        break;
      case "--equip-slot":
        ({ value: result.equipSlot, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--use-item-uid":
        result.useItemUid = Number.parseInt(collectOptionValue(argv, index).value, 10);
        index = collectOptionValue(argv, index).nextIndex;
        break;
      case "--discard-uid":
        result.discardUid = Number.parseInt(collectOptionValue(argv, index).value, 10);
        index = collectOptionValue(argv, index).nextIndex;
        break;
      case "--discard-count":
        result.discardCount = Number.parseInt(collectOptionValue(argv, index).value, 10);
        index = collectOptionValue(argv, index).nextIndex;
        break;
      case "--deposit-uid":
        result.depositUid = Number.parseInt(collectOptionValue(argv, index).value, 10);
        index = collectOptionValue(argv, index).nextIndex;
        break;
      case "--deposit-count":
        result.depositCount = Number.parseInt(collectOptionValue(argv, index).value, 10);
        index = collectOptionValue(argv, index).nextIndex;
        break;
      case "--warehouse-action":
        ({ value: result.warehouseAction, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--add-item-id":
        result.addItemId = Number.parseInt(collectOptionValue(argv, index).value, 10);
        index = collectOptionValue(argv, index).nextIndex;
        break;
      case "--add-count":
        result.addCount = Number.parseInt(collectOptionValue(argv, index).value, 10);
        index = collectOptionValue(argv, index).nextIndex;
        break;
      case "--add-binded":
        result.addBinded =
          collectOptionValue(argv, index).value === "true" ||
          collectOptionValue(argv, index).value === "1";
        index = collectOptionValue(argv, index).nextIndex;
        break;
    }
  }

  return result;
}
