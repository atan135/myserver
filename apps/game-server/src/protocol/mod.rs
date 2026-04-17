mod message_type;

use prost::Message;

pub use message_type::MessageType;

pub const HEADER_LEN: usize = 14;
pub const MAGIC: u16 = 0xCAFE;
pub const VERSION: u8 = 1;

#[derive(Debug, Clone, Copy)]
pub struct PacketHeader {
    pub msg_type: u16,
    pub seq: u32,
    pub body_len: u32,
}

#[derive(Debug, Clone)]
pub struct Packet {
    pub header: PacketHeader,
    pub body: Vec<u8>,
}

impl Packet {
    pub fn new(header: PacketHeader, body: Vec<u8>) -> Self {
        Self { header, body }
    }

    pub fn message_type(&self) -> Option<MessageType> {
        MessageType::from_u16(self.header.msg_type)
    }

    pub fn decode_body<M>(&self, error_code: &'static str) -> Result<M, &'static str>
    where
        M: Message + Default,
    {
        M::decode(self.body.as_slice()).map_err(|_| error_code)
    }
}

pub fn parse_header(bytes: [u8; HEADER_LEN]) -> Result<PacketHeader, &'static str> {
    let magic = u16::from_be_bytes([bytes[0], bytes[1]]);
    if magic != MAGIC {
        return Err("INVALID_MAGIC");
    }

    let version = bytes[2];
    if version != VERSION {
        return Err("INVALID_VERSION");
    }

    let flags = bytes[3];
    if flags != 0 {
        return Err("UNSUPPORTED_FLAGS");
    }

    Ok(PacketHeader {
        msg_type: u16::from_be_bytes([bytes[4], bytes[5]]),
        seq: u32::from_be_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]),
        body_len: u32::from_be_bytes([bytes[10], bytes[11], bytes[12], bytes[13]]),
    })
}

pub fn encode_body<M: Message>(message: &M) -> Vec<u8> {
    let mut body = Vec::new();
    message.encode(&mut body).expect("protobuf encode failed");
    body
}

