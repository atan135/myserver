use super::*;

#[tokio::test]
async fn export_room_transfer_rejects_logic_without_transfer_contract() {
    let manager = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(UnsupportedTransferRoomLogicFactory),
    );
    let (tx, _rx) = mpsc::channel(1024);
    manager
        .join_room(
            "room-test",
            "player-a",
            tx,
            MemberRole::Player,
            Some("default_match"),
        )
        .await
        .unwrap();
    manager
        .disconnect_room_member("room-test", "player-a")
        .await;
    manager
        .freeze_room_for_transfer("epoch-1", "room-test")
        .await
        .unwrap();

    let result = manager.export_room_transfer("epoch-1", "room-test").await;

    assert_eq!(result, Err("UNSUPPORTED_ROOM_TRANSFER"));
    with_room_for_test(&manager, "room-test", |room| {
        assert_eq!(room.transfer_state.status, RoomTransferStatus::Frozen);
        assert!(room.transfer_state.last_transfer_checksum.is_none());
    })
    .await;
}

#[tokio::test]
async fn export_room_transfer_rejects_invalid_epoch_or_missing_room() {
    let factory = RecordingRoomLogicFactory::default();
    let manager = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(factory),
    );

    assert_eq!(
        manager.export_room_transfer("", "room-test").await,
        Err("INVALID_ROLLOUT_EPOCH")
    );
    assert_eq!(
        manager
            .export_room_transfer("epoch-1", "room-missing")
            .await,
        Err("ROOM_NOT_FOUND")
    );
}

#[tokio::test]
async fn export_room_transfer_rejects_room_that_was_not_frozen() {
    let factory = RecordingRoomLogicFactory::default();
    let manager = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(factory),
    );
    let (tx, _rx) = mpsc::channel(1024);
    manager
        .join_room(
            "room-test",
            "player-a",
            tx,
            MemberRole::Player,
            Some("default_match"),
        )
        .await
        .unwrap();

    let result = manager.export_room_transfer("epoch-1", "room-test").await;

    assert_eq!(result, Err("ROOM_TRANSFER_NOT_FROZEN"));
}

#[tokio::test]
async fn export_room_transfer_rejects_mismatched_epoch() {
    let (manager, _factory, _receivers) =
        setup_started_room("default_match", &["player-a", "player-b"]).await;
    manager
        .disconnect_room_member("room-test", "player-a")
        .await;
    manager
        .disconnect_room_member("room-test", "player-b")
        .await;
    manager
        .freeze_room_for_transfer("epoch-1", "room-test")
        .await
        .unwrap();

    let result = manager.export_room_transfer("epoch-2", "room-test").await;

    assert_eq!(result, Err("ROOM_TRANSFER_EPOCH_MISMATCH"));
}

#[tokio::test]
async fn export_room_transfer_checksum_is_deterministic() {
    let (manager, _factory, _receivers) =
        setup_started_room("default_match", &["player-a", "player-b"]).await;
    manager
        .disconnect_room_member("room-test", "player-a")
        .await;
    manager
        .disconnect_room_member("room-test", "player-b")
        .await;
    manager
        .freeze_room_for_transfer("epoch-1", "room-test")
        .await
        .unwrap();

    let payload = manager
        .export_room_transfer("epoch-1", "room-test")
        .await
        .unwrap();

    assert!(!payload.checksum.is_empty());
    assert_eq!(payload.checksum, room_transfer_checksum(&payload));
    let transfer_state = room_transfer_state_from_payload(&payload).unwrap();
    assert_eq!(transfer_state.schema_version, ROOM_TRANSFER_SCHEMA_VERSION);
    assert_eq!(transfer_state.logic_state_json, "recording-state-v1");
    assert_eq!(
        transfer_state.movement_state_json,
        r#"{"movement":"recording-v1"}"#
    );
    assert_eq!(
        transfer_state.combat_state_json,
        r#"{"combat":"recording-v1"}"#
    );
    assert_eq!(transfer_state.npc_state_json, r#"{"npc":"recording-v1"}"#);
    let timer_state =
        RoomRuntimeTimerTransferState::from_json(&transfer_state.timer_state_json).unwrap();
    assert_eq!(timer_state.schema_version, ROOM_TRANSFER_SCHEMA_VERSION);
    assert_eq!(
        timer_state.runtime_summary.owner_kind,
        "recording-room-logic"
    );
    assert_eq!(timer_state.timer_entries.len(), 1);
    assert_eq!(timer_state.timer_entries[0].id, "recording-timer");
    assert_eq!(timer_state.timer_entries[0].remaining_frames, 1);
    assert_eq!(
        timer_state.metadata.get("fixture").map(String::as_str),
        Some("recording-v1")
    );
    assert_eq!(payload.snapshot.as_ref().unwrap().room_id, "room-test");
}

#[tokio::test]
async fn repeated_export_room_transfer_is_idempotent() {
    let (manager, _factory, _receivers) =
        setup_started_room("default_match", &["player-a", "player-b"]).await;
    manager
        .disconnect_room_member("room-test", "player-a")
        .await;
    manager
        .disconnect_room_member("room-test", "player-b")
        .await;
    manager
        .freeze_room_for_transfer("epoch-1", "room-test")
        .await
        .unwrap();

    let first = manager
        .export_room_transfer("epoch-1", "room-test")
        .await
        .unwrap();
    let second = manager
        .export_room_transfer("epoch-1", "room-test")
        .await
        .unwrap();

    assert_eq!(second.room_version, first.room_version);
    assert_eq!(second.checksum, first.checksum);
    assert_eq!(second.checksum, room_transfer_checksum(&second));
}
