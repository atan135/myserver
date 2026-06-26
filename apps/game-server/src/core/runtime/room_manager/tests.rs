use std::sync::{Arc, Mutex as StdMutex};

use prost::Message;
use tokio::sync::mpsc;

use crate::core::logic::{
    ROOM_TRANSFER_SCHEMA_VERSION, RoomLogic, RoomLogicFactory, RoomLogicTransfer,
    RoomLogicTransferState, RoomNpcTransferState, RoomRuntimeTimerTransferState,
    RoomTimerTransferEntry,
};
use crate::core::room::PlayerInputRecord;
use crate::core::runtime::room_policy::{MissingInputStrategy, RoomRuntimePolicy};
use crate::gameroom::GameRoomLogicFactory;
use crate::pb::{GameMessagePush, RoomFrameRatePush, RoomMigrationState, ServerRedirectPush};
use crate::protocol::MessageType;

use super::transfer_codec::{
    resolve_tick_inputs, room_transfer_checksum, room_transfer_state_from_payload,
};
use super::*;

#[derive(Clone, Default)]
struct RecordingRoomLogicFactory {
    ticks: Arc<StdMutex<Vec<(u32, Vec<PlayerInputRecord>)>>>,
    inputs: Arc<StdMutex<Vec<(String, String, String)>>>,
    imported_transfer_states: Arc<StdMutex<Vec<RoomLogicTransferState>>>,
}

impl RecordingRoomLogicFactory {
    fn recorded_ticks(&self) -> Vec<(u32, Vec<PlayerInputRecord>)> {
        self.ticks.lock().unwrap().clone()
    }

    fn recorded_inputs(&self) -> Vec<(String, String, String)> {
        self.inputs.lock().unwrap().clone()
    }

    fn imported_transfer_states(&self) -> Vec<RoomLogicTransferState> {
        self.imported_transfer_states.lock().unwrap().clone()
    }
}

struct RecordingRoomLogic {
    ticks: Arc<StdMutex<Vec<(u32, Vec<PlayerInputRecord>)>>>,
    inputs: Arc<StdMutex<Vec<(String, String, String)>>>,
    imported_transfer_states: Arc<StdMutex<Vec<RoomLogicTransferState>>>,
    state: String,
}

fn recording_timer_state_json() -> String {
    let mut timer_state = RoomRuntimeTimerTransferState::new("recording-room-logic", 0, 0);
    timer_state.timer_entries.push(RoomTimerTransferEntry {
        id: "recording-timer".to_string(),
        timer_kind: "recording-fixture".to_string(),
        remaining_frames: 1,
        repeat_interval_frames: Some(1),
        payload_json: r#"{"timer":"recording-v1"}"#.to_string(),
    });
    timer_state
        .metadata
        .insert("fixture".to_string(), "recording-v1".to_string());
    timer_state.to_json().unwrap()
}

impl RoomLogic for RecordingRoomLogic {
    fn on_character_input(&mut self, character_id: &str, action: &str, payload_json: &str) {
        self.inputs.lock().unwrap().push((
            character_id.to_string(),
            action.to_string(),
            payload_json.to_string(),
        ));
    }

    fn on_tick(&mut self, frame_id: u32, _fps: u16, inputs: &[PlayerInputRecord]) {
        self.ticks.lock().unwrap().push((frame_id, inputs.to_vec()));
    }

    fn get_serialized_state(&self) -> String {
        self.state.clone()
    }
}

impl RoomLogicTransfer for RecordingRoomLogic {
    fn export_transfer_state(&self) -> Result<RoomLogicTransferState, &'static str> {
        Ok(RoomLogicTransferState {
            schema_version: ROOM_TRANSFER_SCHEMA_VERSION,
            logic_state_json: self.state.clone(),
            movement_state_json: r#"{"movement":"recording-v1"}"#.to_string(),
            combat_state_json: r#"{"combat":"recording-v1"}"#.to_string(),
            npc_state_json: r#"{"npc":"recording-v1"}"#.to_string(),
            timer_state_json: recording_timer_state_json(),
        })
    }

    fn import_transfer_state(
        &mut self,
        state: &RoomLogicTransferState,
    ) -> Result<(), &'static str> {
        if state.schema_version != ROOM_TRANSFER_SCHEMA_VERSION {
            return Err("ROOM_TRANSFER_UNSUPPORTED_SCHEMA");
        }

        self.state = state.logic_state_json.clone();
        self.imported_transfer_states
            .lock()
            .unwrap()
            .push(state.clone());
        Ok(())
    }
}

impl RoomLogicFactory for RecordingRoomLogicFactory {
    fn create(&self, _policy_id: &str) -> Box<dyn RoomLogic> {
        Box::new(RecordingRoomLogic {
            ticks: Arc::clone(&self.ticks),
            inputs: Arc::clone(&self.inputs),
            imported_transfer_states: Arc::clone(&self.imported_transfer_states),
            state: "recording-state-v1".to_string(),
        })
    }
}

struct UnsupportedTransferRoomLogic;

impl RoomLogicTransfer for UnsupportedTransferRoomLogic {}

impl RoomLogic for UnsupportedTransferRoomLogic {}

struct UnsupportedTransferRoomLogicFactory;

impl RoomLogicFactory for UnsupportedTransferRoomLogicFactory {
    fn create(&self, _policy_id: &str) -> Box<dyn RoomLogic> {
        Box::new(UnsupportedTransferRoomLogic)
    }
}

async fn setup_started_room(
    policy_id: &str,
    characters: &[&str],
) -> (
    RoomManager,
    RecordingRoomLogicFactory,
    Vec<mpsc::Receiver<OutboundMessage>>,
) {
    let factory = RecordingRoomLogicFactory::default();
    let manager = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(factory.clone()),
    );

    let mut receivers = Vec::new();
    for character_id in characters {
        let (tx, rx) = mpsc::channel(1024);
        receivers.push(rx);
        manager
            .join_room(
                "room-test",
                character_id,
                tx,
                MemberRole::Player,
                Some(policy_id),
            )
            .await
            .unwrap();
        manager
            .set_ready_state("room-test", character_id, true)
            .await
            .unwrap();
    }
    manager
        .start_game("room-test", characters[0])
        .await
        .unwrap();
    stop_runtime_for_test(&manager, "room-test").await;

    (manager, factory, receivers)
}

async fn stop_runtime_for_test(manager: &RoomManager, room_id: &str) {
    if let Some(runtime_entry) = manager.get_runtime_entry(room_id).await {
        let mut runtime = runtime_entry.lock().await;
        if let Some(handle) = runtime.tick_handle.take() {
            handle.abort();
        }
        runtime.tick_running = false;
    }
}

async fn with_runtime_for_test<R>(
    manager: &RoomManager,
    room_id: &str,
    f: impl FnOnce(&RoomRuntime) -> R,
) -> R {
    let runtime_entry = manager
        .get_runtime_entry(room_id)
        .await
        .expect("room runtime should exist");
    let runtime = runtime_entry.lock().await;
    f(&runtime)
}

async fn runtime_exists_for_test(manager: &RoomManager, room_id: &str) -> bool {
    manager.get_runtime_entry(room_id).await.is_some()
}

async fn insert_room_for_test(manager: &RoomManager, room_id: &str, room: Room) {
    let members = room_member_index_entries(&room);
    manager
        .rooms
        .write()
        .await
        .insert(room_id.to_string(), std::sync::Arc::new(Mutex::new(room)));
    replace_room_member_indexes(
        &manager.character_rooms,
        &manager.offline_characters,
        room_id,
        members,
    )
    .await;
}

async fn character_room_index_for_test(
    manager: &RoomManager,
    character_id: &str,
) -> Option<String> {
    manager
        .character_rooms
        .read()
        .await
        .get(character_id)
        .cloned()
}

async fn offline_character_index_for_test(
    manager: &RoomManager,
    character_id: &str,
) -> Option<String> {
    manager
        .offline_characters
        .read()
        .await
        .get(character_id)
        .cloned()
}

async fn with_room_for_test<R>(
    manager: &RoomManager,
    room_id: &str,
    f: impl FnOnce(&Room) -> R,
) -> R {
    let room_entry = manager
        .get_room_entry(room_id)
        .await
        .expect("room should exist");
    let room = room_entry.lock().await;
    f(&room)
}

