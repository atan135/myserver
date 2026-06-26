use std::collections::HashMap;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, RwLock, mpsc};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::core::logic::{RoomLogicBroadcast, SharedRoomLogicFactory};
use crate::core::room::{
    ConnectionCloseState, MemberRole, OutboundChannel, OutboundMessage, OutboundQueueLogContext,
    PendingInputUpsert, PlayerInputRecord, Room, RoomMemberState, RoomPhase, RoomTransferStatus,
    try_send_outbound,
};
use crate::core::runtime::room_policy::SharedRoomPolicyRegistry;
use crate::match_client::SharedMatchClient;
use crate::metrics::METRICS;
use crate::pb::{
    FrameInput, MovementCorrectionReason, MovementRecoveryState as PbMovementRecoveryState,
    RoomRouteStatus, RoomSnapshot,
};

const MAX_MISSING_INPUT_STREAK_BEFORE_OFFLINE: u32 = 3;
const DEFAULT_ROOM_CLEANUP_INTERVAL_SECS: u64 = 10;
pub const SERVER_REDIRECT_CLOSE_REASON: &str = "server_redirect_reconnect_required";
pub const ROLLOUT_DRAIN_STATUS_ROUTE_SAMPLE_LIMIT: usize = 50;

fn transfer_status_label(status: RoomTransferStatus) -> &'static str {
    match status {
        RoomTransferStatus::Owned => "Owned",
        RoomTransferStatus::Frozen => "Frozen",
        RoomTransferStatus::Exported => "Exported",
        RoomTransferStatus::Importing => "Importing",
        RoomTransferStatus::OwnedByNew => "OwnedByNew",
        RoomTransferStatus::Retired => "Retired",
    }
}

fn detach_member_outbound(member: &mut RoomMemberState) {
    let (placeholder_sender, _placeholder_receiver) = mpsc::channel(1);
    member.sender = placeholder_sender;
    member.close_state = ConnectionCloseState::new();
}

#[derive(Debug)]
pub struct RoomRuntime {
    pub current_fps: u16,
    pub tick_running: bool,
    pub tick_handle: Option<JoinHandle<()>>,
}

