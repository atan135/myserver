// Protocol constants for game-server communication
export const MAGIC = 0xCAFE;
export const VERSION = 1;
export const HEADER_LEN = 14;

// Message types
export const MESSAGE_TYPE = {
  // Auth
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
  ROOM_STATE_PUSH: 1201,
  GET_ROOM_DATA_REQ: 1301,
  GET_ROOM_DATA_RES: 1302,
  GAME_MESSAGE_PUSH: 1202,
  FRAME_BUNDLE_PUSH: 1203,
  ROOM_FRAME_RATE_PUSH: 1204,
  ROOM_MEMBER_OFFLINE_PUSH: 1205,
  // Chat (1401-1422)
  CHAT_PRIVATE_REQ: 1401,
  CHAT_PRIVATE_RES: 1402,
  CHAT_GROUP_REQ: 1403,
  CHAT_GROUP_RES: 1404,
  CHAT_PUSH: 1405,
  GROUP_CREATE_REQ: 1411,
  GROUP_CREATE_RES: 1412,
  GROUP_JOIN_REQ: 1413,
  GROUP_JOIN_RES: 1414,
  GROUP_LEAVE_REQ: 1415,
  GROUP_LEAVE_RES: 1416,
  GROUP_DISMISS_REQ: 1417,
  GROUP_DISMISS_RES: 1418,
  GROUP_LIST_REQ: 1419,
  GROUP_LIST_RES: 1420,
  CHAT_HISTORY_REQ: 1421,
  CHAT_HISTORY_RES: 1422,
  // Mail notification (1501)
  MAIL_NOTIFY_PUSH: 1501,
  // Error
  ERROR_RES: 9000
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
  GAMEPLAY_ROUNDTRIP: "gameplay-roundtrip",
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
  CHAT_INTERACTIVE: "chat-interactive"
};
