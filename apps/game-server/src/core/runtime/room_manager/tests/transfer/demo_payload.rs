use super::*;

#[tokio::test]
async fn movement_demo_transfer_restores_movement_payload_consistently() {
    let config_tables = crate::core::config_table::ConfigTableRuntime::load_with_scene_dir(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("csv"),
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scene"),
    )
    .expect("game-server csv fixture should load");
    let factory = Arc::new(GameRoomLogicFactory::new(config_tables.clone()));
    let source = RoomManager::with_policy_registry_and_cleanup_interval(
        crate::match_client::create_match_client_shared(),
        factory.clone(),
        config_tables.room_policy_registry(),
        3600,
    );

    let (tx, _rx) = mpsc::channel(1024);
    source
        .join_room(
            "room-movement-transfer",
            "player-a",
            tx,
            MemberRole::Player,
            Some("movement_demo"),
        )
        .await
        .unwrap();
    source
        .set_ready_state("room-movement-transfer", "player-a", true)
        .await
        .unwrap();
    source
        .start_game("room-movement-transfer", "player-a")
        .await
        .unwrap();
    stop_runtime_for_test(&source, "room-movement-transfer").await;

    source
            .accept_player_input(
                "room-movement-transfer",
                "player-a",
                1,
                "move_dir",
                "{\"dirX\":1.0,\"dirY\":0.0,\"hasClientState\":true,\"clientX\":1.0,\"clientY\":1.0,\"clientFrameId\":1}",
            )
            .await
            .unwrap();
    source
        .process_room_tick("room-movement-transfer", 20)
        .await
        .unwrap();
    source
        .accept_player_input("room-movement-transfer", "player-a", 2, "", "")
        .await
        .unwrap();
    source
        .process_room_tick("room-movement-transfer", 20)
        .await
        .unwrap();

    with_room_mut_for_test(&source, "room-movement-transfer", |room| {
        let member = room
            .members
            .get_mut("player-a")
            .expect("source member should exist");
        member.offline = true;
        member.offline_since = Some(Instant::now());
        room.mark_empty();
    })
    .await;
    source
        .freeze_room_for_transfer("epoch-1", "room-movement-transfer")
        .await
        .unwrap();

    let payload = source
        .export_room_transfer("epoch-1", "room-movement-transfer")
        .await
        .unwrap();
    let checksum = payload.checksum.clone();
    let transfer_state = room_transfer_state_from_payload(&payload).unwrap();
    let logic_json =
        serde_json::from_str::<serde_json::Value>(&transfer_state.logic_state_json).unwrap();
    let movement_json =
        serde_json::from_str::<serde_json::Value>(&transfer_state.movement_state_json).unwrap();

    assert_eq!(logic_json["schema"], "movement-demo.logic.v1");
    assert_eq!(logic_json["tick_count"], 2);
    assert_eq!(logic_json["recipients"], serde_json::json!(["player-a"]));
    assert_eq!(movement_json["schema"], "room-movement-state.v1");
    assert_eq!(movement_json["scene_id"], 1);
    assert_eq!(movement_json["last_snapshot_frame"], 1);
    assert_eq!(movement_json["last_full_sync_frame"], 0);
    assert_eq!(movement_json["movement_control_stop_frames"], 3);
    assert!(movement_json.get("latest_client_state_by_player").is_none());
    assert!(
        movement_json
            .get("missing_control_frames_by_player")
            .is_none()
    );
    assert_eq!(
        movement_json["latest_client_state_by_character"][0]["character_id"],
        "player-a"
    );
    assert_eq!(
        movement_json["missing_control_frames_by_character"][0]["frame_id"],
        1
    );
    assert_eq!(movement_json["entities"][0]["character_id"], "player-a");
    assert_eq!(movement_json["entities"][0]["moving"], true);
    assert_eq!(movement_json["entities"][0]["last_input_frame"], 1);

    let target = RoomManager::with_policy_registry_and_cleanup_interval(
        crate::match_client::create_match_client_shared(),
        factory,
        config_tables.room_policy_registry(),
        3600,
    );
    let imported = target.import_room_transfer(payload).await.unwrap();
    assert_eq!(imported.0, checksum);

    with_room_for_test(&target, "room-movement-transfer", |room| {
        assert_eq!(room.current_frame, 2);
        assert_eq!(room.transfer_state.status, RoomTransferStatus::OwnedByNew);
        let game_state =
            serde_json::from_str::<serde_json::Value>(&room.snapshot().game_state).unwrap();
        assert_eq!(game_state["tick_count"], 2);
        assert_eq!(game_state["entity_count"], 1);
        assert_eq!(game_state["entities"][0]["moving"], true);
        assert_eq!(game_state["entities"][0]["last_input_frame"], 1);
    })
    .await;

    let (reconnect_tx, _reconnect_rx) = mpsc::channel(1024);
    let recovery = target
        .reconnect_room("room-movement-transfer", "player-a", reconnect_tx)
        .await
        .unwrap();
    let movement_recovery = recovery
        .movement_recovery
        .expect("movement recovery should exist after import");
    assert_eq!(movement_recovery.frame_id, 2);
    assert_eq!(movement_recovery.reference_frame_id, 2);
    assert!(movement_recovery.aoi_enabled);
    assert_eq!(movement_recovery.entities.len(), 1);
    assert_eq!(movement_recovery.entities[0].character_id, "player-a");
    assert!(movement_recovery.entities[0].moving);
    assert_eq!(movement_recovery.entities[0].last_input_frame, 1);

    assert_eq!(
        target
            .export_room_transfer("epoch-1", "room-movement-transfer")
            .await,
        Err("ROOM_TRANSFER_OWNED_BY_NEW")
    );

    let mut invalid_json_payload = source
        .export_room_transfer("epoch-1", "room-movement-transfer")
        .await
        .unwrap();
    let mut movement_wrapper =
        serde_json::from_str::<serde_json::Value>(&invalid_json_payload.movement_state_json)
            .unwrap();
    movement_wrapper["movementStateJson"] = serde_json::json!("{bad");
    invalid_json_payload.movement_state_json = movement_wrapper.to_string();
    invalid_json_payload.checksum = room_transfer_checksum(&invalid_json_payload);
    let invalid_json_target = RoomManager::with_policy_registry_and_cleanup_interval(
        crate::match_client::create_match_client_shared(),
        Arc::new(GameRoomLogicFactory::new(config_tables.clone())),
        config_tables.room_policy_registry(),
        3600,
    );
    assert_eq!(
        invalid_json_target
            .import_room_transfer(invalid_json_payload)
            .await,
        Err("ROOM_TRANSFER_INVALID_MOVEMENT_STATE")
    );

    let mut unsupported_schema_payload = source
        .export_room_transfer("epoch-1", "room-movement-transfer")
        .await
        .unwrap();
    let mut movement_wrapper =
        serde_json::from_str::<serde_json::Value>(&unsupported_schema_payload.movement_state_json)
            .unwrap();
    let mut movement_inner = serde_json::from_str::<serde_json::Value>(
        movement_wrapper["movementStateJson"].as_str().unwrap(),
    )
    .unwrap();
    movement_inner["schemaVersion"] = serde_json::json!(2);
    movement_wrapper["movementStateJson"] = serde_json::json!(movement_inner.to_string());
    unsupported_schema_payload.movement_state_json = movement_wrapper.to_string();
    unsupported_schema_payload.checksum = room_transfer_checksum(&unsupported_schema_payload);
    let unsupported_schema_target = RoomManager::with_policy_registry_and_cleanup_interval(
        crate::match_client::create_match_client_shared(),
        Arc::new(GameRoomLogicFactory::new(config_tables)),
        SharedRoomPolicyRegistry::default(),
        3600,
    );
    assert_eq!(
        unsupported_schema_target
            .import_room_transfer(unsupported_schema_payload)
            .await,
        Err("ROOM_TRANSFER_UNSUPPORTED_SCHEMA")
    );
}

