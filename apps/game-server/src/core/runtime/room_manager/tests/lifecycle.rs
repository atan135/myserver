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
            "room-test",
            character_a,
            tx_a,
            MemberRole::Player,
            Some("default_match"),
        )
        .await
        .unwrap();
    let (tx_b, _rx_b) = mpsc::channel(1024);
    manager
        .join_room(
            "room-test",
            character_b,
            tx_b,
            MemberRole::Player,
            Some("default_match"),
        )
        .await
        .unwrap();

    with_room_for_test(&manager, "room-test", |room| {
        assert_eq!(room.members.len(), 2);
        assert!(room.members.contains_key(character_a));
        assert!(room.members.contains_key(character_b));
    })
    .await;
    assert_eq!(
        character_room_index_for_test(&manager, character_a).await,
        Some("room-test".to_string())
    );
    assert_eq!(
        character_room_index_for_test(&manager, character_b).await,
        Some("room-test".to_string())
    );
}

#[tokio::test]
async fn room_exists_reflects_room_creation() {
    let factory = RecordingRoomLogicFactory::default();
    let manager = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(factory),
    );

    assert!(!manager.room_exists("room-test").await);

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

    assert!(manager.room_exists("room-test").await);
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
            "room-test",
            "player-a",
            tx,
            MemberRole::Player,
            Some("default_match"),
        )
        .await
        .unwrap();

    assert!(manager.room_exists("room-test").await);
    assert!(runtime_exists_for_test(&manager, "room-test").await);
    with_room_for_test(&manager, "room-test", |room| {
        assert_eq!(room.members.len(), 1);
        assert!(room.members.contains_key("player-a"));
    })
    .await;
}

#[tokio::test]
async fn marked_for_destruction_room_rejects_later_operations() {
    let (manager, _factory, _receivers) =
        setup_started_room("default_match", &["player-a", "player-b"]).await;
    with_room_mut_for_test(&manager, "room-test", |room| {
        room.mark_for_destruction();
    })
    .await;

    assert_eq!(
        manager
            .join_room(
                "room-test",
                "player-c",
                mpsc::channel(1024).0,
                MemberRole::Player,
                Some("default_match"),
            )
            .await,
        Err("ROOM_NOT_FOUND")
    );
    assert_eq!(
        manager.set_ready_state("room-test", "player-a", true).await,
        Err("ROOM_NOT_FOUND")
    );
    assert_eq!(
        manager
            .accept_player_input("room-test", "player-a", 1, "move", "{}")
            .await,
        Err("ROOM_NOT_FOUND")
    );
    assert!(manager.process_room_tick("room-test", 10).await.is_none());
    assert_eq!(
        manager.find_room_by_offline_character("player-a").await,
        None
    );
}
