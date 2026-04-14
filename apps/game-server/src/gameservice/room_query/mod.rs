use crate::core::context::{ConnectionContext, ServiceContext};
use crate::pb::{GetRoomDataReq, GetRoomDataRes};
use crate::protocol::{MessageType, Packet};

pub async fn handle_get_room_data(
    services: &ServiceContext,
    connection: &ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(_player_id) = connection.ensure_authenticated(packet.header.seq)? else {
        return Ok(());
    };

    let request = match packet.decode_body::<GetRoomDataReq>("INVALID_GET_ROOM_DATA_BODY") {
        Ok(value) => value,
        Err(error_code) => {
            connection.queue_error(packet.header.seq, error_code, "invalid get room data body")?;
            return Ok(());
        }
    };

    if request.id_start > request.id_end {
        connection.queue_message(
            MessageType::GetRoomDataRes,
            packet.header.seq,
            GetRoomDataRes {
                ok: false,
                field_0_list: Vec::new(),
                error_code: "INVALID_ID_RANGE".to_string(),
            },
        )?;
        return Ok(());
    }

    let tables = services.config_tables.snapshot().await;
    let table = &tables.testtable_100;
    let mut field_0_list = Vec::new();

    for id in request.id_start..=request.id_end {
        if let Some(row) = table.get(id) {
            for key in &row.field_0 {
                field_0_list.push(table.resolve_string(*key).unwrap_or_default().to_string());
            }
        }
    }

    if field_0_list.is_empty() {
        connection.queue_message(
            MessageType::GetRoomDataRes,
            packet.header.seq,
            GetRoomDataRes {
                ok: false,
                field_0_list,
                error_code: "CONFIG_NOT_FOUND".to_string(),
            },
        )?;
    } else {
        connection.queue_message(
            MessageType::GetRoomDataRes,
            packet.header.seq,
            GetRoomDataRes {
                ok: true,
                field_0_list,
                error_code: String::new(),
            },
        )?;
    }

    Ok(())
}
