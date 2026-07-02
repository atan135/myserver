use super::*;

async fn setup_imported_room_for_confirm() -> (RoomManager, RecordingRoomLogicFactory, String, u64)
{
    let (source, _source_factory, _receivers) =
        setup_started_room(DEFAULT_POLICY, &[PLAYER_A, PLAYER_B]).await;
    source.disconnect_room_member(TEST_ROOM_ID, PLAYER_A).await;
    source.disconnect_room_member(TEST_ROOM_ID, PLAYER_B).await;
    source
        .freeze_room_for_transfer(ROLLOUT_EPOCH, TEST_ROOM_ID)
        .await
        .unwrap();
    let payload = source
        .export_room_transfer(ROLLOUT_EPOCH, TEST_ROOM_ID)
        .await
        .unwrap();
    let checksum = payload.checksum.clone();

    let target_factory = RecordingRoomLogicFactory::default();
    let target = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(target_factory.clone()),
    );
    let (_imported_checksum, room_version) = target.import_room_transfer(payload).await.unwrap();

    (target, target_factory, checksum, room_version)
}

#[tokio::test]
async fn confirm_room_ownership_succeeds_for_imported_room() {
    let (target, _target_factory, checksum, room_version) = setup_imported_room_for_confirm().await;

    let confirmed = target
        .confirm_room_ownership(ROLLOUT_EPOCH, TEST_ROOM_ID, &checksum, room_version)
        .await
        .unwrap();

    assert_eq!(confirmed.0, checksum);
    assert_eq!(confirmed.1, room_version);
}

#[tokio::test]
async fn confirm_room_ownership_rejects_mismatched_epoch_checksum_or_version() {
    let (target, _target_factory, checksum, room_version) = setup_imported_room_for_confirm().await;

    assert_eq!(
        target
            .confirm_room_ownership("epoch-2", TEST_ROOM_ID, &checksum, room_version)
            .await,
        Err("ROOM_TRANSFER_EPOCH_MISMATCH")
    );
    assert_eq!(
        target
            .confirm_room_ownership(ROLLOUT_EPOCH, TEST_ROOM_ID, "wrong", room_version)
            .await,
        Err("ROOM_TRANSFER_CHECKSUM_MISMATCH")
    );
    assert_eq!(
        target
            .confirm_room_ownership(
                ROLLOUT_EPOCH,
                TEST_ROOM_ID,
                &checksum,
                room_version.saturating_add(1)
            )
            .await,
        Err("ROOM_TRANSFER_VERSION_MISMATCH")
    );
    assert_eq!(
        target
            .confirm_room_ownership("", TEST_ROOM_ID, &checksum, room_version)
            .await,
        Err("INVALID_ROLLOUT_EPOCH")
    );
}

#[tokio::test]
async fn imported_room_is_treated_as_taken_over_room_for_join_and_reconnect() {
    let (target, target_factory, checksum, room_version) = setup_imported_room_for_confirm().await;
    target
        .confirm_room_ownership(ROLLOUT_EPOCH, TEST_ROOM_ID, &checksum, room_version)
        .await
        .unwrap();

    with_room_for_test(&target, TEST_ROOM_ID, |room| {
        assert_eq!(room.transfer_state.status, RoomTransferStatus::OwnedByNew);
        assert_eq!(
            room.transfer_state.rollout_epoch.as_deref(),
            Some(ROLLOUT_EPOCH)
        );
        assert_eq!(room.transfer_state.room_version, room_version);
        assert_eq!(
            room.transfer_state.last_transfer_checksum.as_deref(),
            Some(checksum.as_str())
        );
        assert!(room.members.contains_key(PLAYER_A));
        assert!(room.members.contains_key(PLAYER_B));
    })
    .await;

    let (reconnect_tx, _reconnect_rx) = mpsc::channel(1024);
    let reconnect = target
        .reconnect_room(TEST_ROOM_ID, PLAYER_A, reconnect_tx)
        .await
        .unwrap();
    assert_eq!(reconnect.snapshot.room_id, TEST_ROOM_ID);

    let (join_tx, _join_rx) = mpsc::channel(1024);
    let join_snapshot = target
        .join_room(
            TEST_ROOM_ID,
            PLAYER_B,
            join_tx,
            MemberRole::Player,
            Some(DEFAULT_POLICY),
        )
        .await
        .unwrap();
    assert_eq!(join_snapshot.room_id, TEST_ROOM_ID);
    assert!(
        join_snapshot
            .members
            .iter()
            .any(|member| member.character_id == PLAYER_B)
    );

    assert_eq!(target.room_count().await, 1);
    with_room_for_test(&target, TEST_ROOM_ID, |room| {
        assert_eq!(room.transfer_state.status, RoomTransferStatus::OwnedByNew);
        assert_eq!(room.transfer_state.room_version, room_version);
    })
    .await;
    assert_eq!(target_factory.imported_transfer_states().len(), 1);
}

#[tokio::test]
async fn confirm_room_ownership_rejects_room_not_owned_by_new() {
    let (manager, _factory, _receivers) =
        setup_started_room(DEFAULT_POLICY, &[PLAYER_A, PLAYER_B]).await;

    let result = manager
        .confirm_room_ownership(ROLLOUT_EPOCH, TEST_ROOM_ID, "checksum", 1)
        .await;

    assert_eq!(result, Err("ROOM_TRANSFER_NOT_OWNED_BY_NEW"));
}
