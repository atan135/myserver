use super::*;

#[tokio::test]
async fn disconnect_path_releases_previous_outbound_sender() {
    let factory = RecordingRoomLogicFactory::default();
    let manager = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(factory),
    );
    let (tx, mut rx) = mpsc::channel(1024);

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
    while rx.try_recv().is_ok() {}

    manager
        .disconnect_room_member("room-test", "player-a")
        .await;

    let closed = tokio::time::timeout(Duration::from_millis(50), rx.recv())
        .await
        .expect("previous outbound receiver should close after disconnect");
    assert!(closed.is_none());
}

#[tokio::test]
async fn offline_player_index_tracks_disconnect_leave_and_reconnect() {
    let (manager, _factory, _receivers) =
        setup_started_room("default_match", &["player-a", "player-b"]).await;

    manager
        .disconnect_room_member("room-test", "player-a")
        .await;
    assert_eq!(
        character_room_index_for_test(&manager, "player-a").await,
        Some("room-test".to_string())
    );
    assert_eq!(
        offline_character_index_for_test(&manager, "player-a").await,
        Some("room-test".to_string())
    );
    assert_eq!(
        manager.find_room_by_offline_character("player-a").await,
        Some("room-test".to_string())
    );

    let (reconnect_tx, _reconnect_rx) = mpsc::channel(1024);
    manager
        .reconnect_room("room-test", "player-a", reconnect_tx)
        .await
        .unwrap();
    assert_eq!(
        character_room_index_for_test(&manager, "player-a").await,
        Some("room-test".to_string())
    );
    assert_eq!(
        offline_character_index_for_test(&manager, "player-a").await,
        None
    );
    assert_eq!(
        manager.find_room_by_offline_character("player-a").await,
        None
    );

    manager.leave_room("room-test", "player-a").await;
    assert_eq!(
        manager.find_room_by_offline_character("player-a").await,
        Some("room-test".to_string())
    );
    assert_eq!(
        offline_character_index_for_test(&manager, "player-a").await,
        Some("room-test".to_string())
    );
}

#[tokio::test]
async fn cleanup_expired_offline_characters_removes_character_indexes() {
    let (manager, _factory, _receivers) =
        setup_started_room("default_match", &["player-a", "player-b"]).await;

    manager
        .disconnect_room_member("room-test", "player-a")
        .await;
    with_room_mut_for_test(&manager, "room-test", |room| {
        let member = room.members.get_mut("player-a").unwrap();
        member.offline_since = Some(Instant::now() - Duration::from_secs(120));
    })
    .await;

    manager.cleanup_expired_offline_characters().await;

    assert_eq!(
        character_room_index_for_test(&manager, "player-a").await,
        None
    );
    assert_eq!(
        offline_character_index_for_test(&manager, "player-a").await,
        None
    );
    assert_eq!(
        manager.find_room_by_offline_character("player-a").await,
        None
    );
    with_room_for_test(&manager, "room-test", |room| {
        assert!(!room.members.contains_key("player-a"));
        assert!(room.members.contains_key("player-b"));
    })
    .await;
}

#[tokio::test]
async fn cleanup_task_removes_player_index_before_room_id_reuse() {
    let factory = RecordingRoomLogicFactory::default();
    let manager = RoomManager::with_match_client_and_cleanup_interval(
        crate::match_client::create_match_client_shared(),
        Arc::new(factory),
        1,
    );

    let (tx, _rx) = mpsc::channel(1024);
    manager
        .join_room(
            "room-reused-index",
            "player-a",
            tx,
            MemberRole::Player,
            Some("disposable_match"),
        )
        .await
        .unwrap();
    manager.leave_room("room-reused-index", "player-a").await;

    for _ in 0..30 {
        if !manager.room_exists("room-reused-index").await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!manager.room_exists("room-reused-index").await);
    assert_eq!(
        character_room_index_for_test(&manager, "player-a").await,
        None
    );
    assert_eq!(
        offline_character_index_for_test(&manager, "player-a").await,
        None
    );

    let (tx, _rx) = mpsc::channel(1024);
    manager
        .join_room(
            "room-reused-index",
            "player-a",
            tx,
            MemberRole::Player,
            Some("default_match"),
        )
        .await
        .unwrap();
    assert_eq!(
        character_room_index_for_test(&manager, "player-a").await,
        Some("room-reused-index".to_string())
    );
    assert_eq!(
        offline_character_index_for_test(&manager, "player-a").await,
        None
    );
}