impl Default for RoomRuntime {
    fn default() -> Self {
        Self {
            current_fps: 1,
            tick_running: false,
            tick_handle: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RoomLeaveResult {
    pub snapshot: Option<RoomSnapshot>,
    pub room_removed: bool,
}

#[derive(Debug, Clone)]
pub struct RoomRecoveryState {
    pub snapshot: RoomSnapshot,
    pub current_frame_id: u32,
    pub recent_inputs: Vec<FrameInput>,
    pub waiting_frame_id: u32,
    pub waiting_inputs: Vec<FrameInput>,
    pub input_delay_frames: u32,
    pub movement_recovery: Option<PbMovementRecoveryState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerRedirectDelivery {
    pub delivered_count: u64,
    pub failed_count: u64,
    pub online_member_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolloutDrainNotice {
    pub room_id: String,
    pub rollout_epoch: String,
    pub reason: String,
    pub message: String,
    pub retry_after_ms: u32,
    pub deadline_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolloutDrainNoticeDelivery {
    pub delivered_count: u64,
    pub failed_count: u64,
    pub online_member_count: u64,
}

#[derive(Clone)]
pub struct RolloutDrainSnapshot {
    pub rollout_epoch: String,
    pub owner_server_id: String,
    pub owned_room_count: u64,
    pub migrating_room_count: u64,
    pub routes: Vec<RoomRouteStatus>,
    pub transferable_empty_room_count: u64,
    pub transferable_empty_room_samples: Vec<RoomRouteStatus>,
    pub retired_room_count: u64,
}

#[derive(Clone)]
pub struct RoomManager {
    rooms: std::sync::Arc<RwLock<HashMap<String, SharedRoom>>>,
    runtimes: std::sync::Arc<RwLock<HashMap<String, SharedRoomRuntime>>>,
    character_rooms: CharacterRoomIndex,
    offline_characters: CharacterRoomIndex,
    policies: SharedRoomPolicyRegistry,
    logic_factory: SharedRoomLogicFactory,
    match_client: SharedMatchClient,
}

type SharedRoom = std::sync::Arc<Mutex<Room>>;
type SharedRoomRuntime = std::sync::Arc<Mutex<RoomRuntime>>;
type CharacterRoomIndex = std::sync::Arc<RwLock<HashMap<String, String>>>;

fn room_member_index_entries(room: &Room) -> Vec<(String, bool)> {
    if room.marked_for_destruction || room.transfer_state.status == RoomTransferStatus::Retired {
        return Vec::new();
    }

    room.members
        .values()
        .map(|member| (member.character_id.clone(), member.offline))
        .collect()
}

async fn replace_room_member_indexes(
    character_rooms: &CharacterRoomIndex,
    offline_characters: &CharacterRoomIndex,
    room_id: &str,
    members: Vec<(String, bool)>,
) {
    {
        let mut character_rooms = character_rooms.write().await;
        character_rooms.retain(|_, indexed_room_id| indexed_room_id != room_id);
        for (character_id, _offline) in &members {
            character_rooms.insert(character_id.clone(), room_id.to_string());
        }
    }

    {
        let mut offline_characters = offline_characters.write().await;
        offline_characters.retain(|_, indexed_room_id| indexed_room_id != room_id);
        for (character_id, offline) in &members {
            if *offline {
                offline_characters.insert(character_id.clone(), room_id.to_string());
            } else {
                offline_characters.remove(character_id);
            }
        }
    }
}

async fn set_character_room_index(
    character_rooms: &CharacterRoomIndex,
    offline_characters: &CharacterRoomIndex,
    character_id: &str,
    room_id: &str,
    offline: bool,
) {
    {
        let mut character_rooms = character_rooms.write().await;
        character_rooms.insert(character_id.to_string(), room_id.to_string());
    }

    {
        let mut offline_characters = offline_characters.write().await;
        if offline {
            offline_characters.insert(character_id.to_string(), room_id.to_string());
        } else {
            offline_characters.remove(character_id);
        }
    }
}

async fn remove_room_member_indexes(
    character_rooms: &CharacterRoomIndex,
    offline_characters: &CharacterRoomIndex,
    room_id: &str,
) {
    {
        let mut character_rooms = character_rooms.write().await;
        character_rooms.retain(|_, indexed_room_id| indexed_room_id != room_id);
    }

    {
        let mut offline_characters = offline_characters.write().await;
        offline_characters.retain(|_, indexed_room_id| indexed_room_id != room_id);
    }
}

async fn remove_character_index_for_room(
    character_rooms: &CharacterRoomIndex,
    offline_characters: &CharacterRoomIndex,
    character_id: &str,
    room_id: &str,
) {
    {
        let mut character_rooms = character_rooms.write().await;
        if character_rooms.get(character_id).map(String::as_str) == Some(room_id) {
            character_rooms.remove(character_id);
        }
    }

    {
        let mut offline_characters = offline_characters.write().await;
        if offline_characters.get(character_id).map(String::as_str) == Some(room_id) {
            offline_characters.remove(character_id);
        }
    }
}

async fn remove_offline_character_index_for_room(
    offline_characters: &CharacterRoomIndex,
    character_id: &str,
    room_id: &str,
) {
    let mut offline_characters = offline_characters.write().await;
    if offline_characters.get(character_id).map(String::as_str) == Some(room_id) {
        offline_characters.remove(character_id);
    }
}

async fn sync_room_member_indexes_from_entry(
    character_rooms: &CharacterRoomIndex,
    offline_characters: &CharacterRoomIndex,
    room_id: &str,
    room_entry: &SharedRoom,
) {
    let members = {
        let room = room_entry.lock().await;
        room_member_index_entries(&room)
    };
    replace_room_member_indexes(character_rooms, offline_characters, room_id, members).await;
}

fn room_rejects_mutation(room: &Room) -> bool {
    room.marked_for_destruction || room.transfer_state.status.rejects_room_mutation()
}

fn room_mutation_error_code(room: &Room) -> &'static str {
    if room.marked_for_destruction {
        "ROOM_NOT_FOUND"
    } else {
        room.transfer_state.mutation_error_code()
    }
}

fn room_rollout_route_status(room: &Room, owner_server_id: &str) -> RoomRouteStatus {
    RoomRouteStatus {
        room_id: room.room_id.clone(),
        owner_server_id: owner_server_id.to_string(),
        migration_state: room.transfer_state.status.migration_state() as i32,
        member_count: room.members.len() as u32,
        online_member_count: room
            .members
            .values()
            .filter(|member| !member.offline)
            .count() as u32,
        empty_since_ms: room
            .empty_since
            .map(|empty_since| empty_since.elapsed().as_millis() as u64)
            .unwrap_or_default(),
        room_version: room.transfer_state.room_version,
    }
}

fn log_room_entered_transferable_empty_candidate(
    room: &Room,
    trigger_character_id: &str,
    trigger_action: &'static str,
) {
    let online_member_count = room
        .members
        .values()
        .filter(|member| !member.offline)
        .count();

    info!(
        room_id = %room.room_id,
        rollout_epoch = %room.transfer_state.rollout_epoch.as_deref().unwrap_or_default(),
        migration_state = ?room.transfer_state.status.migration_state(),
        current_status = transfer_status_label(room.transfer_state.status),
        member_count = room.members.len(),
        online_member_count = online_member_count,
        empty_since_ms = room
            .empty_since
            .map(|empty_since| empty_since.elapsed().as_millis() as u64)
            .unwrap_or_default(),
        room_version = room.transfer_state.room_version,
        trigger_character_id = %trigger_character_id,
        trigger_action = trigger_action,
        "room entered empty transferable candidate state"
    );
}

mod broadcast;
mod lifecycle;
mod match_notify;
mod rollout;
mod storage;
mod tick;
mod transfer;
mod transfer_codec;

#[cfg(test)]
mod tests;

impl RoomManager {
    pub fn new(logic_factory: SharedRoomLogicFactory) -> Self {
        Self::with_match_client_and_cleanup_interval(
            crate::match_client::create_match_client_shared(),
            logic_factory,
            DEFAULT_ROOM_CLEANUP_INTERVAL_SECS,
        )
    }

    pub fn with_match_client(
        match_client: SharedMatchClient,
        logic_factory: SharedRoomLogicFactory,
    ) -> Self {
        Self::with_match_client_and_cleanup_interval(
            match_client,
            logic_factory,
            DEFAULT_ROOM_CLEANUP_INTERVAL_SECS,
        )
    }

    pub fn with_match_client_and_cleanup_interval(
        match_client: SharedMatchClient,
        logic_factory: SharedRoomLogicFactory,
        cleanup_interval_secs: u64,
    ) -> Self {
        Self::with_policy_registry_and_cleanup_interval(
            match_client,
            logic_factory,
            SharedRoomPolicyRegistry::default(),
            cleanup_interval_secs,
        )
    }

    pub fn with_policy_registry_and_cleanup_interval(
        match_client: SharedMatchClient,
        logic_factory: SharedRoomLogicFactory,
        policies: SharedRoomPolicyRegistry,
        cleanup_interval_secs: u64,
    ) -> Self {
        let this = Self {
            rooms: std::sync::Arc::new(RwLock::new(HashMap::new())),
            runtimes: std::sync::Arc::new(RwLock::new(HashMap::new())),
            character_rooms: std::sync::Arc::new(RwLock::new(HashMap::new())),
            offline_characters: std::sync::Arc::new(RwLock::new(HashMap::new())),
            policies,
            logic_factory,
            match_client,
        };
        this.spawn_cleanup_task(cleanup_interval_secs);
        this
    }
}