async fn with_room_mut_for_test<R>(
    manager: &RoomManager,
    room_id: &str,
    f: impl FnOnce(&mut Room) -> R,
) -> R {
    let room_entry = manager
        .get_room_entry(room_id)
        .await
        .expect("room should exist");
    let mut room = room_entry.lock().await;
    f(&mut room)
}

async fn setup_started_room_with_id(
    manager: &RoomManager,
    room_id: &str,
    characters: &[String],
    receivers: &mut Vec<mpsc::Receiver<OutboundMessage>>,
) {
    for character_id in characters {
        let (tx, rx) = mpsc::channel(1024);
        receivers.push(rx);
        manager
            .join_room(
                room_id,
                character_id,
                tx,
                MemberRole::Player,
                Some("default_match"),
            )
            .await
            .unwrap();
        manager
            .set_ready_state(room_id, character_id, true)
            .await
            .unwrap();
    }
    manager.start_game(room_id, &characters[0]).await.unwrap();
    stop_runtime_for_test(manager, room_id).await;
}

fn drain_messages_of_type(
    receiver: &mut mpsc::Receiver<OutboundMessage>,
    message_type: MessageType,
) -> Vec<OutboundMessage> {
    let mut messages = Vec::new();
    while let Ok(message) = receiver.try_recv() {
        if message.message_type == message_type {
            messages.push(message);
        }
    }
    messages
}

fn combat_demo_entity_by_player<'a>(
    game_state: &'a serde_json::Value,
    character_id: &str,
) -> &'a serde_json::Value {
    game_state["snapshot"]["entities"]
        .as_array()
        .expect("combat demo snapshot should contain entities")
        .iter()
        .find(|entity| entity["character_id"].as_str() == Some(character_id))
        .expect("combat demo player entity should exist")
}

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

#[tokio::test]
async fn fps_change_pushes_room_frame_rate_update_to_online_members() {
    let (manager, _factory, mut receivers) =
        setup_started_room("disposable_match", &["player-a", "player-b"]).await;
    for receiver in &mut receivers {
        drain_messages_of_type(receiver, MessageType::RoomFrameRatePush);
    }

    manager
        .disconnect_room_member("room-test", "player-b")
        .await;

    let pushes = drain_messages_of_type(&mut receivers[0], MessageType::RoomFrameRatePush);
    assert_eq!(pushes.len(), 1);
    let push = RoomFrameRatePush::decode(pushes[0].body.as_slice()).unwrap();
    assert_eq!(push.room_id, "room-test");
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
            "room-test",
            "player-a",
            join_tx,
            MemberRole::Player,
            Some("default_match"),
        )
        .await
        .unwrap();

    let join_pushes = drain_messages_of_type(&mut join_rx, MessageType::RoomFrameRatePush);
    assert_eq!(join_pushes.len(), 1);
    let push = RoomFrameRatePush::decode(join_pushes[0].body.as_slice()).unwrap();
    assert_eq!(push.room_id, "room-test");
    assert_eq!(push.fps, 2);
    assert_eq!(push.reason, "runtime_policy_changed");
}

#[tokio::test]
async fn unchanged_fps_does_not_push_duplicate_room_frame_rate_update() {
    let (manager, _factory, mut receivers) =
        setup_started_room("default_match", &["player-a", "player-b"]).await;
    for receiver in &mut receivers {
        drain_messages_of_type(receiver, MessageType::RoomFrameRatePush);
    }

    manager
        .disconnect_room_member("room-test", "player-b")
        .await;

    assert!(drain_messages_of_type(&mut receivers[0], MessageType::RoomFrameRatePush).is_empty());
    assert!(drain_messages_of_type(&mut receivers[1], MessageType::RoomFrameRatePush).is_empty());
}

#[tokio::test]
async fn export_room_transfer_rejects_logic_without_transfer_contract() {
    let manager = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(UnsupportedTransferRoomLogicFactory),
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
    manager
        .disconnect_room_member("room-test", "player-a")
        .await;
    manager
        .freeze_room_for_transfer("epoch-1", "room-test")
        .await
        .unwrap();

    let result = manager.export_room_transfer("epoch-1", "room-test").await;

    assert_eq!(result, Err("UNSUPPORTED_ROOM_TRANSFER"));
    with_room_for_test(&manager, "room-test", |room| {
        assert_eq!(room.transfer_state.status, RoomTransferStatus::Frozen);
        assert!(room.transfer_state.last_transfer_checksum.is_none());
    })
    .await;
}

#[tokio::test]
async fn export_room_transfer_rejects_invalid_epoch_or_missing_room() {
    let factory = RecordingRoomLogicFactory::default();
    let manager = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(factory),
    );

    assert_eq!(
        manager.export_room_transfer("", "room-test").await,
        Err("INVALID_ROLLOUT_EPOCH")
    );
    assert_eq!(
        manager
            .export_room_transfer("epoch-1", "room-missing")
            .await,
        Err("ROOM_NOT_FOUND")
    );
}

#[tokio::test]
async fn export_room_transfer_rejects_room_that_was_not_frozen() {
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

    let result = manager.export_room_transfer("epoch-1", "room-test").await;

    assert_eq!(result, Err("ROOM_TRANSFER_NOT_FROZEN"));
}

#[tokio::test]
async fn export_room_transfer_rejects_mismatched_epoch() {
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

    let result = manager.export_room_transfer("epoch-2", "room-test").await;

    assert_eq!(result, Err("ROOM_TRANSFER_EPOCH_MISMATCH"));
}

