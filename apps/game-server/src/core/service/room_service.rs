use serde_json::json;
use std::time::Instant;

use tracing::{info, warn};

use crate::core::context::{ConnectionContext, ServiceContext};
use crate::core::room::MemberRole;
use crate::core::system::movement::player_input_from_move_req;
use crate::pb::{
    CreateMatchedRoomReq, CreateMatchedRoomRes, MoveInputReq, MoveInputRes, PlayerInputReq,
    PlayerInputRes, RoomEndReq, RoomEndRes, RoomJoinAsObserverReq, RoomJoinAsObserverRes,
    RoomJoinReq, RoomJoinRes, RoomLeaveRes, RoomReadyReq, RoomReadyRes, RoomReconnectReq,
    RoomReconnectRes, RoomStartRes,
};
use crate::protocol::{MessageType, Packet};
use crate::server::{
    DEFAULT_DRAIN_MODE_REASON, DEFAULT_DRAIN_MODE_SOURCE, InputAnomalyKind, RuntimeConfig,
    current_unix_ms,
};

const DRAIN_MODE_REJECT_NEW_ROOM_ERROR: &str = "SERVER_DRAINING_REJECT_NEW_ROOM";

pub async fn handle_room_join(
    services: &ServiceContext,
    connection: &mut ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(identity) = connection.ensure_authenticated_identity(packet.header.seq)? else {
        return Ok(());
    };
    let player_id = identity.account_player_id;
    let character_id = identity.character_id;
    let world_id = identity.world_id;

    let request = match packet.decode_body::<RoomJoinReq>("INVALID_ROOM_JOIN_BODY") {
        Ok(value) => value,
        Err(error_code) => {
            connection.queue_error(packet.header.seq, error_code, "invalid room join body")?;
            return Ok(());
        }
    };

    let room_id = if request.room_id.is_empty() {
        "room-default".to_string()
    } else {
        request.room_id
    };
    let requested_policy_id = (!request.policy_id.is_empty()).then_some(request.policy_id.as_str());

    info!(
        session_id = connection.session.id,
        account_player_id = %player_id,
        player_id = %player_id,
        character_id = %character_id,
        world_id = ?world_id,
        room_id = %room_id,
        "handle_room_join"
    );

    if let Some(current_room_id) = &connection.session.room_id {
        if current_room_id != &room_id {
            connection.queue_message(
                MessageType::RoomJoinRes,
                packet.header.seq,
                RoomJoinRes {
                    ok: false,
                    room_id: current_room_id.clone(),
                    error_code: "ALREADY_IN_OTHER_ROOM".to_string(),
                },
            )?;
            return Ok(());
        }

        connection.queue_message(
            MessageType::RoomJoinRes,
            packet.header.seq,
            RoomJoinRes {
                ok: true,
                room_id: room_id.clone(),
                error_code: String::new(),
            },
        )?;
        return Ok(());
    }

    if let DrainNewRoomDecision::RejectNewRoom(state) =
        evaluate_drain_new_room_creation(services, DrainRoomCreateKind::DefaultRoom, &room_id).await
    {
        log_drain_mode_room_creation_rejected(
            "room_join",
            Some(&player_id),
            &room_id,
            None,
            &state,
        );
        connection.queue_message(
            MessageType::RoomJoinRes,
            packet.header.seq,
            RoomJoinRes {
                ok: false,
                room_id: room_id.clone(),
                error_code: DRAIN_MODE_REJECT_NEW_ROOM_ERROR.to_string(),
            },
        )?;
        return Ok(());
    }

    let join_result = services
        .room_manager
        .join_room(
            &room_id,
            &player_id,
            connection.outbound_channel(),
            MemberRole::Player,
            requested_policy_id,
        )
        .await;

    match join_result {
        Ok(snapshot) => {
            connection.session.room_id = Some(room_id.clone());
            let sync_before_broadcast = services
                .room_manager
                .is_member_syncing(&room_id, &player_id)
                .await;
            connection.queue_message(
                MessageType::RoomJoinRes,
                packet.header.seq,
                RoomJoinRes {
                    ok: true,
                    room_id: room_id.clone(),
                    error_code: String::new(),
                },
            )?;
            if sync_before_broadcast {
                connection.queue_message(
                    MessageType::RoomStatePush,
                    0,
                    crate::pb::RoomStatePush {
                        event: "member_joined".to_string(),
                        snapshot: Some(snapshot.clone()),
                    },
                )?;
            }
            services
                .db_store
                .append_room_event_with_identity(
                    &room_id,
                    Some(&player_id),
                    Some(&player_id),
                    Some(&character_id),
                    Some(&snapshot.owner_player_id),
                    "room_joined",
                    Some(&snapshot.state),
                    snapshot.members.len(),
                    Some(json!({
                        "seq": packet.header.seq,
                        "accountPlayerId": player_id,
                        "characterId": character_id,
                        "worldId": world_id,
                        "members": snapshot.members.iter().map(|member| json!({
                            "playerId": member.player_id,
                            "ready": member.ready,
                            "isOwner": member.is_owner
                        })).collect::<Vec<_>>()
                    })),
                )
                .await;
            let broadcast_result = services
                .room_manager
                .broadcast_snapshot(&room_id, "member_joined", snapshot)
                .await;
            if sync_before_broadcast {
                services
                    .room_manager
                    .finish_member_sync(&room_id, &player_id)
                    .await;
            }
            broadcast_result?;
        }
        Err(error_code) => {
            connection.queue_message(
                MessageType::RoomJoinRes,
                packet.header.seq,
                RoomJoinRes {
                    ok: false,
                    room_id: room_id.clone(),
                    error_code: error_code.to_string(),
                },
            )?;
            services
                .db_store
                .append_room_event_with_identity(
                    &room_id,
                    Some(&player_id),
                    Some(&player_id),
                    Some(&character_id),
                    None,
                    "room_join_failed",
                    None,
                    0,
                    Some(json!({
                        "errorCode": error_code,
                        "seq": packet.header.seq,
                        "accountPlayerId": player_id,
                        "characterId": character_id,
                        "worldId": world_id
                    })),
                )
                .await;
        }
    }

    Ok(())
}

pub async fn handle_room_leave(
    services: &ServiceContext,
    connection: &mut ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(room_id) = connection.session.room_id.clone() else {
        connection.queue_message(
            MessageType::RoomLeaveRes,
            packet.header.seq,
            RoomLeaveRes {
                ok: false,
                room_id: String::new(),
                error_code: "ROOM_NOT_JOINED".to_string(),
            },
        )?;
        return Ok(());
    };

    let Some(identity) = connection.ensure_authenticated_identity(packet.header.seq)? else {
        return Ok(());
    };
    let player_id = identity.account_player_id;
    let character_id = identity.character_id;
    let world_id = identity.world_id;

    let leave_result = services.room_manager.leave_room(&room_id, &player_id).await;
    connection.session.room_id = None;

    connection.queue_message(
        MessageType::RoomLeaveRes,
        packet.header.seq,
        RoomLeaveRes {
            ok: true,
            room_id: room_id.clone(),
            error_code: String::new(),
        },
    )?;

    if let Some(snapshot) = leave_result.snapshot {
        services
            .db_store
            .append_room_event_with_identity(
                &room_id,
                Some(&player_id),
                Some(&player_id),
                Some(&character_id),
                Some(&snapshot.owner_player_id),
                "room_left",
                Some(&snapshot.state),
                snapshot.members.len(),
                Some(json!({
                    "seq": packet.header.seq,
                    "accountPlayerId": player_id,
                    "characterId": character_id,
                    "worldId": world_id
                })),
            )
            .await;
        services
            .room_manager
            .broadcast_snapshot(&room_id, "member_left", snapshot)
            .await?;
    } else if leave_result.room_removed {
        services
            .db_store
            .append_room_event_with_identity(
                &room_id,
                Some(&player_id),
                Some(&player_id),
                Some(&character_id),
                None,
                "room_disbanded",
                None,
                0,
                Some(json!({
                    "seq": packet.header.seq,
                    "accountPlayerId": player_id,
                    "characterId": character_id,
                    "worldId": world_id
                })),
            )
            .await;
    }

    Ok(())
}

