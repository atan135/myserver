use super::*;

pub(super) const TEST_ROOM_ID: &str = "room-test";
pub(super) const PLAYER_A: &str = "player-a";
pub(super) const PLAYER_B: &str = "player-b";
pub(super) const PLAYER_C: &str = "player-c";
pub(super) const OBSERVER_1: &str = "observer-1";
pub(super) const ROLLOUT_EPOCH: &str = "epoch-1";
pub(super) const DEFAULT_POLICY: &str = "default_match";
pub(super) const MOVEMENT_DEMO_POLICY: &str = "movement_demo";
pub(super) const COMBAT_DEMO_POLICY: &str = "combat_demo";
pub(super) const DISPOSABLE_MATCH_POLICY: &str = "disposable_match";

#[derive(Clone, Default)]
pub(super) struct RecordingRoomLogicFactory {
    pub(super) ticks: Arc<StdMutex<Vec<(u32, Vec<PlayerInputRecord>)>>>,
    pub(super) inputs: Arc<StdMutex<Vec<(String, String, String)>>>,
    pub(super) imported_transfer_states: Arc<StdMutex<Vec<RoomLogicTransferState>>>,
}

impl RecordingRoomLogicFactory {
    pub(super) fn recorded_ticks(&self) -> Vec<(u32, Vec<PlayerInputRecord>)> {
        self.ticks.lock().unwrap().clone()
    }

    pub(super) fn recorded_inputs(&self) -> Vec<(String, String, String)> {
        self.inputs.lock().unwrap().clone()
    }

    pub(super) fn imported_transfer_states(&self) -> Vec<RoomLogicTransferState> {
        self.imported_transfer_states.lock().unwrap().clone()
    }
}

pub(super) struct RecordingRoomLogic {
    pub(super) ticks: Arc<StdMutex<Vec<(u32, Vec<PlayerInputRecord>)>>>,
    pub(super) inputs: Arc<StdMutex<Vec<(String, String, String)>>>,
    pub(super) imported_transfer_states: Arc<StdMutex<Vec<RoomLogicTransferState>>>,
    pub(super) state: String,
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

pub(super) struct UnsupportedTransferRoomLogic;

impl RoomLogicTransfer for UnsupportedTransferRoomLogic {}

impl RoomLogic for UnsupportedTransferRoomLogic {}

pub(super) struct UnsupportedTransferRoomLogicFactory;

impl RoomLogicFactory for UnsupportedTransferRoomLogicFactory {
    fn create(&self, _policy_id: &str) -> Box<dyn RoomLogic> {
        Box::new(UnsupportedTransferRoomLogic)
    }
}

pub(super) async fn setup_started_room(
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
                TEST_ROOM_ID,
                character_id,
                tx,
                MemberRole::Player,
                Some(policy_id),
            )
            .await
            .unwrap();
        manager
            .set_ready_state(TEST_ROOM_ID, character_id, true)
            .await
            .unwrap();
    }
    manager
        .start_game(TEST_ROOM_ID, characters[0])
        .await
        .unwrap();
    stop_runtime_for_test(&manager, TEST_ROOM_ID).await;

    (manager, factory, receivers)
}

pub(super) async fn stop_runtime_for_test(manager: &RoomManager, room_id: &str) {
    if let Some(runtime_entry) = manager.get_runtime_entry(room_id).await {
        let mut runtime = runtime_entry.lock().await;
        if let Some(handle) = runtime.tick_handle.take() {
            handle.abort();
        }
        runtime.tick_running = false;
    }
}

pub(super) async fn with_runtime_for_test<R>(
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

pub(super) async fn runtime_exists_for_test(manager: &RoomManager, room_id: &str) -> bool {
    manager.get_runtime_entry(room_id).await.is_some()
}

pub(super) async fn insert_room_for_test(manager: &RoomManager, room_id: &str, room: Room) {
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

pub(super) async fn character_room_index_for_test(
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

pub(super) async fn offline_character_index_for_test(
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

pub(super) async fn with_room_for_test<R>(
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

pub(super) async fn with_room_mut_for_test<R>(
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

pub(super) async fn setup_started_room_with_id(
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
                Some(DEFAULT_POLICY),
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

pub(super) fn drain_messages_of_type(
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

pub(super) fn combat_demo_entity_by_character<'a>(
    game_state: &'a serde_json::Value,
    character_id: &str,
) -> &'a serde_json::Value {
    game_state["snapshot"]["entities"]
        .as_array()
        .expect("combat demo snapshot should contain entities")
        .iter()
        .find(|entity| entity["character_id"].as_str() == Some(character_id))
        .expect("combat demo character entity should exist")
}
