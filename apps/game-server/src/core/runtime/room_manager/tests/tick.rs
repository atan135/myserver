use super::*;

#[tokio::test]
async fn fps_change_pushes_room_frame_rate_update_to_online_members() {
    let (manager, _factory, mut receivers) =
        setup_started_room(DISPOSABLE_MATCH_POLICY, &[PLAYER_A, PLAYER_B]).await;
    for receiver in &mut receivers {
        drain_messages_of_type(receiver, MessageType::RoomFrameRatePush);
    }

    manager.disconnect_room_member(TEST_ROOM_ID, PLAYER_B).await;

    let pushes = drain_messages_of_type(&mut receivers[0], MessageType::RoomFrameRatePush);
    assert_eq!(pushes.len(), 1);
    let push = RoomFrameRatePush::decode(pushes[0].body.as_slice()).unwrap();
    assert_eq!(push.room_id, TEST_ROOM_ID);
    assert_eq!(push.fps, 15);
    assert_eq!(push.reason, "runtime_policy_changed");
    assert!(drain_messages_of_type(&mut receivers[1], MessageType::RoomFrameRatePush).is_empty());
}

#[tokio::test]
async fn join_room_pushes_initial_room_frame_rate_update() {
    let factory = RecordingRoomLogicFactory::default();
    let manager = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(factory),
    );

    let (join_tx, mut join_rx) = mpsc::channel(1024);
    manager
        .join_room(
            TEST_ROOM_ID,
            PLAYER_A,
            join_tx,
            MemberRole::Player,
            Some(DEFAULT_POLICY),
        )
        .await
        .unwrap();

    let join_pushes = drain_messages_of_type(&mut join_rx, MessageType::RoomFrameRatePush);
    assert_eq!(join_pushes.len(), 1);
    let push = RoomFrameRatePush::decode(join_pushes[0].body.as_slice()).unwrap();
    assert_eq!(push.room_id, TEST_ROOM_ID);
    assert_eq!(push.fps, 2);
    assert_eq!(push.reason, "runtime_policy_changed");
}

#[tokio::test]
async fn unchanged_fps_does_not_push_duplicate_room_frame_rate_update() {
    let (manager, _factory, mut receivers) =
        setup_started_room(DEFAULT_POLICY, &[PLAYER_A, PLAYER_B]).await;
    for receiver in &mut receivers {
        drain_messages_of_type(receiver, MessageType::RoomFrameRatePush);
    }

    manager.disconnect_room_member(TEST_ROOM_ID, PLAYER_B).await;

    assert!(drain_messages_of_type(&mut receivers[0], MessageType::RoomFrameRatePush).is_empty());
    assert!(drain_messages_of_type(&mut receivers[1], MessageType::RoomFrameRatePush).is_empty());
}

#[tokio::test]
async fn strict_wait_strategy_blocks_until_all_inputs_arrive() {
    let (manager, factory, _receivers) =
        setup_started_room(DEFAULT_POLICY, &[PLAYER_A, PLAYER_B]).await;

    manager
        .accept_player_input(TEST_ROOM_ID, PLAYER_A, 1, "move", "{\"x\":1}")
        .await
        .unwrap();

    let progressed = manager.process_room_tick(TEST_ROOM_ID, 10).await;
    assert!(progressed.is_none());
    assert!(factory.recorded_ticks().is_empty());

    manager
        .accept_player_input(TEST_ROOM_ID, PLAYER_B, 1, "move", "{\"x\":2}")
        .await
        .unwrap();

    let progressed = manager.process_room_tick(TEST_ROOM_ID, 10).await;
    assert!(progressed.is_some());
    let recorded = factory.recorded_ticks();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].0, 1);
    assert_eq!(recorded[0].1.len(), 2);
}

#[tokio::test]
async fn optimistic_strategy_advances_with_partial_inputs() {
    let (manager, factory, _receivers) =
        setup_started_room(MOVEMENT_DEMO_POLICY, &[PLAYER_A, PLAYER_B]).await;

    manager
        .accept_player_input(
            TEST_ROOM_ID,
            PLAYER_A,
            1,
            "move_dir",
            "{\"dirX\":1,\"dirY\":0}",
        )
        .await
        .unwrap();

    let progressed = manager.process_room_tick(TEST_ROOM_ID, 20).await;
    assert!(progressed.is_some());

    let recorded = factory.recorded_ticks();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].0, 1);
    assert_eq!(recorded[0].1.len(), 2);
    assert!(
        recorded[0]
            .1
            .iter()
            .any(|input| input.character_id == PLAYER_B && input.action.is_empty())
    );
}

