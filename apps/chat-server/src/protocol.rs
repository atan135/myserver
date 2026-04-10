pub const HEADER_LEN: usize = 14;

#[derive(Debug, Clone)]
pub struct OutboundMessage {
    pub message_type: u16,
    pub seq: u32,
    pub body: Vec<u8>,
}

#[derive(Debug)]
pub struct PacketHeader {
    pub magic: u16,
    pub version: u8,
    pub flags: u8,
    pub msg_type: u16,
    pub seq: u32,
    pub body_len: u32,
}

pub struct Packet {
    pub header: PacketHeader,
    pub body: Vec<u8>,
}

impl Packet {
    pub fn new(header: PacketHeader, body: Vec<u8>) -> Self {
        Self { header, body }
    }

    pub fn decode_body<T: prost::Message + Default>(&self) -> Result<T, String> {
        T::decode(&*self.body).map_err(|e| format!("decode error: {}", e))
    }
}

pub fn parse_header(data: [u8; HEADER_LEN]) -> Result<PacketHeader, String> {
    let magic = u16::from_be_bytes([data[0], data[1]]);
    if magic != 0x4D53 {
        return Err("INVALID_MAGIC".to_string());
    }

    let version = data[2];
    let flags = data[3];
    let msg_type = u16::from_be_bytes([data[4], data[5]]);
    let seq = u32::from_be_bytes([data[6], data[7], data[8], data[9]]);
    let body_len = u32::from_be_bytes([data[10], data[11], data[12], data[13]]);

    Ok(PacketHeader {
        magic,
        version,
        flags,
        msg_type,
        seq,
        body_len,
    })
}

pub fn encode_body<T: prost::Message>(message: &T) -> Vec<u8> {
    let mut buf = Vec::with_capacity(message.encoded_len());
    message.encode(&mut buf).unwrap_or_default();
    buf
}

pub fn encode_packet(message_type: u16, seq: u32, body: &[u8]) -> Vec<u8> {
    let body_len = body.len() as u32;
    let mut packet = Vec::with_capacity(HEADER_LEN + body_len as usize);

    // MAGIC: 0x4D53 ('MS')
    packet.push(0x4D);
    packet.push(0x53);
    // Version
    packet.push(1);
    // Flags
    packet.push(0);

    // Message type
    packet.extend_from_slice(&message_type.to_be_bytes());
    // Sequence
    packet.extend_from_slice(&seq.to_be_bytes());
    // Body length
    packet.extend_from_slice(&body_len.to_be_bytes());

    // Body
    packet.extend_from_slice(body);

    packet
}