pub async fn handle_room_ready(
    services: &ServiceContext,
    connection: &ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(identity) = connection.ensure_authenticated_identity(packet.header.seq)? else {
        return Ok(());
    };
    let player_id = identity.account_player_id;
    let character_id = identity.character_id;
    let world_id = identity.world_id;
    let Some(room_id) = connection.session.room_id.clone() else {
        connection.queue_message(
            MessageType::RoomReadyRes,
            packet.header.seq,
            RoomReadyRes {
                ok: false,
                room_id: String::new(),
                ready: false,
                error_code: "ROOM_NOT_JOINED".to_string(),
            },
        )?;
        return Ok(());
    };

    let request = match packet.decode_body::<RoomReadyReq>("INVALID_ROOM_READY_BODY") {
        Ok(value) => value,
        Err(error_code) => {
            connection.queue_error(packet.header.seq, error_code, "invalid room ready body")?;
            return Ok(());
        }
    };

    let ready_result = services
        .room_manager
        .set_ready_state(&room_id, &player_id, request.ready)
        .await;

    match ready_result {
        Ok(snapshot) => {
            connection.queue_message(
                MessageType::RoomReadyRes,
                packet.header.seq,
                RoomReadyRes {
                    ok: true,
                    room_id: room_id.clone(),
                    ready: request.ready,
                    error_code: String::new(),
                },
            )?;
            services
                .db_store
                .append_room_event_with_identity(
                    &room_id,
                    Some(&player_id),
                    Some(&player_id),
                    Some(&character_id),
                    Some(&snapshot.owner_player_id),
                    "room_ready_changed",
                    Some(&snapshot.state),
                    snapshot.members.len(),
                    Some(json!({
                        "ready": request.ready,
                        "seq": packet.header.seq,
                        "accountPlayerId": player_id,
                        "characterId": character_id,
                        "worldId": world_id
                    })),
                )
                .await;
            services
                .room_manager
                .broadcast_snapshot(&room_id, "ready_changed", snapshot)
                .await?;
        }
        Err(error_code) => {
            connection.queue_message(
                MessageType::RoomReadyRes,
                packet.header.seq,
                RoomReadyRes {
                    ok: false,
                    room_id,
                    ready: request.ready,
                    error_code: error_code.to_string(),
                },
            )?;
        }
    }

    Ok(())
}

pub async fn handle_room_start(
    services: &ServiceContext,
    connection: &ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(identity) = connection.ensure_authenticated_identity(packet.header.seq)? else {
        return Ok(());
    };
    let player_id = identity.account_player_id;
    let character_id = identity.character_id;
    let world_id = identity.world_id;
    let Some(room_id) = connection.session.room_id.clone() else {
        connection.queue_message(
            MessageType::RoomStartRes,
            packet.header.seq,
            RoomStartRes {
                ok: false,
                room_id: String::new(),
                error_code: "ROOM_NOT_JOINED".to_string(),
            },
        )?;
        return Ok(());
    };

    let start_result = services.room_manager.start_game(&room_id, &player_id).await;

    match start_result {
        Ok(snapshot) => {
            connection.queue_message(
                MessageType::RoomStartRes,
                packet.header.seq,
                RoomStartRes {
                    ok: true,
                    room_id: room_id.clone(),
                    error_code: String::new(),
                },
            )?;
            services
                .db_store
                .append_room_event_with_identity(
                    &room_id,
                    Some(&player_id),
                    Some(&player_id),
                    Some(&character_id),
                    Some(&snapshot.owner_player_id),
                    "game_started",
                    Some(&snapshot.state),
                    snapshot.members.len(),
                    Some(json!({
                        "seq": packet.header.seq,
                        "accountPlayerId": player_id,
                        "characterId": character_id,
                        "worldId": world_id
                    })),
                )
                .await;
            services
                .room_manager
                .broadcast_snapshot(&room_id, "game_started", snapshot)
                .await?;
        }
        Err(error_code) => {
            connection.queue_message(
                MessageType::RoomStartRes,
                packet.header.seq,
                RoomStartRes {
                    ok: false,
                    room_id,
                    error_code: error_code.to_string(),
                },
            )?;
        }
    }

    Ok(())
}

pub async fn handle_player_input(
    services: &ServiceContext,
    connection: &ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(identity) = connection.ensure_authenticated_identity(packet.header.seq)? else {
        return Ok(());
    };
    let player_id = identity.account_player_id;
    let character_id = identity.character_id;
    let world_id = identity.world_id;
    let Some(room_id) = connection.session.room_id.clone() else {
        connection.queue_message(
            MessageType::PlayerInputRes,
            packet.header.seq,
            PlayerInputRes {
                ok: false,
                room_id: String::new(),
                error_code: "ROOM_NOT_JOINED".to_string(),
            },
        )?;
        return Ok(());
    };

    let request = match packet.decode_body::<PlayerInputReq>("INVALID_PLAYER_INPUT_BODY") {
        Ok(value) => value,
        Err(error_code) => {
            connection.queue_error(packet.header.seq, error_code, "invalid player input body")?;
            return Ok(());
        }
    };

    if let Err(error_code) = validate_input_timestamp(
        &*services.runtime_config.read().await,
        request.client_timestamp_ms,
    ) {
        warn!(
            room_id = %room_id,
            account_player_id = %player_id,
            player_id = %player_id,
            character_id = %character_id,
            frame_id = request.frame_id,
            client_timestamp_ms = request.client_timestamp_ms,
            error_code = %error_code,
            "player input timestamp rejected"
        );
        record_input_anomaly(
            services,
            &room_id,
            &player_id,
            request.frame_id,
            packet.header.seq,
            "PlayerInputReq",
            InputAnomalyKind::Timestamp,
            error_code,
        )
        .await;
        connection.queue_message(
            MessageType::PlayerInputRes,
            packet.header.seq,
            PlayerInputRes {
                ok: false,
                room_id,
                error_code: error_code.to_string(),
            },
        )?;
        return Ok(());
    }

    if reject_if_input_anomaly_blocked(
        services,
        connection,
        MessageType::PlayerInputRes,
        packet.header.seq,
        &room_id,
        &player_id,
        request.frame_id,
        "PlayerInputReq",
    )
    .await?
    {
        return Ok(());
    }

    let input_fingerprint = input_fingerprint(&request.action, &request.payload_json);
    if remember_input_frame_and_record_duplicate(
        services,
        &room_id,
        &player_id,
        request.frame_id,
        &input_fingerprint,
        packet.header.seq,
        "PlayerInputReq",
    )
    .await
    {
        if reject_if_input_anomaly_blocked(
            services,
            connection,
            MessageType::PlayerInputRes,
            packet.header.seq,
            &room_id,
            &player_id,
            request.frame_id,
            "PlayerInputReq",
        )
        .await?
        {
            return Ok(());
        }
    }

    let input_result = services
        .room_manager
        .accept_player_input(
            &room_id,
            &player_id,
            request.frame_id,
            &request.action,
            &request.payload_json,
        )
        .await;

    match input_result {
        Ok(_) => {
            connection.queue_message(
                MessageType::PlayerInputRes,
                packet.header.seq,
                PlayerInputRes {
                    ok: true,
                    room_id: room_id.clone(),
                    error_code: String::new(),
                },
            )?;
            services
                .db_store
                .append_room_event_with_identity(
                    &room_id,
                    Some(&player_id),
                    Some(&player_id),
                    Some(&character_id),
                    None,
                    "player_input",
                    Some("in_game"),
                    0,
                    Some(json!({
                        "seq": packet.header.seq,
                        "accountPlayerId": player_id,
                        "characterId": character_id,
                        "worldId": world_id,
                        "frameId": request.frame_id,
                        "action": request.action,
                        "payloadJson": request.payload_json
                    })),
                )
                .await;
        }
        Err(error_code) => {
            if let Some(kind) = input_anomaly_kind_from_error(error_code) {
                record_input_anomaly(
                    services,
                    &room_id,
                    &player_id,
                    request.frame_id,
                    packet.header.seq,
                    "PlayerInputReq",
                    kind,
                    error_code,
                )
                .await;
            }
            warn!(
                room_id = %room_id,
                account_player_id = %player_id,
                player_id = %player_id,
                character_id = %character_id,
                frame_id = request.frame_id,
                action = %request.action,
                error_code = %error_code,
                "player input rejected"
            );
            connection.queue_message(
                MessageType::PlayerInputRes,
                packet.header.seq,
                PlayerInputRes {
                    ok: false,
                    room_id,
                    error_code: error_code.to_string(),
                },
            )?;
        }
    }

    Ok(())
}

