use tracing::{info, warn};

use crate::core::context::{ConnectionContext, ServiceContext};
use crate::core::inventory::{EquipSlot, PlayerData};
use crate::core::player::player_manager::WarehouseAssetAction;
use crate::pb::{
    AttrChangePush, AttrPanel as PbAttrPanel, AttrRecord as PbAttrRecord, GetInventoryRes,
    InventoryUpdatePush, Item as PbItem, ItemDiscardReq, ItemDiscardRes, ItemEquipReq,
    ItemEquipRes, ItemUseReq, ItemUseRes, VisualChangePush, WarehouseAccessReq, WarehouseAccessRes,
};
use crate::protocol::{MessageType, Packet};

/// 处理装备穿戴请求
pub async fn handle_item_equip(
    services: &ServiceContext,
    connection: &ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(identity) = connection.ensure_authenticated_identity(packet.header.seq)? else {
        return Ok(());
    };
    let account_player_id = identity.account_player_id;
    let character_id = identity.character_id;
    let world_id = identity.world_id;

    let request = match packet.decode_body::<ItemEquipReq>("INVALID_ITEM_EQUIP_BODY") {
        Ok(value) => value,
        Err(error_code) => {
            connection.queue_error(packet.header.seq, error_code, "invalid item equip body")?;
            return Ok(());
        }
    };

    info!(
        session_id = connection.session.id,
        account_player_id = %account_player_id,
        character_id = %character_id,
        world_id = ?world_id,
        item_uid = request.item_uid,
        equip_slot = request.equip_slot,
        "handle_item_equip"
    );

    let config_tables = services.config_tables.tables_snapshot().await;

    // 解析装备槽位
    let _slot = match EquipSlot::from_str(&request.equip_slot) {
        Some(slot) => slot,
        None => {
            connection.queue_message(
                MessageType::ItemEquipRes,
                packet.header.seq,
                ItemEquipRes {
                    ok: false,
                    error_code: "INVALID_SLOT".to_string(),
                    unequipped_item: None,
                },
            )?;
            return Ok(());
        }
    };

    match services
        .player_manager
        .equip_item_in_asset_transaction(&character_id, request.item_uid, &config_tables.item_table)
        .await
    {
        Ok(player_data) => {
            connection.queue_message(
                MessageType::ItemEquipRes,
                packet.header.seq,
                ItemEquipRes {
                    ok: true,
                    error_code: String::new(),
                    unequipped_item: None,
                },
            )?;

            // 发送属性变化推送
            send_attr_change_push(connection, &player_data).await;

            // 发送外观变化推送
            send_visual_change_push(connection, &player_data).await;

            // 发送背包更新推送
            send_inventory_update_push(connection, &player_data).await;
        }
        Err(error) => {
            connection.queue_message(
                MessageType::ItemEquipRes,
                packet.header.seq,
                ItemEquipRes {
                    ok: false,
                    error_code: error.error_code().to_string(),
                    unequipped_item: None,
                },
            )?;
        }
    }

    Ok(())
}

/// 处理物品使用请求
pub async fn handle_item_use(
    services: &ServiceContext,
    connection: &ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(identity) = connection.ensure_authenticated_identity(packet.header.seq)? else {
        return Ok(());
    };
    let account_player_id = identity.account_player_id;
    let character_id = identity.character_id;
    let world_id = identity.world_id;

    let request = match packet.decode_body::<ItemUseReq>("INVALID_ITEM_USE_BODY") {
        Ok(value) => value,
        Err(error_code) => {
            connection.queue_error(packet.header.seq, error_code, "invalid item use body")?;
            return Ok(());
        }
    };

    info!(
        session_id = connection.session.id,
        account_player_id = %account_player_id,
        character_id = %character_id,
        world_id = ?world_id,
        item_uid = request.item_uid,
        "handle_item_use"
    );

    let config_tables = services.config_tables.tables_snapshot().await;
    match services
        .player_manager
        .use_item_in_asset_transaction(&character_id, request.item_uid, &config_tables.item_table)
        .await
    {
        Ok(outcome) => {
            let player_data = outcome.player_data;
            let hp_change = outcome.hp_change;

            connection.queue_message(
                MessageType::ItemUseRes,
                packet.header.seq,
                ItemUseRes {
                    ok: true,
                    error_code: String::new(),
                    hp_change,
                    new_buff_ids: vec![],
                },
            )?;

            // 如果属性变化了，发送属性推送
            if hp_change != 0 {
                send_attr_change_push(connection, &player_data).await;
            }

            // 发送背包更新
            send_inventory_update_push(connection, &player_data).await;
        }
        Err(error) => {
            connection.queue_message(
                MessageType::ItemUseRes,
                packet.header.seq,
                ItemUseRes {
                    ok: false,
                    error_code: error.error_code().to_string(),
                    hp_change: 0,
                    new_buff_ids: vec![],
                },
            )?;
        }
    }

    Ok(())
}

