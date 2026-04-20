use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use redis::aio::MultiplexedConnection;
use tokio::sync::{RwLock, mpsc};

use crate::config::Config;
use crate::core::config_table::ConfigTableRuntime;
use crate::core::player::PlayerManager;
use crate::core::room::OutboundMessage;
use crate::core::runtime::RoomManager;
use crate::mysql_store::MySqlAuditStore;
use crate::protocol::{MessageType, encode_body};
use crate::server::RuntimeConfig;
use crate::session::{Session, SessionState};

pub type SharedRoomManager = Arc<RoomManager>;
pub type SharedRuntimeConfig = Arc<RwLock<RuntimeConfig>>;

#[derive(Clone)]
pub struct ServiceContext {
    pub config: Config,
    pub mysql_store: MySqlAuditStore,
    pub room_manager: SharedRoomManager,
    pub runtime_config: SharedRuntimeConfig,
    pub config_tables: ConfigTableRuntime,
    pub player_manager: PlayerManager,
    pub online_player_count: Arc<AtomicU64>,
}

pub struct ConnectionContext {
    pub peer_addr: String,
    pub redis: MultiplexedConnection,
    pub session: Session,
    pub tx: mpsc::UnboundedSender<OutboundMessage>,
}

pub struct ServerSharedState {
    pub room_manager: SharedRoomManager,
    pub runtime_config: SharedRuntimeConfig,
    pub connection_count: Arc<AtomicU64>,
    pub online_player_count: Arc<AtomicU64>,
}

impl ConnectionContext {
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
        self.tx
            .send(OutboundMessage {
                message_type,
                seq,
                body,
            })
            .map_err(|_| std::io::Error::other("failed to queue outbound"))
    }
}
