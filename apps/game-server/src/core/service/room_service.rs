use serde_json::json;
use tracing::info;

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

pub async fn handle_room_join(
    services: &ServiceContext,
    connection: &mut ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(player_id) = connection.ensure_authenticated(packet.header.seq)? else {
        return Ok(());
    };

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
        player_id = %player_id,
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

    let join_result = services
        .room_manager
        .join_room(
            &room_id,
            &player_id,
            connection.tx.clone(),
            MemberRole::Player,
            requested_policy_id,
        )
        .await;

    match join_result {
        Ok(snapshot) => {
            connection.session.room_id = Some(room_id.clone());
            connection.queue_message(
                MessageType::RoomJoinRes,
                packet.header.seq,
                RoomJoinRes {
                    ok: true,
                    room_id: room_id.clone(),
                    error_code: String::new(),
                },
            )?;
            services
                .mysql_store
                .append_room_event(
                    &room_id,
                    Some(&player_id),
                    Some(&snapshot.owner_player_id),
                    "room_joined",
                    Some(&snapshot.state),
                    snapshot.members.len(),
                    Some(json!({
                        "seq": packet.header.seq,
                        "members": snapshot.members.iter().map(|member| json!({
                            "playerId": member.player_id,
                            "ready": member.ready,
                            "isOwner": member.is_owner
                        })).collect::<Vec<_>>()
                    })),
                )
                .await;
            services
                .room_manager
                .broadcast_snapshot(&room_id, "member_joined", snapshot)
                .await?;
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
                .mysql_store
                .append_room_event(
                    &room_id,
                    Some(&player_id),
                    None,
                    "room_join_failed",
                    None,
                    0,
                    Some(json!({ "errorCode": error_code, "seq": packet.header.seq })),
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

    let Some(player_id) = connection.session.player_id.clone() else {
        connection.queue_error(
            packet.header.seq,
            "NOT_AUTHENTICATED",
            "authenticate before leaving a room",
        )?;
        return Ok(());
    };

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
            .mysql_store
            .append_room_event(
                &room_id,
                Some(&player_id),
                Some(&snapshot.owner_player_id),
                "room_left",
                Some(&snapshot.state),
                snapshot.members.len(),
                None,
            )
            .await;
        services
            .room_manager
            .broadcast_snapshot(&room_id, "member_left", snapshot)
            .await?;
    } else if leave_result.room_removed {
        services
            .mysql_store
            .append_room_event(&room_id, Some(&player_id), None, "room_disbanded", None, 0, None)
            .await;
    }

    Ok(())
}

pub async fn handle_room_ready(
    services: &ServiceContext,
    connection: &ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(player_id) = connection.ensure_authenticated(packet.header.seq)? else {
        return Ok(());
    };
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
                .mysql_store
                .append_room_event(
                    &room_id,
                    Some(&player_id),
                    Some(&snapshot.owner_player_id),
                    "room_ready_changed",
                    Some(&snapshot.state),
                    snapshot.members.len(),
                    Some(json!({ "ready": request.ready, "seq": packet.header.seq })),
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
    let Some(player_id) = connection.ensure_authenticated(packet.header.seq)? else {
        return Ok(());
    };
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
                .mysql_store
                .append_room_event(
                    &room_id,
                    Some(&player_id),
                    Some(&snapshot.owner_player_id),
                    "game_started",
                    Some(&snapshot.state),
                    snapshot.members.len(),
                    Some(json!({ "seq": packet.header.seq })),
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
    let Some(player_id) = connection.ensure_authenticated(packet.header.seq)? else {
        return Ok(());
    };
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
                .mysql_store
                .append_room_event(
                    &room_id,
                    Some(&player_id),
                    None,
                    "player_input",
                    Some("in_game"),
                    0,
                    Some(json!({
                        "seq": packet.header.seq,
                        "action": request.action,
                        "payloadJson": request.payload_json
                    })),
                )
                .await;
        }
        Err(error_code) => {
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
    let Some(player_id) = connection.ensure_authenticated(packet.header.seq)? else {
        return Ok(());
    };
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

    let input_result = services
        .room_manager
        .accept_player_input(&room_id, &player_id, request.frame_id, action, &payload_json)
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
                .mysql_store
                .append_room_event(
                    &room_id,
                    Some(&player_id),
                    None,
                    "move_input",
                    Some("in_game"),
                    0,
                    Some(json!({
                        "seq": packet.header.seq,
                        "frameId": request.frame_id,
                        "inputType": request.input_type,
                        "dirX": request.dir_x,
                        "dirY": request.dir_y
                    })),
                )
                .await;
        }
        Err(error_code) => {
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

pub async fn handle_room_end(
    services: &ServiceContext,
    connection: &ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(player_id) = connection.ensure_authenticated(packet.header.seq)? else {
        return Ok(());
    };
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
                .mysql_store
                .append_room_event(
                    &room_id,
                    Some(&player_id),
                    Some(&snapshot.owner_player_id),
                    "game_ended",
                    Some(&snapshot.state),
                    snapshot.members.len(),
                    Some(json!({
                        "seq": packet.header.seq,
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

pub async fn handle_disconnect_cleanup(
    services: &ServiceContext,
    connection: &ConnectionContext,
) {
    let session = &connection.session;
    let room_id = session.room_id.clone();
    let player_id = session.player_id.clone();

    info!(
        session_id = session.id,
        room_id = ?room_id,
        player_id = ?player_id,
        "handle_disconnect_cleanup called"
    );

    if let (Some(room_id), Some(player_id)) = (room_id, player_id) {
        let leave_result = services
            .room_manager
            .disconnect_room_member(&room_id, &player_id)
            .await;

        if let Some(snapshot) = leave_result.snapshot {
            services
                .mysql_store
                .append_room_event(
                    &room_id,
                    Some(&player_id),
                    Some(&snapshot.owner_player_id),
                    "member_disconnected",
                    Some(&snapshot.state),
                    snapshot.members.len(),
                    None,
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
    let Some(player_id) = connection.ensure_authenticated(packet.header.seq)? else {
        return Ok(());
    };

    let request = match packet.decode_body::<RoomReconnectReq>("INVALID_ROOM_RECONNECT_BODY") {
        Ok(value) => value,
        Err(error_code) => {
            connection.queue_error(packet.header.seq, error_code, "invalid room reconnect body")?;
            return Ok(());
        }
    };

    // Use the player_id from reconnect request
    let reconnect_player_id = if request.player_id.is_empty() {
        player_id.clone()
    } else {
        request.player_id
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
            },
        )?;
        return Ok(());
    }

    // Find the room the player is offline in (via MySQL audit log or cache)
    // For now, client should provide room_id - we'll search for the player
    // This is a simplified implementation - in production you'd track this in Redis
    let room_id = services
        .mysql_store
        .find_room_by_offline_player(&reconnect_player_id)
        .await;

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
            },
        )?;
        return Ok(());
    };

    let reconnect_result = services
        .room_manager
        .reconnect_room(&room_id, &reconnect_player_id, connection.tx.clone())
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
                },
            )?;
            services
                .mysql_store
                .append_room_event(
                    &room_id,
                    Some(&reconnect_player_id),
                    Some(&snapshot.owner_player_id),
                    "player_reconnected",
                    Some(&snapshot.state),
                    snapshot.members.len(),
                    None,
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
                },
            )?;
        }
    }

    Ok(())
}

pub async fn handle_join_as_observer(
    services: &ServiceContext,
    connection: &mut ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(player_id) = connection.ensure_authenticated(packet.header.seq)? else {
        return Ok(());
    };

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
        player_id = %player_id,
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
            },
        )?;
        return Ok(());
    }

    let join_result = services
        .room_manager
        .join_room_as_observer(&room_id, &player_id, connection.tx.clone())
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
                },
            )?;
            services
                .mysql_store
                .append_room_event(
                    &room_id,
                    Some(&player_id),
                    Some(&snapshot.owner_player_id),
                    "observer_joined",
                    Some(&snapshot.state),
                    snapshot.members.len(),
                    Some(json!({
                        "seq": packet.header.seq,
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
                },
            )?;
            services
                .mysql_store
                .append_room_event(
                    &room_id,
                    Some(&player_id),
                    None,
                    "observer_join_failed",
                    None,
                    0,
                    Some(json!({ "errorCode": error_code, "seq": packet.header.seq })),
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
    let Some(player_id) = connection.ensure_authenticated(packet.header.seq)? else {
        return Ok(());
    };

    let request = match packet.decode_body::<CreateMatchedRoomReq>("INVALID_CREATE_MATCHED_ROOM_BODY") {
        Ok(value) => value,
        Err(error_code) => {
            connection.queue_error(packet.header.seq, error_code, "invalid create matched room body")?;
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
        player_id = %player_id,
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

    match services
        .room_manager
        .create_matched_room(match_id, room_id, player_ids, mode)
        .await
    {
        Ok(snapshot) => {
            services
                .mysql_store
                .append_room_event(
                    room_id,
                    actor_player_id,
                    Some(&owner_player_id),
                    "matched_room_created",
                    Some(&snapshot.state),
                    snapshot.members.len(),
                    Some(json!({
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
                .mysql_store
                .append_room_event(
                    room_id,
                    actor_player_id,
                    None,
                    "matched_room_create_failed",
                    None,
                    0,
                    Some(json!({
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
