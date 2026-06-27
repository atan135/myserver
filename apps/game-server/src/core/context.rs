use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use redis::aio::MultiplexedConnection;
use tokio::sync::{Mutex, Notify, RwLock};

use crate::config::Config;
use crate::core::character_discipline::DisciplineService;
use crate::core::character_element::CharacterElementService;
use crate::core::character_progress::CharacterProgressService;
use crate::core::character_push::CharacterPushService;
use crate::core::character_title::TitleService;
use crate::core::character_title_unlock::TitleUnlockService;
use crate::core::config_table::ConfigTableRuntime;
use crate::core::global_id::ItemUidGenerator;
use crate::core::player::PlayerManager;
use crate::core::room::{
    ConnectionCloseState, OutboundChannel, OutboundMessage, OutboundQueueLogContext,
    OutboundSender, try_send_outbound,
};
use crate::core::runtime::RoomManager;
use crate::db_store::PgAuditStore;
use crate::protocol::{MessageType, encode_body};
use crate::server::{PlayerInputAnomalyTracker, PlayerMessageRateLimiter, RuntimeConfig};
use crate::session::{AuthenticatedSessionIdentity, Session, SessionState};

pub type SharedRoomManager = Arc<RoomManager>;
pub type SharedRuntimeConfig = Arc<RwLock<RuntimeConfig>>;
pub type ShutdownSignal = Arc<Notify>;
pub type PlayerRegistry = Arc<RwLock<OnlinePlayerRegistry>>;
pub type SharedPlayerMessageRateLimiter = Arc<Mutex<PlayerMessageRateLimiter>>;
pub type SharedPlayerInputAnomalyTracker = Arc<Mutex<PlayerInputAnomalyTracker>>;

#[derive(Clone)]
pub struct PlayerConnectionHandle {
    /// Account-level player id. P0 keeps one live connection per account, even
    /// when the same account switches between different characters.
    pub account_player_id: String,
    pub character_id: String,
    pub kick_notify: Arc<Notify>,
    pub session_id: u64,
    pub outbound: OutboundChannel,
    pub kick_reason: Arc<RwLock<String>>,
}

/// Online connection index for this game-server instance.
///
/// The primary index is account_player_id so concurrent login and GM/session
/// kick remain account-scoped in P0. The character index is maintained as a
/// lookup aid for later character-scoped systems; it is not used to allow
/// simultaneous online characters from the same account.
#[derive(Default)]
pub struct OnlinePlayerRegistry {
    by_account_player_id: HashMap<String, PlayerConnectionHandle>,
    character_to_account_player_id: HashMap<String, String>,
}

impl OnlinePlayerRegistry {
    pub fn insert_by_account(
        &mut self,
        handle: PlayerConnectionHandle,
    ) -> Option<PlayerConnectionHandle> {
        let account_player_id = handle.account_player_id.clone();
        let character_id = handle.character_id.clone();
        let old_handle = self
            .by_account_player_id
            .insert(account_player_id.clone(), handle);

        if let Some(old_handle) = old_handle.as_ref() {
            self.remove_character_index_if_current(
                &old_handle.character_id,
                &old_handle.account_player_id,
            );
        }

        self.character_to_account_player_id
            .insert(character_id.clone(), account_player_id);
        debug_assert!(self.get_by_character(&character_id).is_some());
        old_handle
    }

    pub fn get_by_account(&self, account_player_id: &str) -> Option<&PlayerConnectionHandle> {
        self.by_account_player_id.get(account_player_id)
    }

    pub fn get_by_character(&self, character_id: &str) -> Option<&PlayerConnectionHandle> {
        let account_player_id = self.character_to_account_player_id.get(character_id)?;
        self.by_account_player_id.get(account_player_id)
    }

    pub fn remove_by_account_if_session(
        &mut self,
        account_player_id: &str,
        session_id: u64,
    ) -> Option<PlayerConnectionHandle> {
        let handle = self.by_account_player_id.get(account_player_id)?;
        if handle.session_id != session_id {
            return None;
        }

        let removed = self.by_account_player_id.remove(account_player_id)?;
        self.remove_character_index_if_current(&removed.character_id, account_player_id);
        Some(removed)
    }

    pub fn online_connections(&self) -> Vec<PlayerConnectionHandle> {
        self.by_account_player_id.values().cloned().collect()
    }

    fn remove_character_index_if_current(&mut self, character_id: &str, account_player_id: &str) {
        if self
            .character_to_account_player_id
            .get(character_id)
            .map(String::as_str)
            == Some(account_player_id)
        {
            self.character_to_account_player_id.remove(character_id);
        }
    }
}

#[derive(Clone)]
pub struct ServiceContext {
    pub config: Config,
    pub db_store: PgAuditStore,
    pub room_manager: SharedRoomManager,
    pub runtime_config: SharedRuntimeConfig,
    pub connection_count: Arc<AtomicU64>,
    pub config_tables: ConfigTableRuntime,
    pub item_uid_generator: ItemUidGenerator,
    pub player_manager: PlayerManager,
    pub character_element_service: CharacterElementService,
    pub discipline_service: DisciplineService,
    pub title_service: TitleService,
    pub character_progress_service: CharacterProgressService,
    pub title_unlock_service: TitleUnlockService,
    pub character_push_service: CharacterPushService,
    pub online_player_count: Arc<AtomicU64>,
    pub player_registry: PlayerRegistry,
    pub player_msg_rate_limiter: SharedPlayerMessageRateLimiter,
    pub player_input_anomaly_tracker: SharedPlayerInputAnomalyTracker,
    pub shutdown_signal: ShutdownSignal,
}