#[tokio::test]
async fn export_room_transfer_checksum_is_deterministic() {
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

    assert!(!payload.checksum.is_empty());
    assert_eq!(payload.checksum, room_transfer_checksum(&payload));
    let transfer_state = room_transfer_state_from_payload(&payload).unwrap();
    assert_eq!(transfer_state.schema_version, ROOM_TRANSFER_SCHEMA_VERSION);
    assert_eq!(transfer_state.logic_state_json, "recording-state-v1");
    assert_eq!(
        transfer_state.movement_state_json,
        r#"{"movement":"recording-v1"}"#
    );
    assert_eq!(
        transfer_state.combat_state_json,
        r#"{"combat":"recording-v1"}"#
    );
    assert_eq!(transfer_state.npc_state_json, r#"{"npc":"recording-v1"}"#);
    let timer_state =
        RoomRuntimeTimerTransferState::from_json(&transfer_state.timer_state_json).unwrap();
    assert_eq!(timer_state.schema_version, ROOM_TRANSFER_SCHEMA_VERSION);
    assert_eq!(
        timer_state.runtime_summary.owner_kind,
        "recording-room-logic"
    );
    assert_eq!(timer_state.timer_entries.len(), 1);
    assert_eq!(timer_state.timer_entries[0].id, "recording-timer");
    assert_eq!(timer_state.timer_entries[0].remaining_frames, 1);
    assert_eq!(
        timer_state.metadata.get("fixture").map(String::as_str),
        Some("recording-v1")
    );
    assert_eq!(payload.snapshot.as_ref().unwrap().room_id, "room-test");
}

#[tokio::test]
async fn movement_demo_transfer_restores_movement_payload_consistently() {
    let config_tables = crate::core::config_table::ConfigTableRuntime::load_with_scene_dir(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("csv"),
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scene"),
    )
    .expect("game-server csv fixture should load");
    let factory = Arc::new(GameRoomLogicFactory::new(config_tables.clone()));
    let source = RoomManager::with_policy_registry_and_cleanup_interval(
        crate::match_client::create_match_client_shared(),
        factory.clone(),
        config_tables.room_policy_registry(),
        3600,
    );

    let (tx, _rx) = mpsc::channel(1024);
    source
        .join_room(
            "room-movement-transfer",
            "player-a",
            tx,
            MemberRole::Player,
            Some("movement_demo"),
        )
        .await
        .unwrap();
    source
        .set_ready_state("room-movement-transfer", "player-a", true)
        .await
        .unwrap();
    source
        .start_game("room-movement-transfer", "player-a")
        .await
        .unwrap();
    stop_runtime_for_test(&source, "room-movement-transfer").await;

    source
            .accept_player_input(
                "room-movement-transfer",
                "player-a",
                1,
                "move_dir",
                "{\"dirX\":1.0,\"dirY\":0.0,\"hasClientState\":true,\"clientX\":1.0,\"clientY\":1.0,\"clientFrameId\":1}",
            )
            .await
            .unwrap();
    source
        .process_room_tick("room-movement-transfer", 20)
        .await
        .unwrap();
    source
        .accept_player_input("room-movement-transfer", "player-a", 2, "", "")
        .await
        .unwrap();
    source
        .process_room_tick("room-movement-transfer", 20)
        .await
        .unwrap();

    with_room_mut_for_test(&source, "room-movement-transfer", |room| {
        let member = room
            .members
            .get_mut("player-a")
            .expect("source member should exist");
        member.offline = true;
        member.offline_since = Some(Instant::now());
        room.mark_empty();
    })
    .await;
    source
        .freeze_room_for_transfer("epoch-1", "room-movement-transfer")
        .await
        .unwrap();

    let payload = source
        .export_room_transfer("epoch-1", "room-movement-transfer")
        .await
        .unwrap();
    let checksum = payload.checksum.clone();
    let transfer_state = room_transfer_state_from_payload(&payload).unwrap();
    let logic_json =
        serde_json::from_str::<serde_json::Value>(&transfer_state.logic_state_json).unwrap();
    let movement_json =
        serde_json::from_str::<serde_json::Value>(&transfer_state.movement_state_json).unwrap();

    assert_eq!(logic_json["schema"], "movement-demo.logic.v1");
    assert_eq!(logic_json["tick_count"], 2);
    assert_eq!(logic_json["recipients"], serde_json::json!(["player-a"]));
    assert_eq!(movement_json["schema"], "room-movement-state.v1");
    assert_eq!(movement_json["scene_id"], 1);
    assert_eq!(movement_json["last_snapshot_frame"], 1);
    assert_eq!(movement_json["last_full_sync_frame"], 0);
    assert_eq!(movement_json["movement_control_stop_frames"], 3);
    assert_eq!(
        movement_json["latest_client_state_by_player"][0]["character_id"],
        "player-a"
    );
    assert_eq!(
        movement_json["missing_control_frames_by_player"][0]["frame_id"],
        1
    );
    assert_eq!(movement_json["entities"][0]["character_id"], "player-a");
    assert_eq!(movement_json["entities"][0]["moving"], true);
    assert_eq!(movement_json["entities"][0]["last_input_frame"], 1);

    let target = RoomManager::with_policy_registry_and_cleanup_interval(
        crate::match_client::create_match_client_shared(),
        factory,
        config_tables.room_policy_registry(),
        3600,
    );
    let imported = target.import_room_transfer(payload).await.unwrap();
    assert_eq!(imported.0, checksum);

    with_room_for_test(&target, "room-movement-transfer", |room| {
        assert_eq!(room.current_frame, 2);
        assert_eq!(room.transfer_state.status, RoomTransferStatus::OwnedByNew);
        let game_state =
            serde_json::from_str::<serde_json::Value>(&room.snapshot().game_state).unwrap();
        assert_eq!(game_state["tick_count"], 2);
        assert_eq!(game_state["entity_count"], 1);
        assert_eq!(game_state["entities"][0]["moving"], true);
        assert_eq!(game_state["entities"][0]["last_input_frame"], 1);
    })
    .await;

    let (reconnect_tx, _reconnect_rx) = mpsc::channel(1024);
    let recovery = target
        .reconnect_room("room-movement-transfer", "player-a", reconnect_tx)
        .await
        .unwrap();
    let movement_recovery = recovery
        .movement_recovery
        .expect("movement recovery should exist after import");
    assert_eq!(movement_recovery.frame_id, 2);
    assert_eq!(movement_recovery.reference_frame_id, 2);
    assert!(movement_recovery.aoi_enabled);
    assert_eq!(movement_recovery.entities.len(), 1);
    assert_eq!(movement_recovery.entities[0].character_id, "player-a");
    assert!(movement_recovery.entities[0].moving);
    assert_eq!(movement_recovery.entities[0].last_input_frame, 1);

    assert_eq!(
        target
            .export_room_transfer("epoch-1", "room-movement-transfer")
            .await,
        Err("ROOM_TRANSFER_OWNED_BY_NEW")
    );

    let mut invalid_json_payload = source
        .export_room_transfer("epoch-1", "room-movement-transfer")
        .await
        .unwrap();
    let mut movement_wrapper =
        serde_json::from_str::<serde_json::Value>(&invalid_json_payload.movement_state_json)
            .unwrap();
    movement_wrapper["movementStateJson"] = serde_json::json!("{bad");
    invalid_json_payload.movement_state_json = movement_wrapper.to_string();
    invalid_json_payload.checksum = room_transfer_checksum(&invalid_json_payload);
    let invalid_json_target = RoomManager::with_policy_registry_and_cleanup_interval(
        crate::match_client::create_match_client_shared(),
        Arc::new(GameRoomLogicFactory::new(config_tables.clone())),
        config_tables.room_policy_registry(),
        3600,
    );
    assert_eq!(
        invalid_json_target
            .import_room_transfer(invalid_json_payload)
            .await,
        Err("ROOM_TRANSFER_INVALID_MOVEMENT_STATE")
    );

    let mut unsupported_schema_payload = source
        .export_room_transfer("epoch-1", "room-movement-transfer")
        .await
        .unwrap();
    let mut movement_wrapper =
        serde_json::from_str::<serde_json::Value>(&unsupported_schema_payload.movement_state_json)
            .unwrap();
    let mut movement_inner = serde_json::from_str::<serde_json::Value>(
        movement_wrapper["movementStateJson"].as_str().unwrap(),
    )
    .unwrap();
    movement_inner["schemaVersion"] = serde_json::json!(2);
    movement_wrapper["movementStateJson"] = serde_json::json!(movement_inner.to_string());
    unsupported_schema_payload.movement_state_json = movement_wrapper.to_string();
    unsupported_schema_payload.checksum = room_transfer_checksum(&unsupported_schema_payload);
    let unsupported_schema_target = RoomManager::with_policy_registry_and_cleanup_interval(
        crate::match_client::create_match_client_shared(),
        Arc::new(GameRoomLogicFactory::new(config_tables)),
        SharedRoomPolicyRegistry::default(),
        3600,
    );
    assert_eq!(
        unsupported_schema_target
            .import_room_transfer(unsupported_schema_payload)
            .await,
        Err("ROOM_TRANSFER_UNSUPPORTED_SCHEMA")
    );
}

#[tokio::test]
async fn combat_demo_transfer_restores_combat_payload_consistently() {
    let config_tables = crate::core::config_table::ConfigTableRuntime::load_with_scene_dir(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("csv"),
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scene"),
    )
    .expect("game-server csv fixture should load");
    let factory = Arc::new(GameRoomLogicFactory::new(config_tables.clone()));
    let source = RoomManager::with_policy_registry_and_cleanup_interval(
        crate::match_client::create_match_client_shared(),
        factory.clone(),
        config_tables.room_policy_registry(),
        3600,
    );

    let (tx_a, _rx_a) = mpsc::channel(1024);
    source
        .join_room(
            "room-combat-transfer",
            "player-a",
            tx_a,
            MemberRole::Player,
            Some("combat_demo"),
        )
        .await
        .unwrap();
    let (tx_b, _rx_b) = mpsc::channel(1024);
    source
        .join_room(
            "room-combat-transfer",
            "player-b",
            tx_b,
            MemberRole::Player,
            Some("combat_demo"),
        )
        .await
        .unwrap();
    source
        .set_ready_state("room-combat-transfer", "player-a", true)
        .await
        .unwrap();
    source
        .set_ready_state("room-combat-transfer", "player-b", true)
        .await
        .unwrap();
    source
        .start_game("room-combat-transfer", "player-a")
        .await
        .unwrap();
    stop_runtime_for_test(&source, "room-combat-transfer").await;

    source
        .accept_player_input(
            "room-combat-transfer",
            "player-a",
            1,
            "combat_cast_skill",
            "{\"skillId\":4,\"targetEntityId\":3}",
        )
        .await
        .unwrap();
    source
        .process_room_tick("room-combat-transfer", 20)
        .await
        .unwrap();
    source
        .accept_player_input(
            "room-combat-transfer",
            "player-a",
            2,
            "combat_apply_buff",
            "{\"buffId\":2,\"targetPlayerId\":\"player-b\",\"durationFrames\":77}",
        )
        .await
        .unwrap();
    source
        .process_room_tick("room-combat-transfer", 20)
        .await
        .unwrap();
    source
        .accept_player_input(
            "room-combat-transfer",
            "player-a",
            3,
            "combat_cast_skill",
            "{\"skillId\":2,\"targetPlayerId\":\"player-b\"}",
        )
        .await
        .unwrap();

    let source_game_state = with_room_for_test(&source, "room-combat-transfer", |room| {
        serde_json::from_str::<serde_json::Value>(&room.snapshot().game_state).unwrap()
    })
    .await;
    let source_player_b = combat_demo_entity_by_player(&source_game_state, "player-b");
    let source_player_b_hp = source_player_b["hp"].as_i64().unwrap();
    source
        .disconnect_room_member("room-combat-transfer", "player-a")
        .await;
    source
        .disconnect_room_member("room-combat-transfer", "player-b")
        .await;
    source
        .freeze_room_for_transfer("epoch-1", "room-combat-transfer")
        .await
        .unwrap();

    let payload = source
        .export_room_transfer("epoch-1", "room-combat-transfer")
        .await
        .unwrap();
    let checksum = payload.checksum.clone();
    let transfer_state = room_transfer_state_from_payload(&payload).unwrap();
    let logic_json =
        serde_json::from_str::<serde_json::Value>(&transfer_state.logic_state_json).unwrap();
    let combat_json =
        serde_json::from_str::<serde_json::Value>(&transfer_state.combat_state_json).unwrap();
    let npc_state = RoomNpcTransferState::from_json(&transfer_state.npc_state_json).unwrap();
    let timer_state =
        RoomRuntimeTimerTransferState::from_json(&transfer_state.timer_state_json).unwrap();

    assert_eq!(logic_json["schema"], "combat-demo.logic.v1");
    assert_eq!(logic_json["tick_count"], 2);
    assert_eq!(logic_json["next_snapshot_frame"], 5);
    assert_eq!(
        logic_json["roster"],
        serde_json::json!(["player-a", "player-b"])
    );
    assert_eq!(combat_json["schema"], "room-combat-ecs.v1");
    assert_eq!(combat_json["last_tick_frame"], 2);
    assert_eq!(combat_json["pending_events_replayed"], false);
    assert!(combat_json.get("pending_events").is_none());
    assert_eq!(combat_json["entities"].as_array().unwrap().len(), 4);
    assert_eq!(
        combat_json["player_entity_map"],
        serde_json::json!([
            {"character_id": "player-a", "entity_id": 1},
            {"character_id": "player-b", "entity_id": 2}
        ])
    );
    assert_eq!(combat_json["skill_slots"][0][3]["skill_id"], 4);
    assert_eq!(combat_json["skill_slots"][0][3]["cooldown_remaining"], 59);
    assert_eq!(combat_json["buff_slots"][1][0]["buff_id"], 2);
    assert_eq!(combat_json["buff_slots"][1][0]["duration_remaining"], 76);
    assert_eq!(combat_json["buff_slots"][1][0]["source_entity"], 1);
    assert_eq!(
        combat_json["pending_skill_requests"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert!(combat_json["move_states"][2]["progress"].as_f64().unwrap() < 1.0);
    assert_eq!(npc_state.schema, "room-transfer.npc-state.v1");
    assert_eq!(npc_state.entities.len(), 2);
    let npc_dummy = npc_state
        .entities
        .iter()
        .find(|entity| entity.entity_id == 3)
        .expect("dummy npc transfer entity should exist");
    let combat_dummy_index = combat_json["entities"]
        .as_array()
        .unwrap()
        .iter()
        .position(|entity| entity["entity_id"] == 3)
        .expect("dummy combat transfer entity should exist");
    let exported_dummy_x = combat_json["positions_x"][combat_dummy_index]
        .as_f64()
        .unwrap();
    let exported_dummy_y = combat_json["positions_y"][combat_dummy_index]
        .as_f64()
        .unwrap();
    assert_eq!(npc_dummy.entity_kind, "monster");
    assert_eq!(npc_dummy.behavior_node, "training_dummy.idle");
    assert_eq!(npc_dummy.position.x, exported_dummy_x as f32);
    assert_eq!(npc_dummy.position.y, exported_dummy_y as f32);
    assert_eq!(
        i64::from(npc_dummy.hp),
        combat_json["healths"][combat_dummy_index]["current"]
            .as_i64()
            .unwrap()
    );
    assert_eq!(
        i64::from(npc_dummy.max_hp),
        combat_json["healths"][combat_dummy_index]["max"]
            .as_i64()
            .unwrap()
    );
    assert!(npc_dummy.target_entity_id.is_none());
    assert!(npc_dummy.threat_entries.is_empty());
    assert!(npc_dummy.blackboard.is_empty());
    assert!(npc_dummy.context.is_empty());
    assert!(npc_dummy.rng_state.is_none());
    assert!(npc_dummy.path.waypoints.is_empty());
    assert!(npc_dummy.wait_timer.is_none());
    assert_eq!(
        npc_dummy
            .skill_cooldowns
            .iter()
            .map(|skill| (skill.skill_id, skill.cooldown_remaining))
            .collect::<Vec<_>>(),
        vec![(1, 0), (5, 0)]
    );
    assert_eq!(timer_state.runtime_summary.owner_kind, "combat-demo");
    assert_eq!(timer_state.runtime_summary.logical_frame, 2);
    assert_eq!(timer_state.runtime_summary.logical_tick, 2);
    assert_eq!(timer_state.scheduler_entries.len(), 1);
    assert_eq!(
        timer_state.scheduler_entries[0].id,
        "combat-demo.snapshot-push"
    );
    assert_eq!(timer_state.scheduler_entries[0].next_frame, 5);
    assert_eq!(timer_state.scheduler_entries[0].interval_frames, Some(5));
    assert_eq!(timer_state.timer_entries.len(), 1);
    assert_eq!(timer_state.timer_entries[0].remaining_frames, 3);

    let target = RoomManager::with_policy_registry_and_cleanup_interval(
        crate::match_client::create_match_client_shared(),
        factory,
        config_tables.room_policy_registry(),
        3600,
    );
    let imported = target.import_room_transfer(payload.clone()).await.unwrap();
    assert_eq!(imported.0, checksum);

    with_room_for_test(&target, "room-combat-transfer", |room| {
        assert_eq!(room.current_frame, 2);
        assert_eq!(room.transfer_state.status, RoomTransferStatus::OwnedByNew);
        let imported_game_state =
            serde_json::from_str::<serde_json::Value>(&room.snapshot().game_state).unwrap();
        assert_eq!(imported_game_state, source_game_state);
        assert_eq!(imported_game_state["next_snapshot_frame"], 5);
    })
    .await;

    let (reconnect_a_tx, _reconnect_a_rx) = mpsc::channel(1024);
    target
        .reconnect_room("room-combat-transfer", "player-a", reconnect_a_tx)
        .await
        .unwrap();
    let (reconnect_b_tx, _reconnect_b_rx) = mpsc::channel(1024);
    target
        .reconnect_room("room-combat-transfer", "player-b", reconnect_b_tx)
        .await
        .unwrap();
    target
        .process_room_tick("room-combat-transfer", 20)
        .await
        .unwrap();

    with_room_for_test(&target, "room-combat-transfer", |room| {
        assert_eq!(room.current_frame, 3);
        let advanced_game_state =
            serde_json::from_str::<serde_json::Value>(&room.snapshot().game_state).unwrap();
        assert_eq!(advanced_game_state["tick_count"], 3);
        assert_eq!(advanced_game_state["next_snapshot_frame"], 5);
        assert_eq!(advanced_game_state["snapshot"]["frame_id"], 3);
        let player_a = combat_demo_entity_by_player(&advanced_game_state, "player-a");
        let fireball = player_a["skills"]
            .as_array()
            .unwrap()
            .iter()
            .find(|skill| skill["skill_id"] == 2)
            .expect("fireball skill should exist");
        assert_eq!(fireball["cooldown_remaining"], 90);
        let player_b = combat_demo_entity_by_player(&advanced_game_state, "player-b");
        assert!(player_b["hp"].as_i64().unwrap() < source_player_b_hp);
        assert_eq!(player_b["buffs"][0]["buff_id"], 2);
        assert_eq!(player_b["buffs"][0]["duration_remaining"], 75);
        let dummy = advanced_game_state["snapshot"]["entities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entity| entity["entity_id"] == 3)
            .unwrap();
        assert!(dummy["x"].as_f64().unwrap() > exported_dummy_x);
    })
    .await;

    let mut invalid_json_payload = payload.clone();
    let mut logic_wrapper =
        serde_json::from_str::<serde_json::Value>(&invalid_json_payload.logic_state_json).unwrap();
    logic_wrapper["combatStateJson"] = serde_json::json!("{bad");
    invalid_json_payload.logic_state_json = logic_wrapper.to_string();
    invalid_json_payload.checksum = room_transfer_checksum(&invalid_json_payload);
    let invalid_json_target = RoomManager::with_policy_registry_and_cleanup_interval(
        crate::match_client::create_match_client_shared(),
        Arc::new(GameRoomLogicFactory::new(config_tables.clone())),
        config_tables.room_policy_registry(),
        3600,
    );
    assert_eq!(
        invalid_json_target
            .import_room_transfer(invalid_json_payload)
            .await,
        Err("ROOM_TRANSFER_INVALID_COMBAT_STATE")
    );

    let mut mismatched_npc_payload = payload.clone();
    let mut logic_wrapper =
        serde_json::from_str::<serde_json::Value>(&mismatched_npc_payload.logic_state_json)
            .unwrap();
    let mut npc_inner =
        serde_json::from_str::<serde_json::Value>(logic_wrapper["npcStateJson"].as_str().unwrap())
            .unwrap();
    npc_inner["entities"][0]["position"]["x"] = serde_json::json!(999.0);
    logic_wrapper["npcStateJson"] = serde_json::json!(npc_inner.to_string());
    mismatched_npc_payload.logic_state_json = logic_wrapper.to_string();
    mismatched_npc_payload.checksum = room_transfer_checksum(&mismatched_npc_payload);
    let mismatched_npc_target = RoomManager::with_policy_registry_and_cleanup_interval(
        crate::match_client::create_match_client_shared(),
        Arc::new(GameRoomLogicFactory::new(config_tables.clone())),
        config_tables.room_policy_registry(),
        3600,
    );
    assert_eq!(
        mismatched_npc_target
            .import_room_transfer(mismatched_npc_payload)
            .await,
        Err("ROOM_TRANSFER_INVALID_NPC_STATE")
    );

    let mut duplicate_npc_payload = payload.clone();
    let mut logic_wrapper =
        serde_json::from_str::<serde_json::Value>(&duplicate_npc_payload.logic_state_json).unwrap();
    let mut npc_inner =
        serde_json::from_str::<serde_json::Value>(logic_wrapper["npcStateJson"].as_str().unwrap())
            .unwrap();
    let first_entity = npc_inner["entities"][0].clone();
    npc_inner["entities"]
        .as_array_mut()
        .unwrap()
        .push(first_entity);
    logic_wrapper["npcStateJson"] = serde_json::json!(npc_inner.to_string());
    duplicate_npc_payload.logic_state_json = logic_wrapper.to_string();
    duplicate_npc_payload.checksum = room_transfer_checksum(&duplicate_npc_payload);
    let duplicate_npc_target = RoomManager::with_policy_registry_and_cleanup_interval(
        crate::match_client::create_match_client_shared(),
        Arc::new(GameRoomLogicFactory::new(config_tables.clone())),
        config_tables.room_policy_registry(),
        3600,
    );
    assert_eq!(
        duplicate_npc_target
            .import_room_transfer(duplicate_npc_payload)
            .await,
        Err("ROOM_TRANSFER_INVALID_NPC_STATE")
    );

    let mut unsupported_schema_payload = payload;
    let mut logic_wrapper =
        serde_json::from_str::<serde_json::Value>(&unsupported_schema_payload.logic_state_json)
            .unwrap();
    let mut npc_inner =
        serde_json::from_str::<serde_json::Value>(logic_wrapper["npcStateJson"].as_str().unwrap())
            .unwrap();
    npc_inner["schemaVersion"] = serde_json::json!(2);
    logic_wrapper["npcStateJson"] = serde_json::json!(npc_inner.to_string());
    unsupported_schema_payload.logic_state_json = logic_wrapper.to_string();
    unsupported_schema_payload.checksum = room_transfer_checksum(&unsupported_schema_payload);
    let unsupported_schema_target = RoomManager::with_policy_registry_and_cleanup_interval(
        crate::match_client::create_match_client_shared(),
        Arc::new(GameRoomLogicFactory::new(config_tables)),
        SharedRoomPolicyRegistry::default(),
        3600,
    );
    assert_eq!(
        unsupported_schema_target
            .import_room_transfer(unsupported_schema_payload)
            .await,
        Err("ROOM_TRANSFER_UNSUPPORTED_SCHEMA")
    );
}

#[tokio::test]
async fn repeated_export_room_transfer_is_idempotent() {
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

    let first = manager
        .export_room_transfer("epoch-1", "room-test")
        .await
        .unwrap();
    let second = manager
        .export_room_transfer("epoch-1", "room-test")
        .await
        .unwrap();

    assert_eq!(second.room_version, first.room_version);
    assert_eq!(second.checksum, first.checksum);
    assert_eq!(second.checksum, room_transfer_checksum(&second));
}

#[tokio::test]
async fn import_room_transfer_rejects_bad_checksum() {
    let (source, _factory, _receivers) =
        setup_started_room("default_match", &["player-a", "player-b"]).await;
    source.disconnect_room_member("room-test", "player-a").await;
    source.disconnect_room_member("room-test", "player-b").await;
    source
        .freeze_room_for_transfer("epoch-1", "room-test")
        .await
        .unwrap();
    let mut payload = source
        .export_room_transfer("epoch-1", "room-test")
        .await
        .unwrap();
    payload.checksum = "bad-checksum".to_string();

    let target_factory = RecordingRoomLogicFactory::default();
    let target = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(target_factory),
    );

    let result = target.import_room_transfer(payload).await;

    assert_eq!(result, Err("ROOM_TRANSFER_CHECKSUM_MISMATCH"));
    assert!(!target.room_exists("room-test").await);
}

#[tokio::test]
async fn import_room_transfer_rejects_logic_without_transfer_contract() {
    let (source, _factory, _receivers) =
        setup_started_room("default_match", &["player-a", "player-b"]).await;
    source.disconnect_room_member("room-test", "player-a").await;
    source.disconnect_room_member("room-test", "player-b").await;
    source
        .freeze_room_for_transfer("epoch-1", "room-test")
        .await
        .unwrap();
    let payload = source
        .export_room_transfer("epoch-1", "room-test")
        .await
        .unwrap();

    let target = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(UnsupportedTransferRoomLogicFactory),
    );

    let result = target.import_room_transfer(payload).await;

    assert_eq!(result, Err("UNSUPPORTED_ROOM_TRANSFER"));
    assert!(!target.room_exists("room-test").await);
}

#[tokio::test]
async fn import_room_transfer_rejects_unsupported_schema_without_creating_room() {
    let (source, _factory, _receivers) =
        setup_started_room("default_match", &["player-a", "player-b"]).await;
    source.disconnect_room_member("room-test", "player-a").await;
    source.disconnect_room_member("room-test", "player-b").await;
    source
        .freeze_room_for_transfer("epoch-1", "room-test")
        .await
        .unwrap();
    let mut payload = source
        .export_room_transfer("epoch-1", "room-test")
        .await
        .unwrap();
    let mut logic_state =
        serde_json::from_str::<serde_json::Value>(&payload.logic_state_json).unwrap();
    logic_state["schemaVersion"] = serde_json::json!(ROOM_TRANSFER_SCHEMA_VERSION + 1);
    payload.logic_state_json = logic_state.to_string();
    payload.checksum = room_transfer_checksum(&payload);

    let target_factory = RecordingRoomLogicFactory::default();
    let target = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(target_factory),
    );

    let result = target.import_room_transfer(payload).await;

    assert_eq!(result, Err("ROOM_TRANSFER_UNSUPPORTED_SCHEMA"));
    assert!(!target.room_exists("room-test").await);
}

#[tokio::test]
async fn import_room_transfer_rejects_invalid_timer_wrapper_contract() {
    let (source, _factory, _receivers) =
        setup_started_room("default_match", &["player-a", "player-b"]).await;
    source.disconnect_room_member("room-test", "player-a").await;
    source.disconnect_room_member("room-test", "player-b").await;
    source
        .freeze_room_for_transfer("epoch-1", "room-test")
        .await
        .unwrap();
    let payload = source
        .export_room_transfer("epoch-1", "room-test")
        .await
        .unwrap();
    let base_timers =
        serde_json::from_str::<serde_json::Value>(&payload.runtime_timers_json).unwrap();

    let mut unsupported_schema = base_timers.clone();
    unsupported_schema["schema"] = serde_json::json!("room-transfer.runtime-timers.v2");

    let mut unsupported_version = base_timers.clone();
    unsupported_version["schemaVersion"] = serde_json::json!(ROOM_TRANSFER_SCHEMA_VERSION + 1);

    let mut non_string_timer_state = base_timers.clone();
    non_string_timer_state["timerStateJson"] = serde_json::json!({});

    let mut missing_runtime_summary = base_timers.clone();
    missing_runtime_summary
        .as_object_mut()
        .unwrap()
        .remove("runtimeSummary");

    let mut bad_runtime_summary_type = base_timers.clone();
    bad_runtime_summary_type["runtimeSummary"]["hasWaitStarted"] = serde_json::json!("false");

    let mut bad_snapshot_interval = base_timers.clone();
    bad_snapshot_interval["runtimeSummary"]["snapshotIntervalFrames"] = serde_json::json!(0);

    let mut timer_inner_unsupported_schema =
        serde_json::from_str::<serde_json::Value>(base_timers["timerStateJson"].as_str().unwrap())
            .unwrap();
    timer_inner_unsupported_schema["schema"] =
        serde_json::json!("room-transfer.runtime-timer-state.v2");
    let mut bad_timer_inner_schema = base_timers.clone();
    bad_timer_inner_schema["timerStateJson"] =
        serde_json::json!(timer_inner_unsupported_schema.to_string());

    let mut timer_inner_missing_owner =
        serde_json::from_str::<serde_json::Value>(base_timers["timerStateJson"].as_str().unwrap())
            .unwrap();
    timer_inner_missing_owner["runtimeSummary"]["ownerKind"] = serde_json::json!("");
    let mut bad_timer_inner_owner = base_timers.clone();
    bad_timer_inner_owner["timerStateJson"] =
        serde_json::json!(timer_inner_missing_owner.to_string());

    let mut timer_inner_bad_interval =
        serde_json::from_str::<serde_json::Value>(base_timers["timerStateJson"].as_str().unwrap())
            .unwrap();
    timer_inner_bad_interval["timerEntries"][0]["repeatIntervalFrames"] = serde_json::json!(0);
    let mut bad_timer_inner_interval = base_timers.clone();
    bad_timer_inner_interval["timerStateJson"] =
        serde_json::json!(timer_inner_bad_interval.to_string());

    let mut timer_inner_duplicate_id =
        serde_json::from_str::<serde_json::Value>(base_timers["timerStateJson"].as_str().unwrap())
            .unwrap();
    timer_inner_duplicate_id["schedulerEntries"] = serde_json::json!([{
        "id": "recording-timer",
        "schedulerKind": "duplicate",
        "nextFrame": 1
    }]);
    let mut bad_timer_inner_duplicate_id = base_timers;
    bad_timer_inner_duplicate_id["timerStateJson"] =
        serde_json::json!(timer_inner_duplicate_id.to_string());

    let target_factory = RecordingRoomLogicFactory::default();
    let target = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(target_factory),
    );
    for (runtime_timers_json, expected) in [
        (unsupported_schema, "ROOM_TRANSFER_UNSUPPORTED_SCHEMA"),
        (unsupported_version, "ROOM_TRANSFER_UNSUPPORTED_SCHEMA"),
        (non_string_timer_state, "ROOM_TRANSFER_INVALID_TIMER_STATE"),
        (missing_runtime_summary, "ROOM_TRANSFER_INVALID_TIMER_STATE"),
        (
            bad_runtime_summary_type,
            "ROOM_TRANSFER_INVALID_TIMER_STATE",
        ),
        (bad_snapshot_interval, "ROOM_TRANSFER_INVALID_TIMER_STATE"),
        (bad_timer_inner_schema, "ROOM_TRANSFER_UNSUPPORTED_SCHEMA"),
        (bad_timer_inner_owner, "ROOM_TRANSFER_INVALID_TIMER_STATE"),
        (
            bad_timer_inner_interval,
            "ROOM_TRANSFER_INVALID_TIMER_STATE",
        ),
        (
            bad_timer_inner_duplicate_id,
            "ROOM_TRANSFER_INVALID_TIMER_STATE",
        ),
    ] {
        let mut bad_payload = payload.clone();
        bad_payload.runtime_timers_json = runtime_timers_json.to_string();
        bad_payload.checksum = room_transfer_checksum(&bad_payload);

        assert_eq!(
            target.import_room_transfer(bad_payload).await,
            Err(expected)
        );
        assert!(!target.room_exists("room-test").await);
    }
}

