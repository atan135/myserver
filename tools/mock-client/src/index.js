import { parseArgs } from "./args.js";
import { SCENARIO } from "./constants.js";
import {
  fetchTicket,
  formatLoginSummary,
  runLogout,
  runKickSession,
  runPasswordTicketRevoke
} from "./auth.js";
import { TcpProtocolClient } from "./client.js";
import {
  MESSAGE_TYPE,
  // Room scenarios
  runHappyPath,
  runGetRoomData,
  runGetRoomDataInRoom,
  runStartGameSingleClient,
  runTwoClientRoom,
  runStartGameReadyRoom,
  runInvalidTicket,
  runUnauthRoomJoin,
  runUnknownMessage,
  runOversizedRoomJoin,
  runReconnect,
  runReconnectAllDisconnected,
  runDrainNewRoomRejected,
  runDrainExistingRoomJoin,
  runDrainExistingRoomReconnect,
  runDrainExistingRoomObserver,
  runDrainCreateMatchedRoomRejected,
  runRolloutDrainStatus,
  runRequestServerShutdown,
  runServerRedirectListen,
  runServerRedirectReconnect,
  runServerRedirectTransferReconnect,
  runCreateMatchedRoom,
  runCreateMatchedRoomAndJoin,
  // Game scenarios
  runGameplayRoundtrip,
  runCombatDualClient,
  runRobotSyncRoom,
  // Movement scenarios
  runMovementDemo,
  runMovementSyncValidation,
  runMovementDualClientSync,
  runMovementSnapshotThrottle,
  runMovementFaceTo,
  runMovementAuthoritativeCorrection,
  runMovementReconnectRecovery,
  runMovementInteractive,
  // Chat scenarios
  runChatPrivate,
  runChatGroup,
  runGroupCreate,
  runGroupList,
  runChatHistory,
  runChatTwoClient,
  runChatPrivateTwoClient,
  runChatInteractive,
  // Mail scenarios
  runMailSend,
  runMailList,
  runMailGet,
  runMailRead,
  runMailClaim,
  runMailSendAndNotify,
  // Announcement scenarios
  runAnnounceList,
  runAnnounceGet,
  runAnnounceCreate,
  runAnnounceUpdate,
  runAnnounceDelete,
  // Inventory scenarios
  runInventoryEquip,
  runInventoryUse,
  runInventoryDiscard,
  runInventoryWarehouse,
  runGetInventory,
  runInventoryFull,
  // Character scenarios
  runCharacterList,
  runCharacterCreate,
  runCharacterSelect,
  runCharacterProfile,
  runCharacterDelete,
  runCharacterRestore,
  runCharacterLoginAuth,
  runCharacterRoomJoin,
  runCharacterElementsDebug,
  runCharacterTitlesDebug,
  runCharacterDisciplinesDebug,
  runCharacterDisciplineLearn,
  runCharacterDisciplineActivate,
  runCharacterDisciplineDeactivate,
  runCharacterDisciplineSwitch,
  runCharacterDisciplinePoints,
  runCharacterProgressApply,
  runCharacterRoleSystemCheck,
  runAdminCharacterReadonlyCheck,
  runCharacterDuplicateName,
  runCharacterLimit
} from "./scenarios/index.js";