pub async fn handle_move_input(
    services: &ServiceContext,
    connection: &ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(identity) = connection.ensure_authenticated_identity(packet.header.seq)? else {
        return Ok(());
    };
    let player_id = identity.account_player_id;
    let character_id = identity.character_id;
    let world_id = identity.world_id;
    let Some(room_id) = connection.session.room_id.clone() else {
        connection.queue_message(
            MessageType::MoveInputRes,
            packet.header.seq,
            MoveInputRes {
                ok: false,
                room_id: String::new(),
                error_code: "ROOM_NOT_JOINED".to_string(),
            },
        )?;
        return Ok(());
    };

    let request = match packet.decode_body::<MoveInputReq>("INVALID_MOVE_INPUT_BODY") {
        Ok(value) => value,
        Err(error_code) => {
            connection.queue_error(packet.header.seq, error_code, "invalid move input body")?;
            return Ok(());
        }
    };

    if let Err(error_code) = validate_input_timestamp(
        &*services.runtime_config.read().await,
        request.client_timestamp_ms,
    ) {
        warn!(
            room_id = %room_id,
            account_player_id = %player_id,
            player_id = %player_id,
            character_id = %character_id,
            frame_id = request.frame_id,
            client_timestamp_ms = request.client_timestamp_ms,
            error_code = %error_code,
            "move input timestamp rejected"
        );
        record_input_anomaly(
            services,
            &room_id,
            &player_id,
            request.frame_id,
            packet.header.seq,
            "MoveInputReq",
            InputAnomalyKind::Timestamp,
            error_code,
        )
        .await;
        connection.queue_message(
            MessageType::MoveInputRes,
            packet.header.seq,
            MoveInputRes {
                ok: false,
                room_id,
                error_code: error_code.to_string(),
            },
        )?;
        return Ok(());
    }

    if reject_if_input_anomaly_blocked(
        services,
        connection,
        MessageType::MoveInputRes,
        packet.header.seq,
        &room_id,
        &player_id,
        request.frame_id,
        "MoveInputReq",
    )
    .await?
    {
        return Ok(());
    }

    let (action, payload_json) = match player_input_from_move_req(&request) {
        Ok(value) => value,
        Err(error) => {
            connection.queue_message(
                MessageType::MoveInputRes,
                packet.header.seq,
                MoveInputRes {
                    ok: false,
                    room_id,
                    error_code: error.error_code.to_string(),
                },
            )?;
            return Ok(());
        }
    };

    let input_fingerprint = input_fingerprint(action, &payload_json);
    if remember_input_frame_and_record_duplicate(
        services,
        &room_id,
        &player_id,
        request.frame_id,
        &input_fingerprint,
        packet.header.seq,
        "MoveInputReq",
    )
    .await
    {
        if reject_if_input_anomaly_blocked(
            services,
            connection,
            MessageType::MoveInputRes,
            packet.header.seq,
            &room_id,
            &player_id,
            request.frame_id,
            "MoveInputReq",
        )
        .await?
        {
            return Ok(());
        }
    }

    let input_result = services
        .room_manager
        .accept_player_input(
            &room_id,
            &player_id,
            request.frame_id,
            action,
            &payload_json,
        )
        .await;

    match input_result {
        Ok(_) => {
            connection.queue_message(
                MessageType::MoveInputRes,
                packet.header.seq,
                MoveInputRes {
                    ok: true,
                    room_id: room_id.clone(),
                    error_code: String::new(),
                },
            )?;
            services
                .db_store
                .append_room_event_with_identity(
                    &room_id,
                    Some(&player_id),
                    Some(&player_id),
                    Some(&character_id),
                    None,
                    "move_input",
                    Some("in_game"),
                    0,
                    Some(json!({
                        "seq": packet.header.seq,
                        "accountPlayerId": player_id,
                        "characterId": character_id,
                        "worldId": world_id,
                        "frameId": request.frame_id,
                        "inputType": request.input_type,
                        "dirX": request.dir_x,
                        "dirY": request.dir_y,
                        "hasClientState": request.has_client_state,
                        "clientX": request.client_x,
                        "clientY": request.client_y,
                        "clientFrameId": request.client_frame_id
                    })),
                )
                .await;
        }
        Err(error_code) => {
            if let Some(kind) = input_anomaly_kind_from_error(error_code) {
                record_input_anomaly(
                    services,
                    &room_id,
                    &player_id,
                    request.frame_id,
                    packet.header.seq,
                    "MoveInputReq",
                    kind,
                    error_code,
                )
                .await;
            }
            connection.queue_message(
                MessageType::MoveInputRes,
                packet.header.seq,
                MoveInputRes {
                    ok: false,
                    room_id,
                    error_code: error_code.to_string(),
                },
            )?;
        }
    }

    Ok(())
}

pub(crate) fn validate_input_timestamp(
    runtime: &RuntimeConfig,
    client_timestamp_ms: i64,
) -> Result<(), &'static str> {
    if client_timestamp_ms <= 0 {
        return if runtime.input_timestamp_required {
            Err("INPUT_TIMESTAMP_REQUIRED")
        } else {
            Ok(())
        };
    }

    if runtime.input_timestamp_max_skew_ms == 0 {
        return Ok(());
    }

    let now_ms = current_unix_ms();
    let skew_ms = now_ms.abs_diff(client_timestamp_ms);
    if skew_ms > runtime.input_timestamp_max_skew_ms {
        Err("INPUT_TIMESTAMP_SKEW")
    } else {
        Ok(())
    }
}

fn input_anomaly_kind_from_error(error_code: &str) -> Option<InputAnomalyKind> {
    match error_code {
        "INPUT_FRAME_EXPIRED" => Some(InputAnomalyKind::Expired),
        "INPUT_FRAME_TOO_FAR" => Some(InputAnomalyKind::Future),
        _ => None,
    }
}

async fn remember_input_frame_and_record_duplicate(
    services: &ServiceContext,
    room_id: &str,
    player_id: &str,
    frame_id: u32,
    input_fingerprint: &str,
    seq: u32,
    request_type: &'static str,
) -> bool {
    let runtime = services.runtime_config.read().await.clone();
    let duplicate = services
        .player_input_anomaly_tracker
        .lock()
        .await
        .remember_frame(
            player_id,
            room_id,
            frame_id,
            input_fingerprint,
            Instant::now(),
            runtime.input_anomaly_window_ms,
        );

    if duplicate {
        record_input_anomaly(
            services,
            room_id,
            player_id,
            frame_id,
            seq,
            request_type,
            InputAnomalyKind::Duplicate,
            "INPUT_FRAME_DUPLICATE",
        )
        .await;
    }

    duplicate
}

