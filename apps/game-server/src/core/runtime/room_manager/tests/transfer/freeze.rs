use super::*;

#[tokio::test]
async fn freeze_empty_or_offline_room_for_transfer_succeeds() {
    let (manager, _factory, _receivers) =
        setup_started_room("default_match", &["player-a", "player-b"]).await;
    manager
        .disconnect_room_member("room-test", "player-a")
        .await;
    manager
        .disconnect_room_member("room-test", "player-b")
        .await;

    let result = manager
        .freeze_room_for_transfer("epoch-1", "room-test")
        .await
        .unwrap();

    assert_eq!(result.0, RoomMigrationState::FrozenForTransfer);
    assert!(result.1 > 1);
    assert_eq!(
        manager
            .accept_player_input("room-test", "player-a", 1, "move", "{}")
            .await,
        Err("ROOM_TRANSFER_FROZEN")
    );
}

#[tokio::test]
async fn freeze_online_room_for_transfer_is_rejected() {
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

    let result = manager
        .freeze_room_for_transfer("epoch-1", "room-test")
        .await;

    assert_eq!(result, Err("ROOM_TRANSFER_HAS_ONLINE_MEMBERS"));
}

#[tokio::test]
async fn freeze_room_for_transfer_rejects_invalid_epoch_or_missing_room() {
    let factory = RecordingRoomLogicFactory::default();
    let manager = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(factory),
    );

    assert_eq!(
        manager.freeze_room_for_transfer("", "room-test").await,
        Err("INVALID_ROLLOUT_EPOCH")
    );
    assert_eq!(
        manager
            .freeze_room_for_transfer("epoch-1", "room-missing")
            .await,
        Err("ROOM_NOT_FOUND")
    );
}

#[tokio::test]
async fn freeze_room_for_transfer_rejects_mismatched_epoch_after_freeze() {
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

    let result = manager
        .freeze_room_for_transfer("epoch-2", "room-test")
        .await;

    assert_eq!(result, Err("ROOM_TRANSFER_EPOCH_MISMATCH"));
}

#[tokio::test]
async fn timer_freeze_stops_runtime_tick_and_clears_wait_started() {
    let factory = RecordingRoomLogicFactory::default();
    let manager = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(factory),
    );

    for character_id in ["player-a", "player-b"] {
        let (tx, _rx) = mpsc::channel(1024);
        manager
            .join_room(
                "room-test",
                character_id,
                tx,
                MemberRole::Player,
                Some("default_match"),
            )
            .await
            .unwrap();
        manager
            .set_ready_state("room-test", character_id, true)
            .await
            .unwrap();
    }
    manager.start_game("room-test", "player-a").await.unwrap();

    with_runtime_for_test(&manager, "room-test", |runtime| {
        assert!(runtime.tick_running);
        assert!(runtime.tick_handle.is_some());
    })
    .await;

    manager
        .disconnect_room_member("room-test", "player-a")
        .await;
    manager
        .disconnect_room_member("room-test", "player-b")
        .await;
    with_room_mut_for_test(&manager, "room-test", |room| {
        assert_eq!(room.phase, RoomPhase::InGame);
        room.wait_started_at = Some(Instant::now());
    })
    .await;

    manager
        .freeze_room_for_transfer("epoch-1", "room-test")
        .await
        .unwrap();

    with_runtime_for_test(&manager, "room-test", |runtime| {
        assert!(!runtime.tick_running);
        assert!(runtime.tick_handle.is_none());
    })
    .await;
    with_room_for_test(&manager, "room-test", |room| {
        assert_eq!(room.transfer_state.status, RoomTransferStatus::Frozen);
        assert!(room.wait_started_at.is_none());
    })
    .await;
}

#[tokio::test]
async fn timer_freeze_export_blocks_later_tick_and_emits_runtime_summary() {
    let (manager, factory, _receivers) =
        setup_started_room("default_match", &["player-a", "player-b"]).await;

    manager
        .accept_player_input("room-test", "player-a", 1, "move", "{\"x\":1}")
        .await
        .unwrap();
    manager
        .accept_player_input("room-test", "player-b", 1, "move", "{\"x\":2}")
        .await
        .unwrap();
    assert!(manager.process_room_tick("room-test", 10).await.is_some());
    assert_eq!(factory.recorded_ticks().len(), 1);

    manager
        .disconnect_room_member("room-test", "player-a")
        .await;
    manager
        .disconnect_room_member("room-test", "player-b")
        .await;
    with_room_mut_for_test(&manager, "room-test", |room| {
        room.wait_started_at = Some(Instant::now());
    })
    .await;
    manager
        .freeze_room_for_transfer("epoch-1", "room-test")
        .await
        .unwrap();
    let payload = manager
        .export_room_transfer("epoch-1", "room-test")
        .await
        .unwrap();

    let timers = serde_json::from_str::<serde_json::Value>(&payload.runtime_timers_json).unwrap();
    assert_eq!(timers["schema"], "room-transfer.runtime-timers.v1");
    assert_eq!(
        timers["schemaVersion"].as_u64(),
        Some(u64::from(ROOM_TRANSFER_SCHEMA_VERSION))
    );
    assert!(timers["timerStateJson"].is_string());
    let summary = timers["runtimeSummary"]
        .as_object()
        .expect("runtime summary should be an object");
    assert_eq!(
        summary
            .get("hasEmptySince")
            .and_then(|value| value.as_bool()),
        Some(true)
    );
    assert_eq!(
        summary
            .get("hasWaitStarted")
            .and_then(|value| value.as_bool()),
        Some(false)
    );
    assert!(
        summary
            .get("inputDelayFrames")
            .and_then(|value| value.as_u64())
            .is_some()
    );
    assert!(
        summary
            .get("snapshotIntervalFrames")
            .and_then(|value| value.as_u64())
            .is_some_and(|value| value > 0)
    );

    let last_active_before_tick =
        with_room_for_test(&manager, "room-test", |room| room.last_active_at).await;
    assert!(manager.process_room_tick("room-test", 10).await.is_none());
    assert_eq!(factory.recorded_ticks().len(), 1);
    with_room_for_test(&manager, "room-test", |room| {
        assert_eq!(room.current_frame, 1);
        assert_eq!(room.last_active_at, last_active_before_tick);
        assert!(room.wait_started_at.is_none());
    })
    .await;
}
