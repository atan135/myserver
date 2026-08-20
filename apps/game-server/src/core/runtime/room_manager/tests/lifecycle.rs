use super::*;

#[tokio::test]
async fn room_member_index_keeps_same_account_characters_distinct() {
    let factory = RecordingRoomLogicFactory::default();
    let manager = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(factory),
    );
    let character_a = "account-a:character-1";
    let character_b = "account-a:character-2";

    let (tx_a, _rx_a) = mpsc::channel(1024);
    manager
        .join_room(
            TEST_ROOM_ID,
            character_a,
            tx_a,
            MemberRole::Player,
            Some(DEFAULT_POLICY),
        )
        .await
        .unwrap();
    let (tx_b, _rx_b) = mpsc::channel(1024);
    manager
        .join_room(
            TEST_ROOM_ID,
            character_b,
            tx_b,
            MemberRole::Player,
            Some(DEFAULT_POLICY),
        )
        .await
        .unwrap();

    with_room_for_test(&manager, TEST_ROOM_ID, |room| {
        assert_eq!(room.members.len(), 2);
        assert!(room.members.contains_key(character_a));
        assert!(room.members.contains_key(character_b));
    })
    .await;
    assert_eq!(
        character_room_index_for_test(&manager, character_a).await,
        Some(TEST_ROOM_ID.to_string())
    );
    assert_eq!(
        character_room_index_for_test(&manager, character_b).await,
        Some(TEST_ROOM_ID.to_string())
    );
}

#[tokio::test]
async fn room_exists_reflects_room_creation() {
    let factory = RecordingRoomLogicFactory::default();
    let manager = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(factory),
    );

    assert!(!manager.room_exists(TEST_ROOM_ID).await);

    let (tx, _rx) = mpsc::channel(1024);
    manager
        .join_room(
            TEST_ROOM_ID,
            PLAYER_A,
            tx,
            MemberRole::Player,
            Some(DEFAULT_POLICY),
        )
        .await
        .unwrap();

    assert!(manager.room_exists(TEST_ROOM_ID).await);
}

#[tokio::test]
async fn delayed_disconnect_does_not_mark_rejoined_character_offline() {
    let manager = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(RecordingRoomLogicFactory::default()),
    );
    let (old_tx, _old_rx) = mpsc::channel(1024);
    manager
        .join_room(
            TEST_ROOM_ID,
            PLAYER_A,
            old_tx.clone(),
            MemberRole::Player,
            Some(DEFAULT_POLICY),
        )
        .await
        .unwrap();

    let (new_tx, _new_rx) = mpsc::channel(1024);
    manager
        .join_room(
            TEST_ROOM_ID,
            PLAYER_A,
            new_tx.clone(),
            MemberRole::Player,
            Some(DEFAULT_POLICY),
        )
        .await
        .unwrap();

    let stale_cleanup = manager
        .disconnect_room_member_for_sender(TEST_ROOM_ID, PLAYER_A, &old_tx)
        .await;
    assert!(stale_cleanup.snapshot.is_none());
    with_room_for_test(&manager, TEST_ROOM_ID, |room| {
        let member = room.members.get(PLAYER_A).expect("member should remain");
        assert!(!member.offline);
        assert!(member.sender.same_channel(&new_tx));
    })
    .await;
    assert_eq!(
        character_room_index_for_test(&manager, PLAYER_A).await,
        Some(TEST_ROOM_ID.to_string())
    );

    let current_cleanup = manager
        .disconnect_room_member_for_sender(TEST_ROOM_ID, PLAYER_A, &new_tx)
        .await;
    assert!(current_cleanup.snapshot.is_some());
    with_room_for_test(&manager, TEST_ROOM_ID, |room| {
        assert!(room.members.get(PLAYER_A).unwrap().offline);
    })
    .await;
}

#[tokio::test]
async fn join_room_rejects_unknown_policy_before_creating_room() {
    let manager = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(RecordingRoomLogicFactory::default()),
    );

    assert_eq!(
        manager
            .join_room(
                "main-world-public",
                PLAYER_A,
                mpsc::channel(1024).0,
                MemberRole::Player,
                Some("unknown_policy"),
            )
            .await,
        Err("ROOM_POLICY_UNSUPPORTED")
    );
    assert!(!manager.room_exists("main-world-public").await);
}

#[tokio::test]
async fn fixed_public_world_rejects_non_movement_policy_before_first_creation() {
    let manager = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(RecordingRoomLogicFactory::default()),
    );

    assert_eq!(
        manager
            .join_room(
                "main-world-public",
                PLAYER_A,
                mpsc::channel(1024).0,
                MemberRole::Player,
                Some(DEFAULT_POLICY),
            )
            .await,
        Err("ROOM_POLICY_MISMATCH")
    );
    assert!(!manager.room_exists("main-world-public").await);
}