fn input_fingerprint(action: &str, payload_json: &str) -> String {
    format!("{action}\n{payload_json}")
}

async fn reject_if_input_anomaly_blocked(
    services: &ServiceContext,
    connection: &ConnectionContext,
    response_type: MessageType,
    seq: u32,
    room_id: &str,
    player_id: &str,
    frame_id: u32,
    request_type: &'static str,
) -> Result<bool, std::io::Error> {
    let runtime = services.runtime_config.read().await.clone();
    let blocked = services
        .player_input_anomaly_tracker
        .lock()
        .await
        .is_blocked(
            player_id,
            Instant::now(),
            runtime.input_anomaly_window_ms,
            runtime.input_anomaly_max,
        );

    if !blocked {
        return Ok(false);
    }

    warn!(
        room_id = %room_id,
        player_id = %player_id,
        frame_id = frame_id,
        seq = seq,
        request_type = request_type,
        anomaly_window_ms = runtime.input_anomaly_window_ms,
        anomaly_max = runtime.input_anomaly_max,
        error_code = "INPUT_ANOMALY_BLOCKED",
        "player input rejected after anomaly threshold"
    );

    queue_input_rejected(
        connection,
        response_type,
        seq,
        room_id,
        "INPUT_ANOMALY_BLOCKED",
    )?;
    Ok(true)
}

async fn record_input_anomaly(
    services: &ServiceContext,
    room_id: &str,
    player_id: &str,
    frame_id: u32,
    seq: u32,
    request_type: &'static str,
    kind: InputAnomalyKind,
    error_code: &str,
) {
    let runtime = services.runtime_config.read().await.clone();
    let outcome = services.player_input_anomaly_tracker.lock().await.record(
        player_id,
        Instant::now(),
        runtime.input_anomaly_window_ms,
        runtime.input_anomaly_max,
    );

    warn!(
        room_id = %room_id,
        player_id = %player_id,
        frame_id = frame_id,
        seq = seq,
        request_type = request_type,
        anomaly_kind = kind.as_str(),
        anomaly_count = outcome.count,
        anomaly_blocked = outcome.blocked,
        anomaly_window_ms = runtime.input_anomaly_window_ms,
        anomaly_max = runtime.input_anomaly_max,
        error_code = error_code,
        "player input anomaly recorded"
    );
}

fn queue_input_rejected(
    connection: &ConnectionContext,
    response_type: MessageType,
    seq: u32,
    room_id: &str,
    error_code: &str,
) -> Result<(), std::io::Error> {
    match response_type {
        MessageType::PlayerInputRes => connection.queue_message(
            MessageType::PlayerInputRes,
            seq,
            PlayerInputRes {
                ok: false,
                room_id: room_id.to_string(),
                error_code: error_code.to_string(),
            },
        ),
        MessageType::MoveInputRes => connection.queue_message(
            MessageType::MoveInputRes,
            seq,
            MoveInputRes {
                ok: false,
                room_id: room_id.to_string(),
                error_code: error_code.to_string(),
            },
        ),
        _ => Ok(()),
    }
}

pub async fn handle_room_end(
    services: &ServiceContext,
    connection: &ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(identity) = connection.ensure_authenticated_identity(packet.header.seq)? else {
        return Ok(());
    };
    let player_id = identity.account_player_id;
    let character_id = identity.character_id;
    let world_id = identity.world_id;
    let Some(room_id) = connection.session.room_id.clone() else {
        connection.queue_message(
            MessageType::RoomEndRes,
            packet.header.seq,
            RoomEndRes {
                ok: false,
                room_id: String::new(),
                error_code: "ROOM_NOT_JOINED".to_string(),
            },
        )?;
        return Ok(());
    };

    let request = match packet.decode_body::<RoomEndReq>("INVALID_ROOM_END_BODY") {
        Ok(value) => value,
        Err(error_code) => {
            connection.queue_error(packet.header.seq, error_code, "invalid room end body")?;
            return Ok(());
        }
    };

    let end_result = services.room_manager.end_game(&room_id, &player_id).await;

    match end_result {
        Ok(snapshot) => {
            connection.queue_message(
                MessageType::RoomEndRes,
                packet.header.seq,
                RoomEndRes {
                    ok: true,
                    room_id: room_id.clone(),
                    error_code: String::new(),
                },
            )?;
            services
                .db_store
                .append_room_event_with_identity(
                    &room_id,
                    Some(&player_id),
                    Some(&player_id),
                    Some(&character_id),
                    Some(&snapshot.owner_player_id),
                    "game_ended",
                    Some(&snapshot.state),
                    snapshot.members.len(),
                    Some(json!({
                        "seq": packet.header.seq,
                        "accountPlayerId": player_id,
                        "characterId": character_id,
                        "worldId": world_id,
                        "reason": request.reason
                    })),
                )
                .await;
            services
                .room_manager
                .broadcast_snapshot(&room_id, "game_ended", snapshot)
                .await?;
        }
        Err(error_code) => {
            connection.queue_message(
                MessageType::RoomEndRes,
                packet.header.seq,
                RoomEndRes {
                    ok: false,
                    room_id,
                    error_code: error_code.to_string(),
                },
            )?;
        }
    }

    Ok(())
}

pub async fn handle_disconnect_cleanup(services: &ServiceContext, connection: &ConnectionContext) {
    let session = &connection.session;
    let room_id = session.room_id.clone();
    let identity = session.authenticated_identity();
    let player_id = identity
        .as_ref()
        .map(|value| value.account_player_id.clone())
        .or_else(|| session.player_id.clone());
    let character_id = identity.as_ref().map(|value| value.character_id.clone());
    let world_id = identity.as_ref().and_then(|value| value.world_id);

    info!(
        session_id = session.id,
        room_id = ?room_id,
        account_player_id = ?player_id,
        player_id = ?player_id,
        character_id = ?character_id,
        world_id = ?world_id,
        "handle_disconnect_cleanup called"
    );

    if let (Some(room_id), Some(player_id)) = (room_id, player_id) {
        let leave_result = services
            .room_manager
            .disconnect_room_member(&room_id, &player_id)
            .await;

        if let Some(snapshot) = leave_result.snapshot {
            services
                .db_store
                .append_room_event_with_identity(
                    &room_id,
                    Some(&player_id),
                    Some(&player_id),
                    character_id.as_deref(),
                    Some(&snapshot.owner_player_id),
                    "member_disconnected",
                    Some(&snapshot.state),
                    snapshot.members.len(),
                    Some(json!({
                        "accountPlayerId": player_id,
                        "characterId": character_id,
                        "worldId": world_id
                    })),
                )
                .await;
            let _ = services
                .room_manager
                .broadcast_snapshot(&room_id, "member_disconnected", snapshot)
                .await;
        }
    }
}

