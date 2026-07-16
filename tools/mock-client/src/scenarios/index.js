// Room scenarios
export {
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
  expectErrorPacket,
  printResponse,
  authenticateClient,
  waitForFrameBundle,
  delayBeforeFinalLeave
} from "./room.js";

// Game scenarios
export { runGameplayRoundtrip } from "./game.js";
export { runCombatDualClient } from "./combat.js";
export {
  buildRobotMovePayload,
  expectPlayerInputRejected,
  runRobotSyncRoom,
  waitForRobotMoveFrameBundle
} from "./robot-sync.js";
export {
  runMovementDemo,
  runMovementSyncValidation,
  runMovementDualClientSync,
  runMovementSnapshotThrottle,
  runMovementFaceTo,
  runMovementAuthoritativeCorrection,
  runMovementReconnectRecovery
} from "./movement.js";

// Chat scenarios
export {
  runChatPrivate,
  runChatGroup,
  runGroupCreate,
  runGroupJoin,
  runGroupLeave,
  runGroupDismiss,
  runGroupList,
  runChatHistory,
  runChatTwoClient,
  runChatPrivateTwoClient,
  connectToChatServer
} from "./chat.js";

// Interactive chat
export { runChatInteractive } from "./interactive.js";

// Mail scenarios
export {
  runMailSend,
  runMailList,
  runMailGet,
  runMailRead,
  runMailClaim,
  runMailSendAndNotify
} from "./mail.js";

// Announcement scenarios
export {
  runAnnounceList,
  runAnnounceGet,
  runAnnounceCreate,
  runAnnounceUpdate,
  runAnnounceDelete
} from "./announce.js";

// Interactive movement
export { runMovementInteractive } from "./movement-interactive.js";

// Inventory scenarios
export {
  runInventoryEquip,
  runInventoryUse,
  runInventoryDiscard,
  runInventoryWarehouse,
  runGetInventory,
  runInventoryFull
} from "./inventory.js";

// Character scenarios
export {
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
} from "./character.js";

// Re-export MESSAGE_TYPE for convenience
export { MESSAGE_TYPE } from "../constants.js";
