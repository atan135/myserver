//! Shared player wire protocol primitives.
//!
//! `packages/proto/game.proto` and its imported player schemas remain the
//! protobuf source of truth.  This
//! crate owns only the framing and transport settings which must stay aligned
//! between the player ingress and clients such as the load generator.

use prost::Message;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio_kcp::{KcpConfig, KcpNoDelayConfig};

pub const HEADER_LEN: usize = 14;
pub const MAGIC: u16 = 0xCAFE;
pub const VERSION: u8 = 1;

macro_rules! message_types {
    ($($name:ident = $value:expr),+ $(,)?) => {
        #[repr(u16)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum MessageType { $($name = $value),+ }

        impl MessageType {
            pub fn from_u16(value: u16) -> Option<Self> {
                match value { $($value => Some(Self::$name),)+ _ => None }
            }
        }
    };
}

// These values are the single Rust source for both game services and player
// tools. Protobuf body schemas are generated from packages/proto/game.proto
// and its imported player schema files.
message_types! {
    AuthReq = 1001, AuthRes = 1002, PingReq = 1003, PingRes = 1004,
    RoomJoinReq = 1101, RoomJoinRes = 1102, RoomLeaveReq = 1103, RoomLeaveRes = 1104,
    RoomReadyReq = 1105, RoomReadyRes = 1106, RoomStartReq = 1107, RoomStartRes = 1108,
    PlayerInputReq = 1111, PlayerInputRes = 1112, RoomEndReq = 1113, RoomEndRes = 1114,
    RoomReconnectReq = 1115, RoomReconnectRes = 1116,
    RoomJoinAsObserverReq = 1117, RoomJoinAsObserverRes = 1118,
    CreateMatchedRoomReq = 1119, CreateMatchedRoomRes = 1120,
    MoveInputReq = 1121, MoveInputRes = 1122,
    MatchStartReq = 1123, MatchStartRes = 1124,
    MatchCancelReq = 1125, MatchCancelRes = 1126,
    MatchStatusReq = 1127, MatchStatusRes = 1128,
    MatchEventStreamReq = 1129, MatchEventStreamRes = 1130,
    RoomStatePush = 1201, GameMessagePush = 1202, FrameBundlePush = 1203,
    RoomFrameRatePush = 1204, RoomMemberOfflinePush = 1205,
    MovementSnapshotPush = 1206, MovementRejectPush = 1207, ServerRedirectPush = 1208,
    SessionKickPush = 1209, AuthorityMigrationStartPush = 1210,
    AuthorityMigrationCompletePush = 1211,
    MatchEventPush = 1212,
    GetRoomDataReq = 1301, GetRoomDataRes = 1302,
    ItemEquipReq = 1401, ItemEquipRes = 1402, ItemUseReq = 1403, ItemUseRes = 1404,
    ItemDiscardReq = 1405, ItemDiscardRes = 1406,
    DeprecatedItemAddReq = 1407, DeprecatedItemAddRes = 1408,
    WarehouseAccessReq = 1409, WarehouseAccessRes = 1410,
    GetInventoryReq = 1411, GetInventoryRes = 1412,
    GetCharacterElementsReq = 1413, GetCharacterElementsRes = 1414,
    DebugApplyCharacterElementChangeReq = 1415, DebugApplyCharacterElementChangeRes = 1416,
    GetCharacterTitlesReq = 1417, GetCharacterTitlesRes = 1418,
    EquipCharacterTitleReq = 1419, EquipCharacterTitleRes = 1420,
    GetCharacterDisciplinesReq = 1421, GetCharacterDisciplinesRes = 1422,
    DebugCharacterTitleReq = 1423, DebugCharacterTitleRes = 1424,
    LearnCharacterDisciplineReq = 1425, LearnCharacterDisciplineRes = 1426,
    SetCharacterDisciplineActiveReq = 1427, SetCharacterDisciplineActiveRes = 1428,
    SwitchCharacterDisciplineReq = 1429, SwitchCharacterDisciplineRes = 1430,
    AddCharacterDisciplinePointsReq = 1431, AddCharacterDisciplinePointsRes = 1432,
    ApplyCharacterProgressReq = 1433, ApplyCharacterProgressRes = 1434,
    ActivityListReq = 1435, ActivityListRes = 1436,
    ActivityDetailReq = 1437, ActivityDetailRes = 1438,
    ActivityProgressReq = 1439, ActivityProgressRes = 1440,
    ActivityClaimReq = 1441, ActivityClaimRes = 1442,
    ActivityActionReq = 1443, ActivityActionRes = 1444,
    ActivityClaimHistoryReq = 1445, ActivityClaimHistoryRes = 1446,
    InventoryUpdatePush = 1501, AttrChangePush = 1502, VisualChangePush = 1503,
    ItemObtainPush = 1504, CharacterElementsChangePush = 1505,
    CharacterTitleChangePush = 1506, CharacterDisciplineChangePush = 1507,
    FreezeRoomForTransferReq = 1601, FreezeRoomForTransferRes = 1602,
    ExportRoomTransferReq = 1603, ExportRoomTransferRes = 1604,
    ImportRoomTransferReq = 1605, ImportRoomTransferRes = 1606,
    RetireTransferredRoomReq = 1607, RetireTransferredRoomRes = 1608,
    GetRolloutDrainStatusReq = 1609, GetRolloutDrainStatusRes = 1610,
    TriggerServerRedirectReq = 1611, TriggerServerRedirectRes = 1612,
    ConfirmRoomOwnershipReq = 1613, ConfirmRoomOwnershipRes = 1614,
    TriggerRolloutDrainNoticeReq = 1615, TriggerRolloutDrainNoticeRes = 1616,
    RequestServerShutdownReq = 1617, RequestServerShutdownRes = 1618,
    AdminServerStatusReq = 2001, AdminServerStatusRes = 2002,
    AdminUpdateConfigReq = 2003, AdminUpdateConfigRes = 2004,
    MailAttachmentGrantAssertionReq = 2097, AdminOperationAssertionReq = 2098,
    AdminAuthReq = 2099, InternalAuthReq = 2199,
    GmBroadcastReq = 3001, GmBroadcastRes = 3002, GmSendItemReq = 3003,
    GmSendItemRes = 3004, GmKickPlayerReq = 3005, GmKickPlayerRes = 3006,
    GmBanPlayerReq = 3007, GmBanPlayerRes = 3008,
    GrantItemsResultQueryReq = 3009, GrantItemsResultQueryRes = 3010,
    MailAttachmentGrantReq = 3011, MailAttachmentGrantResultQueryReq = 3013,
    ErrorRes = 9000,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketHeader {
    pub msg_type: u16,
    pub seq: u32,
    pub body_len: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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

    pub fn to_bytes(&self) -> Vec<u8> {
        encode_raw_packet(self.header.msg_type, self.header.seq, &self.body)
    }
}

pub fn parse_header(bytes: [u8; HEADER_LEN]) -> Result<PacketHeader, &'static str> {
    if u16::from_be_bytes([bytes[0], bytes[1]]) != MAGIC {
        return Err("INVALID_MAGIC");
    }
    if bytes[2] != VERSION {
        return Err("INVALID_VERSION");
    }
    if bytes[3] != 0 {
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

pub fn encode_packet(message_type: MessageType, seq: u32, body: &[u8]) -> Vec<u8> {
    encode_raw_packet(message_type as u16, seq, body)
}

pub fn encode_raw_packet(msg_type: u16, seq: u32, body: &[u8]) -> Vec<u8> {
    let mut packet = Vec::with_capacity(HEADER_LEN + body.len());
    packet.extend_from_slice(&MAGIC.to_be_bytes());
    packet.push(VERSION);
    packet.push(0);
    packet.extend_from_slice(&msg_type.to_be_bytes());
    packet.extend_from_slice(&seq.to_be_bytes());
    packet.extend_from_slice(&(body.len() as u32).to_be_bytes());
    packet.extend_from_slice(body);
    packet
}

pub async fn read_packet<R>(
    reader: &mut R,
    max_body_len: usize,
) -> Result<Option<Packet>, std::io::Error>
where
    R: AsyncRead + Unpin,
{
    let mut header_buf = [0u8; HEADER_LEN];
    match reader.read_exact(&mut header_buf).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let header = parse_header(header_buf).map_err(std::io::Error::other)?;
    if header.body_len as usize > max_body_len {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "body too large",
        ));
    }
    let mut body = vec![0u8; header.body_len as usize];
    reader.read_exact(&mut body).await?;
    Ok(Some(Packet::new(header, body)))
}