pub async fn handle_room_reconnect(
    services: &ServiceContext,
    connection: &mut ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(identity) = connection.ensure_authenticated_identity(packet.header.seq)? else {
        return Ok(());
    };
    let player_id = identity.account_player_id;
    let character_id = identity.character_id;
    let world_id = identity.world_id;

    let request = match packet.decode_body::<RoomReconnectReq>("INVALID_ROOM_RECONNECT_BODY") {
        Ok(value) => value,
        Err(error_code) => {
            connection.queue_error(packet.header.seq, error_code, "invalid room reconnect body")?;
            return Ok(());
        }
    };

    let reconnect_player_id = match resolve_reconnect_account_player_id(
        &player_id,
        &request.player_id,
    ) {
        Ok(value) => value,
        Err(error_code) => {
            warn!(
                session_id = connection.session.id,
                account_player_id = %player_id,
                player_id = %player_id,
                requested_player_id = %request.player_id,
                character_id = %character_id,
                world_id = ?world_id,
                error_code = error_code,
                "room reconnect rejected because request player_id does not match authenticated account"
            );
            connection.queue_message(
                MessageType::RoomReconnectRes,
                packet.header.seq,
                RoomReconnectRes {
                    ok: false,
                    room_id: String::new(),
                    error_code: error_code.to_string(),
                    snapshot: None,
                    current_frame_id: 0,
                    recent_inputs: vec![],
                    waiting_frame_id: 0,
                    waiting_inputs: vec![],
                    input_delay_frames: 0,
                    movement_recovery: None,
                },
            )?;
            services
                .db_store
                .append_connection_event_with_identity(
                    connection.session.id,
                    Some(&player_id),
                    Some(&player_id),
                    Some(&character_id),
                    Some(&connection.peer_addr),
                    "room_reconnect_account_mismatch",
                    Some(json!({
                        "seq": packet.header.seq,
                        "accountPlayerId": player_id,
                        "requestedPlayerId": request.player_id,
                        "characterId": character_id,
                        "worldId": world_id
                    })),
                )
                .await;
            return Ok(());
        }
    };

    // If already in a room, cannot reconnect
    if connection.session.room_id.is_some() {
        connection.queue_message(
            MessageType::RoomReconnectRes,
            packet.header.seq,
            RoomReconnectRes {
                ok: false,
                room_id: String::new(),
                error_code: "ALREADY_IN_ROOM".to_string(),
                snapshot: None,
                current_frame_id: 0,
                recent_inputs: vec![],
                waiting_frame_id: 0,
                waiting_inputs: vec![],
                input_delay_frames: 0,
                movement_recovery: None,
            },
        )?;
        return Ok(());
    }

    // Find the room the player is offline in (via PostgreSQL audit log or cache)
    // For now, client should provide room_id - we'll search for the player
    // This is a simplified implementation - in production you'd track this in Redis
    let room_id = match services
        .db_store
        .find_room_by_offline_player(&reconnect_player_id)
        .await
    {
        Some(room_id) => Some(room_id),
        None => {
            services
                .room_manager
                .find_room_by_offline_player(&reconnect_player_id)
                .await
        }
    };

    let Some(room_id) = room_id else {
        connection.queue_message(
            MessageType::RoomReconnectRes,
            packet.header.seq,
            RoomReconnectRes {
                ok: false,
                room_id: String::new(),
                error_code: "PLAYER_NOT_OFFLINE".to_string(),
                snapshot: None,
                current_frame_id: 0,
                recent_inputs: vec![],
                waiting_frame_id: 0,
                waiting_inputs: vec![],
                input_delay_frames: 0,
                movement_recovery: None,
            },
        )?;
        return Ok(());
    };

    let reconnect_result = services
        .room_manager
        .reconnect_room(
            &room_id,
            &reconnect_player_id,
            connection.outbound_channel(),
        )
        .await;

    match reconnect_result {
        Ok(recovery) => {
            let snapshot = recovery.snapshot.clone();
            connection.session.room_id = Some(room_id.clone());
            connection.queue_message(
                MessageType::RoomReconnectRes,
                packet.header.seq,
                RoomReconnectRes {
                    ok: true,
                    room_id: room_id.clone(),
                    error_code: String::new(),
                    snapshot: Some(snapshot.clone()),
                    current_frame_id: recovery.current_frame_id,
                    recent_inputs: recovery.recent_inputs,
                    waiting_frame_id: recovery.waiting_frame_id,
                    waiting_inputs: recovery.waiting_inputs,
                    input_delay_frames: recovery.input_delay_frames,
                    movement_recovery: recovery.movement_recovery,
                },
            )?;
            services
                .db_store
                .append_room_event_with_identity(
                    &room_id,
                    Some(&reconnect_player_id),
                    Some(&player_id),
                    Some(&character_id),
                    Some(&snapshot.owner_player_id),
                    "player_reconnected",
                    Some(&snapshot.state),
                    snapshot.members.len(),
                    Some(json!({
                        "seq": packet.header.seq,
                        "accountPlayerId": player_id,
                        "characterId": character_id,
                        "worldId": world_id
                    })),
                )
                .await;
            services
                .room_manager
                .broadcast_snapshot(&room_id, "member_reconnected", snapshot)
                .await?;
        }
        Err(error_code) => {
            connection.queue_message(
                MessageType::RoomReconnectRes,
                packet.header.seq,
                RoomReconnectRes {
                    ok: false,
                    room_id,
                    error_code: error_code.to_string(),
                    snapshot: None,
                    current_frame_id: 0,
                    recent_inputs: vec![],
                    waiting_frame_id: 0,
                    waiting_inputs: vec![],
                    input_delay_frames: 0,
                    movement_recovery: None,
                },
            )?;
        }
    }

    Ok(())
}

fn resolve_reconnect_account_player_id(
    authenticated_account_player_id: &str,
    requested_player_id: &str,
) -> Result<String, &'static str> {
    if requested_player_id.is_empty() || requested_player_id == authenticated_account_player_id {
        Ok(authenticated_account_player_id.to_string())
    } else {
        Err("ACCOUNT_PLAYER_ID_MISMATCH")
    }
}

