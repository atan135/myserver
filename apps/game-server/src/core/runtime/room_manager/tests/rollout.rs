use super::*;

#[tokio::test]
async fn rollout_drain_snapshot_empty_manager_returns_zero_counts() {
    let factory = RecordingRoomLogicFactory::default();
    let manager = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(factory),
    );

    let snapshot = manager.rollout_drain_snapshot("game-server-old", 50).await;

    assert_eq!(snapshot.owner_server_id, "game-server-old");
    assert_eq!(snapshot.owned_room_count, 0);
    assert_eq!(snapshot.migrating_room_count, 0);
    assert!(snapshot.rollout_epoch.is_empty());
    assert!(snapshot.routes.is_empty());
    assert_eq!(snapshot.transferable_empty_room_count, 0);
    assert!(snapshot.transferable_empty_room_samples.is_empty());
    assert_eq!(snapshot.retired_room_count, 0);
}

#[tokio::test]
async fn rollout_drain_snapshot_counts_owned_room() {
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

    let snapshot = manager.rollout_drain_snapshot("game-server-old", 50).await;

    assert_eq!(snapshot.owned_room_count, 1);
    assert_eq!(snapshot.migrating_room_count, 0);
    assert_eq!(snapshot.transferable_empty_room_count, 0);
    assert!(snapshot.transferable_empty_room_samples.is_empty());
    assert_eq!(snapshot.retired_room_count, 0);
    assert_eq!(snapshot.routes.len(), 1);
    let route = &snapshot.routes[0];
    assert_eq!(route.room_id, "room-test");
    assert_eq!(route.owner_server_id, "game-server-old");
    assert_eq!(route.migration_state, RoomMigrationState::OwnedByOld as i32);
    assert_eq!(route.member_count, 1);
    assert_eq!(route.online_member_count, 1);
    assert_eq!(route.room_version, 1);
}

#[tokio::test]
async fn rollout_drain_snapshot_counts_empty_owned_rooms_as_transferable() {
    let factory = RecordingRoomLogicFactory::default();
    let manager = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(factory.clone()),
    );

    let empty_room = Room::new(
        "room-empty".to_string(),
        "owner".to_string(),
        "default_match".to_string(),
        factory.create("default_match"),
    );
    insert_room_for_test(&manager, "room-empty", empty_room).await;

    let (offline_tx, _offline_rx) = mpsc::channel(1024);
    manager
        .join_room(
            "room-offline",
            "player-offline",
            offline_tx,
            MemberRole::Player,
            Some("default_match"),
        )
        .await
        .unwrap();
    manager
        .disconnect_room_member("room-offline", "player-offline")
        .await;

    let (online_tx, _online_rx) = mpsc::channel(1024);
    manager
        .join_room(
            "room-online",
            "player-online",
            online_tx,
            MemberRole::Player,
            Some("default_match"),
        )
        .await
        .unwrap();

    let snapshot = manager.rollout_drain_snapshot("game-server-old", 50).await;

    assert_eq!(snapshot.owned_room_count, 3);
    assert_eq!(snapshot.migrating_room_count, 0);
    assert_eq!(snapshot.transferable_empty_room_count, 2);
    assert_eq!(snapshot.retired_room_count, 0);
    assert_eq!(
        snapshot
            .transferable_empty_room_samples
            .iter()
            .map(|route| route.room_id.as_str())
            .collect::<Vec<_>>(),
        vec!["room-empty", "room-offline"]
    );
    assert!(
        snapshot
            .transferable_empty_room_samples
            .iter()
            .all(|route| route.migration_state == RoomMigrationState::OwnedByOld as i32)
    );
    assert!(
        snapshot
            .transferable_empty_room_samples
            .iter()
            .all(|route| route.online_member_count == 0)
    );
}

#[tokio::test]
async fn rollout_drain_snapshot_counts_transfer_states_as_migrating() {
    let factory = RecordingRoomLogicFactory::default();
    let manager = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(factory.clone()),
    );

    for room_id in ["room-frozen", "room-exported", "room-importing"] {
        let mut room = Room::new(
            room_id.to_string(),
            "owner".to_string(),
            "default_match".to_string(),
            factory.create("default_match"),
        );
        room.mark_empty();
        room.transfer_state.rollout_epoch = Some("epoch-1".to_string());
        room.transfer_state.status = match room_id {
            "room-frozen" => RoomTransferStatus::Frozen,
            "room-exported" => RoomTransferStatus::Exported,
            "room-importing" => RoomTransferStatus::Importing,
            _ => unreachable!(),
        };
        insert_room_for_test(&manager, room_id, room).await;
    }

    let snapshot = manager.rollout_drain_snapshot("game-server-old", 50).await;

    assert_eq!(snapshot.rollout_epoch, "epoch-1");
    assert_eq!(snapshot.owned_room_count, 0);
    assert_eq!(snapshot.migrating_room_count, 3);
    assert_eq!(snapshot.transferable_empty_room_count, 0);
    assert!(snapshot.transferable_empty_room_samples.is_empty());
    assert_eq!(snapshot.retired_room_count, 0);
    assert_eq!(snapshot.routes.len(), 3);
    assert_eq!(
        snapshot
            .routes
            .iter()
            .map(|route| route.migration_state)
            .collect::<Vec<_>>(),
        vec![
            RoomMigrationState::FrozenForTransfer as i32,
            RoomMigrationState::FrozenForTransfer as i32,
            RoomMigrationState::ImportingToNew as i32,
        ]
    );
    assert!(snapshot.routes.iter().all(|route| route.member_count == 0));
    assert!(
        snapshot
            .routes
            .iter()
            .all(|route| route.online_member_count == 0)
    );
}

