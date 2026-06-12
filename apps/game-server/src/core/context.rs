use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use redis::aio::MultiplexedConnection;
use tokio::sync::{Mutex, Notify, RwLock};

use crate::config::Config;
use crate::core::config_table::ConfigTableRuntime;
use crate::core::player::PlayerManager;
use crate::core::room::{
    ConnectionCloseState, OutboundChannel, OutboundMessage, OutboundQueueLogContext,
    OutboundSender, try_send_outbound,
};
use crate::core::runtime::RoomManager;
use crate::mysql_store::MySqlAuditStore;
use crate::protocol::{MessageType, encode_body};
use crate::server::{PlayerInputAnomalyTracker, PlayerMessageRateLimiter, RuntimeConfig};
use crate::session::{Session, SessionState};

pub type SharedRoomManager = Arc<RoomManager>;
pub type SharedRuntimeConfig = Arc<RwLock<RuntimeConfig>>;
pub type ShutdownSignal = Arc<Notify>;
/// Maps player_id -> current authenticated connection on this game-server instance.
pub type PlayerRegistry = Arc<RwLock<HashMap<String, PlayerConnectionHandle>>>;
pub type SharedPlayerMessageRateLimiter = Arc<Mutex<PlayerMessageRateLimiter>>;
pub type SharedPlayerInputAnomalyTracker = Arc<Mutex<PlayerInputAnomalyTracker>>;

#[derive(Clone)]
pub struct PlayerConnectionHandle {
    pub kick_notify: Arc<Notify>,
    pub session_id: u64,
    pub outbound: OutboundChannel,
    pub kick_reason: Arc<RwLock<String>>,
}

#[derive(Clone)]
pub struct ServiceContext {
    pub config: Config,
    pub mysql_store: MySqlAuditStore,
    pub room_manager: SharedRoomManager,
    pub runtime_config: SharedRuntimeConfig,
    pub connection_count: Arc<AtomicU64>,
    pub config_tables: ConfigTableRuntime,
    pub player_manager: PlayerManager,
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

    pub fn ensure_authenticated(&self, seq: u32) -> Result<Option<String>, std::io::Error> {
        if self.session.state != SessionState::Authenticated {
            self.queue_error(seq, "NOT_AUTHENTICATED", "authenticate first")?;
            return Ok(None);
        }

        Ok(self.session.player_id.clone())
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
                player_id: self.session.player_id.as_deref(),
                peer_addr: Some(&self.peer_addr),
                room_id: self.session.room_id.as_deref(),
                operation: "connection_queue_message",
            },
        )
        .map_err(std::io::Error::other)
    }
}