/// 处理物品丢弃请求
pub async fn handle_item_discard(
    services: &ServiceContext,
    connection: &ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(identity) = connection.ensure_authenticated_identity(packet.header.seq)? else {
        return Ok(());
    };
    let account_player_id = identity.account_player_id;
    let character_id = identity.character_id;
    let world_id = identity.world_id;

    let request = match packet.decode_body::<ItemDiscardReq>("INVALID_ITEM_DISCARD_BODY") {
        Ok(value) => value,
        Err(error_code) => {
            connection.queue_error(packet.header.seq, error_code, "invalid item discard body")?;
            return Ok(());
        }
    };

    info!(
        session_id = connection.session.id,
        account_player_id = %account_player_id,
        character_id = %character_id,
        world_id = ?world_id,
        item_uid = request.item_uid,
        count = request.count,
        "handle_item_discard"
    );

    match services
        .player_manager
        .discard_item_in_asset_transaction(&character_id, request.item_uid, request.count)
        .await
    {
        Ok(player_data) => {
            connection.queue_message(
                MessageType::ItemDiscardRes,
                packet.header.seq,
                ItemDiscardRes {
                    ok: true,
                    error_code: String::new(),
                },
            )?;

            send_inventory_update_push(connection, &player_data).await;
        }
        Err(error) => {
            connection.queue_message(
                MessageType::ItemDiscardRes,
                packet.header.seq,
                ItemDiscardRes {
                    ok: false,
                    error_code: error.error_code().to_string(),
                },
            )?;
        }
    }

    Ok(())
}

/// 处理仓库存取请求
pub async fn handle_warehouse_access(
    services: &ServiceContext,
    connection: &ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(identity) = connection.ensure_authenticated_identity(packet.header.seq)? else {
        return Ok(());
    };
    let account_player_id = identity.account_player_id;
    let character_id = identity.character_id;
    let world_id = identity.world_id;

    let request = match packet.decode_body::<WarehouseAccessReq>("INVALID_WAREHOUSE_ACCESS_BODY") {
        Ok(value) => value,
        Err(error_code) => {
            connection.queue_error(
                packet.header.seq,
                error_code,
                "invalid warehouse access body",
            )?;
            return Ok(());
        }
    };

    info!(
        session_id = connection.session.id,
        account_player_id = %account_player_id,
        character_id = %character_id,
        world_id = ?world_id,
        action = request.action,
        item_uid = request.item_uid,
        count = request.count,
        "handle_warehouse_access"
    );

    let action = match request.action.as_str() {
        "deposit" => WarehouseAssetAction::Deposit,
        "withdraw" => WarehouseAssetAction::Withdraw,
        _ => {
            connection.queue_message(
                MessageType::WarehouseAccessRes,
                packet.header.seq,
                WarehouseAccessRes {
                    ok: false,
                    error_code: "UNKNOWN_ERROR".to_string(),
                },
            )?;
            return Ok(());
        }
    };
    let config_tables = services.config_tables.tables_snapshot().await;
    let item_uid_generator = services.item_uid_generator.clone();
    match services
        .player_manager
        .move_warehouse_item_in_asset_transaction(
            &character_id,
            action,
            request.item_uid,
            request.count,
            &config_tables.item_table,
            move || {
                item_uid_generator
                    .next()
                    .map_err(|_| crate::core::inventory::ItemError::Unknown)
            },
        )
        .await
    {
        Ok(player_data) => {
            connection.queue_message(
                MessageType::WarehouseAccessRes,
                packet.header.seq,
                WarehouseAccessRes {
                    ok: true,
                    error_code: String::new(),
                },
            )?;

            send_inventory_update_push(connection, &player_data).await;
        }
        Err(error) => {
            connection.queue_message(
                MessageType::WarehouseAccessRes,
                packet.header.seq,
                WarehouseAccessRes {
                    ok: false,
                    error_code: error.error_code().to_string(),
                },
            )?;
        }
    }

    Ok(())
}

