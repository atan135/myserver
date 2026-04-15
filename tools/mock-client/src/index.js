import { parseArgs } from "./args.js";
import { SCENARIO } from "./constants.js";
import { fetchTicket, formatLoginSummary } from "./auth.js";
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
  runCreateMatchedRoom,
  runCreateMatchedRoomAndJoin,
  // Game scenarios
  runGameplayRoundtrip,
  // Movement scenarios
  runMovementDemo,
  runMovementSyncValidation,
  runMovementDualClientSync,
  runMovementSnapshotThrottle,
  runMovementFaceTo,
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
  // Inventory scenarios
  runInventoryEquip,
  runInventoryUse,
  runInventoryDiscard,
  runInventoryWarehouse,
  runInventoryAdd,
  runGetInventory,
  runInventoryFull
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

  // Determine if login is needed
  const needsLogin = [
    SCENARIO.HAPPY,
    SCENARIO.INVALID_TICKET,
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
        await runUnknownMessage(client, options);
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
      case SCENARIO.INVENTORY_ADD:
        await runInventoryAdd(options);
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
