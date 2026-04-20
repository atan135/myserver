use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::RwLock;
use tracing::{info, warn};

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum UpstreamOperationState {
    Active,
    Draining,
    Disabled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum UpstreamHealthState {
    Healthy,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum UpstreamState {
    Active,
    Draining,
    Disabled,
    Unavailable,
}

#[derive(Clone, Debug, Serialize)]
pub struct UpstreamRoute {
    pub server_id: String,
    pub local_socket_name: String,
    pub operation_state: UpstreamOperationState,
    pub health_state: UpstreamHealthState,
}

impl UpstreamRoute {
    pub fn effective_state(&self) -> UpstreamState {
        match (self.health_state, self.operation_state) {
            (UpstreamHealthState::Unavailable, _) => UpstreamState::Unavailable,
            (UpstreamHealthState::Healthy, UpstreamOperationState::Active) => UpstreamState::Active,
            (UpstreamHealthState::Healthy, UpstreamOperationState::Draining) => {
                UpstreamState::Draining
            }
            (UpstreamHealthState::Healthy, UpstreamOperationState::Disabled) => {
                UpstreamState::Disabled
            }
        }
    }

    pub fn can_accept_bound_sessions(&self) -> bool {
        matches!(
            self.effective_state(),
            UpstreamState::Active | UpstreamState::Draining
        )
    }

    pub fn accepts_new_rooms(&self) -> bool {
        self.effective_state() == UpstreamState::Active
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum RolloutSessionState {
    Active,
    Ending,
    Interrupted,
}

#[derive(Clone, Debug, Serialize)]
pub struct RolloutSession {
    pub rollout_epoch: String,
    pub old_server_id: String,
    pub new_server_id: String,
    pub state: RolloutSessionState,
    pub started_at_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum RoomMigrationState {
    OwnedByOld,
    DrainingOnOld,
    FrozenForTransfer,
    ImportingToNew,
    OwnedByNew,
    TransferFailed,
    RetiredOnOld,
}

impl RoomMigrationState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OwnedByOld => "OwnedByOld",
            Self::DrainingOnOld => "DrainingOnOld",
            Self::FrozenForTransfer => "FrozenForTransfer",
            Self::ImportingToNew => "ImportingToNew",
            Self::OwnedByNew => "OwnedByNew",
            Self::TransferFailed => "TransferFailed",
            Self::RetiredOnOld => "RetiredOnOld",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "OwnedByOld" => Some(Self::OwnedByOld),
            "DrainingOnOld" => Some(Self::DrainingOnOld),
            "FrozenForTransfer" => Some(Self::FrozenForTransfer),
            "ImportingToNew" => Some(Self::ImportingToNew),
            "OwnedByNew" => Some(Self::OwnedByNew),
            "TransferFailed" => Some(Self::TransferFailed),
            "RetiredOnOld" => Some(Self::RetiredOnOld),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct RoomRouteRecord {
    pub room_id: String,
    pub owner_server_id: String,
    pub migration_state: RoomMigrationState,
    pub member_count: u32,
    pub online_member_count: u32,
    pub empty_since_ms: Option<u64>,
    pub room_version: u64,
    pub rollout_epoch: String,
    pub last_transfer_checksum: String,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct PlayerRouteRecord {
    pub player_id: String,
    pub current_room_id: Option<String>,
    pub preferred_server_id: Option<String>,
    pub rollout_epoch: String,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug)]
pub struct RouteCounts {
    pub room_routes: usize,
    pub player_routes: usize,
}

#[derive(Default)]
struct RouteStoreState {
    routes: HashMap<String, UpstreamRoute>,
    rollout_session: Option<RolloutSession>,
    room_routes: HashMap<String, RoomRouteRecord>,
    player_routes: HashMap<String, PlayerRouteRecord>,
}

#[derive(Clone, Default)]
pub struct ProxyRouteStore {
    state: Arc<RwLock<RouteStoreState>>,
}

impl ProxyRouteStore {
    pub async fn set_static_routes(&self, routes: Vec<UpstreamRoute>) {
        let mut state = self.state.write().await;
        state.routes.clear();
        for route in routes {
            state.routes.insert(route.server_id.clone(), route);
        }
    }

    pub async fn sync_discovered_routes(&self, routes: Vec<UpstreamRoute>) {
        let mut state = self.state.write().await;
        for route in state.routes.values_mut() {
            route.health_state = UpstreamHealthState::Unavailable;
        }

        for route in routes {
            match state.routes.get_mut(&route.server_id) {
                Some(existing) => {
                    existing.local_socket_name = route.local_socket_name;
                    existing.health_state = route.health_state;
                }
                None => {
                    state.routes.insert(route.server_id.clone(), route);
                }
            }
        }
    }

    pub async fn list_routes(&self) -> Vec<UpstreamRoute> {
        let state = self.state.read().await;
        let mut routes: Vec<_> = state.routes.values().cloned().collect();
        routes.sort_by(|left, right| left.server_id.cmp(&right.server_id));
        routes
    }

    pub async fn update_operation_state(
        &self,
        server_id: &str,
        operation_state: UpstreamOperationState,
    ) -> bool {
        let mut state = self.state.write().await;
        let Some(route) = state.routes.get_mut(server_id) else {
            return false;
        };
        route.operation_state = operation_state;
        true
    }

    pub async fn active_upstream_server_id(&self) -> Option<String> {
        self.select_default_upstream()
            .await
            .map(|route| route.server_id)
    }

    pub async fn get_rollout_session(&self) -> Option<RolloutSession> {
        self.state.read().await.rollout_session.clone()
    }

    pub async fn begin_rollout(
        &self,
        rollout_epoch: String,
        old_server_id: String,
        new_server_id: String,
    ) {
        let mut state = self.state.write().await;
        state.rollout_session = Some(RolloutSession {
            rollout_epoch,
            old_server_id,
            new_server_id,
            state: RolloutSessionState::Active,
            started_at_ms: now_ms(),
        });
    }

    pub async fn end_rollout(&self) {
        let mut state = self.state.write().await;
        let ended_session = state.rollout_session.take();
        let removed_player_route_count = state.player_routes.len();
        let room_route_count_before = state.room_routes.len();
        state.player_routes.clear();
        if let Some(ended_session) = ended_session {
            state
                .room_routes
                .retain(|_, record| record.rollout_epoch != ended_session.rollout_epoch);
            let removed_room_route_count =
                room_route_count_before.saturating_sub(state.room_routes.len());
            info!(
                rollout_epoch = %ended_session.rollout_epoch,
                old_server_id = %ended_session.old_server_id,
                new_server_id = %ended_session.new_server_id,
                rollout_state = ?ended_session.state,
                removed_room_route_count,
                removed_player_route_count,
                remaining_room_route_count = state.room_routes.len(),
                "proxy rollout ended"
            );
        }
    }

    pub async fn mark_rollout_state(&self, rollout_state: RolloutSessionState) {
        let mut state = self.state.write().await;
        if let Some(session) = state.rollout_session.as_mut() {
            session.state = rollout_state;
        }
    }

    pub async fn list_room_routes(&self) -> Vec<RoomRouteRecord> {
        let state = self.state.read().await;
        let mut routes: Vec<_> = state.room_routes.values().cloned().collect();
        routes.sort_by(|left, right| left.room_id.cmp(&right.room_id));
        routes
    }

    pub async fn list_player_routes(&self) -> Vec<PlayerRouteRecord> {
        let state = self.state.read().await;
        let mut routes: Vec<_> = state.player_routes.values().cloned().collect();
        routes.sort_by(|left, right| left.player_id.cmp(&right.player_id));
        routes
    }

    pub async fn route_counts(&self) -> RouteCounts {
        let state = self.state.read().await;
        RouteCounts {
            room_routes: state.room_routes.len(),
            player_routes: state.player_routes.len(),
        }
    }

    pub async fn upsert_room_route(
        &self,
        mut record: RoomRouteRecord,
        expected_room_version: Option<u64>,
        expected_last_transfer_checksum: Option<String>,
    ) -> Result<(), &'static str> {
        let mut state = self.state.write().await;
        validate_rollout_epoch(&state.rollout_session, &record.rollout_epoch)?;
        let existing = state.room_routes.get(&record.room_id).cloned();

        match existing.as_ref() {
            Some(existing) if room_route_records_match(existing, &record) => {
                return Ok(());
            }
            Some(existing) if record.room_version < existing.room_version => {
                return Err("STALE_ROOM_ROUTE_UPDATE");
            }
            Some(existing) if record.room_version == existing.room_version => {
                return Err("ROOM_ROUTE_CONFLICT");
            }
            Some(existing) => {
                if let Some(expected_room_version) = expected_room_version {
                    if expected_room_version != existing.room_version {
                        return Err("ROOM_ROUTE_VERSION_MISMATCH");
                    }
                }

                if let Some(expected_last_transfer_checksum) =
                    expected_last_transfer_checksum.as_deref()
                {
                    if existing.last_transfer_checksum != expected_last_transfer_checksum {
                        return Err("ROOM_ROUTE_CHECKSUM_MISMATCH");
                    }
                }

                if record.room_version != existing.room_version.saturating_add(1) {
                    return Err("ROOM_ROUTE_VERSION_GAP");
                }

                validate_transition_checksum(existing, &record)?;
            }
            None => {
                validate_room_route_create(
                    &record,
                    expected_room_version,
                    expected_last_transfer_checksum.as_deref(),
                )?;
            }
        }

        record.updated_at_ms = now_ms();
        state.room_routes.insert(record.room_id.clone(), record.clone());
        log_room_route_update("admin_upsert", existing.as_ref(), &record);
        Ok(())
    }

    pub async fn upsert_player_route(
        &self,
        mut record: PlayerRouteRecord,
    ) -> Result<(), &'static str> {
        let mut state = self.state.write().await;
        validate_rollout_epoch(&state.rollout_session, &record.rollout_epoch)?;
        let existing = state.player_routes.get(&record.player_id).cloned();
        record.updated_at_ms = now_ms();
        state
            .player_routes
            .insert(record.player_id.clone(), record.clone());
        log_player_route_update("admin_upsert", existing.as_ref(), &record);
        Ok(())
    }

    pub async fn bind_room_owner(
        &self,
        room_id: &str,
        owner_server_id: &str,
        player_id: Option<&str>,
        observer_only: bool,
    ) {
        let mut state = self.state.write().await;
        let current_rollout_epoch = state
            .rollout_session
            .as_ref()
            .map(|session| session.rollout_epoch.clone())
            .unwrap_or_default();
        let existing_room_route = state.room_routes.get(room_id).cloned();
        let initial_member_count = if observer_only { 0 } else { 1 };
        let rollout_active = state.rollout_session.is_some();
        let (bound_owner_server_id, migration_state, member_count, online_member_count, empty_since_ms, room_version, checksum, rollout_epoch) =
            match existing_room_route.as_ref() {
                Some(record) if record.owner_server_id == owner_server_id => (
                    owner_server_id.to_string(),
                    record.migration_state,
                    record.member_count.max(initial_member_count),
                    record.online_member_count.max(initial_member_count),
                    if record.online_member_count.max(initial_member_count) == 0 {
                        record.empty_since_ms
                    } else {
                        None
                    },
                    record.room_version,
                    record.last_transfer_checksum.clone(),
                    preserve_observed_rollout_epoch(record, &current_rollout_epoch),
                ),
                Some(record) if rollout_active => {
                    warn!(
                        room_id = room_id,
                        existing_owner_server_id = %record.owner_server_id,
                        observed_owner_server_id = %owner_server_id,
                        "ignored observed room owner mismatch during rollout"
                    );
                    (
                        record.owner_server_id.clone(),
                        record.migration_state,
                        record.member_count.max(initial_member_count),
                        record.online_member_count.max(initial_member_count),
                        if record.online_member_count.max(initial_member_count) == 0 {
                            record.empty_since_ms
                        } else {
                            None
                        },
                        record.room_version,
                        record.last_transfer_checksum.clone(),
                        preserve_observed_rollout_epoch(record, &current_rollout_epoch),
                    )
                }
                Some(record) => (
                    owner_server_id.to_string(),
                    infer_migration_state(owner_server_id, state.rollout_session.as_ref()),
                    record.member_count.max(initial_member_count),
                    record.online_member_count.max(initial_member_count),
                    None,
                    record.room_version.saturating_add(1),
                    record.last_transfer_checksum.clone(),
                    current_rollout_epoch.clone(),
                ),
                None => {
                    let migration_state =
                        infer_migration_state(owner_server_id, state.rollout_session.as_ref());
                    let online_member_count = initial_member_count;
                    (
                        owner_server_id.to_string(),
                        migration_state,
                        initial_member_count,
                        online_member_count,
                        None,
                        1,
                        String::new(),
                        current_rollout_epoch.clone(),
                    )
                }
            };

        let next_room_route = RoomRouteRecord {
            room_id: room_id.to_string(),
            owner_server_id: bound_owner_server_id.clone(),
            migration_state,
            member_count,
            online_member_count,
            empty_since_ms,
            room_version,
            rollout_epoch: rollout_epoch.clone(),
            last_transfer_checksum: checksum,
            updated_at_ms: now_ms(),
        };
        state.room_routes.insert(
            room_id.to_string(),
            next_room_route.clone(),
        );
        log_room_route_update("bind_room_owner", existing_room_route.as_ref(), &next_room_route);

        if let Some(player_id) = player_id {
            let existing_player_route = state.player_routes.get(player_id).cloned();
            let next_player_route = PlayerRouteRecord {
                player_id: player_id.to_string(),
                current_room_id: Some(room_id.to_string()),
                preferred_server_id: Some(bound_owner_server_id),
                rollout_epoch,
                updated_at_ms: now_ms(),
            };
            state.player_routes.insert(
                player_id.to_string(),
                next_player_route.clone(),
            );
            log_player_route_update(
                "bind_room_owner",
                existing_player_route.as_ref(),
                &next_player_route,
            );
        }
    }

    pub async fn select_upstream_for_room(&self, room_id: &str) -> Option<UpstreamRoute> {
        let state = self.state.read().await;
        if let Some(record) = state.room_routes.get(room_id) {
            if let Some(route) = state.find_connectable_route(&record.owner_server_id) {
                return Some(route.clone());
            }
        }

        state.select_default_for_new_room().cloned()
    }

    pub async fn select_upstream_for_player(&self, player_id: &str) -> Option<UpstreamRoute> {
        let state = self.state.read().await;
        if let Some(record) = state.player_routes.get(player_id) {
            if let Some(server_id) = record.preferred_server_id.as_deref() {
                if let Some(route) = state.find_connectable_route(server_id) {
                    return Some(route.clone());
                }
            }
            if let Some(room_id) = record.current_room_id.as_deref() {
                if let Some(room_route) = state.room_routes.get(room_id) {
                    if let Some(route) = state.find_connectable_route(&room_route.owner_server_id) {
                        return Some(route.clone());
                    }
                }
            }
        }

        state.select_default_for_new_room().cloned()
    }

    pub async fn select_default_upstream(&self) -> Option<UpstreamRoute> {
        self.state
            .read()
            .await
            .select_default_for_new_room()
            .cloned()
    }
}

impl RouteStoreState {
    fn find_connectable_route(&self, server_id: &str) -> Option<&UpstreamRoute> {
        self.routes
            .get(server_id)
            .filter(|route| route.can_accept_bound_sessions())
    }

    fn select_default_for_new_room(&self) -> Option<&UpstreamRoute> {
        if let Some(session) = &self.rollout_session {
            return self
                .routes
                .get(&session.new_server_id)
                .filter(|route| route.accepts_new_rooms());
        }

        let mut routes: Vec<_> = self.routes.values().collect();
        routes.sort_by(|left, right| left.server_id.cmp(&right.server_id));

        routes
            .iter()
            .copied()
            .find(|route| route.accepts_new_rooms())
            .or_else(|| routes.iter().copied().find(|route| route.can_accept_bound_sessions()))
    }
}

fn validate_rollout_epoch(
    session: &Option<RolloutSession>,
    record_rollout_epoch: &str,
) -> Result<(), &'static str> {
    let Some(session) = session else {
        return Ok(());
    };
    if record_rollout_epoch.is_empty() || record_rollout_epoch == session.rollout_epoch {
        return Ok(());
    }
    Err("ROLLOUT_EPOCH_MISMATCH")
}

fn infer_migration_state(
    owner_server_id: &str,
    session: Option<&RolloutSession>,
) -> RoomMigrationState {
    let Some(session) = session else {
        return RoomMigrationState::OwnedByNew;
    };

    if owner_server_id == session.old_server_id {
        RoomMigrationState::OwnedByOld
    } else if owner_server_id == session.new_server_id {
        RoomMigrationState::OwnedByNew
    } else {
        RoomMigrationState::OwnedByNew
    }
}

fn preserve_observed_rollout_epoch(existing: &RoomRouteRecord, current_rollout_epoch: &str) -> String {
    if current_rollout_epoch.is_empty() {
        existing.rollout_epoch.clone()
    } else {
        current_rollout_epoch.to_string()
    }
}

fn room_route_records_match(left: &RoomRouteRecord, right: &RoomRouteRecord) -> bool {
    left.room_id == right.room_id
        && left.owner_server_id == right.owner_server_id
        && left.migration_state == right.migration_state
        && left.member_count == right.member_count
        && left.online_member_count == right.online_member_count
        && left.empty_since_ms == right.empty_since_ms
        && left.room_version == right.room_version
        && left.rollout_epoch == right.rollout_epoch
        && left.last_transfer_checksum == right.last_transfer_checksum
}

fn player_route_records_match(left: &PlayerRouteRecord, right: &PlayerRouteRecord) -> bool {
    left.player_id == right.player_id
        && left.current_room_id == right.current_room_id
        && left.preferred_server_id == right.preferred_server_id
        && left.rollout_epoch == right.rollout_epoch
}

fn log_room_route_update(
    update_source: &'static str,
    previous: Option<&RoomRouteRecord>,
    current: &RoomRouteRecord,
) {
    if previous.is_some_and(|previous| room_route_records_match(previous, current)) {
        return;
    }

    info!(
        update_source,
        action = if previous.is_some() { "updated" } else { "created" },
        room_id = %current.room_id,
        owner_server_id = %current.owner_server_id,
        migration_state = current.migration_state.as_str(),
        member_count = current.member_count,
        online_member_count = current.online_member_count,
        empty_since_ms = ?current.empty_since_ms,
        room_version = current.room_version,
        rollout_epoch = %current.rollout_epoch,
        last_transfer_checksum = %current.last_transfer_checksum,
        previous_owner_server_id = %previous.map(|record| record.owner_server_id.as_str()).unwrap_or_default(),
        previous_migration_state = previous.map(|record| record.migration_state.as_str()).unwrap_or_default(),
        previous_member_count = previous.map(|record| record.member_count).unwrap_or(0),
        previous_online_member_count = previous.map(|record| record.online_member_count).unwrap_or(0),
        previous_empty_since_ms = ?previous.and_then(|record| record.empty_since_ms),
        previous_room_version = previous.map(|record| record.room_version).unwrap_or(0),
        previous_rollout_epoch = %previous.map(|record| record.rollout_epoch.as_str()).unwrap_or_default(),
        previous_last_transfer_checksum = %previous.map(|record| record.last_transfer_checksum.as_str()).unwrap_or_default(),
        "room route updated"
    );
}

fn log_player_route_update(
    update_source: &'static str,
    previous: Option<&PlayerRouteRecord>,
    current: &PlayerRouteRecord,
) {
    if previous.is_some_and(|previous| player_route_records_match(previous, current)) {
        return;
    }

    info!(
        update_source,
        action = if previous.is_some() { "updated" } else { "created" },
        player_id = %current.player_id,
        current_room_id = %current.current_room_id.as_deref().unwrap_or_default(),
        preferred_server_id = %current.preferred_server_id.as_deref().unwrap_or_default(),
        rollout_epoch = %current.rollout_epoch,
        previous_current_room_id = %previous
            .and_then(|record| record.current_room_id.as_deref())
            .unwrap_or_default(),
        previous_preferred_server_id = %previous
            .and_then(|record| record.preferred_server_id.as_deref())
            .unwrap_or_default(),
        previous_rollout_epoch = %previous.map(|record| record.rollout_epoch.as_str()).unwrap_or_default(),
        "player route updated"
    );
}

fn validate_room_route_create(
    record: &RoomRouteRecord,
    expected_room_version: Option<u64>,
    expected_last_transfer_checksum: Option<&str>,
) -> Result<(), &'static str> {
    if let Some(expected_room_version) = expected_room_version {
        if expected_room_version != 0 {
            return Err("ROOM_ROUTE_VERSION_MISMATCH");
        }
    }

    if let Some(expected_last_transfer_checksum) = expected_last_transfer_checksum {
        if !expected_last_transfer_checksum.is_empty() {
            return Err("ROOM_ROUTE_CHECKSUM_MISMATCH");
        }
    }

    if record.room_version != 1 {
        return Err("INVALID_ROOM_ROUTE_VERSION");
    }

    if transition_requires_checksum(record.migration_state) && record.last_transfer_checksum.is_empty()
    {
        return Err("MISSING_TRANSFER_CHECKSUM");
    }

    Ok(())
}

fn validate_transition_checksum(
    existing: &RoomRouteRecord,
    incoming: &RoomRouteRecord,
) -> Result<(), &'static str> {
    if transition_requires_checksum(incoming.migration_state) && incoming.last_transfer_checksum.is_empty()
    {
        return Err("MISSING_TRANSFER_CHECKSUM");
    }

    if !existing.last_transfer_checksum.is_empty() && incoming.last_transfer_checksum.is_empty() {
        return Err("MISSING_TRANSFER_CHECKSUM");
    }

    Ok(())
}