#[tokio::test]
async fn import_room_transfer_accepts_empty_timer_state_json() {
    let (source, _factory, _receivers) =
        setup_started_room("default_match", &["player-a", "player-b"]).await;
    source.disconnect_room_member("room-test", "player-a").await;
    source.disconnect_room_member("room-test", "player-b").await;
    source
        .freeze_room_for_transfer("epoch-1", "room-test")
        .await
        .unwrap();
    let mut payload = source
        .export_room_transfer("epoch-1", "room-test")
        .await
        .unwrap();
    let mut timers =
        serde_json::from_str::<serde_json::Value>(&payload.runtime_timers_json).unwrap();
    timers["timerStateJson"] = serde_json::json!("");
    payload.runtime_timers_json = timers.to_string();
    payload.checksum = room_transfer_checksum(&payload);

    let target_factory = RecordingRoomLogicFactory::default();
    let target = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(target_factory.clone()),
    );

    target.import_room_transfer(payload).await.unwrap();

    let imported_states = target_factory.imported_transfer_states();
    assert_eq!(imported_states.len(), 1);
    assert!(imported_states[0].timer_state_json.is_empty());
}

#[tokio::test]
async fn import_room_transfer_restores_basic_room_state() {
    let (source, _source_factory, _receivers) =
        setup_started_room("default_match", &["player-a", "player-b"]).await;
    source.disconnect_room_member("room-test", "player-a").await;
    source.disconnect_room_member("room-test", "player-b").await;
    source
        .freeze_room_for_transfer("epoch-1", "room-test")
        .await
        .unwrap();
    let payload = source
        .export_room_transfer("epoch-1", "room-test")
        .await
        .unwrap();
    let checksum = payload.checksum.clone();

    let target_factory = RecordingRoomLogicFactory::default();
    let target = RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(target_factory.clone()),
    );

    let imported = target.import_room_transfer(payload).await.unwrap();

    assert_eq!(imported.0, checksum);
    assert!(target.room_exists("room-test").await);
    let imported_states = target_factory.imported_transfer_states();
    assert_eq!(imported_states.len(), 1);
    let imported_state = &imported_states[0];
    assert_eq!(imported_state.schema_version, ROOM_TRANSFER_SCHEMA_VERSION);
    assert_eq!(imported_state.logic_state_json, "recording-state-v1");
    assert_eq!(
        imported_state.movement_state_json,
        r#"{"movement":"recording-v1"}"#
    );
    assert_eq!(
        imported_state.combat_state_json,
        r#"{"combat":"recording-v1"}"#
    );
    assert_eq!(imported_state.npc_state_json, r#"{"npc":"recording-v1"}"#);
    let timer_state =
        RoomRuntimeTimerTransferState::from_json(&imported_state.timer_state_json).unwrap();
    assert_eq!(
        timer_state.runtime_summary.owner_kind,
        "recording-room-logic"
    );
    assert_eq!(timer_state.timer_entries.len(), 1);
    assert_eq!(timer_state.timer_entries[0].id, "recording-timer");
    assert_eq!(
        timer_state.metadata.get("fixture").map(String::as_str),
        Some("recording-v1")
    );

    let (tx, _rx) = mpsc::channel(1024);
    let snapshot = target
        .reconnect_room("room-test", "player-a", tx)
        .await
        .unwrap()
        .snapshot;
    assert_eq!(snapshot.room_id, "room-test");
}

