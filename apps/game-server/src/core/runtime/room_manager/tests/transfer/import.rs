use super::*;

#[tokio::test]
async fn import_room_transfer_rejects_bad_checksum() {
    let (source, _factory, _receivers) =
        setup_started_room("default_match", &["player-a", "player-b"]).await;
    source.disconnect_room_member("room-test", "player-a").await;
    source.disconnect_room_member("room-test", "player-b").await;
    source
        .freeze_room_for_transfer("epoch-1", "room-test")
        .await
        .unwrap();
    let mut payload = source
        .export_room_transfer("epoch-1", "room-test")
        .await
        .unwrap();
    payload.checksum = "bad-checksum".to_string();

    let target_factory = RecordingRoomLogicFactory::default();
    let target = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(target_factory),
    );

    let result = target.import_room_transfer(payload).await;

    assert_eq!(result, Err("ROOM_TRANSFER_CHECKSUM_MISMATCH"));
    assert!(!target.room_exists("room-test").await);
}

#[tokio::test]
async fn import_room_transfer_rejects_logic_without_transfer_contract() {
    let (source, _factory, _receivers) =
        setup_started_room("default_match", &["player-a", "player-b"]).await;
    source.disconnect_room_member("room-test", "player-a").await;
    source.disconnect_room_member("room-test", "player-b").await;
    source
        .freeze_room_for_transfer("epoch-1", "room-test")
        .await
        .unwrap();
    let payload = source
        .export_room_transfer("epoch-1", "room-test")
        .await
        .unwrap();

    let target = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(UnsupportedTransferRoomLogicFactory),
    );

    let result = target.import_room_transfer(payload).await;

    assert_eq!(result, Err("UNSUPPORTED_ROOM_TRANSFER"));
    assert!(!target.room_exists("room-test").await);
}

#[tokio::test]
async fn import_room_transfer_rejects_unsupported_schema_without_creating_room() {
    let (source, _factory, _receivers) =
        setup_started_room("default_match", &["player-a", "player-b"]).await;
    source.disconnect_room_member("room-test", "player-a").await;
    source.disconnect_room_member("room-test", "player-b").await;
    source
        .freeze_room_for_transfer("epoch-1", "room-test")
        .await
        .unwrap();
    let mut payload = source
        .export_room_transfer("epoch-1", "room-test")
        .await
        .unwrap();
    let mut logic_state =
        serde_json::from_str::<serde_json::Value>(&payload.logic_state_json).unwrap();
    logic_state["schemaVersion"] = serde_json::json!(ROOM_TRANSFER_SCHEMA_VERSION + 1);
    payload.logic_state_json = logic_state.to_string();
    payload.checksum = room_transfer_checksum(&payload);

    let target_factory = RecordingRoomLogicFactory::default();
    let target = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(target_factory),
    );

    let result = target.import_room_transfer(payload).await;

    assert_eq!(result, Err("ROOM_TRANSFER_UNSUPPORTED_SCHEMA"));
    assert!(!target.room_exists("room-test").await);
}

