// Protocol constants for game-server communication
export const MAGIC = 0xCAFE;
export const VERSION = 1;
export const HEADER_LEN = 14;

// Message types
export const MESSAGE_TYPE = {
  // Game player TCP (1000-1999)
  // Auth / heartbeat
  AUTH_REQ: 1001,
  AUTH_RES: 1002,
  PING_REQ: 1003,
  PING_RES: 1004,
  // Room
  ROOM_JOIN_REQ: 1101,
  ROOM_JOIN_RES: 1102,
  ROOM_LEAVE_REQ: 1103,
  ROOM_LEAVE_RES: 1104,
  ROOM_READY_REQ: 1105,
  ROOM_READY_RES: 1106,
  ROOM_START_REQ: 1107,
  ROOM_START_RES: 1108,
  PLAYER_INPUT_REQ: 1111,
  PLAYER_INPUT_RES: 1112,
  ROOM_END_REQ: 1113,
  ROOM_END_RES: 1114,
  ROOM_RECONNECT_REQ: 1115,
  ROOM_RECONNECT_RES: 1116,
  ROOM_JOIN_AS_OBSERVER_REQ: 1117,
  ROOM_JOIN_AS_OBSERVER_RES: 1118,
  CREATE_MATCHED_ROOM_REQ: 1119,
  CREATE_MATCHED_ROOM_RES: 1120,
  MOVE_INPUT_REQ: 1121,
  MOVE_INPUT_RES: 1122,
  ROOM_STATE_PUSH: 1201,
  GET_ROOM_DATA_REQ: 1301,
  GET_ROOM_DATA_RES: 1302,
  GAME_MESSAGE_PUSH: 1202,
  FRAME_BUNDLE_PUSH: 1203,
  ROOM_FRAME_RATE_PUSH: 1204,
  ROOM_MEMBER_OFFLINE_PUSH: 1205,
  MOVEMENT_SNAPSHOT_PUSH: 1206,
  MOVEMENT_REJECT_PUSH: 1207,
  // Inventory
  ITEM_EQUIP_REQ: 1401,
  ITEM_EQUIP_RES: 1402,
  ITEM_USE_REQ: 1403,
  ITEM_USE_RES: 1404,
  ITEM_DISCARD_REQ: 1405,
  ITEM_DISCARD_RES: 1406,
  ITEM_ADD_REQ: 1407,
  ITEM_ADD_RES: 1408,
  WAREHOUSE_ACCESS_REQ: 1409,
  WAREHOUSE_ACCESS_RES: 1410,
  GET_INVENTORY_REQ: 1411,
  GET_INVENTORY_RES: 1412,
  INVENTORY_UPDATE_PUSH: 1501,
  ATTR_CHANGE_PUSH: 1502,
  VISUAL_CHANGE_PUSH: 1503,
  ITEM_OBTAIN_PUSH: 1504,
  // Chat TCP (20000-20999)
  CHAT_AUTH_REQ: 20001,
  CHAT_AUTH_RES: 20002,
  CHAT_PRIVATE_REQ: 20101,
  CHAT_PRIVATE_RES: 20102,
  CHAT_GROUP_REQ: 20103,
  CHAT_GROUP_RES: 20104,
  CHAT_PUSH: 20105,
  GROUP_CREATE_REQ: 20201,
  GROUP_CREATE_RES: 20202,
  GROUP_JOIN_REQ: 20203,
  GROUP_JOIN_RES: 20204,
  GROUP_LEAVE_REQ: 20205,
  GROUP_LEAVE_RES: 20206,
  GROUP_DISMISS_REQ: 20207,
  GROUP_DISMISS_RES: 20208,
  GROUP_LIST_REQ: 20209,
  GROUP_LIST_RES: 20210,
  CHAT_HISTORY_REQ: 20211,
  CHAT_HISTORY_RES: 20212,
  MAIL_NOTIFY_PUSH: 20301,
  // Error
  ERROR_RES: 9000
};

export const MOVE_INPUT_TYPE = {
  UNKNOWN: 0,
  MOVE_DIR: 1,
  MOVE_STOP: 2,
  FACE_TO: 3
};

// Test scenarios
export const SCENARIO = {
  HAPPY: "happy",
  INVALID_TICKET: "invalid-ticket",
  UNAUTH_ROOM_JOIN: "unauth-room-join",
  UNKNOWN_MESSAGE: "unknown-message",
  OVERSIZED_ROOM_JOIN: "oversized-room-join",
  TWO_CLIENT_ROOM: "two-client-room",
  START_GAME_SINGLE_CLIENT: "start-game-single-client",
  START_GAME_READY_ROOM: "start-game-ready-room",
  COMBAT_DUAL_CLIENT: "combat-dual-client",
  GAMEPLAY_ROUNDTRIP: "gameplay-roundtrip",
  MOVEMENT_DEMO: "movement-demo",
  MOVEMENT_SYNC_VALIDATION: "movement-sync-validation",
  MOVEMENT_DUAL_CLIENT_SYNC: "movement-dual-client-sync",
  MOVEMENT_SNAPSHOT_THROTTLE: "movement-snapshot-throttle",
  MOVEMENT_FACE_TO: "movement-face-to",
  GET_ROOM_DATA: "get-room-data",
  GET_ROOM_DATA_IN_ROOM: "get-room-data-in-room",
  RECONNECT: "reconnect",
  // Match scenarios
  CREATE_MATCHED_ROOM: "create-matched-room",
  CREATE_MATCHED_ROOM_AND_JOIN: "create-matched-room-and-join",
  // Chat scenarios
  CHAT_PRIVATE: "chat-private",
  CHAT_GROUP: "chat-group",
  GROUP_CREATE: "group-create",
  GROUP_JOIN: "group-join",
  GROUP_LEAVE: "group-leave",
  GROUP_DISMISS: "group-dismiss",
  GROUP_LIST: "group-list",
  CHAT_HISTORY: "chat-history",
  CHAT_TWO_CLIENT: "chat-two-client",
  CHAT_PRIVATE_TWO_CLIENT: "chat-private-two-client",
  CHAT_INTERACTIVE: "chat-interactive",
  // Mail scenarios
  MAIL_SEND: "mail-send",
  MAIL_LIST: "mail-list",
  MAIL_GET: "mail-get",
  MAIL_READ: "mail-read",
  MAIL_CLAIM: "mail-claim",
  MAIL_SEND_AND_NOTIFY: "mail-send-and-notify",
  MOVEMENT_INTERACTIVE: "movement-interactive",
  // Inventory scenarios
  INVENTORY_EQUIP: "inventory-equip",
  INVENTORY_USE: "inventory-use",
  INVENTORY_DISCARD: "inventory-discard",
  INVENTORY_WAREHOUSE: "inventory-warehouse",
  INVENTORY_ADD: "inventory-add",
  INVENTORY_GET: "inventory-get",
  INVENTORY_FULL: "inventory-full"
};