pub struct ConnectionContext {
    pub peer_addr: String,
    pub redis: MultiplexedConnection,
    pub session: Session,
    pub tx: OutboundSender,
    pub close_state: ConnectionCloseState,
    pub kick_notify: Arc<Notify>,
    pub kick_reason: Arc<RwLock<String>>,
}

pub struct ServerSharedState {
    pub room_manager: SharedRoomManager,
    pub runtime_config: SharedRuntimeConfig,
    pub connection_count: Arc<AtomicU64>,
    pub online_player_count: Arc<AtomicU64>,
    pub player_msg_rate_limiter: SharedPlayerMessageRateLimiter,
    pub player_input_anomaly_tracker: SharedPlayerInputAnomalyTracker,
    pub shutdown_signal: ShutdownSignal,
}

impl ConnectionContext {
    pub fn outbound_channel(&self) -> OutboundChannel {
        OutboundChannel::new(self.tx.clone(), self.close_state.clone())
    }

    pub fn authenticated_identity(&self) -> Option<AuthenticatedSessionIdentity> {
        self.session.authenticated_identity()
    }

    pub fn ensure_authenticated_identity(
        &self,
        seq: u32,
    ) -> Result<Option<AuthenticatedSessionIdentity>, std::io::Error> {
        if self.session.state != SessionState::Authenticated {
            self.queue_error(seq, "NOT_AUTHENTICATED", "authenticate first")?;
            return Ok(None);
        }

        let identity = self.authenticated_identity();
        if identity.is_none() {
            self.queue_error(seq, "AUTH_CONTEXT_INCOMPLETE", "auth context incomplete")?;
        }

        Ok(identity)
    }

    pub fn queue_error(
        &self,
        seq: u32,
        error_code: &str,
        message: &str,
    ) -> Result<(), std::io::Error> {
        self.queue_message(
            MessageType::ErrorRes,
            seq,
            crate::pb::ErrorRes {
                error_code: error_code.to_string(),
                message: message.to_string(),
            },
        )
    }

    pub fn queue_message<M: prost::Message>(
        &self,
        message_type: MessageType,
        seq: u32,
        message: M,
    ) -> Result<(), std::io::Error> {
        let body = encode_body(&message);
        self.queue_raw_message(message_type, seq, body)
    }

    pub fn queue_raw_message(
        &self,
        message_type: MessageType,
        seq: u32,
        body: Vec<u8>,
    ) -> Result<(), std::io::Error> {
        try_send_outbound(
            &self.tx,
            &self.close_state,
            OutboundMessage {
                message_type,
                seq,
                body,
            },
            OutboundQueueLogContext {
                session_id: Some(self.session.id),
                subject_id: self.session.account_player_id.as_deref(),
                peer_addr: Some(&self.peer_addr),
                room_id: self.session.room_id.as_deref(),
                operation: "connection_queue_message",
            },
        )
        .map_err(std::io::Error::other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    use crate::core::room::ConnectionCloseState;

    fn handle(
        account_player_id: &str,
        character_id: &str,
        session_id: u64,
    ) -> PlayerConnectionHandle {
        let (tx, _rx) = mpsc::channel(1);
        PlayerConnectionHandle {
            account_player_id: account_player_id.to_string(),
            character_id: character_id.to_string(),
            kick_notify: Arc::new(Notify::new()),
            session_id,
            outbound: OutboundChannel::new(tx, ConnectionCloseState::new()),
            kick_reason: Arc::new(RwLock::new("session_kicked".to_string())),
        }
    }

    #[test]
    fn online_registry_indexes_account_and_character() {
        let mut registry = OnlinePlayerRegistry::default();

        registry.insert_by_account(handle("plr_0000000000001", "chr_0000000000001", 10));

        assert_eq!(
            registry
                .get_by_account("plr_0000000000001")
                .map(|handle| handle.session_id),
            Some(10)
        );
        assert_eq!(
            registry
                .get_by_character("chr_0000000000001")
                .map(|handle| handle.account_player_id.as_str()),
            Some("plr_0000000000001")
        );
    }

    #[test]
    fn online_registry_replaces_same_account_even_when_character_changes() {
        let mut registry = OnlinePlayerRegistry::default();

        registry.insert_by_account(handle("plr_0000000000001", "chr_0000000000001", 10));
        let old = registry
            .insert_by_account(handle("plr_0000000000001", "chr_0000000000002", 11))
            .expect("old account connection should be returned");

        assert_eq!(old.session_id, 10);
        assert_eq!(old.character_id, "chr_0000000000001");
        assert!(registry.get_by_character("chr_0000000000001").is_none());
        assert_eq!(
            registry
                .get_by_character("chr_0000000000002")
                .map(|handle| handle.session_id),
            Some(11)
        );
        assert_eq!(registry.online_connections().len(), 1);
    }

    #[test]
    fn online_registry_cleanup_requires_matching_session() {
        let mut registry = OnlinePlayerRegistry::default();

        registry.insert_by_account(handle("plr_0000000000001", "chr_0000000000001", 10));
        assert!(
            registry
                .remove_by_account_if_session("plr_0000000000001", 9)
                .is_none()
        );
        assert!(registry.get_by_account("plr_0000000000001").is_some());

        let removed = registry
            .remove_by_account_if_session("plr_0000000000001", 10)
            .expect("matching session should remove account connection");

        assert_eq!(removed.character_id, "chr_0000000000001");
        assert!(registry.get_by_account("plr_0000000000001").is_none());
        assert!(registry.get_by_character("chr_0000000000001").is_none());
    }
}
