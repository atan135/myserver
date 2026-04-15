import { SCENARIO } from "./constants.js";

const DEFAULT_OPTIONS = {
  host: "127.0.0.1",
  port: 7000,
  chatPort: 9001,
  httpBaseUrl: "http://127.0.0.1:3000",
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
    const next = argv[index + 1];

    if (!next) continue;

    switch (arg) {
      case "--host":
        result.host = next;
        index += 1;
        break;
      case "--port":
        result.port = Number.parseInt(next, 10);
        index += 1;
        break;
      case "--chat-port":
        result.chatPort = Number.parseInt(next, 10);
        index += 1;
        break;
      case "--http-base-url":
        result.httpBaseUrl = next;
        index += 1;
        break;
      case "--room-id":
        result.roomId = next;
        index += 1;
        break;
      case "--guest-id":
        result.guestId = next;
        index += 1;
        break;
      case "--login-name":
        result.loginName = next;
        index += 1;
        break;
      case "--password":
        result.password = next;
        index += 1;
        break;
      case "--login-name-a":
        result.loginNameA = next;
        index += 1;
        break;
      case "--password-a":
        result.passwordA = next;
        index += 1;
        break;
      case "--login-name-b":
        result.loginNameB = next;
        index += 1;
        break;
      case "--password-b":
        result.passwordB = next;
        index += 1;
        break;
      case "--ticket":
        result.ticket = next;
        index += 1;
        break;
      case "--timeout-ms":
        result.timeoutMs = Number.parseInt(next, 10);
        index += 1;
        break;
      case "--scenario":
        result.scenario = next;
        index += 1;
        break;
      case "--max-body-len":
        result.maxBodyLen = Number.parseInt(next, 10);
        index += 1;
        break;
      case "--id-start":
        result.idStart = Number.parseInt(next, 10);
        index += 1;
        break;
      case "--id-end":
        result.idEnd = Number.parseInt(next, 10);
        index += 1;
        break;
      case "--target-id":
        result.targetId = next;
        index += 1;
        break;
      case "--group-id":
        result.groupId = next;
        index += 1;
        break;
      case "--content":
        result.content = next;
        index += 1;
        break;
      case "--group-name":
        result.groupName = next;
        index += 1;
        break;
      case "--limit":
        result.limit = Number.parseInt(next, 10);
        index += 1;
        break;
      case "--before-time":
        result.beforeTime = Number.parseInt(next, 10);
        index += 1;
        break;
      case "--match-id":
        result.matchId = next;
        index += 1;
        break;
      case "--player-ids":
        result.playerIds = next.split(",");
        index += 1;
        break;
      case "--mode":
        result.mode = next;
        index += 1;
        break;
      case "--policy-id":
        result.policyId = next;
        index += 1;
        break;
      case "--move-frames":
        result.moveFrames = next
          .split(",")
          .map((value) => Number.parseInt(value, 10))
          .filter((value) => Number.isFinite(value) && value > 0);
        index += 1;
        break;
      // Inventory arguments
      case "--item-uid":
        result.itemUid = Number.parseInt(next, 10);
        index += 1;
        break;
      case "--equip-slot":
        result.equipSlot = next;
        index += 1;
        break;
      case "--use-item-uid":
        result.useItemUid = Number.parseInt(next, 10);
        index += 1;
        break;
      case "--discard-uid":
        result.discardUid = Number.parseInt(next, 10);
        index += 1;
        break;
      case "--discard-count":
        result.discardCount = Number.parseInt(next, 10);
        index += 1;
        break;
      case "--deposit-uid":
        result.depositUid = Number.parseInt(next, 10);
        index += 1;
        break;
      case "--deposit-count":
        result.depositCount = Number.parseInt(next, 10);
        index += 1;
        break;
      case "--warehouse-action":
        result.warehouseAction = next;
        index += 1;
        break;
      case "--add-item-id":
        result.addItemId = Number.parseInt(next, 10);
        index += 1;
        break;
      case "--add-count":
        result.addCount = Number.parseInt(next, 10);
        index += 1;
        break;
      case "--add-binded":
        result.addBinded = next === "true" || next === "1";
        index += 1;
        break;
    }
  }

  return result;
}