#[tokio::test]
async fn import_room_transfer_rejects_invalid_timer_wrapper_contract() {
    let (source, _factory, _receivers) =
        setup_started_room("default_match", &["player-a", "player-b"]).await;
    source.disconnect_room_member("room-test", "player-a").await;
    source.disconnect_room_member("room-test", "player-b").await;
    source
        .freeze_room_for_transfer("epoch-1", "room-test")
        .await
        .unwrap();
    let payload = source
        .export_room_transfer("epoch-1", "room-test")
        .await
        .unwrap();
    let base_timers =
        serde_json::from_str::<serde_json::Value>(&payload.runtime_timers_json).unwrap();

    let mut unsupported_schema = base_timers.clone();
    unsupported_schema["schema"] = serde_json::json!("room-transfer.runtime-timers.v2");

    let mut unsupported_version = base_timers.clone();
    unsupported_version["schemaVersion"] = serde_json::json!(ROOM_TRANSFER_SCHEMA_VERSION + 1);

    let mut non_string_timer_state = base_timers.clone();
    non_string_timer_state["timerStateJson"] = serde_json::json!({});

    let mut missing_runtime_summary = base_timers.clone();
    missing_runtime_summary
        .as_object_mut()
        .unwrap()
        .remove("runtimeSummary");

    let mut bad_runtime_summary_type = base_timers.clone();
    bad_runtime_summary_type["runtimeSummary"]["hasWaitStarted"] = serde_json::json!("false");

    let mut bad_snapshot_interval = base_timers.clone();
    bad_snapshot_interval["runtimeSummary"]["snapshotIntervalFrames"] = serde_json::json!(0);

    let mut timer_inner_unsupported_schema =
        serde_json::from_str::<serde_json::Value>(base_timers["timerStateJson"].as_str().unwrap())
            .unwrap();
    timer_inner_unsupported_schema["schema"] =
        serde_json::json!("room-transfer.runtime-timer-state.v2");
    let mut bad_timer_inner_schema = base_timers.clone();
    bad_timer_inner_schema["timerStateJson"] =
        serde_json::json!(timer_inner_unsupported_schema.to_string());

    let mut timer_inner_missing_owner =
        serde_json::from_str::<serde_json::Value>(base_timers["timerStateJson"].as_str().unwrap())
            .unwrap();
    timer_inner_missing_owner["runtimeSummary"]["ownerKind"] = serde_json::json!("");
    let mut bad_timer_inner_owner = base_timers.clone();
    bad_timer_inner_owner["timerStateJson"] =
        serde_json::json!(timer_inner_missing_owner.to_string());

    let mut timer_inner_bad_interval =
        serde_json::from_str::<serde_json::Value>(base_timers["timerStateJson"].as_str().unwrap())
            .unwrap();
    timer_inner_bad_interval["timerEntries"][0]["repeatIntervalFrames"] = serde_json::json!(0);
    let mut bad_timer_inner_interval = base_timers.clone();
    bad_timer_inner_interval["timerStateJson"] =
        serde_json::json!(timer_inner_bad_interval.to_string());

    let mut timer_inner_duplicate_id =
        serde_json::from_str::<serde_json::Value>(base_timers["timerStateJson"].as_str().unwrap())
            .unwrap();
    timer_inner_duplicate_id["schedulerEntries"] = serde_json::json!([{
        "id": "recording-timer",
        "schedulerKind": "duplicate",
        "nextFrame": 1
    }]);
    let mut bad_timer_inner_duplicate_id = base_timers;
    bad_timer_inner_duplicate_id["timerStateJson"] =
        serde_json::json!(timer_inner_duplicate_id.to_string());

    let target_factory = RecordingRoomLogicFactory::default();
    let target = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(target_factory),
    );
    for (runtime_timers_json, expected) in [
        (unsupported_schema, "ROOM_TRANSFER_UNSUPPORTED_SCHEMA"),
        (unsupported_version, "ROOM_TRANSFER_UNSUPPORTED_SCHEMA"),
        (non_string_timer_state, "ROOM_TRANSFER_INVALID_TIMER_STATE"),
        (missing_runtime_summary, "ROOM_TRANSFER_INVALID_TIMER_STATE"),
        (
            bad_runtime_summary_type,
            "ROOM_TRANSFER_INVALID_TIMER_STATE",
        ),
        (bad_snapshot_interval, "ROOM_TRANSFER_INVALID_TIMER_STATE"),
        (bad_timer_inner_schema, "ROOM_TRANSFER_UNSUPPORTED_SCHEMA"),
        (bad_timer_inner_owner, "ROOM_TRANSFER_INVALID_TIMER_STATE"),
        (
            bad_timer_inner_interval,
            "ROOM_TRANSFER_INVALID_TIMER_STATE",
        ),
        (
            bad_timer_inner_duplicate_id,
            "ROOM_TRANSFER_INVALID_TIMER_STATE",
        ),
    ] {
        let mut bad_payload = payload.clone();
        bad_payload.runtime_timers_json = runtime_timers_json.to_string();
        bad_payload.checksum = room_transfer_checksum(&bad_payload);

        assert_eq!(
            target.import_room_transfer(bad_payload).await,
            Err(expected)
        );
        assert!(!target.room_exists("room-test").await);
    }
}