pub fn encode_packet(msg_type: MessageType, seq: u32, body: &[u8]) -> Vec<u8> {
    let mut packet = Vec::with_capacity(HEADER_LEN + body.len());

    packet.extend_from_slice(&MAGIC.to_be_bytes());
    packet.push(VERSION);
    packet.push(0);
    packet.extend_from_slice(&(msg_type as u16).to_be_bytes());
    packet.extend_from_slice(&seq.to_be_bytes());
    packet.extend_from_slice(&(body.len() as u32).to_be_bytes());
    packet.extend_from_slice(body);

    packet
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin_pb::{ServerStatusReq, ServerStatusRes};
    use crate::pb::{
        AuthReq, FrameBundlePush, FrameInput, GetRoomDataReq, GetRoomDataRes, RoomMember,
        RoomSnapshot, RoomStatePush,
    };

    #[test]
    fn auth_req_round_trip_through_packet() {
        let message = AuthReq {
            ticket: "ticket-123".to_string(),
        };
        let body = encode_body(&message);
        let packet_bytes = encode_packet(MessageType::AuthReq, 42, &body);

        let header = parse_header(packet_bytes[..HEADER_LEN].try_into().unwrap()).unwrap();
        assert_eq!(header.msg_type, MessageType::AuthReq as u16);
        assert_eq!(header.seq, 42);
        assert_eq!(header.body_len as usize, body.len());

        let packet = Packet::new(header, packet_bytes[HEADER_LEN..].to_vec());
        assert!(matches!(packet.message_type(), Some(MessageType::AuthReq)));

        let decoded = packet.decode_body::<AuthReq>("INVALID_AUTH_BODY").unwrap();
        assert_eq!(decoded, message);
    }

    #[test]
    fn nested_room_state_push_round_trip() {
        let message = RoomStatePush {
            event: "member_joined".to_string(),
            snapshot: Some(RoomSnapshot {
                room_id: "room-1".to_string(),
                owner_player_id: "player-a".to_string(),
                state: "waiting".to_string(),
                members: vec![
                    RoomMember {
                        player_id: "player-a".to_string(),
                        ready: true,
                        is_owner: true,
                        offline: false,
                        role: 0,
                    },
                    RoomMember {
                        player_id: "player-b".to_string(),
                        ready: false,
                        is_owner: false,
                        offline: false,
                        role: 0,
                    },
                ],
                current_frame_id: 0,
                game_state: String::new(),
            }),
        };

        let body = encode_body(&message);
        let packet = Packet::new(
            PacketHeader {
                msg_type: MessageType::RoomStatePush as u16,
                seq: 7,
                body_len: body.len() as u32,
            },
            body,
        );

        let decoded = packet
            .decode_body::<RoomStatePush>("INVALID_ROOM_STATE_PUSH_BODY")
            .unwrap();
        assert_eq!(decoded, message);
    }

    #[test]
    fn frame_bundle_push_round_trip() {
        let message = FrameBundlePush {
            room_id: "room-1".to_string(),
            frame_id: 12,
            fps: 10,
            inputs: vec![FrameInput {
                player_id: "player-a".to_string(),
                action: "move".to_string(),
                payload_json: "{\"x\":1}".to_string(),
                frame_id: 12,
            }],
            is_silent_frame: false,
            snapshot: None,
        };

        let body = encode_body(&message);
        let packet = Packet::new(
            PacketHeader {
                msg_type: MessageType::FrameBundlePush as u16,
                seq: 0,
                body_len: body.len() as u32,
            },
            body,
        );

        let decoded = packet
            .decode_body::<FrameBundlePush>("INVALID_FRAME_BUNDLE_PUSH_BODY")
            .unwrap();
        assert_eq!(decoded, message);
    }

    #[test]
    fn get_room_data_round_trip() {
        let request = GetRoomDataReq {
            id_start: 1000,
            id_end: 1002,
        };
        let request_body = encode_body(&request);
        let request_packet = encode_packet(MessageType::GetRoomDataReq, 11, &request_body);
        let request_header = parse_header(request_packet[..HEADER_LEN].try_into().unwrap()).unwrap();
        let request_packet = Packet::new(request_header, request_packet[HEADER_LEN..].to_vec());
        let decoded = request_packet
            .decode_body::<GetRoomDataReq>("INVALID_GET_ROOM_DATA_BODY")
            .unwrap();
        assert_eq!(decoded, request);

        let response = GetRoomDataRes {
            ok: true,
            field_0_list: vec!["alpha".to_string(), "beta".to_string()],
            error_code: String::new(),
        };
        let response_body = encode_body(&response);
        let response_packet = encode_packet(MessageType::GetRoomDataRes, 11, &response_body);
        let response_header = parse_header(response_packet[..HEADER_LEN].try_into().unwrap()).unwrap();
        let response_packet = Packet::new(response_header, response_packet[HEADER_LEN..].to_vec());
        let decoded = response_packet
            .decode_body::<GetRoomDataRes>("INVALID_GET_ROOM_DATA_RES_BODY")
            .unwrap();
        assert_eq!(decoded, response);
    }

    #[test]
    fn admin_server_status_round_trip() {
        let request = ServerStatusReq {};
        let request_body = encode_body(&request);
        let request_packet = encode_packet(MessageType::AdminServerStatusReq, 9, &request_body);
        let request_header = parse_header(request_packet[..HEADER_LEN].try_into().unwrap()).unwrap();
        let packet = Packet::new(request_header, request_packet[HEADER_LEN..].to_vec());
        let decoded = packet
            .decode_body::<ServerStatusReq>("INVALID_ADMIN_STATUS_BODY")
            .unwrap();
        assert_eq!(decoded, request);

        let response = ServerStatusRes {
            connection_count: 3,
            room_count: 1,
            status: "ok".to_string(),
            max_body_len: 4096,
            heartbeat_timeout_secs: 30,
        };
        let response_body = encode_body(&response);
        let response_packet = encode_packet(MessageType::AdminServerStatusRes, 9, &response_body);
        let response_header = parse_header(response_packet[..HEADER_LEN].try_into().unwrap()).unwrap();
        let response_packet = Packet::new(response_header, response_packet[HEADER_LEN..].to_vec());
        let decoded = response_packet
            .decode_body::<ServerStatusRes>("INVALID_ADMIN_STATUS_RES_BODY")
            .unwrap();
        assert_eq!(decoded, response);
    }

    #[test]
    fn invalid_protobuf_body_returns_expected_error_code() {
        let packet = Packet::new(
            PacketHeader {
                msg_type: MessageType::AuthReq as u16,
                seq: 1,
                body_len: 3,
            },
            vec![0xff, 0xff, 0xff],
        );

        let result = packet.decode_body::<AuthReq>("INVALID_AUTH_BODY");
        assert_eq!(result, Err("INVALID_AUTH_BODY"));
    }
}