#[tokio::test]
async fn rollout_drain_snapshot_excludes_transferred_rooms_from_blockers_and_counts_retired() {
    let factory = RecordingRoomLogicFactory::default();
    let manager = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(factory.clone()),
    );

    for (room_id, status) in [
        ("room-new-owner", RoomTransferStatus::OwnedByNew),
        ("room-retired", RoomTransferStatus::Retired),
    ] {
        let mut room = Room::new(
            room_id.to_string(),
            "owner".to_string(),
            "default_match".to_string(),
            factory.create("default_match"),
        );
        room.transfer_state.rollout_epoch = Some("epoch-1".to_string());
        room.transfer_state.status = status;
        insert_room_for_test(&manager, room_id, room).await;
    }

    let snapshot = manager.rollout_drain_snapshot("game-server-old", 50).await;

    assert_eq!(snapshot.owned_room_count, 0);
    assert_eq!(snapshot.migrating_room_count, 0);
    assert_eq!(snapshot.transferable_empty_room_count, 0);
    assert!(snapshot.transferable_empty_room_samples.is_empty());
    assert_eq!(snapshot.retired_room_count, 1);
    assert_eq!(snapshot.routes.len(), 2);
    assert_eq!(
        snapshot
            .routes
            .iter()
            .map(|route| route.migration_state)
            .collect::<Vec<_>>(),
        vec![
            RoomMigrationState::OwnedByNew as i32,
            RoomMigrationState::RetiredOnOld as i32,
        ]
    );
}

#[tokio::test]
async fn trigger_server_redirect_only_pushes_online_members_in_target_room() {
    let (manager, _factory, mut receivers) =
        setup_started_room("default_match", &["player-a", "player-b"]).await;
    manager
        .disconnect_room_member("room-test", "player-b")
        .await;

    with_room_for_test(&manager, "room-test", |room| {
        assert_eq!(room.members["player-a"].close_state.reason(), None);
        assert_eq!(room.members["player-b"].close_state.reason(), None);
    })
    .await;

    let delivery = manager
        .trigger_server_redirect(
            "room-test",
            ServerRedirectPush {
                reason: "rollout".to_string(),
                room_id: "room-test".to_string(),
                rollout_epoch: "epoch-1".to_string(),
                reconnect_required: true,
                retry_after_ms: 250,
                target_host: "127.0.0.1".to_string(),
                target_port: 4000,
                target_server_id: "game-server-new".to_string(),
                transport: "kcp".to_string(),
            },
        )
        .await
        .unwrap();

    assert_eq!(
        delivery,
        ServerRedirectDelivery {
            delivered_count: 1,
            failed_count: 0,
            online_member_count: 1,
        }
    );

    let pushed = drain_messages_of_type(&mut receivers[0], MessageType::ServerRedirectPush)
        .pop()
        .expect("online member push");
    assert_eq!(pushed.message_type, MessageType::ServerRedirectPush);
    let push = ServerRedirectPush::decode(pushed.body.as_slice()).unwrap();
    assert_eq!(push.room_id, "room-test");
    assert_eq!(push.rollout_epoch, "epoch-1");
    assert_eq!(push.target_host, "127.0.0.1");
    assert_eq!(push.target_port, 4000);
    assert!(push.reconnect_required);
    assert!(drain_messages_of_type(&mut receivers[1], MessageType::ServerRedirectPush).is_empty());

    with_room_for_test(&manager, "room-test", |room| {
        assert_eq!(
            room.members["player-a"].close_state.reason().as_deref(),
            Some(SERVER_REDIRECT_CLOSE_REASON)
        );
        assert_eq!(room.members["player-b"].close_state.reason(), None);
    })
    .await;
}