pub async fn handle_join_as_observer(
    services: &ServiceContext,
    connection: &mut ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(identity) = connection.ensure_authenticated_identity(packet.header.seq)? else {
        return Ok(());
    };
    let player_id = identity.account_player_id;
    let character_id = identity.character_id;
    let world_id = identity.world_id;

    let request = match packet.decode_body::<RoomJoinAsObserverReq>("INVALID_OBSERVER_JOIN_BODY") {
        Ok(value) => value,
        Err(error_code) => {
            connection.queue_error(packet.header.seq, error_code, "invalid observer join body")?;
            return Ok(());
        }
    };

    let room_id = if request.room_id.is_empty() {
        "room-default".to_string()
    } else {
        request.room_id
    };

    info!(
        session_id = connection.session.id,
        account_player_id = %player_id,
        player_id = %player_id,
        character_id = %character_id,
        world_id = ?world_id,
        room_id = %room_id,
        "handle_join_as_observer"
    );

    if let Some(current_room_id) = &connection.session.room_id {
        connection.queue_message(
            MessageType::RoomJoinAsObserverRes,
            packet.header.seq,
            RoomJoinAsObserverRes {
                ok: false,
                room_id: current_room_id.clone(),
                error_code: "ALREADY_IN_OTHER_ROOM".to_string(),
                snapshot: None,
                current_frame_id: 0,
                recent_inputs: vec![],
                waiting_frame_id: 0,
                waiting_inputs: vec![],
                input_delay_frames: 0,
                movement_recovery: None,
            },
        )?;
        return Ok(());
    }

    if let DrainNewRoomDecision::RejectNewRoom(state) =
        evaluate_drain_new_room_creation(services, DrainRoomCreateKind::DefaultRoom, &room_id).await
    {
        log_drain_mode_room_creation_rejected(
            "observer_join",
            Some(&player_id),
            &room_id,
            None,
            &state,
        );
        connection.queue_message(
            MessageType::RoomJoinAsObserverRes,
            packet.header.seq,
            RoomJoinAsObserverRes {
                ok: false,
                room_id: room_id.clone(),
                error_code: DRAIN_MODE_REJECT_NEW_ROOM_ERROR.to_string(),
                snapshot: None,
                current_frame_id: 0,
                recent_inputs: vec![],
                waiting_frame_id: 0,
                waiting_inputs: vec![],
                input_delay_frames: 0,
                movement_recovery: None,
            },
        )?;
        return Ok(());
    }

    let join_result = services
        .room_manager
        .join_room_as_observer(&room_id, &player_id, connection.outbound_channel())
        .await;

    match join_result {
        Ok(recovery) => {
            let snapshot = recovery.snapshot.clone();
            connection.session.room_id = Some(room_id.clone());
            connection.queue_message(
                MessageType::RoomJoinAsObserverRes,
                packet.header.seq,
                RoomJoinAsObserverRes {
                    ok: true,
                    room_id: room_id.clone(),
                    error_code: String::new(),
                    snapshot: Some(snapshot.clone()),
                    current_frame_id: recovery.current_frame_id,
                    recent_inputs: recovery.recent_inputs,
                    waiting_frame_id: recovery.waiting_frame_id,
                    waiting_inputs: recovery.waiting_inputs,
                    input_delay_frames: recovery.input_delay_frames,
                    movement_recovery: recovery.movement_recovery,
                },
            )?;
            services
                .db_store
                .append_room_event_with_identity(
                    &room_id,
                    Some(&player_id),
                    Some(&player_id),
                    Some(&character_id),
                    Some(&snapshot.owner_player_id),
                    "observer_joined",
                    Some(&snapshot.state),
                    snapshot.members.len(),
                    Some(json!({
                        "seq": packet.header.seq,
                        "accountPlayerId": player_id,
                        "characterId": character_id,
                        "worldId": world_id,
                        "currentFrameId": recovery.current_frame_id,
                        "waitingFrameId": recovery.waiting_frame_id
                    })),
                )
                .await;
        }
        Err(error_code) => {
            connection.queue_message(
                MessageType::RoomJoinAsObserverRes,
                packet.header.seq,
                RoomJoinAsObserverRes {
                    ok: false,
                    room_id: room_id.clone(),
                    error_code: error_code.to_string(),
                    snapshot: None,
                    current_frame_id: 0,
                    recent_inputs: vec![],
                    waiting_frame_id: 0,
                    waiting_inputs: vec![],
                    input_delay_frames: 0,
                    movement_recovery: None,
                },
            )?;
            services
                .db_store
                .append_room_event_with_identity(
                    &room_id,
                    Some(&player_id),
                    Some(&player_id),
                    Some(&character_id),
                    None,
                    "observer_join_failed",
                    None,
                    0,
                    Some(json!({
                        "errorCode": error_code,
                        "seq": packet.header.seq,
                        "accountPlayerId": player_id,
                        "characterId": character_id,
                        "worldId": world_id
                    })),
                )
                .await;
        }
    }

    Ok(())
}

pub async fn handle_create_matched_room(
    services: &ServiceContext,
    connection: &mut ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(identity) = connection.ensure_authenticated_identity(packet.header.seq)? else {
        return Ok(());
    };
    let player_id = identity.account_player_id;
    let character_id = identity.character_id;
    let world_id = identity.world_id;

    let request =
        match packet.decode_body::<CreateMatchedRoomReq>("INVALID_CREATE_MATCHED_ROOM_BODY") {
            Ok(value) => value,
            Err(error_code) => {
                connection.queue_error(
                    packet.header.seq,
                    error_code,
                    "invalid create matched room body",
                )?;
                return Ok(());
            }
        };

    let CreateMatchedRoomReq {
        match_id,
        room_id,
        player_ids,
        mode,
    } = request;

    info!(
        session_id = connection.session.id,
        account_player_id = %player_id,
        player_id = %player_id,
        character_id = %character_id,
        world_id = ?world_id,
        match_id = %match_id,
        room_id = %room_id,
        player_ids = ?player_ids,
        mode = %mode,
        "handle_create_matched_room"
    );

    // Verify the player is in the player_ids list
    if !player_ids.contains(&player_id) {
        connection.queue_message(
            MessageType::CreateMatchedRoomRes,
            packet.header.seq,
            CreateMatchedRoomRes {
                ok: false,
                room_id: room_id.clone(),
                error_code: "PLAYER_NOT_IN_MATCH".to_string(),
                snapshot: None,
            },
        )?;
        return Ok(());
    }

    let response = create_matched_room_impl(
        services,
        Some(&player_id),
        Some(&character_id),
        world_id,
        &match_id,
        &room_id,
        &player_ids,
        &mode,
        "client",
    )
    .await;

    if response.ok {
        connection.session.room_id = Some(room_id);
    }

    connection.queue_message(
        MessageType::CreateMatchedRoomRes,
        packet.header.seq,
        response,
    )?;

    Ok(())
}

pub async fn handle_create_matched_room_internal(
    services: &ServiceContext,
    request: CreateMatchedRoomReq,
) -> CreateMatchedRoomRes {
    let CreateMatchedRoomReq {
        match_id,
        room_id,
        player_ids,
        mode,
    } = request;

    info!(
        match_id = %match_id,
        room_id = %room_id,
        player_ids = ?player_ids,
        mode = %mode,
        "handle_create_matched_room_internal"
    );

    create_matched_room_impl(
        services,
        None,
        None,
        None,
        &match_id,
        &room_id,
        &player_ids,
        &mode,
        "internal",
    )
    .await
}