#[tokio::test]
async fn import_room_transfer_accepts_empty_timer_state_json() {
    let (source, _factory, _receivers) =
        setup_started_room("default_match", &["player-a", "player-b"]).await;
    source.disconnect_room_member("room-test", "player-a").await;
    source.disconnect_room_member("room-test", "player-b").await;
    source
        .freeze_room_for_transfer("epoch-1", "room-test")
        .await
        .unwrap();
    let mut payload = source
        .export_room_transfer("epoch-1", "room-test")
        .await
        .unwrap();
    let mut timers =
        serde_json::from_str::<serde_json::Value>(&payload.runtime_timers_json).unwrap();
    timers["timerStateJson"] = serde_json::json!("");
    payload.runtime_timers_json = timers.to_string();
    payload.checksum = room_transfer_checksum(&payload);

    let target_factory = RecordingRoomLogicFactory::default();
    let target = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(target_factory.clone()),
    );

    target.import_room_transfer(payload).await.unwrap();

    let imported_states = target_factory.imported_transfer_states();
    assert_eq!(imported_states.len(), 1);
    assert!(imported_states[0].timer_state_json.is_empty());
}

#[tokio::test]
async fn import_room_transfer_restores_basic_room_state() {
    let (source, _source_factory, _receivers) =
        setup_started_room("default_match", &["player-a", "player-b"]).await;
    source.disconnect_room_member("room-test", "player-a").await;
    source.disconnect_room_member("room-test", "player-b").await;
    source
        .freeze_room_for_transfer("epoch-1", "room-test")
        .await
        .unwrap();
    let payload = source
        .export_room_transfer("epoch-1", "room-test")
        .await
        .unwrap();
    let checksum = payload.checksum.clone();

    let target_factory = RecordingRoomLogicFactory::default();
    let target = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(target_factory.clone()),
    );

    let imported = target.import_room_transfer(payload).await.unwrap();

    assert_eq!(imported.0, checksum);
    assert!(target.room_exists("room-test").await);
    let imported_states = target_factory.imported_transfer_states();
    assert_eq!(imported_states.len(), 1);
    let imported_state = &imported_states[0];
    assert_eq!(imported_state.schema_version, ROOM_TRANSFER_SCHEMA_VERSION);
    assert_eq!(imported_state.logic_state_json, "recording-state-v1");
    assert_eq!(
        imported_state.movement_state_json,
        r#"{"movement":"recording-v1"}"#
    );
    assert_eq!(
        imported_state.combat_state_json,
        r#"{"combat":"recording-v1"}"#
    );
    assert_eq!(imported_state.npc_state_json, r#"{"npc":"recording-v1"}"#);
    let timer_state =
        RoomRuntimeTimerTransferState::from_json(&imported_state.timer_state_json).unwrap();
    assert_eq!(
        timer_state.runtime_summary.owner_kind,
        "recording-room-logic"
    );
    assert_eq!(timer_state.timer_entries.len(), 1);
    assert_eq!(timer_state.timer_entries[0].id, "recording-timer");
    assert_eq!(
        timer_state.metadata.get("fixture").map(String::as_str),
        Some("recording-v1")
    );

    let (tx, _rx) = mpsc::channel(1024);
    let snapshot = target
        .reconnect_room("room-test", "player-a", tx)
        .await
        .unwrap()
        .snapshot;
    assert_eq!(snapshot.room_id, "room-test");
}