#[tokio::test]
async fn trigger_server_redirect_queue_failure_does_not_overwrite_close_reason() {
    let (manager, _factory, _receivers) =
        setup_started_room("default_match", &["player-a", "player-b"]).await;
    let (full_tx, _full_rx) = mpsc::channel(1);
    full_tx
        .try_send(OutboundMessage {
            message_type: MessageType::RoomStatePush,
            seq: 0,
            body: Vec::new(),
        })
        .unwrap();
    let close_state = ConnectionCloseState::new();
    assert!(close_state.request_close("existing_reason"));

    with_room_mut_for_test(&manager, "room-test", |room| {
        let member = room.members.get_mut("player-a").unwrap();
        member.sender = full_tx;
        member.close_state = close_state;
    })
    .await;

    let delivery = manager
        .trigger_server_redirect(
            "room-test",
            ServerRedirectPush {
                reason: "rollout".to_string(),
                room_id: "room-test".to_string(),
                rollout_epoch: "epoch-1".to_string(),
                reconnect_required: true,
                retry_after_ms: 250,
                target_host: "127.0.0.1".to_string(),
                target_port: 4000,
                target_server_id: "game-server-new".to_string(),
                transport: "kcp".to_string(),
            },
        )
        .await
        .unwrap();

    assert_eq!(
        delivery,
        ServerRedirectDelivery {
            delivered_count: 1,
            failed_count: 1,
            online_member_count: 2,
        }
    );

    with_room_for_test(&manager, "room-test", |room| {
        assert_eq!(
            room.members["player-a"].close_state.reason().as_deref(),
            Some("existing_reason")
        );
        assert_eq!(
            room.members["player-b"].close_state.reason().as_deref(),
            Some(SERVER_REDIRECT_CLOSE_REASON)
        );
    })
    .await;
}

#[tokio::test]
async fn rollout_drain_notice_pushes_game_message_to_online_non_syncing_room_members() {
    let (manager, _factory, mut receivers) =
        setup_started_room("default_match", &["player-a", "player-b", "player-c"]).await;
    manager
        .disconnect_room_member("room-test", "player-b")
        .await;
    with_room_mut_for_test(&manager, "room-test", |room| {
        room.members.get_mut("player-c").unwrap().syncing = true;
    })
    .await;

    let delivery = manager
        .trigger_rollout_drain_notice(RolloutDrainNotice {
            room_id: "room-test".to_string(),
            rollout_epoch: "epoch-1".to_string(),
            reason: "rollout".to_string(),
            message: "Please leave after this match".to_string(),
            retry_after_ms: 500,
            deadline_ms: 123_456,
        })
        .await
        .unwrap();

    assert_eq!(
        delivery,
        RolloutDrainNoticeDelivery {
            delivered_count: 1,
            failed_count: 0,
            online_member_count: 1,
        }
    );

    let pushed = drain_messages_of_type(&mut receivers[0], MessageType::GameMessagePush)
        .pop()
        .expect("online member notice");
    let push = GameMessagePush::decode(pushed.body.as_slice()).unwrap();
    assert_eq!(push.event, "rollout_drain_notice");
    assert_eq!(push.room_id, "room-test");
    assert_eq!(push.action, "leave_room");
    assert!(push.character_id.is_empty());
    let payload: serde_json::Value = serde_json::from_str(&push.payload_json).unwrap();
    assert_eq!(payload["room_id"], "room-test");
    assert_eq!(payload["rollout_epoch"], "epoch-1");
    assert_eq!(payload["reason"], "rollout");
    assert_eq!(payload["message"], "Please leave after this match");
    assert_eq!(payload["retry_after_ms"], 500);
    assert_eq!(payload["deadline_ms"], 123_456);
    assert!(drain_messages_of_type(&mut receivers[1], MessageType::GameMessagePush).is_empty());
    assert!(drain_messages_of_type(&mut receivers[2], MessageType::GameMessagePush).is_empty());

    with_room_for_test(&manager, "room-test", |room| {
        assert!(
            room.members
                .values()
                .all(|member| member.close_state.reason().is_none())
        );
    })
    .await;
}

#[tokio::test]
async fn rollout_drain_notice_counts_queue_failure_without_closing_connection() {
    let factory = RecordingRoomLogicFactory::default();
    let manager = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(factory),
    );
    let (full_tx, _full_rx) = mpsc::channel(1);
    full_tx
        .try_send(OutboundMessage {
            message_type: MessageType::RoomStatePush,
            seq: 0,
            body: Vec::new(),
        })
        .unwrap();
    let close_state = ConnectionCloseState::new();
    manager
        .join_room(
            "room-test",
            "player-a",
            OutboundChannel::new(full_tx, close_state.clone()),
            MemberRole::Player,
            Some("default_match"),
        )
        .await
        .unwrap();
    let delivery = manager
        .trigger_rollout_drain_notice(RolloutDrainNotice {
            room_id: "room-test".to_string(),
            rollout_epoch: "epoch-1".to_string(),
            reason: "rollout".to_string(),
            message: "Leave room".to_string(),
            retry_after_ms: 0,
            deadline_ms: 0,
        })
        .await
        .unwrap();

    assert_eq!(
        delivery,
        RolloutDrainNoticeDelivery {
            delivered_count: 0,
            failed_count: 1,
            online_member_count: 1,
        }
    );
    assert_ne!(
        close_state.reason().as_deref(),
        Some(SERVER_REDIRECT_CLOSE_REASON)
    );
}