async function main() {
  const options = parseArgs(process.argv.slice(2));

  // Multi-client scenarios (handled separately)
  if (options.scenario === SCENARIO.TWO_CLIENT_ROOM) {
    await runTwoClientRoom(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.START_GAME_READY_ROOM) {
    await runStartGameReadyRoom(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.GAMEPLAY_ROUNDTRIP) {
    await runGameplayRoundtrip(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.COMBAT_DUAL_CLIENT) {
    await runCombatDualClient(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.ROBOT_SYNC_ROOM) {
    await runRobotSyncRoom(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.MOVEMENT_DEMO) {
    const login = await fetchTicket(options);
    console.log("login:", JSON.stringify(formatLoginSummary(login), null, 2));

    const client = new TcpProtocolClient(options, "client");
    await client.connect();
    try {
      await runMovementDemo(client, options, login);
      console.log(`scenario completed: ${options.scenario}`);
    } finally {
      client.close();
    }
    return;
  }

  if (options.scenario === SCENARIO.MOVEMENT_SYNC_VALIDATION) {
    await runMovementSyncValidation(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.MOVEMENT_DUAL_CLIENT_SYNC) {
    await runMovementDualClientSync(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.MOVEMENT_SNAPSHOT_THROTTLE) {
    await runMovementSnapshotThrottle(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.MOVEMENT_FACE_TO) {
    await runMovementFaceTo(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.MOVEMENT_AUTHORITATIVE_CORRECTION) {
    await runMovementAuthoritativeCorrection(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.MOVEMENT_RECONNECT_RECOVERY) {
    await runMovementReconnectRecovery(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.MOVEMENT_INTERACTIVE) {
    await runMovementInteractive(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.RECONNECT) {
    await runReconnect(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.RECONNECT_ALL_DISCONNECTED) {
    await runReconnectAllDisconnected(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.DRAIN_NEW_ROOM_REJECTED) {
    await runDrainNewRoomRejected(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.DRAIN_EXISTING_ROOM_JOIN) {
    await runDrainExistingRoomJoin(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.DRAIN_EXISTING_ROOM_RECONNECT) {
    await runDrainExistingRoomReconnect(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.DRAIN_EXISTING_ROOM_OBSERVER) {
    await runDrainExistingRoomObserver(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.DRAIN_CREATE_MATCHED_ROOM_REJECTED) {
    await runDrainCreateMatchedRoomRejected(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.ROLLOUT_DRAIN_STATUS) {
    await runRolloutDrainStatus(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.REQUEST_SERVER_SHUTDOWN) {
    const result = await runRequestServerShutdown(options);
    if (options.jsonOutput) {
      if (!result.ok) {
        process.exitCode = 1;
      }
      return;
    }
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.SERVER_REDIRECT_LISTEN) {
    await runServerRedirectListen(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.SERVER_REDIRECT_RECONNECT) {
    await runServerRedirectReconnect(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.SERVER_REDIRECT_TRANSFER_RECONNECT) {
    await runServerRedirectTransferReconnect(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.MAIL_SEND) {
    await runMailSend(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.MAIL_LIST) {
    await runMailList(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.MAIL_GET) {
    await runMailGet(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.MAIL_READ) {
    await runMailRead(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.MAIL_CLAIM) {
    await runMailClaim(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.MAIL_SEND_AND_NOTIFY) {
    await runMailSendAndNotify(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.ANNOUNCE_LIST) {
    await runAnnounceList(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.ANNOUNCE_GET) {
    await runAnnounceGet(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.ANNOUNCE_CREATE) {
    await runAnnounceCreate(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.ANNOUNCE_UPDATE) {
    await runAnnounceUpdate(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.ANNOUNCE_DELETE) {
    await runAnnounceDelete(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.LOGOUT) {
    await runLogout(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.KICK_SESSION) {
    await runKickSession(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.PASSWORD_TICKET_REVOKE) {
    await runPasswordTicketRevoke(options);
    console.log(`scenario completed: ${options.scenario}`);
    return;
  }

  if (options.scenario === SCENARIO.CHARACTER_LIST) {
    const result = await runCharacterList(options);
    if (options.jsonOutput && !result.ok) {
      process.exitCode = 1;
      return;
    }
    if (!options.jsonOutput) {
      console.log(`scenario completed: ${options.scenario}`);
    }
    return;
  }

  if (options.scenario === SCENARIO.CHARACTER_CREATE) {
    const result = await runCharacterCreate(options);
    if (options.jsonOutput && !result.ok) {
      process.exitCode = 1;
      return;
    }
    if (!options.jsonOutput) {
      console.log(`scenario completed: ${options.scenario}`);
    }
    return;
  }

  if (options.scenario === SCENARIO.CHARACTER_SELECT) {
    const result = await runCharacterSelect(options);
    if (options.jsonOutput && !result.ok) {
      process.exitCode = 1;
      return;
    }
    if (!options.jsonOutput) {
      console.log(`scenario completed: ${options.scenario}`);
    }
    return;
  }

  if (options.scenario === SCENARIO.CHARACTER_PROFILE) {
    const result = await runCharacterProfile(options);
    if (options.jsonOutput && !result.ok) {
      process.exitCode = 1;
      return;
    }
    if (!options.jsonOutput) {
      console.log(`scenario completed: ${options.scenario}`);
    }
    return;
  }

  if (options.scenario === SCENARIO.CHARACTER_DELETE) {
    const result = await runCharacterDelete(options);
    if (options.jsonOutput && !result.ok) {
      process.exitCode = 1;
      return;
    }
    if (!options.jsonOutput) {
      console.log(`scenario completed: ${options.scenario}`);
    }
    return;
  }

  if (options.scenario === SCENARIO.CHARACTER_RESTORE) {
    const result = await runCharacterRestore(options);
    if (options.jsonOutput && !result.ok) {
      process.exitCode = 1;
      return;
    }
    if (!options.jsonOutput) {
      console.log(`scenario completed: ${options.scenario}`);
    }
    return;
  }

  if (options.scenario === SCENARIO.CHARACTER_DUPLICATE_NAME) {
    const result = await runCharacterDuplicateName(options);
    if (options.jsonOutput && !result.ok) {
      process.exitCode = 1;
      return;
    }
    if (!options.jsonOutput) {
      console.log(`scenario completed: ${options.scenario}`);
    }
    return;
  }

  if (options.scenario === SCENARIO.CHARACTER_LIMIT) {
    const result = await runCharacterLimit(options);
    if (!result.ok) {
      process.exitCode = 1;
    }
    if (!options.jsonOutput) {
      console.log(`scenario completed: ${options.scenario}`);
    }
    return;
  }

  if (options.scenario === SCENARIO.CHARACTER_LOGIN_AUTH) {
    const result = await runCharacterLoginAuth(options);
    if (options.jsonOutput && !result.ok) {
      process.exitCode = 1;
      return;
    }
    if (!options.jsonOutput) {
      console.log(`scenario completed: ${options.scenario}`);
    }
    return;
  }

  if (options.scenario === SCENARIO.CHARACTER_ROOM_JOIN) {
    const result = await runCharacterRoomJoin(options);
    if (options.jsonOutput && !result.ok) {
      process.exitCode = 1;
      return;
    }
    if (!options.jsonOutput) {
      console.log(`scenario completed: ${options.scenario}`);
    }
    return;
  }

  if (options.scenario === SCENARIO.CHARACTER_ELEMENTS_DEBUG) {
    const result = await runCharacterElementsDebug(options);
    if (!result.ok) {
      process.exitCode = 1;
    }
    if (!options.jsonOutput) {
      console.log(`scenario completed: ${options.scenario}`);
    }
    return;
  }

  if (options.scenario === SCENARIO.CHARACTER_TITLES_DEBUG) {
    const result = await runCharacterTitlesDebug(options);
    if (!result.ok) {
      process.exitCode = 1;
    }
    if (!options.jsonOutput) {
      console.log(`scenario completed: ${options.scenario}`);
    }
    return;
  }

  if (options.scenario === SCENARIO.CHARACTER_DISCIPLINES_DEBUG) {
    const result = await runCharacterDisciplinesDebug(options);
    if (!result.ok) {
      process.exitCode = 1;
    }
    if (!options.jsonOutput) {
      console.log(`scenario completed: ${options.scenario}`);
    }
    return;
  }

  if (options.scenario === SCENARIO.CHARACTER_DISCIPLINE_LEARN) {
    const result = await runCharacterDisciplineLearn(options);
    if (!result.ok) {
      process.exitCode = 1;
    }
    if (!options.jsonOutput) {
      console.log(`scenario completed: ${options.scenario}`);
    }
    return;
  }

  if (options.scenario === SCENARIO.CHARACTER_DISCIPLINE_ACTIVATE) {
    const result = await runCharacterDisciplineActivate(options);
    if (!result.ok) {
      process.exitCode = 1;
    }
    if (!options.jsonOutput) {
      console.log(`scenario completed: ${options.scenario}`);
    }
    return;
  }

  if (options.scenario === SCENARIO.CHARACTER_DISCIPLINE_DEACTIVATE) {
    const result = await runCharacterDisciplineDeactivate(options);
    if (!result.ok) {
      process.exitCode = 1;
    }
    if (!options.jsonOutput) {
      console.log(`scenario completed: ${options.scenario}`);
    }
    return;
  }

  if (options.scenario === SCENARIO.CHARACTER_DISCIPLINE_SWITCH) {
    const result = await runCharacterDisciplineSwitch(options);
    if (!result.ok) {
      process.exitCode = 1;
    }
    if (!options.jsonOutput) {
      console.log(`scenario completed: ${options.scenario}`);
    }
    return;
  }

  if (options.scenario === SCENARIO.CHARACTER_DISCIPLINE_POINTS) {
    const result = await runCharacterDisciplinePoints(options);
    if (!result.ok) {
      process.exitCode = 1;
    }
    if (!options.jsonOutput) {
      console.log(`scenario completed: ${options.scenario}`);
    }
    return;
  }

  if (options.scenario === SCENARIO.CHARACTER_PROGRESS_APPLY) {
    const result = await runCharacterProgressApply(options);
    if (!result.ok) {
      process.exitCode = 1;
    }
    if (!options.jsonOutput) {
      console.log(`scenario completed: ${options.scenario}`);
    }
    return;
  }

  if (options.scenario === SCENARIO.CHARACTER_ROLE_SYSTEM_CHECK) {
    const result = await runCharacterRoleSystemCheck(options);
    if (!result.ok) {
      process.exitCode = 1;
    }
    if (!options.jsonOutput) {
      console.log(`scenario completed: ${options.scenario}`);
    }
    return;
  }

  if (options.scenario === SCENARIO.ADMIN_CHARACTER_READONLY_CHECK) {
    const result = await runAdminCharacterReadonlyCheck(options);
    if (!result.ok) {
      process.exitCode = 1;
    }
    if (!options.jsonOutput) {
      console.log(`scenario completed: ${options.scenario}`);
    }
    return;
  }

  // Determine if login is needed
  const needsLogin = [
    SCENARIO.HAPPY,
    SCENARIO.INVALID_TICKET,
    SCENARIO.UNKNOWN_MESSAGE,
    SCENARIO.OVERSIZED_ROOM_JOIN,
    SCENARIO.START_GAME_SINGLE_CLIENT,
    SCENARIO.GET_ROOM_DATA,
    SCENARIO.GET_ROOM_DATA_IN_ROOM,
    SCENARIO.CREATE_MATCHED_ROOM
  ].includes(options.scenario) || Boolean(options.ticket);
  const login = needsLogin ? await fetchTicket(options) : null;

  if (login) {
    console.log("login:", JSON.stringify(formatLoginSummary(login), null, 2));
  }

  const client = new TcpProtocolClient(options, "client");
  await client.connect();

  try {
    switch (options.scenario) {
      case SCENARIO.HAPPY:
        await runHappyPath(client, options, login);
        break;
      case SCENARIO.INVALID_TICKET:
        await runInvalidTicket(client, options, login);
        break;
      case SCENARIO.UNAUTH_ROOM_JOIN:
        await runUnauthRoomJoin(client, options);
        break;
      case SCENARIO.UNKNOWN_MESSAGE:
        await runUnknownMessage(client, options, login);
        break;
      case SCENARIO.OVERSIZED_ROOM_JOIN:
        await runOversizedRoomJoin(client, options, login);
        break;
      case SCENARIO.START_GAME_SINGLE_CLIENT:
        await runStartGameSingleClient(client, options, login);
        break;
      case SCENARIO.GET_ROOM_DATA:
        await runGetRoomData(client, options, login);
        break;
      case SCENARIO.GET_ROOM_DATA_IN_ROOM:
        await runGetRoomDataInRoom(client, options, login);
        break;
      case SCENARIO.CREATE_MATCHED_ROOM:
        await runCreateMatchedRoom(client, options, login);
        break;
      case SCENARIO.CREATE_MATCHED_ROOM_AND_JOIN:
        await runCreateMatchedRoomAndJoin(options);
        break;
      // Chat scenarios
      case SCENARIO.CHAT_PRIVATE:
        await runChatPrivate(options);
        break;
      case SCENARIO.CHAT_GROUP:
        await runChatGroup(options);
        break;
      case SCENARIO.GROUP_CREATE:
        await runGroupCreate(options);
        break;
      case SCENARIO.GROUP_LIST:
        await runGroupList(options);
        break;
      case SCENARIO.CHAT_HISTORY:
        await runChatHistory(options);
        break;
      case SCENARIO.CHAT_TWO_CLIENT:
        await runChatTwoClient(options);
        break;
      case SCENARIO.CHAT_PRIVATE_TWO_CLIENT:
        await runChatPrivateTwoClient(options);
        break;
      case SCENARIO.CHAT_INTERACTIVE:
        await runChatInteractive(options);
        break;
      // Inventory scenarios
      case SCENARIO.INVENTORY_EQUIP:
        await runInventoryEquip(options);
        break;
      case SCENARIO.INVENTORY_USE:
        await runInventoryUse(options);
        break;
      case SCENARIO.INVENTORY_DISCARD:
        await runInventoryDiscard(options);
        break;
      case SCENARIO.INVENTORY_WAREHOUSE:
        await runInventoryWarehouse(options);
        break;
      case SCENARIO.INVENTORY_GET:
        await runGetInventory(options);
        break;
      case SCENARIO.INVENTORY_FULL:
        await runInventoryFull(options);
        break;
      default:
        throw new Error(`unknown scenario: ${options.scenario}`);
    }

    console.log(`scenario completed: ${options.scenario}`);
  } finally {
    client.close();
  }
}

main().catch((error) => {
  console.error(error.message);
  process.exitCode = 1;
});
