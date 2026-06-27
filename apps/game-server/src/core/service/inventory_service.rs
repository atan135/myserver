use tracing::{info, warn};

use crate::core::character_element::{
    CharacterElementApplyResult, CharacterElementChangeSource, CharacterElementError,
    CharacterElementService,
};
use crate::core::context::{ConnectionContext, ServiceContext};
use crate::core::inventory::player_data::PreparedItemUseEffect;
use crate::core::inventory::{EquipSlot, ItemError, PlayerData};
use crate::core::service::character_element_service::queue_character_element_push;
use crate::pb::{
    AttrChangePush, AttrPanel as PbAttrPanel, AttrRecord as PbAttrRecord, GetInventoryRes,
    InventoryUpdatePush, Item as PbItem, ItemAddReq, ItemAddRes, ItemDiscardReq, ItemDiscardRes,
    ItemEquipReq, ItemEquipRes, ItemUseReq, ItemUseRes, VisualChangePush, WarehouseAccessReq,
    WarehouseAccessRes,
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

    // 获取角色玩法数据
    let config_tables = services.config_tables.tables_snapshot().await;
    let mut player_data = services
        .player_manager
        .get_or_create_player(&character_id)
        .await;

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

    // 执行装备操作
    let result = player_data.equip_item(request.item_uid, &config_tables.item_table);

    match result {
        Ok(()) => {
            // 保存更新后的角色玩法数据
            services
                .player_manager
                .save_player(&character_id, player_data.clone())
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

    // 获取角色玩法数据
    let config_tables = services.config_tables.tables_snapshot().await;
    let mut player_data = services
        .player_manager
        .get_or_create_player(&character_id)
        .await;

    let hp_before = player_data.get_hp();
    let prepared_use = player_data.prepare_item_use(request.item_uid, &config_tables.item_table);

    let prepared_use = match prepared_use {
        Ok(prepared_use) => prepared_use,
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
            return Ok(());
        }
    };

    let element_change_result = match apply_prepared_item_element_change(
        &services.character_element_service,
        &character_id,
        &account_player_id,
        &prepared_use,
    )
    .await
    {
        Ok(result) => result,
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
            return Ok(());
        }
    };

    let result = player_data.finalize_prepared_item_use(&prepared_use, &config_tables.item_table);

    match result {
        Ok(()) => {
            let hp_change = player_data.get_hp() - hp_before;

            services
                .player_manager
                .save_player(&character_id, player_data.clone())
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

            if let Some(result) = element_change_result.as_ref() {
                queue_character_element_push(
                    services,
                    connection,
                    &crate::session::AuthenticatedSessionIdentity {
                        account_player_id: account_player_id.clone(),
                        character_id: character_id.clone(),
                        world_id,
                    },
                    result,
                    crate::core::character_push::CharacterPushSource::new(
                        "item_use",
                        format!(
                            "item_id:{}:uid:{}",
                            prepared_use.item_id, prepared_use.item_uid
                        ),
                        "element_change",
                        format!("use item {}", prepared_use.item_id),
                    ),
                )
                .await?;
            }
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

async fn apply_prepared_item_element_change(
    character_element_service: &CharacterElementService,
    character_id: &str,
    account_player_id: &str,
    prepared_use: &crate::core::inventory::player_data::PreparedItemUse,
) -> Result<Option<CharacterElementApplyResult>, CharacterElementError> {
    let PreparedItemUseEffect::CharacterElementChange { change } = &prepared_use.effect else {
        return Ok(None);
    };

    let source = CharacterElementChangeSource::new("item_use")
        .with_source_id(format!(
            "item_id:{}:uid:{}",
            prepared_use.item_id, prepared_use.item_uid
        ))
        .with_operator("player", account_player_id.to_string());
    let reason = format!("use item {}", prepared_use.item_id);

    let result = character_element_service
        .apply_change(character_id, *change, source, Some(reason.as_str()))
        .await?;

    Ok(Some(result))
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

    let mut player_data = services
        .player_manager
        .get_or_create_player(&character_id)
        .await;

    let result = player_data.remove_item(request.item_uid, request.count);

    match result {
        Ok(_item) => {
            services
                .player_manager
                .save_player(&character_id, player_data.clone())
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

    let mut player_data = services
        .player_manager
        .get_or_create_player(&character_id)
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
                .save_player(&character_id, player_data.clone())
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
    let Some(identity) = connection.ensure_authenticated_identity(packet.header.seq)? else {
        return Ok(());
    };
    let account_player_id = identity.account_player_id;
    let character_id = identity.character_id;
    let world_id = identity.world_id;

    let request = match packet.decode_body::<ItemAddReq>("INVALID_ITEM_ADD_BODY") {
        Ok(value) => value,
        Err(error_code) => {
            connection.queue_error(packet.header.seq, error_code, "invalid item add body")?;
            return Ok(());
        }
    };

    info!(
        session_id = connection.session.id,
        account_player_id = %account_player_id,
        character_id = %character_id,
        world_id = ?world_id,
        item_id = request.item_id,
        count = request.count,
        binded = request.binded,
        "handle_item_add"
    );

    // 获取角色玩法数据
    let config_tables = services.config_tables.tables_snapshot().await;
    let mut player_data = services
        .player_manager
        .get_or_create_player(&character_id)
        .await;

    // 检查物品配置是否存在
    let Some(item_row) = config_tables.item_table.get(request.item_id) else {
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
    };

    let item = crate::core::inventory::Item::from_config(
        services.item_uid_generator.next()?,
        request.item_id,
        request.count,
        request.binded,
        Some(&character_id),
        item_row,
        &config_tables.item_table,
    );

    // 添加物品
    match player_data.add_item(item.clone()) {
        Ok(()) => {
            services
                .player_manager
                .save_player(&character_id, player_data.clone())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::character_element::{
        CharacterElementChange, CharacterElementService, CharacterElements, ElementDeltas,
        ElementValues,
    };
    use crate::core::inventory::player_data::{PreparedItemUse, PreparedItemUseEffect};

    #[tokio::test]
    async fn item_element_use_calls_character_element_service_with_stable_source() {
        let service = CharacterElementService::new_in_memory();
        service
            .set_elements(CharacterElements {
                character_id: "chr_0000000000001".to_string(),
                affinity: ElementValues::new(2500, 2500, 2500, 2500),
                mastery: ElementValues::zero(),
            })
            .await;
        let prepared = PreparedItemUse {
            item_uid: 7,
            item_id: 4101,
            effect: PreparedItemUseEffect::CharacterElementChange {
                change: CharacterElementChange::new(
                    ElementDeltas::zero(),
                    ElementDeltas::new(0, 10, 0, 0),
                ),
            },
        };

        apply_prepared_item_element_change(
            &service,
            "chr_0000000000001",
            "plr_0000000000001",
            &prepared,
        )
        .await
        .unwrap();

        let after = service
            .get_elements_for_identity(&crate::session::AuthenticatedSessionIdentity {
                account_player_id: "plr_0000000000001".to_string(),
                character_id: "chr_0000000000001".to_string(),
                world_id: Some(0),
            })
            .await
            .unwrap();
        assert_eq!(after.mastery.fire, 10);

        let logs = service.applied_change_logs().await;
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].source.source_type, "item_use");
        assert_eq!(
            logs[0].source.source_id.as_deref(),
            Some("item_id:4101:uid:7")
        );
        assert_eq!(logs[0].source.operator_type.as_deref(), Some("player"));
        assert_eq!(
            logs[0].source.operator_id.as_deref(),
            Some("plr_0000000000001")
        );
        assert_eq!(logs[0].reason.as_deref(), Some("use item 4101"));
    }

    #[tokio::test]
    async fn item_element_use_failure_does_not_write_log() {
        let service = CharacterElementService::new_in_memory();
        service
            .set_elements(CharacterElements {
                character_id: "chr_0000000000001".to_string(),
                affinity: ElementValues::new(2500, 2500, 2500, 2500),
                mastery: ElementValues::zero(),
            })
            .await;
        let prepared = PreparedItemUse {
            item_uid: 8,
            item_id: 4102,
            effect: PreparedItemUseEffect::CharacterElementChange {
                change: CharacterElementChange::new(
                    ElementDeltas::new(100, 0, 0, 0),
                    ElementDeltas::zero(),
                ),
            },
        };

        let error = apply_prepared_item_element_change(
            &service,
            "chr_0000000000001",
            "plr_0000000000001",
            &prepared,
        )
        .await
        .unwrap_err();

        assert_eq!(error.error_code(), "INVALID_AFFINITY_TOTAL");
        assert!(service.applied_change_logs().await.is_empty());
    }
}