#[tokio::test]
async fn join_room_rejects_policy_mismatch_for_existing_room() {
    let manager = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(RecordingRoomLogicFactory::default()),
    );

    manager
        .join_room(
            "main-world-public",
            PLAYER_A,
            mpsc::channel(1024).0,
            MemberRole::Player,
            Some(MOVEMENT_DEMO_POLICY),
        )
        .await
        .unwrap();

    assert_eq!(
        manager
            .join_room(
                "main-world-public",
                PLAYER_B,
                mpsc::channel(1024).0,
                MemberRole::Player,
                Some(DEFAULT_POLICY),
            )
            .await,
        Err("ROOM_POLICY_MISMATCH")
    );
    with_room_for_test(&manager, "main-world-public", |room| {
        assert_eq!(room.policy_id, MOVEMENT_DEMO_POLICY);
        assert_eq!(room.members.len(), 1);
    })
    .await;
}

#[tokio::test]
async fn join_room_reuses_fixed_public_room_with_matching_policy() {
    let manager = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(RecordingRoomLogicFactory::default()),
    );

    for character_id in [PLAYER_A, PLAYER_B] {
        manager
            .join_room(
                "main-world-public",
                character_id,
                mpsc::channel(1024).0,
                MemberRole::Player,
                Some(MOVEMENT_DEMO_POLICY),
            )
            .await
            .unwrap();
    }

    with_room_for_test(&manager, "main-world-public", |room| {
        assert_eq!(room.policy_id, MOVEMENT_DEMO_POLICY);
        assert_eq!(room.members.len(), 2);
    })
    .await;
}

#[tokio::test]
async fn public_main_world_starts_without_match_ready_gate_and_accepts_rejoins() {
    let manager = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(RecordingRoomLogicFactory::default()),
    );

    for character_id in [PLAYER_A, PLAYER_B] {
        manager
            .join_room(
                "main-world-public",
                character_id,
                mpsc::channel(1024).0,
                MemberRole::Player,
                Some(MOVEMENT_DEMO_POLICY),
            )
            .await
            .unwrap();
    }

    let started = manager
        .start_game("main-world-public", PLAYER_B)
        .await
        .unwrap();
    assert_eq!(started.state, "in_game");

    let rejoined = manager
        .join_room(
            "main-world-public",
            PLAYER_C,
            mpsc::channel(1024).0,
            MemberRole::Player,
            Some(MOVEMENT_DEMO_POLICY),
        )
        .await
        .unwrap();
    assert_eq!(rejoined.state, "in_game");
    assert!(
        manager
            .is_member_syncing("main-world-public", PLAYER_C)
            .await,
        "an in-game public-world join must stay isolated until its recovery snapshot is queued"
    );
    manager
        .finish_member_sync("main-world-public", PLAYER_C)
        .await;
    assert!(
        !manager
            .is_member_syncing("main-world-public", PLAYER_C)
            .await
    );

    let ready = manager
        .set_ready_state("main-world-public", PLAYER_C, true)
        .await
        .unwrap();
    assert_eq!(ready.state, "in_game");

    stop_runtime_for_test(&manager, "main-world-public").await;
}

#[tokio::test]
async fn new_room_publish_creates_runtime_before_room_is_observable() {
    let factory = RecordingRoomLogicFactory::default();
    let manager = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(factory),
    );

    let (tx, _rx) = mpsc::channel(1024);
    manager
        .join_room(
            TEST_ROOM_ID,
            PLAYER_A,
            tx,
            MemberRole::Player,
            Some(DEFAULT_POLICY),
        )
        .await
        .unwrap();

    assert!(manager.room_exists(TEST_ROOM_ID).await);
    assert!(runtime_exists_for_test(&manager, TEST_ROOM_ID).await);
    with_room_for_test(&manager, TEST_ROOM_ID, |room| {
        assert_eq!(room.members.len(), 1);
        assert!(room.members.contains_key(PLAYER_A));
    })
    .await;
}

#[tokio::test]
async fn marked_for_destruction_room_rejects_later_operations() {
    let (manager, _factory, _receivers) =
        setup_started_room(DEFAULT_POLICY, &[PLAYER_A, PLAYER_B]).await;
    with_room_mut_for_test(&manager, TEST_ROOM_ID, |room| {
        room.mark_for_destruction();
    })
    .await;

    assert_eq!(
        manager
            .join_room(
                TEST_ROOM_ID,
                PLAYER_C,
                mpsc::channel(1024).0,
                MemberRole::Player,
                Some(DEFAULT_POLICY),
            )
            .await,
        Err("ROOM_NOT_FOUND")
    );
    assert_eq!(
        manager.set_ready_state(TEST_ROOM_ID, PLAYER_A, true).await,
        Err("ROOM_NOT_FOUND")
    );
    assert_eq!(
        manager
            .accept_player_input(TEST_ROOM_ID, PLAYER_A, 1, "move", "{}")
            .await,
        Err("ROOM_NOT_FOUND")
    );
    assert!(manager.process_room_tick(TEST_ROOM_ID, 10).await.is_none());
    assert_eq!(manager.find_room_by_offline_character(PLAYER_A).await, None);
}

