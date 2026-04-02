use serde_json::json;

use crate::core::context::{ConnectionContext, ServiceContext};
use crate::pb::{
    PlayerInputReq, PlayerInputRes, RoomEndReq, RoomEndRes, RoomJoinReq, RoomJoinRes,
    RoomLeaveRes, RoomReadyReq, RoomReadyRes, RoomStartRes,
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
        .join_room(&room_id, &player_id, connection.tx.clone())
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
    if let (Some(room_id), Some(player_id)) = (
        connection.session.room_id.clone(),
        connection.session.player_id.clone(),
    ) {
        let leave_result = services.room_manager.leave_room(&room_id, &player_id).await;

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