/// 处理获取背包信息请求
pub async fn handle_get_inventory(
    services: &ServiceContext,
    connection: &ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(identity) = connection.ensure_authenticated_identity(packet.header.seq)? else {
        return Ok(());
    };
    let account_player_id = identity.account_player_id;
    let character_id = identity.character_id;
    let world_id = identity.world_id;

    info!(
        session_id = connection.session.id,
        account_player_id = %account_player_id,
        character_id = %character_id,
        world_id = ?world_id,
        "handle_get_inventory"
    );

    // 获取角色玩法数据
    let player_data = services
        .player_manager
        .get_or_create_player(&character_id)
        .await;

    let inventory_items: Vec<PbItem> = player_data
        .get_inventory_items()
        .iter()
        .map(|item| PbItem {
            uid: item.uid,
            item_id: item.item_id,
            count: item.count,
            binded: item.binded,
        })
        .collect();

    let warehouse_items: Vec<PbItem> = player_data
        .get_warehouse_items()
        .iter()
        .map(|item| PbItem {
            uid: item.uid,
            item_id: item.item_id,
            count: item.count,
            binded: item.binded,
        })
        .collect();

    connection.queue_message(
        MessageType::GetInventoryRes,
        packet.header.seq,
        GetInventoryRes {
            ok: true,
            error_code: String::new(),
            inventory_items,
            warehouse_items,
        },
    )?;

    Ok(())
}

// ========== 推送消息辅助函数 ==========

async fn send_attr_change_push(connection: &ConnectionContext, player_data: &PlayerData) {
    let attr = &player_data.attr;

    let bonus: Vec<PbAttrRecord> = attr
        .bonus
        .iter()
        .map(|r| PbAttrRecord {
            source: r.source.as_str(),
            attr_type: r.attr_type.as_str().to_string(),
            value: r.value,
        })
        .collect();

    let push = AttrChangePush {
        base: Some(PbAttrPanel {
            hp: attr.base.hp,
            max_hp: attr.base.max_hp,
            attack: attr.base.attack,
            defense: attr.base.defense,
            speed: attr.base.speed,
            crit_rate: attr.base.crit_rate,
            crit_dmg: attr.base.crit_dmg,
        }),
        bonus,
        r#final: Some(PbAttrPanel {
            hp: attr.final_.hp,
            max_hp: attr.final_.max_hp,
            attack: attr.final_.attack,
            defense: attr.final_.defense,
            speed: attr.final_.speed,
            crit_rate: attr.final_.crit_rate,
            crit_dmg: attr.final_.crit_dmg,
        }),
    };

    if let Err(e) = connection.queue_message(MessageType::AttrChangePush, 0, push) {
        warn!(error = %e, "failed to send AttrChangePush");
    }
}

async fn send_visual_change_push(connection: &ConnectionContext, player_data: &PlayerData) {
    let push = VisualChangePush {
        appearance: player_data.visual.appearance,
        active_buff_ids: player_data
            .visual
            .active_buffs
            .iter()
            .map(|&id| id as i32)
            .collect(),
    };

    if let Err(e) = connection.queue_message(MessageType::VisualChangePush, 0, push) {
        warn!(error = %e, "failed to send VisualChangePush");
    }
}

async fn send_inventory_update_push(connection: &ConnectionContext, player_data: &PlayerData) {
    let inventory_items: Vec<PbItem> = player_data
        .get_inventory_items()
        .iter()
        .map(|item| PbItem {
            uid: item.uid,
            item_id: item.item_id,
            count: item.count,
            binded: item.binded,
        })
        .collect();

    let warehouse_items: Vec<PbItem> = player_data
        .get_warehouse_items()
        .iter()
        .map(|item| PbItem {
            uid: item.uid,
            item_id: item.item_id,
            count: item.count,
            binded: item.binded,
        })
        .collect();

    let push = InventoryUpdatePush {
        inventory_items,
        warehouse_items,
    };

    if let Err(e) = connection.queue_message(MessageType::InventoryUpdatePush, 0, push) {
        warn!(error = %e, "failed to send InventoryUpdatePush");
    }
}
