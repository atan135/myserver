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