async fn setup_imported_room_for_confirm() -> (RoomManager, RecordingRoomLogicFactory, String, u64)
{
    let (source, _source_factory, _receivers) =
        setup_started_room("default_match", &["player-a", "player-b"]).await;
    source.disconnect_room_member("room-test", "player-a").await;
    source.disconnect_room_member("room-test", "player-b").await;
    source
        .freeze_room_for_transfer("epoch-1", "room-test")
        .await
        .unwrap();
    let payload = source
        .export_room_transfer("epoch-1", "room-test")
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
        .confirm_room_ownership("epoch-1", "room-test", &checksum, room_version)
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
            .confirm_room_ownership("epoch-2", "room-test", &checksum, room_version)
            .await,
        Err("ROOM_TRANSFER_EPOCH_MISMATCH")
    );
    assert_eq!(
        target
            .confirm_room_ownership("epoch-1", "room-test", "wrong", room_version)
            .await,
        Err("ROOM_TRANSFER_CHECKSUM_MISMATCH")
    );
    assert_eq!(
        target
            .confirm_room_ownership(
                "epoch-1",
                "room-test",
                &checksum,
                room_version.saturating_add(1)
            )
            .await,
        Err("ROOM_TRANSFER_VERSION_MISMATCH")
    );
    assert_eq!(
        target
            .confirm_room_ownership("", "room-test", &checksum, room_version)
            .await,
        Err("INVALID_ROLLOUT_EPOCH")
    );
}

