use authority_core::{
    AuthorityEndpoint, AuthorityInput, AuthorityKind, AuthorityMigrationPayload, AuthoritySnapshot,
    AuthorityTransport, migration_checksum,
};

use crate::core::room::PlayerInputRecord;
use crate::pb::RoomSnapshot;

pub fn server_authority_endpoint(
    server_id: impl Into<String>,
    room_id: impl Into<String>,
    authority_epoch: u64,
) -> AuthorityEndpoint {
    AuthorityEndpoint {
        kind: AuthorityKind::Server,
        authority_id: server_id.into(),
        player_id: None,
        host: None,
        port: None,
        transport: AuthorityTransport::Loopback,
        room_id: Some(room_id.into()),
        authority_epoch,
    }
}

pub fn client_authority_endpoint(
    player_id: impl Into<String>,
    host: impl Into<String>,
    port: u16,
    transport: AuthorityTransport,
    room_id: impl Into<String>,
    authority_epoch: u64,
) -> AuthorityEndpoint {
    let player_id = player_id.into();
    AuthorityEndpoint {
        kind: AuthorityKind::Client,
        authority_id: player_id.clone(),
        player_id: Some(player_id),
        host: Some(host.into()),
        port: Some(port),
        transport,
        room_id: Some(room_id.into()),
        authority_epoch,
    }
}

pub fn snapshot_from_room(snapshot: &RoomSnapshot, authority_epoch: u64) -> AuthoritySnapshot {
    AuthoritySnapshot {
        room_id: snapshot.room_id.clone(),
        authority_epoch,
        frame_id: snapshot.current_frame_id,
        authority_player_id: snapshot.owner_character_id.clone(),
        player_ids: snapshot
            .members
            .iter()
            .map(|member| member.character_id.clone())
            .collect(),
        game_state_json: snapshot.game_state.clone(),
        checksum: String::new(),
    }
}

pub fn input_from_record(record: &PlayerInputRecord) -> AuthorityInput {
    AuthorityInput {
        player_id: record.character_id.clone(),
        frame_id: record.frame_id,
        action: record.action.clone(),
        payload_json: record.payload_json.clone(),
    }
}

pub fn build_migration_payload(
    room_snapshot: &RoomSnapshot,
    authority_epoch: u64,
    old_authority: AuthorityEndpoint,
    new_authority: AuthorityEndpoint,
    pending_inputs: &[PlayerInputRecord],
    logic_state_json: String,
    runtime_state_json: String,
) -> AuthorityMigrationPayload {
    let mut payload = AuthorityMigrationPayload {
        room_id: room_snapshot.room_id.clone(),
        authority_epoch,
        frozen_frame_id: room_snapshot.current_frame_id,
        old_authority,
        new_authority,
        snapshot: snapshot_from_room(room_snapshot, authority_epoch),
        pending_inputs: pending_inputs.iter().map(input_from_record).collect(),
        logic_state_json,
        runtime_state_json,
        checksum: String::new(),
    };
    payload.snapshot.checksum = migration_checksum(&payload);
    payload.checksum = migration_checksum(&payload);
    payload
}