async fn create_matched_room_impl(
    services: &ServiceContext,
    actor_player_id: Option<&str>,
    actor_character_id: Option<&str>,
    actor_world_id: Option<u64>,
    match_id: &str,
    room_id: &str,
    player_ids: &[String],
    mode: &str,
    source: &str,
) -> CreateMatchedRoomRes {
    if player_ids.is_empty() {
        return CreateMatchedRoomRes {
            ok: false,
            room_id: room_id.to_string(),
            error_code: "EMPTY_PLAYER_IDS".to_string(),
            snapshot: None,
        };
    }

    let owner_player_id = player_ids.first().cloned().unwrap_or_default();

    if let DrainNewRoomDecision::RejectNewRoom(state) =
        evaluate_drain_new_room_creation(services, DrainRoomCreateKind::MatchedRoom, room_id).await
    {
        log_drain_mode_room_creation_rejected(
            "create_matched_room",
            actor_player_id,
            room_id,
            Some(match_id),
            &state,
        );
        return CreateMatchedRoomRes {
            ok: false,
            room_id: room_id.to_string(),
            error_code: DRAIN_MODE_REJECT_NEW_ROOM_ERROR.to_string(),
            snapshot: None,
        };
    }

    match services
        .room_manager
        .create_matched_room(match_id, room_id, player_ids, mode)
        .await
    {
        Ok(snapshot) => {
            services
                .db_store
                .append_room_event_with_identity(
                    room_id,
                    actor_player_id,
                    actor_player_id,
                    actor_character_id,
                    Some(&owner_player_id),
                    "matched_room_created",
                    Some(&snapshot.state),
                    snapshot.members.len(),
                    Some(json!({
                        "accountPlayerId": actor_player_id,
                        "characterId": actor_character_id,
                        "worldId": actor_world_id,
                        "matchId": match_id,
                        "mode": mode,
                        "playerCount": player_ids.len(),
                        "source": source
                    })),
                )
                .await;

            if let Err(error) = services
                .room_manager
                .broadcast_snapshot(room_id, "matched_room_created", snapshot.clone())
                .await
            {
                tracing::error!(
                    room_id = room_id,
                    match_id = match_id,
                    error = %error,
                    "failed to broadcast matched room snapshot"
                );
            }

            CreateMatchedRoomRes {
                ok: true,
                room_id: room_id.to_string(),
                error_code: String::new(),
                snapshot: Some(snapshot),
            }
        }
        Err(error_code) => {
            services
                .db_store
                .append_room_event_with_identity(
                    room_id,
                    actor_player_id,
                    actor_player_id,
                    actor_character_id,
                    None,
                    "matched_room_create_failed",
                    None,
                    0,
                    Some(json!({
                        "accountPlayerId": actor_player_id,
                        "characterId": actor_character_id,
                        "worldId": actor_world_id,
                        "errorCode": error_code,
                        "matchId": match_id,
                        "mode": mode,
                        "playerCount": player_ids.len(),
                        "source": source
                    })),
                )
                .await;

            CreateMatchedRoomRes {
                ok: false,
                room_id: room_id.to_string(),
                error_code: error_code.to_string(),
                snapshot: None,
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrainRoomCreateKind {
    DefaultRoom,
    MatchedRoom,
}

impl DrainRoomCreateKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::DefaultRoom => "default_room",
            Self::MatchedRoom => "matched_room",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DrainModeState {
    entered_at_ms: u64,
    reason: String,
    source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DrainNewRoomDecision {
    AllowDrainOff,
    AllowExistingRoom,
    RejectNewRoom(DrainModeState),
}

async fn evaluate_drain_new_room_creation(
    services: &ServiceContext,
    create_kind: DrainRoomCreateKind,
    room_id: &str,
) -> DrainNewRoomDecision {
    let state = {
        let runtime = services.runtime_config.read().await;
        drain_mode_state_from_runtime(&runtime)
    };
    let Some(state) = state else {
        return DrainNewRoomDecision::AllowDrainOff;
    };

    if services.room_manager.room_exists(room_id).await {
        return DrainNewRoomDecision::AllowExistingRoom;
    }

    info!(
        room_id = %room_id,
        create_kind = create_kind.as_str(),
        drain_mode_entered_at_ms = state.entered_at_ms,
        drain_mode_reason = %state.reason,
        drain_mode_source = %state.source,
        "new room classified for drain-mode rejection"
    );

    DrainNewRoomDecision::RejectNewRoom(state)
}

fn drain_mode_state_from_runtime(runtime: &RuntimeConfig) -> Option<DrainModeState> {
    if !runtime.drain_mode_enabled {
        return None;
    }

    Some(DrainModeState {
        entered_at_ms: runtime.drain_mode_entered_at_ms.unwrap_or_default(),
        reason: default_if_blank(&runtime.drain_mode_reason, DEFAULT_DRAIN_MODE_REASON),
        source: default_if_blank(&runtime.drain_mode_source, DEFAULT_DRAIN_MODE_SOURCE),
    })
}

fn default_if_blank(value: &str, default_value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        default_value.to_string()
    } else {
        trimmed.to_string()
    }
}

fn log_drain_mode_room_creation_rejected(
    request_kind: &'static str,
    player_id: Option<&str>,
    room_id: &str,
    match_id: Option<&str>,
    state: &DrainModeState,
) {
    info!(
        request_kind,
        player_id = %player_id.unwrap_or_default(),
        room_id = %room_id,
        match_id = %match_id.unwrap_or_default(),
        drain_mode_entered_at_ms = state.entered_at_ms,
        drain_mode_reason = %state.reason,
        drain_mode_source = %state.source,
        error_code = DRAIN_MODE_REJECT_NEW_ROOM_ERROR,
        "room creation rejected because server is in drain mode"
    );
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    use tokio::sync::{Mutex, Notify, RwLock};

    use super::*;
    use crate::config::{
        DEFAULT_ADMIN_TOKEN, DEFAULT_INTERNAL_TOKEN, DEFAULT_OUTBOUND_QUEUE_CAPACITY,
        DEFAULT_TICKET_SECRET,
    };
    use crate::core::config_table::ConfigTableRuntime;
    use crate::core::context::PlayerRegistry;
    use crate::core::logic::{RoomLogic, RoomLogicFactory, RoomLogicTransfer};
    use crate::core::player::{PgPlayerStore, PlayerManager};
    use crate::core::runtime::RoomManager;
    use crate::db_store::PgAuditStore;

    struct NoopRoomLogic;

    impl RoomLogic for NoopRoomLogic {}

    impl RoomLogicTransfer for NoopRoomLogic {}

    struct NoopRoomLogicFactory;

    impl RoomLogicFactory for NoopRoomLogicFactory {
        fn create(&self, _policy_id: &str) -> Box<dyn RoomLogic> {
            Box::new(NoopRoomLogic)
        }
    }

    fn runtime_config(
        input_timestamp_required: bool,
        input_timestamp_max_skew_ms: u64,
    ) -> RuntimeConfig {
        RuntimeConfig {
            heartbeat_timeout_secs: 30,
            max_body_len: 4096,
            msg_rate_window_ms: 1000,
            msg_rate_max: 0,
            player_msg_rate_window_ms: 1000,
            player_msg_rate_max: 0,
            input_timestamp_required,
            input_timestamp_max_skew_ms,
            input_anomaly_window_ms: 10_000,
            input_anomaly_max: 0,
            drain_mode_enabled: false,
            drain_mode_entered_at_ms: None,
            drain_mode_reason: DEFAULT_DRAIN_MODE_REASON.to_string(),
            drain_mode_source: DEFAULT_DRAIN_MODE_SOURCE.to_string(),
        }
    }

    fn test_config() -> crate::config::Config {
        crate::config::Config {
            host: "127.0.0.1".to_string(),
            public_host: "127.0.0.1".to_string(),
            port: 7000,
            csv_dir: "csv".to_string(),
            csv_reload_enabled: false,
            csv_reload_interval_secs: 3,
            room_cleanup_interval_secs: 10,
            admin_host: "127.0.0.1".to_string(),
            admin_advertised_host: "127.0.0.1".to_string(),
            admin_port: 7500,
            admin_token: DEFAULT_ADMIN_TOKEN.to_string(),
            admin_audit_enabled: false,
            admin_audit_path: "logs/game-server/admin-audit.jsonl".to_string(),
            admin_audit_require_actor: false,
            internal_token: DEFAULT_INTERNAL_TOKEN.to_string(),
            local_socket_name: "test-game-server.sock".to_string(),
            internal_socket_name: "test-game-server-internal.sock".to_string(),
            log_level: "info".to_string(),
            log_enable_console: false,
            log_enable_file: false,
            log_dir: "logs/game-server".to_string(),
            redis_url: "redis://127.0.0.1:6379".to_string(),
            redis_key_prefix: String::new(),
            global_id_origin_id: 0,
            global_id_worker_id: Some(1),
            nats_url: "nats://127.0.0.1:4222".to_string(),
            db_enabled: false,
            database_url: "postgres://postgres:password@127.0.0.1:5432/myserver_game".to_string(),
            db_pool_size: 1,
            ticket_secret: DEFAULT_TICKET_SECRET.to_string(),
            heartbeat_timeout_secs: 30,
            max_body_len: 4096,
            outbound_queue_capacity: DEFAULT_OUTBOUND_QUEUE_CAPACITY,
            msg_rate_window_ms: 1000,
            msg_rate_max: 0,
            player_msg_rate_window_ms: 1000,
            player_msg_rate_max: 0,
            input_timestamp_required: false,
            input_timestamp_max_skew_ms: 5000,
            input_anomaly_window_ms: 10_000,
            input_anomaly_max: 0,
            registry_enabled: false,
            discovery_required: false,
            registry_url: "redis://127.0.0.1:6379".to_string(),
            registry_key_prefix: String::new(),
            registry_heartbeat_interval_secs: 10,
            service_name: "game-server".to_string(),
            service_instance_id: "game-server-test".to_string(),
            service_build_version: "dev".to_string(),
            service_zone: "local".to_string(),
            service_rollout_epoch: "default".to_string(),
            legacy_direct_config_warnings: Vec::new(),
        }
    }

    async fn service_context_fixture(drain_enabled: bool) -> ServiceContext {
        let config = test_config();
        let config_tables = ConfigTableRuntime::load(std::path::Path::new(&config.csv_dir))
            .expect("test config tables should load");
        let room_manager = Arc::new(RoomManager::with_policy_registry_and_cleanup_interval(
            crate::match_client::create_match_client_shared(),
            Arc::new(NoopRoomLogicFactory),
            config_tables.room_policy_registry(),
            3600,
        ));
        let mut runtime = runtime_config(false, 5000);
        runtime.drain_mode_enabled = drain_enabled;
        runtime.drain_mode_entered_at_ms = drain_enabled.then_some(1234);
        runtime.drain_mode_reason = "rollout-test".to_string();
        runtime.drain_mode_source = "unit-test".to_string();

        ServiceContext {
            config,
            db_store: PgAuditStore::new(&test_config())
                .await
                .expect("disabled PostgreSQL audit store"),
            room_manager,
            runtime_config: Arc::new(RwLock::new(runtime)),
            connection_count: Arc::new(AtomicU64::new(0)),
            config_tables,
            item_uid_generator: crate::core::global_id::ItemUidGenerator::new_for_test(1),
            player_manager: PlayerManager::new(PgPlayerStore::new_disabled()),
            online_player_count: Arc::new(AtomicU64::new(0)),
            player_registry: PlayerRegistry::default(),
            player_msg_rate_limiter: Arc::new(Mutex::new(
                crate::server::PlayerMessageRateLimiter::new(),
            )),
            player_input_anomaly_tracker: Arc::new(Mutex::new(
                crate::server::PlayerInputAnomalyTracker::new(),
            )),
            shutdown_signal: Arc::new(Notify::new()),
        }
    }

    #[tokio::test]
    async fn drain_new_room_policy_allows_when_drain_off() {
        let services = service_context_fixture(false).await;

        let decision = evaluate_drain_new_room_creation(
            &services,
            DrainRoomCreateKind::DefaultRoom,
            "room-new",
        )
        .await;

        assert_eq!(decision, DrainNewRoomDecision::AllowDrainOff);
    }

    #[tokio::test]
    async fn drain_new_room_policy_rejects_missing_default_room_when_drain_on() {
        let services = service_context_fixture(true).await;

        let decision = evaluate_drain_new_room_creation(
            &services,
            DrainRoomCreateKind::DefaultRoom,
            "room-default",
        )
        .await;

        assert_eq!(
            decision,
            DrainNewRoomDecision::RejectNewRoom(DrainModeState {
                entered_at_ms: 1234,
                reason: "rollout-test".to_string(),
                source: "unit-test".to_string(),
            })
        );
    }

    #[tokio::test]
    async fn drain_new_room_policy_allows_existing_room_when_drain_on() {
        let services = service_context_fixture(true).await;
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        services
            .room_manager
            .join_room(
                "room-existing",
                "player-a",
                tx,
                MemberRole::Player,
                Some("default_match"),
            )
            .await
            .unwrap();

        let decision = evaluate_drain_new_room_creation(
            &services,
            DrainRoomCreateKind::DefaultRoom,
            "room-existing",
        )
        .await;

        assert_eq!(decision, DrainNewRoomDecision::AllowExistingRoom);
    }

    #[tokio::test]
    async fn drain_new_room_policy_rejects_missing_matched_room_when_drain_on() {
        let services = service_context_fixture(true).await;

        let decision = evaluate_drain_new_room_creation(
            &services,
            DrainRoomCreateKind::MatchedRoom,
            "room-match-new",
        )
        .await;

        assert!(matches!(decision, DrainNewRoomDecision::RejectNewRoom(_)));
    }

    #[tokio::test]
    async fn create_matched_room_impl_rejects_internal_create_during_drain() {
        let services = service_context_fixture(true).await;
        let response = create_matched_room_impl(
            &services,
            None,
            None,
            None,
            "match-1",
            "room-match-new",
            &["player-a".to_string(), "player-b".to_string()],
            "1v1",
            "internal",
        )
        .await;

        assert!(!response.ok);
        assert_eq!(response.error_code, DRAIN_MODE_REJECT_NEW_ROOM_ERROR);
        assert!(!services.room_manager.room_exists("room-match-new").await);
    }

    #[tokio::test]
    async fn drain_mode_room_end_does_not_create_or_return_to_default_room() {
        let services = service_context_fixture(true).await;
        let room_id = "room-drain-active";

        for player_id in ["player-a", "player-b"] {
            let (tx, _rx) = tokio::sync::mpsc::channel(8);
            services
                .room_manager
                .join_room(
                    room_id,
                    player_id,
                    tx,
                    MemberRole::Player,
                    Some("default_match"),
                )
                .await
                .unwrap();
            services
                .room_manager
                .set_ready_state(room_id, player_id, true)
                .await
                .unwrap();
        }

        services
            .room_manager
            .start_game(room_id, "player-a")
            .await
            .unwrap();

        let ended = services
            .room_manager
            .end_game(room_id, "player-a")
            .await
            .unwrap();

        assert_eq!(ended.room_id, room_id);
        assert_eq!(ended.state, "waiting");
        assert!(services.room_manager.room_exists(room_id).await);
        assert!(!services.room_manager.room_exists("room-default").await);
        assert_eq!(services.room_manager.room_count().await, 1);

        let default_room_decision = evaluate_drain_new_room_creation(
            &services,
            DrainRoomCreateKind::DefaultRoom,
            "room-default",
        )
        .await;
        assert!(matches!(
            default_room_decision,
            DrainNewRoomDecision::RejectNewRoom(_)
        ));
    }

    #[test]
    fn missing_input_timestamp_passes_when_not_required() {
        let runtime = runtime_config(false, 5000);

        assert_eq!(validate_input_timestamp(&runtime, 0), Ok(()));
    }

    #[test]
    fn missing_input_timestamp_rejects_when_required() {
        let runtime = runtime_config(true, 5000);

        assert_eq!(
            validate_input_timestamp(&runtime, 0),
            Err("INPUT_TIMESTAMP_REQUIRED")
        );
    }

    #[test]
    fn input_timestamp_outside_skew_window_rejects() {
        let runtime = runtime_config(false, 5000);
        let stale_timestamp = current_unix_ms() - 6000;

        assert_eq!(
            validate_input_timestamp(&runtime, stale_timestamp),
            Err("INPUT_TIMESTAMP_SKEW")
        );
    }

    #[test]
    fn input_timestamp_skew_zero_disables_window_check() {
        let runtime = runtime_config(true, 0);
        let stale_timestamp = current_unix_ms() - 600_000;

        assert_eq!(validate_input_timestamp(&runtime, stale_timestamp), Ok(()));
    }

    #[test]
    fn reconnect_account_resolution_defaults_to_authenticated_account() {
        assert_eq!(
            resolve_reconnect_account_player_id("plr_0000000000001", "").unwrap(),
            "plr_0000000000001"
        );
    }

    #[test]
    fn reconnect_account_resolution_accepts_matching_account_only() {
        assert_eq!(
            resolve_reconnect_account_player_id("plr_0000000000001", "plr_0000000000001").unwrap(),
            "plr_0000000000001"
        );
        assert_eq!(
            resolve_reconnect_account_player_id("plr_0000000000001", "plr_0000000000002")
                .unwrap_err(),
            "ACCOUNT_PLAYER_ID_MISMATCH"
        );
    }
}