#[tokio::test]
async fn combat_demo_transfer_restores_combat_payload_consistently() {
    let config_tables = crate::core::config_table::ConfigTableRuntime::load_with_scene_dir(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("csv"),
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scene"),
    )
    .expect("game-server csv fixture should load");
    let factory = Arc::new(GameRoomLogicFactory::new(config_tables.clone()));
    let source = RoomManager::with_policy_registry_and_cleanup_interval(
        crate::match_client::create_match_client_shared(),
        factory.clone(),
        config_tables.room_policy_registry(),
        3600,
    );

    let (tx_a, _rx_a) = mpsc::channel(1024);
    source
        .join_room(
            "room-combat-transfer",
            "player-a",
            tx_a,
            MemberRole::Player,
            Some("combat_demo"),
        )
        .await
        .unwrap();
    let (tx_b, _rx_b) = mpsc::channel(1024);
    source
        .join_room(
            "room-combat-transfer",
            "player-b",
            tx_b,
            MemberRole::Player,
            Some("combat_demo"),
        )
        .await
        .unwrap();
    source
        .set_ready_state("room-combat-transfer", "player-a", true)
        .await
        .unwrap();
    source
        .set_ready_state("room-combat-transfer", "player-b", true)
        .await
        .unwrap();
    source
        .start_game("room-combat-transfer", "player-a")
        .await
        .unwrap();
    stop_runtime_for_test(&source, "room-combat-transfer").await;

    source
        .accept_player_input(
            "room-combat-transfer",
            "player-a",
            1,
            "combat_cast_skill",
            "{\"skillId\":4,\"targetEntityId\":3}",
        )
        .await
        .unwrap();
    source
        .process_room_tick("room-combat-transfer", 20)
        .await
        .unwrap();
    source
        .accept_player_input(
            "room-combat-transfer",
            "player-a",
            2,
            "combat_apply_buff",
            "{\"buffId\":2,\"targetCharacterId\":\"player-b\",\"durationFrames\":77}",
        )
        .await
        .unwrap();
    source
        .process_room_tick("room-combat-transfer", 20)
        .await
        .unwrap();
    source
        .accept_player_input(
            "room-combat-transfer",
            "player-a",
            3,
            "combat_cast_skill",
            "{\"skillId\":2,\"targetCharacterId\":\"player-b\"}",
        )
        .await
        .unwrap();

    let source_game_state = with_room_for_test(&source, "room-combat-transfer", |room| {
        serde_json::from_str::<serde_json::Value>(&room.snapshot().game_state).unwrap()
    })
    .await;
    let source_player_b = combat_demo_entity_by_character(&source_game_state, "player-b");
    let source_player_b_hp = source_player_b["hp"].as_i64().unwrap();
    source
        .disconnect_room_member("room-combat-transfer", "player-a")
        .await;
    source
        .disconnect_room_member("room-combat-transfer", "player-b")
        .await;
    source
        .freeze_room_for_transfer("epoch-1", "room-combat-transfer")
        .await
        .unwrap();

    let payload = source
        .export_room_transfer("epoch-1", "room-combat-transfer")
        .await
        .unwrap();
    let checksum = payload.checksum.clone();
    let transfer_state = room_transfer_state_from_payload(&payload).unwrap();
    let logic_json =
        serde_json::from_str::<serde_json::Value>(&transfer_state.logic_state_json).unwrap();
    let combat_json =
        serde_json::from_str::<serde_json::Value>(&transfer_state.combat_state_json).unwrap();
    let npc_state = RoomNpcTransferState::from_json(&transfer_state.npc_state_json).unwrap();
    let timer_state =
        RoomRuntimeTimerTransferState::from_json(&transfer_state.timer_state_json).unwrap();

    assert_eq!(logic_json["schema"], "combat-demo.logic.v1");
    assert_eq!(logic_json["tick_count"], 2);
    assert_eq!(logic_json["next_snapshot_frame"], 5);
    assert_eq!(
        logic_json["roster"],
        serde_json::json!(["player-a", "player-b"])
    );
    assert_eq!(combat_json["schema"], "room-combat-ecs.v1");
    assert_eq!(combat_json["last_tick_frame"], 2);
    assert_eq!(combat_json["pending_events_replayed"], false);
    assert!(combat_json.get("pending_events").is_none());
    assert_eq!(combat_json["entities"].as_array().unwrap().len(), 4);
    assert!(combat_json.get("player_entity_map").is_none());
    assert_eq!(
        combat_json["character_entity_map"],
        serde_json::json!([
            {"character_id": "player-a", "entity_id": 1},
            {"character_id": "player-b", "entity_id": 2}
        ])
    );
    assert_eq!(combat_json["skill_slots"][0][3]["skill_id"], 4);
    assert_eq!(combat_json["skill_slots"][0][3]["cooldown_remaining"], 59);
    assert_eq!(combat_json["buff_slots"][1][0]["buff_id"], 2);
    assert_eq!(combat_json["buff_slots"][1][0]["duration_remaining"], 76);
    assert_eq!(combat_json["buff_slots"][1][0]["source_entity"], 1);
    assert_eq!(
        combat_json["pending_skill_requests"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert!(combat_json["move_states"][2]["progress"].as_f64().unwrap() < 1.0);
    assert_eq!(npc_state.schema, "room-transfer.npc-state.v1");
    assert_eq!(npc_state.entities.len(), 2);
    let npc_dummy = npc_state
        .entities
        .iter()
        .find(|entity| entity.entity_id == 3)
        .expect("dummy npc transfer entity should exist");
    let combat_dummy_index = combat_json["entities"]
        .as_array()
        .unwrap()
        .iter()
        .position(|entity| entity["entity_id"] == 3)
        .expect("dummy combat transfer entity should exist");
    let exported_dummy_x = combat_json["positions_x"][combat_dummy_index]
        .as_f64()
        .unwrap();
    let exported_dummy_y = combat_json["positions_y"][combat_dummy_index]
        .as_f64()
        .unwrap();
    assert_eq!(npc_dummy.entity_kind, "monster");
    assert_eq!(npc_dummy.behavior_node, "training_dummy.idle");
    assert_eq!(npc_dummy.position.x, exported_dummy_x as f32);
    assert_eq!(npc_dummy.position.y, exported_dummy_y as f32);
    assert_eq!(
        i64::from(npc_dummy.hp),
        combat_json["healths"][combat_dummy_index]["current"]
            .as_i64()
            .unwrap()
    );
    assert_eq!(
        i64::from(npc_dummy.max_hp),
        combat_json["healths"][combat_dummy_index]["max"]
            .as_i64()
            .unwrap()
    );
    assert!(npc_dummy.target_entity_id.is_none());
    assert!(npc_dummy.threat_entries.is_empty());
    assert!(npc_dummy.blackboard.is_empty());
    assert!(npc_dummy.context.is_empty());
    assert!(npc_dummy.rng_state.is_none());
    assert!(npc_dummy.path.waypoints.is_empty());
    assert!(npc_dummy.wait_timer.is_none());
    assert_eq!(
        npc_dummy
            .skill_cooldowns
            .iter()
            .map(|skill| (skill.skill_id, skill.cooldown_remaining))
            .collect::<Vec<_>>(),
        vec![(1, 0), (5, 0)]
    );
    assert_eq!(timer_state.runtime_summary.owner_kind, "combat-demo");
    assert_eq!(timer_state.runtime_summary.logical_frame, 2);
    assert_eq!(timer_state.runtime_summary.logical_tick, 2);
    assert_eq!(timer_state.scheduler_entries.len(), 1);
    assert_eq!(
        timer_state.scheduler_entries[0].id,
        "combat-demo.snapshot-push"
    );
    assert_eq!(timer_state.scheduler_entries[0].next_frame, 5);
    assert_eq!(timer_state.scheduler_entries[0].interval_frames, Some(5));
    assert_eq!(timer_state.timer_entries.len(), 1);
    assert_eq!(timer_state.timer_entries[0].remaining_frames, 3);

    let target = RoomManager::with_policy_registry_and_cleanup_interval(
        crate::match_client::create_match_client_shared(),
        factory,
        config_tables.room_policy_registry(),
        3600,
    );
    let imported = target.import_room_transfer(payload.clone()).await.unwrap();
    assert_eq!(imported.0, checksum);

    with_room_for_test(&target, "room-combat-transfer", |room| {
        assert_eq!(room.current_frame, 2);
        assert_eq!(room.transfer_state.status, RoomTransferStatus::OwnedByNew);
        let imported_game_state =
            serde_json::from_str::<serde_json::Value>(&room.snapshot().game_state).unwrap();
        assert_eq!(imported_game_state, source_game_state);
        assert_eq!(imported_game_state["next_snapshot_frame"], 5);
    })
    .await;

    let (reconnect_a_tx, _reconnect_a_rx) = mpsc::channel(1024);
    target
        .reconnect_room("room-combat-transfer", "player-a", reconnect_a_tx)
        .await
        .unwrap();
    let (reconnect_b_tx, _reconnect_b_rx) = mpsc::channel(1024);
    target
        .reconnect_room("room-combat-transfer", "player-b", reconnect_b_tx)
        .await
        .unwrap();
    target
        .process_room_tick("room-combat-transfer", 20)
        .await
        .unwrap();

    with_room_for_test(&target, "room-combat-transfer", |room| {
        assert_eq!(room.current_frame, 3);
        let advanced_game_state =
            serde_json::from_str::<serde_json::Value>(&room.snapshot().game_state).unwrap();
        assert_eq!(advanced_game_state["tick_count"], 3);
        assert_eq!(advanced_game_state["next_snapshot_frame"], 5);
        assert_eq!(advanced_game_state["snapshot"]["frame_id"], 3);
        let player_a = combat_demo_entity_by_character(&advanced_game_state, "player-a");
        let fireball = player_a["skills"]
            .as_array()
            .unwrap()
            .iter()
            .find(|skill| skill["skill_id"] == 2)
            .expect("fireball skill should exist");
        assert_eq!(fireball["cooldown_remaining"], 90);
        let player_b = combat_demo_entity_by_character(&advanced_game_state, "player-b");
        assert!(player_b["hp"].as_i64().unwrap() < source_player_b_hp);
        assert_eq!(player_b["buffs"][0]["buff_id"], 2);
        assert_eq!(player_b["buffs"][0]["duration_remaining"], 75);
        let dummy = advanced_game_state["snapshot"]["entities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entity| entity["entity_id"] == 3)
            .unwrap();
        assert!(dummy["x"].as_f64().unwrap() > exported_dummy_x);
    })
    .await;

    let mut invalid_json_payload = payload.clone();
    let mut logic_wrapper =
        serde_json::from_str::<serde_json::Value>(&invalid_json_payload.logic_state_json).unwrap();
    logic_wrapper["combatStateJson"] = serde_json::json!("{bad");
    invalid_json_payload.logic_state_json = logic_wrapper.to_string();
    invalid_json_payload.checksum = room_transfer_checksum(&invalid_json_payload);
    let invalid_json_target = RoomManager::with_policy_registry_and_cleanup_interval(
        crate::match_client::create_match_client_shared(),
        Arc::new(GameRoomLogicFactory::new(config_tables.clone())),
        config_tables.room_policy_registry(),
        3600,
    );
    assert_eq!(
        invalid_json_target
            .import_room_transfer(invalid_json_payload)
            .await,
        Err("ROOM_TRANSFER_INVALID_COMBAT_STATE")
    );

    let mut mismatched_npc_payload = payload.clone();
    let mut logic_wrapper =
        serde_json::from_str::<serde_json::Value>(&mismatched_npc_payload.logic_state_json)
            .unwrap();
    let mut npc_inner =
        serde_json::from_str::<serde_json::Value>(logic_wrapper["npcStateJson"].as_str().unwrap())
            .unwrap();
    npc_inner["entities"][0]["position"]["x"] = serde_json::json!(999.0);
    logic_wrapper["npcStateJson"] = serde_json::json!(npc_inner.to_string());
    mismatched_npc_payload.logic_state_json = logic_wrapper.to_string();
    mismatched_npc_payload.checksum = room_transfer_checksum(&mismatched_npc_payload);
    let mismatched_npc_target = RoomManager::with_policy_registry_and_cleanup_interval(
        crate::match_client::create_match_client_shared(),
        Arc::new(GameRoomLogicFactory::new(config_tables.clone())),
        config_tables.room_policy_registry(),
        3600,
    );
    assert_eq!(
        mismatched_npc_target
            .import_room_transfer(mismatched_npc_payload)
            .await,
        Err("ROOM_TRANSFER_INVALID_NPC_STATE")
    );

    let mut duplicate_npc_payload = payload.clone();
    let mut logic_wrapper =
        serde_json::from_str::<serde_json::Value>(&duplicate_npc_payload.logic_state_json).unwrap();
    let mut npc_inner =
        serde_json::from_str::<serde_json::Value>(logic_wrapper["npcStateJson"].as_str().unwrap())
            .unwrap();
    let first_entity = npc_inner["entities"][0].clone();
    npc_inner["entities"]
        .as_array_mut()
        .unwrap()
        .push(first_entity);
    logic_wrapper["npcStateJson"] = serde_json::json!(npc_inner.to_string());
    duplicate_npc_payload.logic_state_json = logic_wrapper.to_string();
    duplicate_npc_payload.checksum = room_transfer_checksum(&duplicate_npc_payload);
    let duplicate_npc_target = RoomManager::with_policy_registry_and_cleanup_interval(
        crate::match_client::create_match_client_shared(),
        Arc::new(GameRoomLogicFactory::new(config_tables.clone())),
        config_tables.room_policy_registry(),
        3600,
    );
    assert_eq!(
        duplicate_npc_target
            .import_room_transfer(duplicate_npc_payload)
            .await,
        Err("ROOM_TRANSFER_INVALID_NPC_STATE")
    );

    let mut unsupported_schema_payload = payload;
    let mut logic_wrapper =
        serde_json::from_str::<serde_json::Value>(&unsupported_schema_payload.logic_state_json)
            .unwrap();
    let mut npc_inner =
        serde_json::from_str::<serde_json::Value>(logic_wrapper["npcStateJson"].as_str().unwrap())
            .unwrap();
    npc_inner["schemaVersion"] = serde_json::json!(2);
    logic_wrapper["npcStateJson"] = serde_json::json!(npc_inner.to_string());
    unsupported_schema_payload.logic_state_json = logic_wrapper.to_string();
    unsupported_schema_payload.checksum = room_transfer_checksum(&unsupported_schema_payload);
    let unsupported_schema_target = RoomManager::with_policy_registry_and_cleanup_interval(
        crate::match_client::create_match_client_shared(),
        Arc::new(GameRoomLogicFactory::new(config_tables)),
        SharedRoomPolicyRegistry::default(),
        3600,
    );
    assert_eq!(
        unsupported_schema_target
            .import_room_transfer(unsupported_schema_payload)
            .await,
        Err("ROOM_TRANSFER_UNSUPPORTED_SCHEMA")
    );
}