#[tokio::test]
async fn observer_leave_preserves_started_room_until_owner_ends_game() {
    let (manager, _factory, _receivers) =
        setup_started_room(DEFAULT_POLICY, &[PLAYER_A, PLAYER_B]).await;
    manager
        .accept_player_input(TEST_ROOM_ID, PLAYER_A, 1, "move", "{}")
        .await
        .unwrap();

    let (observer_tx, _observer_rx) = mpsc::channel(1024);
    let observer = manager
        .join_room_as_observer(TEST_ROOM_ID, OBSERVER_1, observer_tx)
        .await
        .unwrap();
    assert_eq!(observer.snapshot.state, "in_game");

    let leave = manager.leave_room(TEST_ROOM_ID, OBSERVER_1).await;
    let snapshot = leave
        .snapshot
        .expect("observer leave should return snapshot");
    assert_eq!(snapshot.state, "in_game");

    with_room_for_test(&manager, TEST_ROOM_ID, |room| {
        assert_eq!(room.phase, RoomPhase::InGame);
        assert_eq!(room.pending_inputs_for_frame(1).len(), 1);

        let owner = room.members.get(PLAYER_A).expect("owner should remain");
        assert!(owner.ready);
        assert!(!owner.offline);
        let other_player = room
            .members
            .get(PLAYER_B)
            .expect("other player should remain");
        assert!(other_player.ready);
        assert!(!other_player.offline);

        let observer = room
            .members
            .get(OBSERVER_1)
            .expect("observer membership should remain recoverable");
        assert_eq!(observer.role, MemberRole::Observer);
        assert!(observer.offline);
    })
    .await;

    assert_eq!(
        offline_character_index_for_test(&manager, OBSERVER_1).await,
        Some(TEST_ROOM_ID.to_string())
    );
    let ended = manager.end_game(TEST_ROOM_ID, PLAYER_A).await.unwrap();
    assert_eq!(ended.state, "waiting");
    with_room_for_test(&manager, TEST_ROOM_ID, |room| {
        assert_eq!(room.phase, RoomPhase::Waiting);
        assert!(room.pending_inputs.is_empty());
        assert!(!room.members.get(PLAYER_A).unwrap().ready);
    })
    .await;
}

#[tokio::test]
async fn player_leave_keeps_started_room_running_for_remaining_players() {
    let (manager, factory, _receivers) =
        setup_started_room(MOVEMENT_DEMO_POLICY, &[PLAYER_A, PLAYER_B]).await;
    manager
        .accept_player_input(TEST_ROOM_ID, PLAYER_A, 1, "move", "{}")
        .await
        .unwrap();

    let leave = manager.leave_room(TEST_ROOM_ID, PLAYER_B).await;
    let snapshot = leave.snapshot.expect("player leave should return snapshot");
    assert_eq!(snapshot.state, "in_game");

    with_room_for_test(&manager, TEST_ROOM_ID, |room| {
        assert_eq!(room.phase, RoomPhase::InGame);
        assert_eq!(room.pending_inputs_for_frame(1).len(), 1);
        assert_eq!(room.owner_character_id, PLAYER_A);
        assert!(room.members.get(PLAYER_A).unwrap().ready);
        let leaving_player = room.members.get(PLAYER_B).unwrap();
        assert!(leaving_player.offline);
        assert!(leaving_player.ready);
    })
    .await;

    let progressed = manager.process_room_tick(TEST_ROOM_ID, 20).await;
    assert!(progressed.is_some());
    assert_eq!(factory.recorded_ticks().len(), 1);
    assert_eq!(factory.recorded_ticks()[0].0, 1);
    assert_eq!(factory.recorded_ticks()[0].1.len(), 1);
    assert_eq!(factory.recorded_ticks()[0].1[0].character_id, PLAYER_A);
}

#[tokio::test]
async fn disconnect_broadcasts_offline_presence_to_remaining_players() {
    let (manager, _factory, mut receivers) =
        setup_started_room(MOVEMENT_DEMO_POLICY, &[PLAYER_A, PLAYER_B]).await;

    manager.disconnect_room_member(TEST_ROOM_ID, PLAYER_B).await;

    let pushes = drain_messages_of_type(&mut receivers[0], MessageType::RoomMemberOfflinePush);
    assert_eq!(pushes.len(), 1);
    let push = RoomMemberOfflinePush::decode(pushes[0].body.as_slice()).unwrap();
    assert_eq!(push.room_id, TEST_ROOM_ID);
    assert_eq!(push.character_id, PLAYER_B);
    assert!(push.offline);
    assert!(
        drain_messages_of_type(&mut receivers[1], MessageType::RoomMemberOfflinePush).is_empty()
    );
}
