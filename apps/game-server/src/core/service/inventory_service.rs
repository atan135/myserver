use tracing::{info, warn};

use crate::core::context::{ConnectionContext, ServiceContext};
use crate::core::inventory::{EquipSlot, ItemError, PlayerData};
use crate::pb::{
    AttrChangePush, AttrPanel as PbAttrPanel, AttrRecord as PbAttrRecord, GetInventoryReq, GetInventoryRes,
    Item as PbItem, ItemAddReq, ItemAddRes, ItemDiscardReq, ItemDiscardRes, ItemEquipReq, ItemEquipRes,
    ItemUseReq, ItemUseRes, InventoryUpdatePush, VisualChangePush, WarehouseAccessReq, WarehouseAccessRes,
};
use crate::protocol::{MessageType, Packet};

/// 处理装备穿戴请求
pub async fn handle_item_equip(
    services: &ServiceContext,
    connection: &ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(player_id) = connection.ensure_authenticated(packet.header.seq)? else {
        return Ok(());
    };

    let request = match packet.decode_body::<ItemEquipReq>("INVALID_ITEM_EQUIP_BODY") {
        Ok(value) => value,
        Err(error_code) => {
            connection.queue_error(packet.header.seq, error_code, "invalid item equip body")?;
            return Ok(());
        }
    };

    info!(
        session_id = connection.session.id,
        player_id = %player_id,
        item_uid = request.item_uid,
        equip_slot = request.equip_slot,
        "handle_item_equip"
    );

    // 获取玩家数据
    let config_tables = services.config_tables.snapshot().await;
    let mut player_data = services
        .player_manager
        .get_or_create_player(&player_id)
        .await;

    // 解析装备槽位
    let slot = match EquipSlot::from_str(&request.equip_slot) {
        Some(s) => s,
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

    // 执行装备操作
    let result = player_data.equip_item(request.item_uid, &config_tables.item_table);

    match result {
        Ok(()) => {
            // 保存更新后的玩家数据
            services
                .player_manager
                .save_player(&player_id, player_data.clone())
                .await;

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
        Err(e) => {
            connection.queue_message(
                MessageType::ItemEquipRes,
                packet.header.seq,
                ItemEquipRes {
                    ok: false,
                    error_code: e.as_str().to_string(),
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
    let Some(player_id) = connection.ensure_authenticated(packet.header.seq)? else {
        return Ok(());
    };

    let request = match packet.decode_body::<ItemUseReq>("INVALID_ITEM_USE_BODY") {
        Ok(value) => value,
        Err(error_code) => {
            connection.queue_error(packet.header.seq, error_code, "invalid item use body")?;
            return Ok(());
        }
    };

    info!(
        session_id = connection.session.id,
        player_id = %player_id,
        item_uid = request.item_uid,
        "handle_item_use"
    );

    // 获取玩家数据
    let config_tables = services.config_tables.snapshot().await;
    let mut player_data = services
        .player_manager
        .get_or_create_player(&player_id)
        .await;

    let hp_before = player_data.get_hp();
    let result = player_data.use_item(request.item_uid, &config_tables.item_table);

    match result {
        Ok(()) => {
            let hp_change = player_data.get_hp() - hp_before;

            services
                .player_manager
                .save_player(&player_id, player_data.clone())
                .await;

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
        Err(e) => {
            connection.queue_message(
                MessageType::ItemUseRes,
                packet.header.seq,
                ItemUseRes {
                    ok: false,
                    error_code: e.as_str().to_string(),
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
    let Some(player_id) = connection.ensure_authenticated(packet.header.seq)? else {
        return Ok(());
    };

    let request = match packet.decode_body::<ItemDiscardReq>("INVALID_ITEM_DISCARD_BODY") {
        Ok(value) => value,
        Err(error_code) => {
            connection.queue_error(packet.header.seq, error_code, "invalid item discard body")?;
            return Ok(());
        }
    };

    info!(
        session_id = connection.session.id,
        player_id = %player_id,
        item_uid = request.item_uid,
        count = request.count,
        "handle_item_discard"
    );

    let mut player_data = services
        .player_manager
        .get_or_create_player(&player_id)
        .await;

    let result = player_data.remove_item(request.item_uid, request.count);

    match result {
        Ok(_item) => {
            services
                .player_manager
                .save_player(&player_id, player_data.clone())
                .await;

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
        Err(e) => {
            connection.queue_message(
                MessageType::ItemDiscardRes,
                packet.header.seq,
                ItemDiscardRes {
                    ok: false,
                    error_code: e.as_str().to_string(),
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
    let Some(player_id) = connection.ensure_authenticated(packet.header.seq)? else {
        return Ok(());
    };

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
        player_id = %player_id,
        action = request.action,
        item_uid = request.item_uid,
        count = request.count,
        "handle_warehouse_access"
    );

    let mut player_data = services
        .player_manager
        .get_or_create_player(&player_id)
        .await;

    let result = match request.action.as_str() {
        "deposit" => player_data.warehouse_deposit(request.item_uid, request.count),
        "withdraw" => player_data.warehouse_withdraw(request.item_uid, request.count),
        _ => Err(ItemError::Unknown),
    };

    match result {
        Ok(()) => {
            services
                .player_manager
                .save_player(&player_id, player_data.clone())
                .await;

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
        Err(e) => {
            connection.queue_message(
                MessageType::WarehouseAccessRes,
                packet.header.seq,
                WarehouseAccessRes {
                    ok: false,
                    error_code: e.as_str().to_string(),
                },
            )?;
        }
    }

    Ok(())
}

/// 处理添加物品请求（测试用）
pub async fn handle_item_add(
    services: &ServiceContext,
    connection: &ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(player_id) = connection.ensure_authenticated(packet.header.seq)? else {
        return Ok(());
    };

    let request = match packet.decode_body::<ItemAddReq>("INVALID_ITEM_ADD_BODY") {
        Ok(value) => value,
        Err(error_code) => {
            connection.queue_error(packet.header.seq, error_code, "invalid item add body")?;
            return Ok(());
        }
    };

    info!(
        session_id = connection.session.id,
        player_id = %player_id,
        item_id = request.item_id,
        count = request.count,
        binded = request.binded,
        "handle_item_add"
    );

    // 获取玩家数据
    let config_tables = services.config_tables.snapshot().await;
    let mut player_data = services
        .player_manager
        .get_or_create_player(&player_id)
        .await;

    // 检查物品配置是否存在
    if config_tables.item_table.get(request.item_id).is_none() {
        connection.queue_message(
            MessageType::ItemAddRes,
            packet.header.seq,
            ItemAddRes {
                ok: false,
                error_code: "ITEM_NOT_FOUND".to_string(),
                item: None,
            },
        )?;
        return Ok(());
    }

    // 创建物品实例（使用时间戳生成唯一UID）
    let uid = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    let item = crate::core::inventory::Item {
        uid,
        item_id: request.item_id,
        count: request.count,
        binded: request.binded,
    };

    // 添加物品
    match player_data.add_item(item.clone()) {
        Ok(()) => {
            services
                .player_manager
                .save_player(&player_id, player_data.clone())
                .await;

            connection.queue_message(
                MessageType::ItemAddRes,
                packet.header.seq,
                ItemAddRes {
                    ok: true,
                    error_code: String::new(),
                    item: Some(PbItem {
                        uid: item.uid,
                        item_id: item.item_id,
                        count: item.count,
                        binded: item.binded,
                    }),
                },
            )?;

            // 发送背包更新推送
            send_inventory_update_push(connection, &player_data).await;
        }
        Err(e) => {
            connection.queue_message(
                MessageType::ItemAddRes,
                packet.header.seq,
                ItemAddRes {
                    ok: false,
                    error_code: e.as_str().to_string(),
                    item: None,
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
    let Some(player_id) = connection.ensure_authenticated(packet.header.seq)? else {
        return Ok(());
    };

    info!(
        session_id = connection.session.id,
        player_id = %player_id,
        "handle_get_inventory"
    );

    // 获取玩家数据
    let player_data = services
        .player_manager
        .get_or_create_player(&player_id)
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
