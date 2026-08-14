use crate::core::context::{ConnectionContext, ServiceContext};
use crate::core::room::{OutboundMessage, OutboundQueueLogContext};
use crate::pb::{
    MatchCancelReq, MatchCancelRes, MatchEventPush, MatchEventStreamReq, MatchEventStreamRes,
    MatchStartReq, MatchStartRes, MatchStatusReq, MatchStatusRes,
};
use crate::protocol::{encode_body, MessageType, Packet};

fn identity(connection: &ConnectionContext, seq: u32) -> Result<String, std::io::Error> {
    connection
        .ensure_authenticated_identity(seq)?
        .map(|value| value.character_id().to_string())
        .ok_or_else(|| std::io::Error::other("AUTH_CONTEXT_INCOMPLETE"))
}

pub async fn handle_match_start(
    services: &ServiceContext,
    connection: &mut ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let request = packet.decode_body::<MatchStartReq>("INVALID_MATCH_START_BODY")?;
    let character_id = identity(connection, packet.header.seq)?;
    let client = services.match_client.lock().await.as_ref().cloned();
    let response = match client {
        Some(client) => client
            .match_start(&character_id, &request.mode, request.rank_tier)
            .await
            .map(|value| MatchStartRes {
                ok: value.ok,
                match_id: value.match_id,
                error_code: value.error_code,
            })
            .unwrap_or_else(|_| MatchStartRes {
                ok: false,
                match_id: String::new(),
                error_code: "MATCH_SERVICE_UNAVAILABLE".to_string(),
            }),
        None => MatchStartRes {
            ok: false,
            match_id: String::new(),
            error_code: "MATCH_SERVICE_UNAVAILABLE".to_string(),
        },
    };
    connection.queue_message(MessageType::MatchStartRes, packet.header.seq, response)?;
    Ok(())
}

pub async fn handle_match_cancel(
    services: &ServiceContext,
    connection: &mut ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let request = packet.decode_body::<MatchCancelReq>("INVALID_MATCH_CANCEL_BODY")?;
    let character_id = identity(connection, packet.header.seq)?;
    let client = services.match_client.lock().await.as_ref().cloned();
    let response = match client {
        Some(client) => client
            .match_cancel(&character_id, &request.match_id)
            .await
            .map(|value| MatchCancelRes {
                ok: value.ok,
                error_code: value.error_code,
            })
            .unwrap_or_else(|_| MatchCancelRes {
                ok: false,
                error_code: "MATCH_SERVICE_UNAVAILABLE".to_string(),
            }),
        None => MatchCancelRes {
            ok: false,
            error_code: "MATCH_SERVICE_UNAVAILABLE".to_string(),
        },
    };
    connection.queue_message(MessageType::MatchCancelRes, packet.header.seq, response)?;
    Ok(())
}

pub async fn handle_match_status(
    services: &ServiceContext,
    connection: &mut ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let _ = packet.decode_body::<MatchStatusReq>("INVALID_MATCH_STATUS_BODY")?;
    let character_id = identity(connection, packet.header.seq)?;
    let client = services.match_client.lock().await.as_ref().cloned();
    let response = match client {
        Some(client) => client
            .match_status(&character_id)
            .await
            .map(|value| MatchStatusRes {
                ok: true,
                status: value.status,
                match_id: value.match_id,
                room_id: value.room_id,
                token: value.token,
                estimated_wait_secs: value.estimated_wait_secs,
                error_code: String::new(),
            })
            .unwrap_or_else(|_| MatchStatusRes {
                ok: false,
                status: "unknown".to_string(),
                match_id: String::new(),
                room_id: String::new(),
                token: String::new(),
                estimated_wait_secs: 0,
                error_code: "MATCH_SERVICE_UNAVAILABLE".to_string(),
            }),
        None => MatchStatusRes {
            ok: false,
            status: "unknown".to_string(),
            match_id: String::new(),
            room_id: String::new(),
            token: String::new(),
            estimated_wait_secs: 0,
            error_code: "MATCH_SERVICE_UNAVAILABLE".to_string(),
        },
    };
    connection.queue_message(MessageType::MatchStatusRes, packet.header.seq, response)?;
    Ok(())
}

pub async fn handle_match_event_stream(
    services: &ServiceContext,
    connection: &mut ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let _ = packet.decode_body::<MatchEventStreamReq>("INVALID_MATCH_EVENT_STREAM_BODY")?;
    let character_id = identity(connection, packet.header.seq)?;
    connection.queue_message(
        MessageType::MatchEventStreamRes,
        packet.header.seq,
        MatchEventStreamRes {
            ok: true,
            error_code: String::new(),
        },
    )?;

    let shared_client = services.match_client.clone();
    let outbound = connection.outbound_channel();
    tokio::spawn(async move {
        let client = shared_client.lock().await.as_ref().cloned();
        let result = match client {
            Some(client) => client.match_event_stream(&character_id).await,
            None => Err("MATCH_SERVICE_UNAVAILABLE".into()),
        };
        let Ok(mut stream) = result else {
            let push = MatchEventPush {
                event: "match_stream_error".to_string(),
                match_id: String::new(),
                room_id: String::new(),
                token: String::new(),
                error_code: "MATCH_SERVICE_UNAVAILABLE".to_string(),
            };
            let _ = outbound.try_send(
                OutboundMessage {
                    message_type: MessageType::MatchEventPush,
                    seq: 0,
                    body: encode_body(&push),
                },
                OutboundQueueLogContext::default(),
            );
            return;
        };
        loop {
            match stream.message().await {
                Ok(Some(event)) => {
                    let push = MatchEventPush {
                        event: event.event,
                        match_id: event.match_id,
                        room_id: event.room_id,
                        token: event.token,
                        error_code: event.error_code,
                    };
                    if outbound
                        .try_send(
                            OutboundMessage {
                                message_type: MessageType::MatchEventPush,
                                seq: 0,
                                body: encode_body(&push),
                            },
                            OutboundQueueLogContext::default(),
                        )
                        .is_err()
                    {
                        break;
                    }
                }
                Ok(None) | Err(_) => break,
            }
            if outbound.is_closed() {
                break;
            }
        }
    });
    Ok(())
}