#[tokio::test]
async fn future_inputs_are_buffered_until_their_frame_is_ready() {
    let (manager, factory, _receivers) =
        setup_started_room(DEFAULT_POLICY, &[PLAYER_A, PLAYER_B]).await;

    manager
        .accept_player_input(TEST_ROOM_ID, PLAYER_A, 2, "move", "{\"x\":20}")
        .await
        .unwrap();
    manager
        .accept_player_input(TEST_ROOM_ID, PLAYER_B, 2, "move", "{\"x\":21}")
        .await
        .unwrap();
    manager
        .accept_player_input(TEST_ROOM_ID, PLAYER_A, 1, "move", "{\"x\":10}")
        .await
        .unwrap();
    manager
        .accept_player_input(TEST_ROOM_ID, PLAYER_B, 1, "move", "{\"x\":11}")
        .await
        .unwrap();

    let first = manager.process_room_tick(TEST_ROOM_ID, 10).await.unwrap();
    assert_eq!(first.0.frame_id, 1);

    let second = manager.process_room_tick(TEST_ROOM_ID, 10).await.unwrap();
    assert_eq!(second.0.frame_id, 2);

    let recorded = factory.recorded_ticks();
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0].0, 1);
    assert_eq!(recorded[1].0, 2);
}

#[tokio::test]
async fn expired_input_frame_is_rejected() {
    let (manager, _factory, _receivers) =
        setup_started_room(DEFAULT_POLICY, &[PLAYER_A, PLAYER_B]).await;

    manager
        .accept_player_input(TEST_ROOM_ID, PLAYER_A, 1, "move", "{\"x\":1}")
        .await
        .unwrap();
    manager
        .accept_player_input(TEST_ROOM_ID, PLAYER_B, 1, "move", "{\"x\":2}")
        .await
        .unwrap();
    let _ = manager.process_room_tick(TEST_ROOM_ID, 10).await;

    let result = manager
        .accept_player_input(TEST_ROOM_ID, PLAYER_A, 1, "move", "{\"x\":3}")
        .await;
    assert_eq!(result, Err("INPUT_FRAME_EXPIRED"));
}

#[tokio::test]
async fn input_too_far_is_rejected() {
    let (manager, _factory, _receivers) =
        setup_started_room(DEFAULT_POLICY, &[PLAYER_A, PLAYER_B]).await;

    let result = manager
        .accept_player_input(TEST_ROOM_ID, PLAYER_A, 5, "move", "{\"x\":1}")
        .await;
    assert_eq!(result, Err("INPUT_FRAME_TOO_FAR"));
}

#[tokio::test]
async fn rejected_input_does_not_trigger_player_input_hook() {
    let (manager, factory, _receivers) =
        setup_started_room(DEFAULT_POLICY, &[PLAYER_A, PLAYER_B]).await;

    let too_far = manager
        .accept_player_input(TEST_ROOM_ID, PLAYER_A, 5, "move", "{\"x\":1}")
        .await;
    assert_eq!(too_far, Err("INPUT_FRAME_TOO_FAR"));
    assert!(factory.recorded_inputs().is_empty());

    manager
        .accept_player_input(TEST_ROOM_ID, PLAYER_A, 1, "move", "{\"x\":1}")
        .await
        .unwrap();
    manager
        .accept_player_input(TEST_ROOM_ID, PLAYER_B, 1, "move", "{\"x\":2}")
        .await
        .unwrap();
    let _ = manager.process_room_tick(TEST_ROOM_ID, 10).await;

    let expired = manager
        .accept_player_input(TEST_ROOM_ID, PLAYER_A, 1, "move", "{\"x\":3}")
        .await;
    assert_eq!(expired, Err("INPUT_FRAME_EXPIRED"));

    let recorded = factory.recorded_inputs();
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0].0, PLAYER_A);
    assert_eq!(recorded[1].0, PLAYER_B);
}

#[tokio::test]
async fn same_frame_input_replaces_previous_one() {
    let (manager, factory, _receivers) =
        setup_started_room(MOVEMENT_DEMO_POLICY, &[PLAYER_A]).await;

    manager
        .accept_player_input(
            TEST_ROOM_ID,
            PLAYER_A,
            1,
            "move_dir",
            "{\"dirX\":1,\"dirY\":0}",
        )
        .await
        .unwrap();
    manager
        .accept_player_input(
            TEST_ROOM_ID,
            PLAYER_A,
            1,
            "face_to",
            "{\"dirX\":0,\"dirY\":1}",
        )
        .await
        .unwrap();

    let _ = manager.process_room_tick(TEST_ROOM_ID, 20).await;
    let recorded = factory.recorded_ticks();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].1.len(), 1);
    assert_eq!(recorded[0].1[0].action, "face_to");
}

