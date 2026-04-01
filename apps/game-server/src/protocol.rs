use prost::Message;

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

#[derive(Debug, Clone, Copy)]
pub enum MessageType {
    AuthReq = 1001,
    AuthRes = 1002,
    PingReq = 1003,
    PingRes = 1004,
    RoomJoinReq = 1101,
    RoomJoinRes = 1102,
    RoomLeaveReq = 1103,
    RoomLeaveRes = 1104,
    RoomReadyReq = 1105,
    RoomReadyRes = 1106,
    RoomStartReq = 1107,
    RoomStartRes = 1108,
    PlayerInputReq = 1111,
    PlayerInputRes = 1112,
    RoomEndReq = 1113,
    RoomEndRes = 1114,
    RoomStatePush = 1201,
    GameMessagePush = 1202,
    AdminServerStatusReq = 2001,
    AdminServerStatusRes = 2002,
    AdminUpdateConfigReq = 2003,
    AdminUpdateConfigRes = 2004,
    ErrorRes = 9000,
}

impl MessageType {
    pub fn from_u16(value: u16) -> Option<Self> {
        match value {
            1001 => Some(Self::AuthReq),
            1002 => Some(Self::AuthRes),
            1003 => Some(Self::PingReq),
            1004 => Some(Self::PingRes),
            1101 => Some(Self::RoomJoinReq),
            1102 => Some(Self::RoomJoinRes),
            1103 => Some(Self::RoomLeaveReq),
            1104 => Some(Self::RoomLeaveRes),
            1105 => Some(Self::RoomReadyReq),
            1106 => Some(Self::RoomReadyRes),
            1107 => Some(Self::RoomStartReq),
            1108 => Some(Self::RoomStartRes),
            1111 => Some(Self::PlayerInputReq),
            1112 => Some(Self::PlayerInputRes),
            1113 => Some(Self::RoomEndReq),
            1114 => Some(Self::RoomEndRes),
            1201 => Some(Self::RoomStatePush),
            1202 => Some(Self::GameMessagePush),
            2001 => Some(Self::AdminServerStatusReq),
            2002 => Some(Self::AdminServerStatusRes),
            2003 => Some(Self::AdminUpdateConfigReq),
            2004 => Some(Self::AdminUpdateConfigRes),
            9000 => Some(Self::ErrorRes),
            _ => None,
        }
    }
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
    use crate::pb::{AuthReq, RoomMember, RoomSnapshot, RoomStatePush};

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
                    },
                    RoomMember {
                        player_id: "player-b".to_string(),
                        ready: false,
                        is_owner: false,
                    },
                ],
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