#[tokio::test]
async fn send_to_character_uses_index_and_self_heals_stale_entry() {
    let factory = RecordingRoomLogicFactory::default();
    let manager = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(factory),
    );
    let (tx, mut rx) = mpsc::channel(1024);
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
    while rx.try_recv().is_ok() {}

    manager
        .send_to_character("player-a", MessageType::GameMessagePush, vec![1, 2, 3])
        .await
        .unwrap();
    let delivered = rx
        .try_recv()
        .expect("indexed character should receive message");
    assert_eq!(delivered.message_type, MessageType::GameMessagePush);
    assert_eq!(delivered.body, vec![1, 2, 3]);

    {
        let mut rooms = manager.rooms.write().await;
        rooms.remove("room-test");
    }
    manager
        .send_to_character("player-a", MessageType::GameMessagePush, vec![4, 5, 6])
        .await
        .unwrap();
    assert!(rx.try_recv().is_err());
    assert_eq!(
        character_room_index_for_test(&manager, "player-a").await,
        None
    );
    assert_eq!(
        offline_character_index_for_test(&manager, "player-a").await,
        None
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_cross_room_runtime_paths_keep_room_state_isolated() {
    let room_count = 16;
    let factory = RecordingRoomLogicFactory::default();
    let manager = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(factory.clone()),
    );
    let mut receivers = Vec::new();

    for room_idx in 0..room_count {
        let characters = [
            format!("player-{room_idx}-a"),
            format!("player-{room_idx}-b"),
        ];
        setup_started_room_with_id(
            &manager,
            &format!("room-{room_idx}"),
            &characters,
            &mut receivers,
        )
        .await;
    }

    let manager = Arc::new(manager);
    let mut handles = Vec::new();
    for room_idx in 0..room_count {
        let manager = Arc::clone(&manager);
        handles.push(tokio::spawn(async move {
            let room_id = format!("room-{room_idx}");
            let player_a = format!("player-{room_idx}-a");
            let player_b = format!("player-{room_idx}-b");

            manager
                .accept_player_input(&room_id, &player_a, 1, "move", "{\"x\":1}")
                .await
                .unwrap();
            manager
                .accept_player_input(&room_id, &player_b, 1, "move", "{\"x\":2}")
                .await
                .unwrap();
            let tick = manager
                .process_room_tick(&room_id, 10)
                .await
                .expect("room should advance after both inputs");
            assert_eq!(tick.0.room_id, room_id);
            assert_eq!(tick.0.frame_id, 1);
            assert_eq!(tick.0.inputs.len(), 2);

            manager.disconnect_room_member(&room_id, &player_b).await;
            assert_eq!(
                manager.find_room_by_offline_character(&player_b).await,
                Some(room_id.clone())
            );

            let (tx, _rx) = mpsc::channel(1024);
            let recovery = manager
                .reconnect_room(&room_id, &player_b, tx)
                .await
                .unwrap();
            assert_eq!(recovery.snapshot.room_id, room_id);
            assert_eq!(
                manager.find_room_by_offline_character(&player_b).await,
                None
            );
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    assert_eq!(manager.room_count().await, room_count);
    assert_eq!(factory.recorded_ticks().len(), room_count);
    for room_idx in 0..room_count {
        let player_a = format!("player-{room_idx}-a");
        let player_b = format!("player-{room_idx}-b");
        assert_eq!(
            character_room_index_for_test(&manager, &player_a).await,
            Some(format!("room-{room_idx}"))
        );
        assert_eq!(
            character_room_index_for_test(&manager, &player_b).await,
            Some(format!("room-{room_idx}"))
        );
        assert_eq!(
            offline_character_index_for_test(&manager, &player_b).await,
            None
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn indexed_player_lookup_scales_without_cross_room_scan_fallback() {
    let room_count = 24;
    let factory = RecordingRoomLogicFactory::default();
    let manager = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(factory),
    );
    let mut receivers = Vec::new();

    for room_idx in 0..room_count {
        let character_id = format!("indexed-player-{room_idx}");
        let (tx, rx) = mpsc::channel(1024);
        receivers.push(rx);
        manager
            .join_room(
                &format!("indexed-room-{room_idx}"),
                &character_id,
                tx,
                MemberRole::Player,
                Some("movement_demo"),
            )
            .await
            .unwrap();
    }

    for receiver in &mut receivers {
        while receiver.try_recv().is_ok() {}
    }

    let manager = Arc::new(manager);
    let mut send_handles = Vec::new();
    for room_idx in 0..room_count {
        let manager = Arc::clone(&manager);
        send_handles.push(tokio::spawn(async move {
            manager
                .send_to_character(
                    &format!("indexed-player-{room_idx}"),
                    MessageType::GameMessagePush,
                    vec![room_idx as u8],
                )
                .await
                .unwrap();
        }));
    }
    for handle in send_handles {
        handle.await.unwrap();
    }

    for (room_idx, receiver) in receivers.iter_mut().enumerate() {
        let delivered = receiver
            .try_recv()
            .expect("indexed send should deliver to the target room");
        assert_eq!(delivered.message_type, MessageType::GameMessagePush);
        assert_eq!(delivered.body, vec![room_idx as u8]);
        assert!(receiver.try_recv().is_err());
    }

    let mut disconnect_handles = Vec::new();
    for room_idx in 0..room_count {
        let manager = Arc::clone(&manager);
        disconnect_handles.push(tokio::spawn(async move {
            let room_id = format!("indexed-room-{room_idx}");
            let character_id = format!("indexed-player-{room_idx}");
            manager
                .disconnect_room_member(&room_id, &character_id)
                .await;
            assert_eq!(
                manager.find_room_by_offline_character(&character_id).await,
                Some(room_id)
            );
        }));
    }
    for handle in disconnect_handles {
        handle.await.unwrap();
    }

    for room_idx in 0..room_count {
        assert_eq!(
            offline_character_index_for_test(&manager, &format!("indexed-player-{room_idx}")).await,
            Some(format!("indexed-room-{room_idx}"))
        );
    }
}

#[tokio::test]
async fn all_players_disconnected_can_reconnect_before_offline_ttl_expires() {
    let (manager, _factory, _receivers) =
        setup_started_room("default_match", &["player-a", "player-b"]).await;

    manager
        .disconnect_room_member("room-test", "player-a")
        .await;
    let disconnected = manager
        .disconnect_room_member("room-test", "player-b")
        .await;
    assert_eq!(
        disconnected.snapshot.expect("disconnect snapshot").state,
        "in_game"
    );

    manager.cleanup_expired_offline_characters().await;

    let (reconnect_a_tx, _reconnect_a_rx) = mpsc::channel(1024);
    let reconnect_a = manager
        .reconnect_room("room-test", "player-a", reconnect_a_tx)
        .await
        .unwrap();
    assert_eq!(reconnect_a.snapshot.state, "in_game");

    let (reconnect_b_tx, _reconnect_b_rx) = mpsc::channel(1024);
    let reconnect_b = manager
        .reconnect_room("room-test", "player-b", reconnect_b_tx)
        .await
        .unwrap();
    assert_eq!(reconnect_b.snapshot.state, "in_game");
}

#[tokio::test]
async fn room_tick_pauses_when_all_players_are_offline() {
    let (manager, factory, _receivers) = setup_started_room("movement_demo", &["player-a"]).await;

    let disconnected = manager
        .disconnect_room_member("room-test", "player-a")
        .await;
    assert_eq!(
        disconnected.snapshot.expect("disconnect snapshot").state,
        "in_game"
    );

    let progressed = manager.process_room_tick("room-test", 10).await;
    assert!(progressed.is_none());
    assert!(factory.recorded_ticks().is_empty());

    with_room_for_test(&manager, "room-test", |room| {
        assert_eq!(room.current_frame, 0);
    })
    .await;
}

#[tokio::test]
async fn reconnect_after_global_disconnect_restarts_wait_timeout_window() {
    let (manager, factory, _receivers) =
        setup_started_room("default_match", &["player-a", "player-b"]).await;

    manager
        .accept_player_input("room-test", "player-a", 1, "move", "{\"x\":1}")
        .await
        .unwrap();
    let progressed = manager.process_room_tick("room-test", 10).await;
    assert!(progressed.is_none());

    with_room_mut_for_test(&manager, "room-test", |room| {
        room.wait_started_at = Some(Instant::now() - Duration::from_millis(500));
    })
    .await;

    manager
        .disconnect_room_member("room-test", "player-a")
        .await;
    manager
        .disconnect_room_member("room-test", "player-b")
        .await;

    let offline_tick = manager.process_room_tick("room-test", 10).await;
    assert!(offline_tick.is_none());

    let (reconnect_a_tx, _reconnect_a_rx) = mpsc::channel(1024);
    manager
        .reconnect_room("room-test", "player-a", reconnect_a_tx)
        .await
        .unwrap();
    let (reconnect_b_tx, _reconnect_b_rx) = mpsc::channel(1024);
    manager
        .reconnect_room("room-test", "player-b", reconnect_b_tx)
        .await
        .unwrap();

    let progressed_after_reconnect = manager.process_room_tick("room-test", 10).await;
    assert!(progressed_after_reconnect.is_none());
    assert!(factory.recorded_ticks().is_empty());
}

#[tokio::test]
async fn drop_after_misses_marks_player_offline_after_threshold() {
    let (sender, _receiver) = mpsc::channel(1024);
    let ticks = Arc::new(StdMutex::new(Vec::new()));
    let inputs = Arc::new(StdMutex::new(Vec::new()));
    let mut room = Room::new(
        "room-test".to_string(),
        "player-a".to_string(),
        "default_match".to_string(),
        Box::new(RecordingRoomLogic {
            ticks,
            inputs,
            imported_transfer_states: Arc::new(StdMutex::new(Vec::new())),
            state: "recording-state-v1".to_string(),
        }),
    );
    room.members.insert(
        "player-a".to_string(),
        RoomMemberState {
            character_id: "player-a".to_string(),
            ready: true,
            sender,
            close_state: ConnectionCloseState::new(),
            offline: false,
            offline_since: None,
            role: MemberRole::Player,
            syncing: false,
        },
    );

    let participants = vec!["player-a".to_string()];
    let policy = RoomRuntimePolicy {
        missing_input_strategy: MissingInputStrategy::DropAfterMisses,
        ..RoomRuntimePolicy::default_match()
    };

    for frame_id in 1..=MAX_MISSING_INPUT_STREAK_BEFORE_OFFLINE {
        let (resolved, newly_offline_characters) =
            resolve_tick_inputs(&mut room, &participants, frame_id, &policy);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].frame_id, frame_id);
        if frame_id < MAX_MISSING_INPUT_STREAK_BEFORE_OFFLINE {
            assert!(newly_offline_characters.is_empty());
        } else {
            assert_eq!(newly_offline_characters, vec!["player-a".to_string()]);
        }
    }

    let member = room.members.get("player-a").expect("player should exist");
    assert!(member.offline);
    assert_eq!(
        room.missing_input_streaks.get("player-a").copied(),
        Some(MAX_MISSING_INPUT_STREAK_BEFORE_OFFLINE)
    );
}