#[tokio::test]
async fn reconnect_and_observer_receive_waiting_inputs_with_frame_ids() {
    let factory = RecordingRoomLogicFactory::default();
    let manager = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(factory),
    );

    let (owner_tx, _owner_rx) = mpsc::channel(1024);
    let (other_tx, _other_rx) = mpsc::channel(1024);
    manager
        .join_room(
            TEST_ROOM_ID,
            PLAYER_A,
            owner_tx,
            MemberRole::Player,
            Some(DEFAULT_POLICY),
        )
        .await
        .unwrap();
    manager
        .join_room(
            TEST_ROOM_ID,
            PLAYER_B,
            other_tx,
            MemberRole::Player,
            Some(DEFAULT_POLICY),
        )
        .await
        .unwrap();
    manager
        .set_ready_state(TEST_ROOM_ID, PLAYER_A, true)
        .await
        .unwrap();
    manager
        .set_ready_state(TEST_ROOM_ID, PLAYER_B, true)
        .await
        .unwrap();
    manager.start_game(TEST_ROOM_ID, PLAYER_A).await.unwrap();
    stop_runtime_for_test(&manager, TEST_ROOM_ID).await;

    manager
        .accept_player_input(TEST_ROOM_ID, PLAYER_A, 1, "move", "{\"x\":1}")
        .await
        .unwrap();

    with_room_mut_for_test(&manager, TEST_ROOM_ID, |room| {
        let member = room.members.get_mut(PLAYER_A).unwrap();
        member.offline = true;
        member.offline_since = Some(Instant::now());
    })
    .await;

    let (reconnect_tx, _reconnect_rx) = mpsc::channel(1024);
    let recovery = manager
        .reconnect_room(TEST_ROOM_ID, PLAYER_A, reconnect_tx)
        .await
        .unwrap();
    assert_eq!(recovery.waiting_frame_id, 1);
    assert_eq!(recovery.input_delay_frames, 2);
    assert_eq!(recovery.waiting_inputs.len(), 1);
    assert_eq!(recovery.waiting_inputs[0].frame_id, 1);

    let (observer_tx, _observer_rx) = mpsc::channel(1024);
    let observer = manager
        .join_room_as_observer(TEST_ROOM_ID, OBSERVER_1, observer_tx)
        .await
        .unwrap();
    assert_eq!(observer.waiting_frame_id, 1);
    assert_eq!(observer.waiting_inputs.len(), 1);
    assert_eq!(observer.waiting_inputs[0].frame_id, 1);
}

#[tokio::test]
async fn existing_room_runtime_paths_continue_for_drain_mode_contract() {
    let factory = RecordingRoomLogicFactory::default();
    let manager = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(factory.clone()),
    );

    for character_id in [PLAYER_A, PLAYER_B] {
        let (tx, _rx) = mpsc::channel(1024);
        manager
            .join_room(
                TEST_ROOM_ID,
                character_id,
                tx,
                MemberRole::Player,
                Some(DEFAULT_POLICY),
            )
            .await
            .unwrap();
    }

    manager
        .set_ready_state(TEST_ROOM_ID, PLAYER_A, false)
        .await
        .unwrap();
    manager
        .set_ready_state(TEST_ROOM_ID, PLAYER_A, true)
        .await
        .unwrap();
    manager
        .set_ready_state(TEST_ROOM_ID, PLAYER_B, true)
        .await
        .unwrap();
    manager.start_game(TEST_ROOM_ID, PLAYER_A).await.unwrap();
    stop_runtime_for_test(&manager, TEST_ROOM_ID).await;

    manager
        .accept_player_input(TEST_ROOM_ID, PLAYER_A, 1, "move", "{\"x\":1}")
        .await
        .unwrap();
    manager
        .accept_player_input(TEST_ROOM_ID, PLAYER_B, 1, "move", "{\"x\":2}")
        .await
        .unwrap();
    let progressed = manager.process_room_tick(TEST_ROOM_ID, 10).await;
    assert!(progressed.is_some());
    assert_eq!(factory.recorded_ticks().len(), 1);

    manager.disconnect_room_member(TEST_ROOM_ID, PLAYER_A).await;
    let (reconnect_tx, _reconnect_rx) = mpsc::channel(1024);
    let recovery = manager
        .reconnect_room(TEST_ROOM_ID, PLAYER_A, reconnect_tx)
        .await
        .unwrap();
    assert_eq!(recovery.snapshot.state, "in_game");

    manager.cleanup_expired_offline_characters().await;
    assert!(manager.room_exists(TEST_ROOM_ID).await);

    let (waiting_tx, _waiting_rx) = mpsc::channel(1024);
    manager
        .join_room(
            "room-observer",
            "player-host",
            waiting_tx,
            MemberRole::Player,
            Some(DEFAULT_POLICY),
        )
        .await
        .unwrap();
    let (observer_tx, _observer_rx) = mpsc::channel(1024);
    let observer = manager
        .join_room_as_observer("room-observer", OBSERVER_1, observer_tx)
        .await
        .unwrap();
    assert_eq!(observer.snapshot.room_id, "room-observer");
}

