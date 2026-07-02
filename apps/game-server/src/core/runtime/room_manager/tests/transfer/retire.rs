use super::*;

#[tokio::test]
async fn retire_transfer_rejects_checksum_mismatch() {
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
    manager
        .export_room_transfer("epoch-1", "room-test")
        .await
        .unwrap();

    let result = manager
        .retire_transferred_room("epoch-1", "room-test", "wrong")
        .await;

    assert_eq!(result, Err("ROOM_TRANSFER_CHECKSUM_MISMATCH"));
    assert!(manager.room_exists("room-test").await);
}

#[tokio::test]
async fn retired_room_rejects_later_mutations() {
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
    manager
        .retire_transferred_room("epoch-1", "room-test", &payload.checksum)
        .await
        .unwrap();

    let (tx, _rx) = mpsc::channel(1024);
    let join_result = manager
        .join_room(
            "room-test",
            "player-b",
            tx,
            MemberRole::Player,
            Some("default_match"),
        )
        .await;
    assert_eq!(join_result.unwrap_err(), "ROOM_TRANSFER_RETIRED");

    let (reconnect_tx, _reconnect_rx) = mpsc::channel(1024);
    assert_eq!(
        manager
            .reconnect_room("room-test", "player-a", reconnect_tx)
            .await
            .unwrap_err(),
        "ROOM_TRANSFER_RETIRED"
    );

    assert_eq!(
        manager
            .accept_player_input("room-test", "player-a", 1, "move", "{}")
            .await,
        Err("ROOM_TRANSFER_RETIRED")
    );
}