#[tokio::test]
async fn imported_room_is_treated_as_taken_over_room_for_join_and_reconnect() {
    let (target, target_factory, checksum, room_version) = setup_imported_room_for_confirm().await;
    target
        .confirm_room_ownership("epoch-1", "room-test", &checksum, room_version)
        .await
        .unwrap();

    with_room_for_test(&target, "room-test", |room| {
        assert_eq!(room.transfer_state.status, RoomTransferStatus::OwnedByNew);
        assert_eq!(
            room.transfer_state.rollout_epoch.as_deref(),
            Some("epoch-1")
        );
        assert_eq!(room.transfer_state.room_version, room_version);
        assert_eq!(
            room.transfer_state.last_transfer_checksum.as_deref(),
            Some(checksum.as_str())
        );
        assert!(room.members.contains_key("player-a"));
        assert!(room.members.contains_key("player-b"));
    })
    .await;

    let (reconnect_tx, _reconnect_rx) = mpsc::channel(1024);
    let reconnect = target
        .reconnect_room("room-test", "player-a", reconnect_tx)
        .await
        .unwrap();
    assert_eq!(reconnect.snapshot.room_id, "room-test");

    let (join_tx, _join_rx) = mpsc::channel(1024);
    let join_snapshot = target
        .join_room(
            "room-test",
            "player-b",
            join_tx,
            MemberRole::Player,
            Some("default_match"),
        )
        .await
        .unwrap();
    assert_eq!(join_snapshot.room_id, "room-test");
    assert!(
        join_snapshot
            .members
            .iter()
            .any(|member| member.character_id == "player-b")
    );

    assert_eq!(target.room_count().await, 1);
    with_room_for_test(&target, "room-test", |room| {
        assert_eq!(room.transfer_state.status, RoomTransferStatus::OwnedByNew);
        assert_eq!(room.transfer_state.room_version, room_version);
    })
    .await;
    assert_eq!(target_factory.imported_transfer_states().len(), 1);
}

