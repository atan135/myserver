use serde::{Deserialize, Serialize};

pub const AUTHORITY_PROTOCOL_VERSION: u16 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthorityTransport {
    Tcp,
    Kcp,
    Loopback,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthorityKind {
    Server,
    Client,
    Local,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityEndpoint {
    pub kind: AuthorityKind,
    pub authority_id: String,
    pub player_id: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub transport: AuthorityTransport,
    pub room_id: Option<String>,
    pub authority_epoch: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityInput {
    pub player_id: String,
    pub frame_id: u32,
    pub action: String,
    pub payload_json: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthoritySnapshot {
    pub room_id: String,
    pub authority_epoch: u64,
    pub frame_id: u32,
    pub authority_player_id: String,
    pub player_ids: Vec<String>,
    pub game_state_json: String,
    pub checksum: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityMigrationPayload {
    pub room_id: String,
    pub authority_epoch: u64,
    pub frozen_frame_id: u32,
    pub old_authority: AuthorityEndpoint,
    pub new_authority: AuthorityEndpoint,
    pub snapshot: AuthoritySnapshot,
    pub pending_inputs: Vec<AuthorityInput>,
    pub logic_state_json: String,
    pub runtime_state_json: String,
    pub checksum: String,
}

pub fn migration_checksum(payload: &AuthorityMigrationPayload) -> String {
    let mut clone = payload.clone();
    clone.checksum.clear();
    let encoded = serde_json::to_vec(&clone).unwrap_or_default();
    stable_hex_hash(&encoded)
}

fn stable_hex_hash(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}
