import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

function read(relativePath) {
  return readFileSync(path.join(rootDir, relativePath), "utf8");
}

const failures = [];

function requireMatch(source, pattern, description) {
  if (!pattern.test(source)) {
    failures.push(description);
  }
}

function rejectMatch(source, pattern, description) {
  if (pattern.test(source)) {
    failures.push(description);
  }
}

const playerDispatch = read("apps/game-server/src/server.rs");
const messageTypes = read("apps/game-server/src/protocol/message_type.rs");
const gameProto = read("packages/proto/game.proto");
const inventoryService = read("apps/game-server/src/core/service/inventory_service.rs");
const playerManager = read("apps/game-server/src/core/player/player_manager.rs");
const gm = read("apps/game-server/src/admin_server/gm.rs");
const mockConstants = read("tools/mock-client/src/constants.js");
const mockMessages = read("tools/mock-client/src/messages.js");
const mockInventoryScenario = read("tools/mock-client/src/scenarios/inventory.js");

requireMatch(
  messageTypes,
  /DeprecatedItemAddReq\s*=\s*1407[\s\S]*DeprecatedItemAddRes\s*=\s*1408/,
  "1407/1408 must remain named deprecated protocol reservations"
);
requireMatch(
  playerDispatch,
  /DeprecatedItemAddReq[\s\S]*MESSAGE_TYPE_DEPRECATED/,
  "retired ItemAdd packets must be rejected deterministically"
);
rejectMatch(gameProto, /message\s+ItemAdd(?:Req|Res)\b/, "ItemAdd protobuf messages must stay removed");
rejectMatch(playerDispatch, /MessageType::ItemAdd(?:Req|Res)/, "player dispatch must not route ItemAdd");
rejectMatch(mockInventoryScenario, /runInventoryAdd|ITEM_ADD|encodeItemAddReq/, "mock-client must not expose inventory-add");
rejectMatch(mockMessages, /encodeItemAddReq|ITEM_ADD_RES/, "mock-client must not encode or decode ItemAdd");
requireMatch(
  mockConstants,
  /DEPRECATED_ITEM_ADD_REQ:\s*1407[\s\S]*DEPRECATED_ITEM_ADD_RES:\s*1408/,
  "mock-client must retain only deprecated 1407/1408 constants"
);

rejectMatch(
  inventoryService,
  /\b(?:save_player|handle_item_add)\b/,
  "inventory protocol handlers must not save snapshots or keep ItemAdd"
);
rejectMatch(
  inventoryService,
  /\.(?:add_item|add_item_with_table|remove_item|equip_item|unequip_item|warehouse_deposit|warehouse_withdraw)\(/,
  "inventory protocol handlers must not mutate PlayerData or ItemContainer directly"
);
requireMatch(
  playerManager,
  /commit_asset_mutation[\s\S]*asset_character_locks[\s\S]*player_data\.set_persistence_revision/,
  "player mutations must use the shared character transaction boundary"
);
requireMatch(
  gm,
  /grant_items_with_request_using_table/,
  "GM and mail claim grants must use capacity-planned asset transactions"
);
requireMatch(
  gm,
  /MAIL_CLAIM_ASSET_TRANSACTIONS_ENABLED[\s\S]*GM_ASSET_CONSTRUCTION_ENABLED[\s\S]*GM_EMERGENCY_ASSET_CORRECTION_ENABLED/,
  "mail, GM construction, and emergency correction must retain independent switches"
);

if (failures.length > 0) {
  console.error("asset write-boundary check failed:");
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log("asset write-boundary checks passed.");