#[tokio::test]
async fn confirm_room_ownership_rejects_room_not_owned_by_new() {
    let (manager, _factory, _receivers) =
        setup_started_room("default_match", &["player-a", "player-b"]).await;

    let result = manager
        .confirm_room_ownership("epoch-1", "room-test", "checksum", 1)
        .await;

    assert_eq!(result, Err("ROOM_TRANSFER_NOT_OWNED_BY_NEW"));
}

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

#[tokio::test]
async fn strict_wait_strategy_blocks_until_all_inputs_arrive() {
    let (manager, factory, _receivers) =
        setup_started_room("default_match", &["player-a", "player-b"]).await;

    manager
        .accept_player_input("room-test", "player-a", 1, "move", "{\"x\":1}")
        .await
        .unwrap();

    let progressed = manager.process_room_tick("room-test", 10).await;
    assert!(progressed.is_none());
    assert!(factory.recorded_ticks().is_empty());

    manager
        .accept_player_input("room-test", "player-b", 1, "move", "{\"x\":2}")
        .await
        .unwrap();

    let progressed = manager.process_room_tick("room-test", 10).await;
    assert!(progressed.is_some());
    let recorded = factory.recorded_ticks();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].0, 1);
    assert_eq!(recorded[0].1.len(), 2);
}

#[tokio::test]
async fn optimistic_strategy_advances_with_partial_inputs() {
    let (manager, factory, _receivers) =
        setup_started_room("movement_demo", &["player-a", "player-b"]).await;

    manager
        .accept_player_input(
            "room-test",
            "player-a",
            1,
            "move_dir",
            "{\"dirX\":1,\"dirY\":0}",
        )
        .await
        .unwrap();

    let progressed = manager.process_room_tick("room-test", 20).await;
    assert!(progressed.is_some());

    let recorded = factory.recorded_ticks();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].0, 1);
    assert_eq!(recorded[0].1.len(), 2);
    assert!(
        recorded[0]
            .1
            .iter()
            .any(|input| input.character_id == "player-b" && input.action.is_empty())
    );
}

#[tokio::test]
async fn future_inputs_are_buffered_until_their_frame_is_ready() {
    let (manager, factory, _receivers) =
        setup_started_room("default_match", &["player-a", "player-b"]).await;

    manager
        .accept_player_input("room-test", "player-a", 2, "move", "{\"x\":20}")
        .await
        .unwrap();
    manager
        .accept_player_input("room-test", "player-b", 2, "move", "{\"x\":21}")
        .await
        .unwrap();
    manager
        .accept_player_input("room-test", "player-a", 1, "move", "{\"x\":10}")
        .await
        .unwrap();
    manager
        .accept_player_input("room-test", "player-b", 1, "move", "{\"x\":11}")
        .await
        .unwrap();

    let first = manager.process_room_tick("room-test", 10).await.unwrap();
    assert_eq!(first.0.frame_id, 1);

    let second = manager.process_room_tick("room-test", 10).await.unwrap();
    assert_eq!(second.0.frame_id, 2);

    let recorded = factory.recorded_ticks();
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0].0, 1);
    assert_eq!(recorded[1].0, 2);
}

#[tokio::test]
async fn expired_input_frame_is_rejected() {
    let (manager, _factory, _receivers) =
        setup_started_room("default_match", &["player-a", "player-b"]).await;

    manager
        .accept_player_input("room-test", "player-a", 1, "move", "{\"x\":1}")
        .await
        .unwrap();
    manager
        .accept_player_input("room-test", "player-b", 1, "move", "{\"x\":2}")
        .await
        .unwrap();
    let _ = manager.process_room_tick("room-test", 10).await;

    let result = manager
        .accept_player_input("room-test", "player-a", 1, "move", "{\"x\":3}")
        .await;
    assert_eq!(result, Err("INPUT_FRAME_EXPIRED"));
}

#[tokio::test]
async fn input_too_far_is_rejected() {
    let (manager, _factory, _receivers) =
        setup_started_room("default_match", &["player-a", "player-b"]).await;

    let result = manager
        .accept_player_input("room-test", "player-a", 5, "move", "{\"x\":1}")
        .await;
    assert_eq!(result, Err("INPUT_FRAME_TOO_FAR"));
}

