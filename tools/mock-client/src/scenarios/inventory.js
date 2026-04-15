import { MESSAGE_TYPE } from "../constants.js";
import {
  encodeItemEquipReq,
  encodeItemUseReq,
  encodeItemDiscardReq,
  encodeWarehouseAccessReq,
  encodeItemAddReq,
  encodeGetInventoryReq,
  encodeRoomJoinReq,
  encodeRoomLeaveReq
} from "../messages.js";
import { decodeByMessageType } from "../messages.js";
import { fetchTicket, formatLoginSummary } from "../auth.js";
import { TcpProtocolClient } from "../client.js";

/**
 * Print response and return decoded body
 */
export function printResponse(label, packet) {
  const decoded = decodeByMessageType(packet.messageType, packet.body);
  console.log(`${label}:`, JSON.stringify({ messageType: packet.messageType, seq: packet.seq, decoded }, null, 2));
  return decoded;
}

/**
 * Wait for specific push message
 */
async function waitForPush(client, expectedMessageTypes, timeoutMs, label = "push") {
  const maxIterations = 50;
  for (let i = 0; i < maxIterations; i++) {
    const packet = await client.readNextPacket(timeoutMs);
    if (!packet) {
      continue;
    }
    if (expectedMessageTypes.includes(packet.messageType)) {
      return printResponse(`${client.label}.${label}`, packet);
    }
    // Log unexpected packet for debugging
    console.log(`${client.label}.${label}[skip ${packet.messageType}]:`, JSON.stringify(decodeByMessageType(packet.messageType, packet.body), null, 2));
  }
  throw new Error(`Timeout waiting for ${expectedMessageTypes} from ${client.label}`);
}

/**
 * Helper to authenticate and join room
 */
async function authenticateAndJoinRoom(client, options, login, roomId) {
  const { encodeAuthReq } = await import("../messages.js");
  await client.send(MESSAGE_TYPE.AUTH_REQ, 1, encodeAuthReq(login.ticket));
  const auth = printResponse(`${client.label}.auth`, await client.readNextPacket(options.timeoutMs));
  if (!auth.ok) {
    throw new Error(`${client.label} auth failed: ${auth.errorCode}`);
  }

  await client.send(MESSAGE_TYPE.ROOM_JOIN_REQ, 2, encodeRoomJoinReq(roomId));
  const joinRes = printResponse(`${client.label}.roomJoin`, await client.readNextPacket(options.timeoutMs));
  if (!joinRes.ok) {
    throw new Error(`${client.label} room join failed: ${joinRes.errorCode}`);
  }

  // Read room state push
  await waitForPush(client, [MESSAGE_TYPE.ROOM_STATE_PUSH], options.timeoutMs, "roomStatePush");
}

/**
 * Test inventory equip operation
 * Requires: --equip-slot <slot> --item-uid <uid>
 */
export async function runInventoryEquip(options) {
  const login = await fetchTicket(options);
  console.log("login:", JSON.stringify(formatLoginSummary(login), null, 2));

  const client = new TcpProtocolClient(options, "client");
  await client.connect();

  try {
    const roomId = options.roomId || "room-inventory-test";
    await authenticateAndJoinRoom(client, options, login, roomId);

    const itemUid = options.itemUid || 1;
    const equipSlot = options.equipSlot || "Weapon";

    console.log(`\n--- INVENTORY EQUIP TEST ---`);
    console.log(`itemUid: ${itemUid}, equipSlot: ${equipSlot}`);

    await client.send(MESSAGE_TYPE.ITEM_EQUIP_REQ, 10, encodeItemEquipReq(itemUid, equipSlot));

    // Wait for response and push messages
    const equipRes = await waitForPush(client, [MESSAGE_TYPE.ITEM_EQUIP_RES], options.timeoutMs, "itemEquipRes");
    console.log("Equip result:", JSON.stringify(equipRes, null, 2));

    if (!equipRes.ok) {
      console.log(`[WARN] Equip failed: ${equipRes.errorCode}`);
    } else {
      console.log("[OK] Equip succeeded");
      if (equipRes.unequippedItem) {
        console.log("Unequipped item:", JSON.stringify(equipRes.unequippedItem, null, 2));
      }
    }

    // Wait for related push messages
    try {
      await waitForPush(client, [MESSAGE_TYPE.INVENTORY_UPDATE_PUSH, MESSAGE_TYPE.ATTR_CHANGE_PUSH, MESSAGE_TYPE.VISUAL_CHANGE_PUSH], options.timeoutMs, "push");
    } catch (e) {
      console.log("[INFO] No push messages received");
    }

    console.log(`\n--- INVENTORY EQUIP TEST COMPLETE ---`);
  } finally {
    client.close();
  }
}