#[tokio::test]
async fn cleanup_removes_runtime_so_reused_room_can_restart_tick() {
    let factory = RecordingRoomLogicFactory::default();
    let manager = RoomManager::with_match_client_and_cleanup_interval(
        crate::match_client::create_match_client_shared(),
        Arc::new(factory),
        1,
    );

    let (tx, _rx) = mpsc::channel(1024);
    manager
        .join_room(
            "room-reused",
            PLAYER_A,
            tx,
            MemberRole::Player,
            Some(DISPOSABLE_MATCH_POLICY),
        )
        .await
        .unwrap();
    assert!(runtime_exists_for_test(&manager, "room-reused").await);
    manager.leave_room("room-reused", PLAYER_A).await;

    for _ in 0..30 {
        if !manager.room_exists("room-reused").await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!manager.room_exists("room-reused").await);
    assert!(!runtime_exists_for_test(&manager, "room-reused").await);

    for character_id in [PLAYER_A, PLAYER_B] {
        let (tx, _rx) = mpsc::channel(1024);
        manager
            .join_room(
                "room-reused",
                character_id,
                tx,
                MemberRole::Player,
                Some(DEFAULT_POLICY),
            )
            .await
            .unwrap();
        manager
            .set_ready_state("room-reused", character_id, true)
            .await
            .unwrap();
    }

    manager.start_game("room-reused", PLAYER_A).await.unwrap();
    with_runtime_for_test(&manager, "room-reused", |runtime| {
        assert!(runtime.tick_running);
        assert!(runtime.tick_handle.is_some());
    })
    .await;
    stop_runtime_for_test(&manager, "room-reused").await;
}

#[tokio::test]
async fn strict_wait_timeout_repeats_last_input() {
    let (manager, factory, _receivers) =
        setup_started_room(DEFAULT_POLICY, &[PLAYER_A, PLAYER_B]).await;

    manager
        .accept_player_input(TEST_ROOM_ID, PLAYER_A, 1, "move", "{\"x\":1}")
        .await
        .unwrap();
    manager
        .accept_player_input(TEST_ROOM_ID, PLAYER_B, 1, "move", "{\"x\":2}")
        .await
        .unwrap();
    let _ = manager.process_room_tick(TEST_ROOM_ID, 10).await;

    manager
        .accept_player_input(TEST_ROOM_ID, PLAYER_A, 2, "move", "{\"x\":3}")
        .await
        .unwrap();
    with_room_mut_for_test(&manager, TEST_ROOM_ID, |room| {
        room.wait_started_at = Some(Instant::now() - Duration::from_millis(500));
    })
    .await;

    let _ = manager.process_room_tick(TEST_ROOM_ID, 10).await;
    let recorded = factory.recorded_ticks();
    assert_eq!(recorded.len(), 2);
    let second_tick = &recorded[1];
    let repeated = second_tick
        .1
        .iter()
        .find(|input| input.character_id == PLAYER_B)
        .unwrap();
    assert_eq!(repeated.frame_id, 2);
    assert_eq!(repeated.action, "move");
    assert_eq!(repeated.payload_json, "{\"x\":2}");
}

#[tokio::test]
async fn disconnect_path_preserves_in_game_waiting_state_for_reconnect() {
    let (manager, _factory, _receivers) =
        setup_started_room(DEFAULT_POLICY, &[PLAYER_A, PLAYER_B]).await;

    manager
        .accept_player_input(TEST_ROOM_ID, PLAYER_A, 1, "move", "{\"x\":1}")
        .await
        .unwrap();

    let disconnected = manager.disconnect_room_member(TEST_ROOM_ID, PLAYER_A).await;
    let snapshot = disconnected.snapshot.expect("disconnect snapshot");
    assert_eq!(snapshot.state, "in_game");
    assert_eq!(snapshot.current_frame_id, 0);

    let (reconnect_tx, _reconnect_rx) = mpsc::channel(1024);
    let recovery = manager
        .reconnect_room(TEST_ROOM_ID, PLAYER_A, reconnect_tx)
        .await
        .unwrap();

    assert_eq!(recovery.waiting_frame_id, 1);
    assert_eq!(recovery.waiting_inputs.len(), 1);
    assert_eq!(recovery.waiting_inputs[0].frame_id, 1);
    assert_eq!(recovery.snapshot.state, "in_game");
}