fn transition_requires_checksum(migration_state: RoomMigrationState) -> bool {
    matches!(
        migration_state,
        RoomMigrationState::ImportingToNew
            | RoomMigrationState::OwnedByNew
            | RoomMigrationState::TransferFailed
            | RoomMigrationState::RetiredOnOld
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room_record(
        room_id: &str,
        owner_server_id: &str,
        migration_state: RoomMigrationState,
        room_version: u64,
        checksum: &str,
    ) -> RoomRouteRecord {
        RoomRouteRecord {
            room_id: room_id.to_string(),
            owner_server_id: owner_server_id.to_string(),
            migration_state,
            member_count: 0,
            online_member_count: 0,
            empty_since_ms: Some(123),
            room_version,
            rollout_epoch: "rollout-1".to_string(),
            last_transfer_checksum: checksum.to_string(),
            updated_at_ms: 0,
        }
    }

    async fn get_room_route(store: &ProxyRouteStore, room_id: &str) -> RoomRouteRecord {
        store
            .list_room_routes()
            .await
            .into_iter()
            .find(|record| record.room_id == room_id)
            .expect("room route should exist")
    }

    #[tokio::test]
    async fn room_route_replay_is_idempotent() {
        let store = ProxyRouteStore::default();
        store
            .upsert_room_route(
                room_record("room-1", "old", RoomMigrationState::OwnedByOld, 1, ""),
                Some(0),
                None,
            )
            .await
            .unwrap();

        store
            .upsert_room_route(
                room_record("room-1", "old", RoomMigrationState::OwnedByOld, 1, ""),
                Some(0),
                None,
            )
            .await
            .unwrap();

        let route = get_room_route(&store, "room-1").await;
        assert_eq!(route.room_version, 1);
    }

    #[tokio::test]
    async fn room_route_rejects_stale_version() {
        let store = ProxyRouteStore::default();
        store
            .upsert_room_route(
                room_record("room-1", "old", RoomMigrationState::OwnedByOld, 1, ""),
                Some(0),
                None,
            )
            .await
            .unwrap();

        let result = store
            .upsert_room_route(
                room_record("room-1", "old", RoomMigrationState::OwnedByOld, 0, ""),
                None,
                None,
            )
            .await;

        assert_eq!(result, Err("STALE_ROOM_ROUTE_UPDATE"));
    }

    #[tokio::test]
    async fn room_route_rejects_same_version_conflict() {
        let store = ProxyRouteStore::default();
        store
            .upsert_room_route(
                room_record("room-1", "old", RoomMigrationState::OwnedByOld, 1, ""),
                Some(0),
                None,
            )
            .await
            .unwrap();

        let mut conflicting = room_record("room-1", "old", RoomMigrationState::OwnedByOld, 1, "");
        conflicting.member_count = 2;

        let result = store
            .upsert_room_route(conflicting, None, None)
            .await;

        assert_eq!(result, Err("ROOM_ROUTE_CONFLICT"));
    }

    #[tokio::test]
    async fn room_route_rejects_version_gap() {
        let store = ProxyRouteStore::default();
        store
            .upsert_room_route(
                room_record("room-1", "old", RoomMigrationState::OwnedByOld, 1, ""),
                Some(0),
                None,
            )
            .await
            .unwrap();

        let result = store
            .upsert_room_route(
                room_record(
                    "room-1",
                    "old",
                    RoomMigrationState::FrozenForTransfer,
                    3,
                    "checksum-1",
                ),
                Some(1),
                None,
            )
            .await;

        assert_eq!(result, Err("ROOM_ROUTE_VERSION_GAP"));
    }

    #[tokio::test]
    async fn room_route_rejects_checksum_mismatch() {
        let store = ProxyRouteStore::default();
        store
            .upsert_room_route(
                room_record(
                    "room-1",
                    "old",
                    RoomMigrationState::FrozenForTransfer,
                    1,
                    "checksum-1",
                ),
                Some(0),
                None,
            )
            .await
            .unwrap();

        let result = store
            .upsert_room_route(
                room_record(
                    "room-1",
                    "new",
                    RoomMigrationState::OwnedByNew,
                    2,
                    "checksum-2",
                ),
                Some(1),
                Some("checksum-legacy".to_string()),
            )
            .await;

        assert_eq!(result, Err("ROOM_ROUTE_CHECKSUM_MISMATCH"));
    }

    #[tokio::test]
    async fn bind_room_owner_preserves_version_for_same_owner() {
        let store = ProxyRouteStore::default();
        store
            .upsert_room_route(
                room_record(
                    "room-1",
                    "server-a",
                    RoomMigrationState::FrozenForTransfer,
                    1,
                    "checksum-1",
                ),
                Some(0),
                None,
            )
            .await
            .unwrap();

        store
            .bind_room_owner("room-1", "server-a", Some("player-1"), false)
            .await;

        let route = get_room_route(&store, "room-1").await;
        assert_eq!(route.owner_server_id, "server-a");
        assert_eq!(route.room_version, 1);
        assert_eq!(route.last_transfer_checksum, "checksum-1");
        assert_eq!(route.member_count, 1);
        assert_eq!(route.online_member_count, 1);
    }

    #[tokio::test]
    async fn bind_room_owner_does_not_override_owner_during_rollout() {
        let store = ProxyRouteStore::default();
        store
            .begin_rollout(
                "rollout-1".to_string(),
                "old".to_string(),
                "new".to_string(),
            )
            .await;
        store
            .upsert_room_route(
                room_record("room-1", "old", RoomMigrationState::OwnedByOld, 1, ""),
                Some(0),
                None,
            )
            .await
            .unwrap();

        store
            .bind_room_owner("room-1", "new", Some("player-1"), false)
            .await;

        let route = get_room_route(&store, "room-1").await;
        assert_eq!(route.owner_server_id, "old");
        assert_eq!(route.room_version, 1);

        let player_route = store
            .list_player_routes()
            .await
            .into_iter()
            .find(|record| record.player_id == "player-1")
            .expect("player route should exist");
        assert_eq!(player_route.preferred_server_id.as_deref(), Some("old"));
    }
}