/**
 * Test inventory use operation
 * Requires: --item-uid <uid>
 */
export async function runInventoryUse(options) {
  const login = await fetchTicket(options);
  console.log("login:", JSON.stringify(formatLoginSummary(login), null, 2));

  const client = new TcpProtocolClient(options, "client");
  await client.connect();

  try {
    const roomId = options.roomId || "room-inventory-test";
    await authenticateAndJoinRoom(client, options, login, roomId);

    const itemUid = options.itemUid || 1;

    console.log(`\n--- INVENTORY USE TEST ---`);
    console.log(`itemUid: ${itemUid}`);

    await client.send(MESSAGE_TYPE.ITEM_USE_REQ, 10, encodeItemUseReq(itemUid));

    const useRes = await waitForPush(client, [MESSAGE_TYPE.ITEM_USE_RES], options.timeoutMs, "itemUseRes");
    console.log("Use result:", JSON.stringify(useRes, null, 2));

    if (!useRes.ok) {
      console.log(`[WARN] Use failed: ${useRes.errorCode}`);
    } else {
      console.log("[OK] Use succeeded");
      if (useRes.hpChange !== 0) {
        console.log(`HP change: ${useRes.hpChange}`);
      }
      if (useRes.newBuffIds && useRes.newBuffIds.length > 0) {
        console.log(`New buff IDs: ${useRes.newBuffIds.join(", ")}`);
      }
    }

    // Wait for related push messages
    try {
      await waitForPush(client, [MESSAGE_TYPE.INVENTORY_UPDATE_PUSH, MESSAGE_TYPE.ATTR_CHANGE_PUSH], options.timeoutMs, "push");
    } catch (e) {
      console.log("[INFO] No push messages received");
    }

    console.log(`\n--- INVENTORY USE TEST COMPLETE ---`);
  } finally {
    client.close();
  }
}

/**
 * Test inventory discard operation
 * Requires: --item-uid <uid> --count <count>
 */
export async function runInventoryDiscard(options) {
  const login = await fetchTicket(options);
  console.log("login:", JSON.stringify(formatLoginSummary(login), null, 2));

  const client = new TcpProtocolClient(options, "client");
  await client.connect();

  try {
    const roomId = options.roomId || "room-inventory-test";
    await authenticateAndJoinRoom(client, options, login, roomId);

    const itemUid = options.itemUid || 1;
    const count = options.count || 1;

    console.log(`\n--- INVENTORY DISCARD TEST ---`);
    console.log(`itemUid: ${itemUid}, count: ${count}`);

    await client.send(MESSAGE_TYPE.ITEM_DISCARD_REQ, 10, encodeItemDiscardReq(itemUid, count));

    const discardRes = await waitForPush(client, [MESSAGE_TYPE.ITEM_DISCARD_RES], options.timeoutMs, "itemDiscardRes");
    console.log("Discard result:", JSON.stringify(discardRes, null, 2));

    if (!discardRes.ok) {
      console.log(`[WARN] Discard failed: ${discardRes.errorCode}`);
    } else {
      console.log("[OK] Discard succeeded");
    }

    // Wait for inventory update push
    try {
      await waitForPush(client, [MESSAGE_TYPE.INVENTORY_UPDATE_PUSH], options.timeoutMs, "inventoryUpdate");
    } catch (e) {
      console.log("[INFO] No inventory update received");
    }

    console.log(`\n--- INVENTORY DISCARD TEST COMPLETE ---`);
  } finally {
    client.close();
  }
}

/**
 * Test warehouse access (deposit/withdraw)
 * Requires: --warehouse-action <deposit|withdraw> --item-uid <uid> --count <count>
 */
