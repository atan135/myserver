pub const HEADER_LEN: usize = 14;
pub const MAGIC: u16 = 0xCAFE;
pub const VERSION: u8 = 1;

#[derive(Debug, Clone, Copy)]
pub struct PacketHeader {
    pub msg_type: u16,
    pub seq: u32,
    pub body_len: u32,
}

#[derive(Debug, Clone, Copy)]
pub enum MessageType {
    AuthReq = 1001,
    AuthRes = 1002,
    PingReq = 1003,
    PingRes = 1004,
    RoomJoinReq = 1101,
    RoomJoinRes = 1102,
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
            9000 => Some(Self::ErrorRes),
            _ => None,
        }
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
