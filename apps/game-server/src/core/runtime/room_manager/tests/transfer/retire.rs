use super::*;

#[tokio::test]
async fn retire_transfer_rejects_checksum_mismatch() {
    let (manager, _factory, _receivers) =
        setup_started_room(DEFAULT_POLICY, &[PLAYER_A, PLAYER_B]).await;
    manager.disconnect_room_member(TEST_ROOM_ID, PLAYER_A).await;
    manager.disconnect_room_member(TEST_ROOM_ID, PLAYER_B).await;
    manager
        .freeze_room_for_transfer(ROLLOUT_EPOCH, TEST_ROOM_ID)
        .await
        .unwrap();
    manager
        .export_room_transfer(ROLLOUT_EPOCH, TEST_ROOM_ID)
        .await
        .unwrap();

    let result = manager
        .retire_transferred_room(ROLLOUT_EPOCH, TEST_ROOM_ID, "wrong")
        .await;

    assert_eq!(result, Err("ROOM_TRANSFER_CHECKSUM_MISMATCH"));
    assert!(manager.room_exists(TEST_ROOM_ID).await);
}

#[tokio::test]
async fn retired_room_rejects_later_mutations() {
    let (manager, _factory, _receivers) =
        setup_started_room(DEFAULT_POLICY, &[PLAYER_A, PLAYER_B]).await;
    manager.disconnect_room_member(TEST_ROOM_ID, PLAYER_A).await;
    manager.disconnect_room_member(TEST_ROOM_ID, PLAYER_B).await;
    manager
        .freeze_room_for_transfer(ROLLOUT_EPOCH, TEST_ROOM_ID)
        .await
        .unwrap();
    let payload = manager
        .export_room_transfer(ROLLOUT_EPOCH, TEST_ROOM_ID)
        .await
        .unwrap();
    manager
        .retire_transferred_room(ROLLOUT_EPOCH, TEST_ROOM_ID, &payload.checksum)
        .await
        .unwrap();

    let (tx, _rx) = mpsc::channel(1024);
    let join_result = manager
        .join_room(
            TEST_ROOM_ID,
            PLAYER_B,
            tx,
            MemberRole::Player,
            Some(DEFAULT_POLICY),
        )
        .await;
    assert_eq!(join_result.unwrap_err(), "ROOM_TRANSFER_RETIRED");

    let (reconnect_tx, _reconnect_rx) = mpsc::channel(1024);
    assert_eq!(
        manager
            .reconnect_room(TEST_ROOM_ID, PLAYER_A, reconnect_tx)
            .await
            .unwrap_err(),
        "ROOM_TRANSFER_RETIRED"
    );

    assert_eq!(
        manager
            .accept_player_input(TEST_ROOM_ID, PLAYER_A, 1, "move", "{}")
            .await,
        Err("ROOM_TRANSFER_RETIRED")
    );
}