export async function runInventoryWarehouse(options) {
  const login = await fetchTicket(options);
  console.log("login:", JSON.stringify(formatLoginSummary(login), null, 2));

  const client = new TcpProtocolClient(options, "client");
  await client.connect();

  try {
    const roomId = options.roomId || "room-inventory-test";
    await authenticateAndJoinRoom(client, options, login, roomId);

    const warehouseAction = options.warehouseAction || "deposit";
    const itemUid = options.itemUid || 1;
    const count = options.count || 1;

    console.log(`\n--- INVENTORY WAREHOUSE TEST ---`);
    console.log(`action: ${warehouseAction}, itemUid: ${itemUid}, count: ${count}`);

    await client.send(MESSAGE_TYPE.WAREHOUSE_ACCESS_REQ, 10, encodeWarehouseAccessReq(warehouseAction, itemUid, count));

    const warehouseRes = await waitForPush(client, [MESSAGE_TYPE.WAREHOUSE_ACCESS_RES], options.timeoutMs, "warehouseAccessRes");
    console.log("Warehouse result:", JSON.stringify(warehouseRes, null, 2));

    if (!warehouseRes.ok) {
      console.log(`[WARN] Warehouse access failed: ${warehouseRes.errorCode}`);
    } else {
      console.log("[OK] Warehouse access succeeded");
    }

    // Wait for inventory update push
    try {
      await waitForPush(client, [MESSAGE_TYPE.INVENTORY_UPDATE_PUSH], options.timeoutMs, "inventoryUpdate");
    } catch (e) {
      console.log("[INFO] No inventory update received");
    }

    console.log(`\n--- INVENTORY WAREHOUSE TEST COMPLETE ---`);
  } finally {
    client.close();
  }
}

/**
 * Test adding items to inventory (for testing purposes)
 * Requires: --add-item-id <item_id> --add-count <count>
 */
export async function runInventoryAdd(options) {
  const login = await fetchTicket(options);
  console.log("login:", JSON.stringify(formatLoginSummary(login), null, 2));

  const client = new TcpProtocolClient(options, "client");
  await client.connect();

  try {
    const roomId = options.roomId || "room-inventory-test";
    await authenticateAndJoinRoom(client, options, login, roomId);

    const itemId = options.addItemId || 1;
    const count = options.addCount || 1;
    const binded = options.addBinded || false;

    console.log(`\n--- INVENTORY ADD TEST ---`);
    console.log(`itemId: ${itemId}, count: ${count}, binded: ${binded}`);

    await client.send(MESSAGE_TYPE.ITEM_ADD_REQ, 10, encodeItemAddReq(itemId, count, binded));

    const addRes = await waitForPush(client, [MESSAGE_TYPE.ITEM_ADD_RES], options.timeoutMs, "itemAddRes");
    console.log("Add result:", JSON.stringify(addRes, null, 2));

    if (!addRes.ok) {
      console.log(`[WARN] Add item failed: ${addRes.errorCode}`);
    } else {
      console.log("[OK] Add item succeeded");
      if (addRes.item) {
        console.log("Added item:", JSON.stringify(addRes.item, null, 2));
      }
    }

    // Wait for inventory update push
    try {
      await waitForPush(client, [MESSAGE_TYPE.INVENTORY_UPDATE_PUSH], options.timeoutMs, "inventoryUpdate");
    } catch (e) {
      console.log("[INFO] No inventory update received");
    }

    console.log(`\n--- INVENTORY ADD TEST COMPLETE ---`);
  } finally {
    client.close();
  }
}

/**
 * Test getting current inventory state
 */
export async function runGetInventory(options) {
  const login = await fetchTicket(options);
  console.log("login:", JSON.stringify(formatLoginSummary(login), null, 2));

  const client = new TcpProtocolClient(options, "client");
  await client.connect();

  try {
    const roomId = options.roomId || "room-inventory-test";
    await authenticateAndJoinRoom(client, options, login, roomId);

    console.log(`\n--- GET INVENTORY ---`);

    await client.send(MESSAGE_TYPE.GET_INVENTORY_REQ, 10, encodeGetInventoryReq());

    const invRes = await waitForPush(client, [MESSAGE_TYPE.GET_INVENTORY_RES], options.timeoutMs, "getInventoryRes");
    console.log("Inventory result:", JSON.stringify(invRes, null, 2));

    if (!invRes.ok) {
      console.log(`[WARN] Get inventory failed: ${invRes.errorCode}`);
    } else {
      console.log("[OK] Get inventory succeeded");
      console.log(`Inventory items (${invRes.inventoryItems.length}):`);
      invRes.inventoryItems.forEach((item, i) => {
        console.log(`  [${i + 1}] uid=${item.uid}, itemId=${item.itemId}, count=${item.count}, binded=${item.binded}`);
      });
      console.log(`Warehouse items (${invRes.warehouseItems.length}):`);
      invRes.warehouseItems.forEach((item, i) => {
        console.log(`  [${i + 1}] uid=${item.uid}, itemId=${item.itemId}, count=${item.count}, binded=${item.binded}`);
      });
    }

    console.log(`\n--- GET INVENTORY COMPLETE ---`);
  } finally {
    client.close();
  }
}