#[tokio::test]
async fn rejected_input_does_not_trigger_player_input_hook() {
    let (manager, factory, _receivers) =
        setup_started_room("default_match", &["player-a", "player-b"]).await;

    let too_far = manager
        .accept_player_input("room-test", "player-a", 5, "move", "{\"x\":1}")
        .await;
    assert_eq!(too_far, Err("INPUT_FRAME_TOO_FAR"));
    assert!(factory.recorded_inputs().is_empty());

    manager
        .accept_player_input("room-test", "player-a", 1, "move", "{\"x\":1}")
        .await
        .unwrap();
    manager
        .accept_player_input("room-test", "player-b", 1, "move", "{\"x\":2}")
        .await
        .unwrap();
    let _ = manager.process_room_tick("room-test", 10).await;

    let expired = manager
        .accept_player_input("room-test", "player-a", 1, "move", "{\"x\":3}")
        .await;
    assert_eq!(expired, Err("INPUT_FRAME_EXPIRED"));

    let recorded = factory.recorded_inputs();
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0].0, "player-a");
    assert_eq!(recorded[1].0, "player-b");
}

#[tokio::test]
async fn same_frame_input_replaces_previous_one() {
    let (manager, factory, _receivers) = setup_started_room("movement_demo", &["player-a"]).await;

    manager
        .accept_player_input(
            "room-test",
            "player-a",
            1,
            "move_dir",
            "{\"dirX\":1,\"dirY\":0}",
        )
        .await
        .unwrap();
    manager
        .accept_player_input(
            "room-test",
            "player-a",
            1,
            "face_to",
            "{\"dirX\":0,\"dirY\":1}",
        )
        .await
        .unwrap();

    let _ = manager.process_room_tick("room-test", 20).await;
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
            "room-test",
            "player-a",
            owner_tx,
            MemberRole::Player,
            Some("default_match"),
        )
        .await
        .unwrap();
    manager
        .join_room(
            "room-test",
            "player-b",
            other_tx,
            MemberRole::Player,
            Some("default_match"),
        )
        .await
        .unwrap();
    manager
        .set_ready_state("room-test", "player-a", true)
        .await
        .unwrap();
    manager
        .set_ready_state("room-test", "player-b", true)
        .await
        .unwrap();
    manager.start_game("room-test", "player-a").await.unwrap();
    stop_runtime_for_test(&manager, "room-test").await;

    manager
        .accept_player_input("room-test", "player-a", 1, "move", "{\"x\":1}")
        .await
        .unwrap();

    with_room_mut_for_test(&manager, "room-test", |room| {
        let member = room.members.get_mut("player-a").unwrap();
        member.offline = true;
        member.offline_since = Some(Instant::now());
    })
    .await;

    let (reconnect_tx, _reconnect_rx) = mpsc::channel(1024);
    let recovery = manager
        .reconnect_room("room-test", "player-a", reconnect_tx)
        .await
        .unwrap();
    assert_eq!(recovery.waiting_frame_id, 1);
    assert_eq!(recovery.input_delay_frames, 2);
    assert_eq!(recovery.waiting_inputs.len(), 1);
    assert_eq!(recovery.waiting_inputs[0].frame_id, 1);

    let (observer_tx, _observer_rx) = mpsc::channel(1024);
    let observer = manager
        .join_room_as_observer("room-test", "observer-1", observer_tx)
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
    }

    manager
        .set_ready_state("room-test", "player-a", false)
        .await
        .unwrap();
    manager
        .set_ready_state("room-test", "player-a", true)
        .await
        .unwrap();
    manager
        .set_ready_state("room-test", "player-b", true)
        .await
        .unwrap();
    manager.start_game("room-test", "player-a").await.unwrap();
    stop_runtime_for_test(&manager, "room-test").await;

    manager
        .accept_player_input("room-test", "player-a", 1, "move", "{\"x\":1}")
        .await
        .unwrap();
    manager
        .accept_player_input("room-test", "player-b", 1, "move", "{\"x\":2}")
        .await
        .unwrap();
    let progressed = manager.process_room_tick("room-test", 10).await;
    assert!(progressed.is_some());
    assert_eq!(factory.recorded_ticks().len(), 1);

    manager
        .disconnect_room_member("room-test", "player-a")
        .await;
    let (reconnect_tx, _reconnect_rx) = mpsc::channel(1024);
    let recovery = manager
        .reconnect_room("room-test", "player-a", reconnect_tx)
        .await
        .unwrap();
    assert_eq!(recovery.snapshot.state, "in_game");

    manager.cleanup_expired_offline_characters().await;
    assert!(manager.room_exists("room-test").await);

    let (waiting_tx, _waiting_rx) = mpsc::channel(1024);
    manager
        .join_room(
            "room-observer",
            "player-host",
            waiting_tx,
            MemberRole::Player,
            Some("default_match"),
        )
        .await
        .unwrap();
    let (observer_tx, _observer_rx) = mpsc::channel(1024);
    let observer = manager
        .join_room_as_observer("room-observer", "observer-1", observer_tx)
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
            "player-a",
            tx,
            MemberRole::Player,
            Some("disposable_match"),
        )
        .await
        .unwrap();
    assert!(runtime_exists_for_test(&manager, "room-reused").await);
    manager.leave_room("room-reused", "player-a").await;

    for _ in 0..30 {
        if !manager.room_exists("room-reused").await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!manager.room_exists("room-reused").await);
    assert!(!runtime_exists_for_test(&manager, "room-reused").await);

    for character_id in ["player-a", "player-b"] {
        let (tx, _rx) = mpsc::channel(1024);
        manager
            .join_room(
                "room-reused",
                character_id,
                tx,
                MemberRole::Player,
                Some("default_match"),
            )
            .await
            .unwrap();
        manager
            .set_ready_state("room-reused", character_id, true)
            .await
            .unwrap();
    }

    manager.start_game("room-reused", "player-a").await.unwrap();
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
        setup_started_room("default_match", &["player-a", "player-b"]).await;

    manager
        .accept_player_input("room-test", "player-a", 1, "move", "{\"x\":1}")
        .await
        .unwrap();
    manager
        .accept_player_input("room-test", "player-b", 1, "move", "{\"x\":2}")
        .await
        .unwrap();
    let _ = manager.process_room_tick("room-test", 10).await;

    manager
        .accept_player_input("room-test", "player-a", 2, "move", "{\"x\":3}")
        .await
        .unwrap();
    with_room_mut_for_test(&manager, "room-test", |room| {
        room.wait_started_at = Some(Instant::now() - Duration::from_millis(500));
    })
    .await;

    let _ = manager.process_room_tick("room-test", 10).await;
    let recorded = factory.recorded_ticks();
    assert_eq!(recorded.len(), 2);
    let second_tick = &recorded[1];
    let repeated = second_tick
        .1
        .iter()
        .find(|input| input.character_id == "player-b")
        .unwrap();
    assert_eq!(repeated.frame_id, 2);
    assert_eq!(repeated.action, "move");
    assert_eq!(repeated.payload_json, "{\"x\":2}");
}

#[tokio::test]
async fn disconnect_path_preserves_in_game_waiting_state_for_reconnect() {
    let (manager, _factory, _receivers) =
        setup_started_room("default_match", &["player-a", "player-b"]).await;

    manager
        .accept_player_input("room-test", "player-a", 1, "move", "{\"x\":1}")
        .await
        .unwrap();

    let disconnected = manager
        .disconnect_room_member("room-test", "player-a")
        .await;
    let snapshot = disconnected.snapshot.expect("disconnect snapshot");
    assert_eq!(snapshot.state, "in_game");
    assert_eq!(snapshot.current_frame_id, 0);

    let (reconnect_tx, _reconnect_rx) = mpsc::channel(1024);
    let recovery = manager
        .reconnect_room("room-test", "player-a", reconnect_tx)
        .await
        .unwrap();

    assert_eq!(recovery.waiting_frame_id, 1);
    assert_eq!(recovery.waiting_inputs.len(), 1);
    assert_eq!(recovery.waiting_inputs[0].frame_id, 1);
    assert_eq!(recovery.snapshot.state, "in_game");
}

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
async fn send_to_player_uses_index_and_self_heals_stale_entry() {
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
        .send_to_player("player-a", MessageType::GameMessagePush, vec![1, 2, 3])
        .await
        .unwrap();
    let delivered = rx
        .try_recv()
        .expect("indexed player should receive message");
    assert_eq!(delivered.message_type, MessageType::GameMessagePush);
    assert_eq!(delivered.body, vec![1, 2, 3]);

    {
        let mut rooms = manager.rooms.write().await;
        rooms.remove("room-test");
    }
    manager
        .send_to_player("player-a", MessageType::GameMessagePush, vec![4, 5, 6])
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
                .send_to_player(
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