/// The game-proxy's formal player KCP transport profile.
pub fn player_kcp_config() -> KcpConfig {
    let mut config = KcpConfig::default();
    config.nodelay = KcpNoDelayConfig::fastest();
    config.stream = true;
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_round_trip_and_reserved_message_types_remain_stable() {
        let bytes = encode_packet(MessageType::AuthReq, 42, b"body");
        let header = parse_header(bytes[..HEADER_LEN].try_into().unwrap()).unwrap();
        assert_eq!(header.msg_type, MessageType::AuthReq as u16);
        assert_eq!(header.seq, 42);
        assert_eq!(header.body_len, 4);
        assert_eq!(
            MessageType::from_u16(1407),
            Some(MessageType::DeprecatedItemAddReq)
        );
        assert_eq!(
            MessageType::from_u16(1435),
            Some(MessageType::ActivityListReq)
        );
        assert_eq!(
            MessageType::from_u16(1444),
            Some(MessageType::ActivityActionRes)
        );
    }

    #[test]
    fn rejects_invalid_framing() {
        let mut bytes = [0_u8; HEADER_LEN];
        bytes[..2].copy_from_slice(&0xCAFE_u16.to_be_bytes());
        bytes[2] = VERSION;
        bytes[3] = 1;
        assert_eq!(parse_header(bytes), Err("UNSUPPORTED_FLAGS"));
    }

    #[test]
    fn kcp_profile_uses_player_stream_fast_mode() {
        let config = player_kcp_config();
        assert!(config.stream);
        assert!(config.nodelay.nodelay);
        assert_eq!(config.nodelay.interval, 10);
        assert_eq!(config.nodelay.resend, 2);
        assert!(config.nodelay.nc);
    }
}