/**
 * Full inventory test: login -> check inventory -> equip -> use -> unequip -> warehouse
 * This scenario tests the complete inventory workflow
 */
export async function runInventoryFull(options) {
  const login = await fetchTicket(options);
  console.log("login:", JSON.stringify(formatLoginSummary(login), null, 2));

  const client = new TcpProtocolClient(options, "client");
  await client.connect();

  try {
    const roomId = options.roomId || "room-inventory-full";

    console.log("\n" + "=".repeat(60));
    console.log("INVENTORY FULL TEST - START");
    console.log("=".repeat(60));

    // Step 1: Authenticate
    await authenticateAndJoinRoom(client, options, login, roomId);

    // Step 2: Wait for initial inventory state
    console.log("\n--- STEP 1: Initial Inventory State ---");
    try {
      await waitForPush(client, [MESSAGE_TYPE.INVENTORY_UPDATE_PUSH], options.timeoutMs, "initialInventory");
    } catch (e) {
      console.log("[INFO] No initial inventory push received");
    }

    // Step 3: Equip item (if specified)
    if (options.itemUid && options.equipSlot) {
      console.log("\n--- STEP 2: Equip Item ---");
      await client.send(MESSAGE_TYPE.ITEM_EQUIP_REQ, 10, encodeItemEquipReq(options.itemUid, options.equipSlot));
      const equipRes = await waitForPush(client, [MESSAGE_TYPE.ITEM_EQUIP_RES, MESSAGE_TYPE.ERROR_RES], options.timeoutMs, "equipRes");
      console.log("Equip result:", JSON.stringify(equipRes, null, 2));

      // Wait for attr/visual updates
      try {
        await waitForPush(client, [MESSAGE_TYPE.ATTR_CHANGE_PUSH], options.timeoutMs, "attrChange");
      } catch (e) {}
      try {
        await waitForPush(client, [MESSAGE_TYPE.VISUAL_CHANGE_PUSH], options.timeoutMs, "visualChange");
      } catch (e) {}
    }

    // Step 4: Use item (if specified)
    if (options.useItemUid) {
      console.log("\n--- STEP 3: Use Item ---");
      await client.send(MESSAGE_TYPE.ITEM_USE_REQ, 11, encodeItemUseReq(options.useItemUid));
      const useRes = await waitForPush(client, [MESSAGE_TYPE.ITEM_USE_RES, MESSAGE_TYPE.ERROR_RES], options.timeoutMs, "useRes");
      console.log("Use result:", JSON.stringify(useRes, null, 2));
    }

    // Step 5: Discard item (if specified)
    if (options.discardUid && options.discardCount) {
      console.log("\n--- STEP 4: Discard Item ---");
      await client.send(MESSAGE_TYPE.ITEM_DISCARD_REQ, 12, encodeItemDiscardReq(options.discardUid, options.discardCount));
      const discardRes = await waitForPush(client, [MESSAGE_TYPE.ITEM_DISCARD_RES, MESSAGE_TYPE.ERROR_RES], options.timeoutMs, "discardRes");
      console.log("Discard result:", JSON.stringify(discardRes, null, 2));
    }

    // Step 6: Warehouse deposit (if specified)
    if (options.depositUid && options.depositCount) {
      console.log("\n--- STEP 5: Warehouse Deposit ---");
      await client.send(MESSAGE_TYPE.WAREHOUSE_ACCESS_REQ, 13, encodeWarehouseAccessReq("deposit", options.depositUid, options.depositCount));
      const depositRes = await waitForPush(client, [MESSAGE_TYPE.WAREHOUSE_ACCESS_RES, MESSAGE_TYPE.ERROR_RES], options.timeoutMs, "depositRes");
      console.log("Deposit result:", JSON.stringify(depositRes, null, 2));
    }

    // Final inventory check
    console.log("\n--- FINAL: Inventory State ---");
    try {
      await waitForPush(client, [MESSAGE_TYPE.INVENTORY_UPDATE_PUSH], options.timeoutMs, "finalInventory");
    } catch (e) {
      console.log("[INFO] No final inventory push received");
    }

    console.log("\n" + "=".repeat(60));
    console.log("INVENTORY FULL TEST - COMPLETE");
    console.log("=".repeat(60));
  } finally {
    client.close();
  }
}
