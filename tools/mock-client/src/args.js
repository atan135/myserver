import { SCENARIO } from "./constants.js";
import {
  applyLocalDebugTargetEnvDefaults,
  createDefaultRolloutTargetOptions
} from "./rollout-targets.js";

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
  gameHost: "",
  port: 14000,
  chatHost: "",
  chatPort: 0,
  httpBaseUrl: "http://127.0.0.1:3000",
  announceBaseUrl: "",
  mailBaseUrl: "",
  roomId: "room-default",
  guestId: "",
  loginName: "",
  password: "",
  newPassword: "",
  restorePasswordAfterTest: true,
  loginNameA: "",
  passwordA: "",
  loginNameB: "",
  passwordB: "",
  ticket: "",
  characterId: "",
  characterName: "",
  characterAppearanceJson: "",
  autoCreateCharacter: false,
  createCharacterIfMissing: false,
  characterNamePrefix: "MockRole",
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
  announceAdminToken: process.env.ANNOUNCE_ADMIN_TOKEN || "",
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
  addBinded: false,
  // Internal API token
  serviceToken: "",
  shutdownReason: "mock-client",
  shutdownWaitPid: 0,
  shutdownWaitTimeoutMs: 10000,
  jsonOutput: false,
  useServiceDiscovery: true,
  allowRedirectJoinFallback: false,
  redirectReconnectDelayMs: 0,
  rolloutEpoch: "",
  ...createDefaultRolloutTargetOptions(),
  oldServerId: "game-server-old",
  newServerId: "game-server-new",
  oldAdminToken: process.env.MYSERVER_OLD_GAME_ADMIN_TOKEN || process.env.GAME_ADMIN_TOKEN || "",
  newAdminToken: process.env.MYSERVER_NEW_GAME_ADMIN_TOKEN || process.env.GAME_ADMIN_TOKEN || "",
  proxyAdminToken: process.env.PROXY_ADMIN_TOKEN || "",
  proxyAdminActor: process.env.MYSERVER_PROXY_ADMIN_ACTOR || "mock-client",
  redirectTargetHost: "",
  redirectTargetPort: 0,
  redirectTargetServerId: "",
  redirectTransport: "tcp",
  redirectReason: "rollout_redirect",
  redirectRetryAfterMs: 0
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
      case "--game-host":
        ({ value: result.gameHost, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--chat-host":
        ({ value: result.chatHost, nextIndex: index } = collectOptionValue(argv, index));
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
      case "--new-password":
        ({ value: result.newPassword, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--no-restore-password":
        result.restorePasswordAfterTest = false;
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
      case "--character-id":
        ({ value: result.characterId, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--character-name":
        ({ value: result.characterName, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--character-appearance-json":
        ({ value: result.characterAppearanceJson, nextIndex: index } = collectJsonLikeOptionValue(argv, index));
        break;
      case "--auto-create-character":
        result.autoCreateCharacter = true;
        break;
      case "--create-character-if-missing":
        result.createCharacterIfMissing = true;
        break;
      case "--character-name-prefix":
        ({ value: result.characterNamePrefix, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--no-service-discovery":
        result.useServiceDiscovery = false;
        break;
      case "--allow-redirect-join-fallback":
        result.allowRedirectJoinFallback = true;
        break;
      case "--redirect-reconnect-delay-ms":
        result.redirectReconnectDelayMs = Number.parseInt(collectOptionValue(argv, index).value, 10);
        index = collectOptionValue(argv, index).nextIndex;
        break;
      case "--rollout-epoch":
        ({ value: result.rolloutEpoch, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--old-server-id":
        ({ value: result.oldServerId, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--new-server-id":
        ({ value: result.newServerId, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--old-admin-instance-id":
        ({ value: result.oldAdminInstanceId, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--old-admin-endpoint-name":
        ({ value: result.oldAdminEndpointName, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--old-admin-host":
        ({ value: result.oldAdminHost, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--old-admin-port":
        result.oldAdminPort = Number.parseInt(collectOptionValue(argv, index).value, 10);
        index = collectOptionValue(argv, index).nextIndex;
        break;
      case "--old-admin-token":
        ({ value: result.oldAdminToken, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--new-admin-instance-id":
        ({ value: result.newAdminInstanceId, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--new-admin-endpoint-name":
        ({ value: result.newAdminEndpointName, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--new-admin-host":
        ({ value: result.newAdminHost, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--new-admin-port":
        result.newAdminPort = Number.parseInt(collectOptionValue(argv, index).value, 10);
        index = collectOptionValue(argv, index).nextIndex;
        break;
      case "--new-admin-token":
        ({ value: result.newAdminToken, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--proxy-instance-id":
        ({ value: result.proxyInstanceId, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--proxy-admin-endpoint-name":
        ({ value: result.proxyAdminEndpointName, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--proxy-admin-url":
        ({ value: result.proxyAdminUrl, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--proxy-admin-token":
        ({ value: result.proxyAdminToken, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--proxy-admin-actor":
        ({ value: result.proxyAdminActor, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--registry-url":
        ({ value: result.registryUrl, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--registry-key-prefix":
        ({ value: result.registryKeyPrefix, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--resolved-control-targets":
        result.resolvedControlTargetsInput = true;
        break;
      case "--local-debug-targets":
        result.localDebugTargets = true;
        applyLocalDebugTargetEnvDefaults(result);
        break;
      case "--redirect-target-host":
        ({ value: result.redirectTargetHost, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--redirect-target-port":
        result.redirectTargetPort = Number.parseInt(collectOptionValue(argv, index).value, 10);
        index = collectOptionValue(argv, index).nextIndex;
        break;
      case "--redirect-target-server-id":
        ({ value: result.redirectTargetServerId, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--redirect-transport":
        ({ value: result.redirectTransport, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--redirect-reason":
        ({ value: result.redirectReason, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--redirect-retry-after-ms":
        result.redirectRetryAfterMs = Number.parseInt(collectOptionValue(argv, index).value, 10);
        index = collectOptionValue(argv, index).nextIndex;
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
      case "--announce-admin-token":
        ({ value: result.announceAdminToken, nextIndex: index } = collectOptionValue(argv, index));
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
      case "--service-token":
        ({ value: result.serviceToken, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--shutdown-reason":
        ({ value: result.shutdownReason, nextIndex: index } = collectOptionValue(argv, index));
        break;
      case "--shutdown-wait-pid": {
        const { value, nextIndex } = collectOptionValue(argv, index);
        result.shutdownWaitPid = Number.parseInt(value, 10);
        index = nextIndex;
        break;
      }
      case "--shutdown-wait-timeout-ms": {
        const { value, nextIndex } = collectOptionValue(argv, index);
        result.shutdownWaitTimeoutMs = Number.parseInt(value, 10);
        index = nextIndex;
        break;
      }
      case "--json-output":
        result.jsonOutput = true;
        break;
    }
  }

  return result;
}
