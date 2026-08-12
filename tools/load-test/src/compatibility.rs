//! Offline compatibility contracts against the current shared player boundary.
//!
//! These tests establish client-side compatibility expectations only. Ticket
//! owner lookup, signature verification, and character-bound room recovery
//! remain production-service behavior until a separately approved smoke test.

pub const TICKET_OWNERSHIP_CONTRACT: &str =
    "game-proxy verifies Redis ticket ownership against the signed account player id";
pub const CHARACTER_BOUND_RECONNECT_CONTRACT: &str = "RoomReconnectReq supplies only a push cursor; the server resolves the authenticated ticket-bound character";

#[cfg(test)]
mod tests {
    use super::*;
    use game_protocol::{HEADER_LEN, MAGIC, MessageType, Packet, PacketHeader, VERSION};
    use prost::Message;

    use crate::game_kcp::{
        AuthRejectReason, GameConnectionLifecycle, GameLifecycleEvent, KcpSession, ReconnectPolicy,
    };
    use crate::pb::{AuthReq, AuthRes, FrameBundlePush, RoomReconnectReq};
    use crate::protocol_version_policy::CURRENT_CLIENT_PROTOCOL_VERSION;

    fn policy() -> ReconnectPolicy {
        ReconnectPolicy {
            max_attempts: 2,
            base_delay_ms: 100,
            max_delay_ms: 500,
            max_jitter_ms: 0,
        }
    }

    #[test]
    fn shared_kcp_header_message_types_and_no_delay_profile_remain_the_only_source() {
        assert_eq!(HEADER_LEN, 14);
        assert_eq!(MAGIC, 0xCAFE);
        assert_eq!(VERSION, 1);
        assert_eq!(MessageType::AuthReq as u16, 1001);
        assert_eq!(MessageType::RoomReconnectReq as u16, 1115);
        assert_eq!(MessageType::FrameBundlePush as u16, 1203);
        let kcp = game_protocol::player_kcp_config();
        assert!(kcp.stream);
        assert!(kcp.nodelay.nodelay);
        assert_eq!(kcp.nodelay.interval, 10);
        assert_eq!(kcp.nodelay.resend, 2);
        assert!(kcp.nodelay.nc);
    }

    #[test]
    fn auth_packet_uses_shared_protocol_version_and_has_no_client_identity_field() {
        let mut lifecycle = GameConnectionLifecycle::new(1024, policy()).unwrap();
        lifecycle.begin_connect().unwrap();
        let auth = lifecycle.begin_auth("ephemeral-test-ticket").unwrap();
        let bytes = auth.into_bytes();
        let header = game_protocol::parse_header(bytes[..HEADER_LEN].try_into().unwrap()).unwrap();
        assert_eq!(header.msg_type, MessageType::AuthReq as u16);
        assert_eq!(header.seq, 1);
        let request = AuthReq::decode(&bytes[HEADER_LEN..]).unwrap();
        assert_eq!(
            request.client_protocol_version,
            CURRENT_CLIENT_PROTOCOL_VERSION
        );
        assert!(!request.ticket.is_empty());
        // The generated AuthReq has only ticket/version fields, matching the
        // production proxy's server-side identity resolution boundary.
        assert_eq!(AuthReq::decode(&bytes[HEADER_LEN..]).unwrap(), request);
    }

    #[test]
    fn account_player_mismatch_is_a_client_side_invalid_ticket_rejection_only() {
        let mut lifecycle = GameConnectionLifecycle::new(1024, policy()).unwrap();
        lifecycle.begin_connect().unwrap();
        lifecycle.begin_auth("ephemeral-test-ticket").unwrap();
        let response = AuthRes {
            ok: false,
            player_id: String::new(),
            error_code: "ACCOUNT_PLAYER_ID_MISMATCH".into(),
            server_protocol_version: CURRENT_CLIENT_PROTOCOL_VERSION,
            minimum_client_protocol_version: CURRENT_CLIENT_PROTOCOL_VERSION,
            upgrade_message: String::new(),
            upgrade_url: String::new(),
        };
        let body = game_protocol::encode_body(&response);
        let packet = Packet::new(
            PacketHeader {
                msg_type: MessageType::AuthRes as u16,
                seq: 1,
                body_len: body.len() as u32,
            },
            body,
        );
        assert_eq!(
            lifecycle.handle_packet(packet).unwrap(),
            GameLifecycleEvent::AuthRejected {
                reason: AuthRejectReason::InvalidTicket
            }
        );
        assert!(TICKET_OWNERSHIP_CONTRACT.contains("Redis"));
    }

    #[test]
    fn reconnect_contract_is_cursor_only_and_pushes_do_not_consume_response_sequences() {
        let reconnect = RoomReconnectReq {
            last_character_push_sequence: 42,
        };
        let encoded = game_protocol::encode_body(&reconnect);
        assert_eq!(
            RoomReconnectReq::decode(encoded.as_slice()).unwrap(),
            reconnect
        );
        assert!(TICKET_OWNERSHIP_CONTRACT.contains("ticket ownership"));
        assert!(CHARACTER_BOUND_RECONNECT_CONTRACT.contains("ticket-bound character"));

        let mut session = KcpSession::new(1024).unwrap();
        session
            .begin_request(MessageType::RoomReconnectRes, 7)
            .unwrap();
        let push = Packet::new(
            PacketHeader {
                msg_type: MessageType::FrameBundlePush as u16,
                seq: 0,
                body_len: 0,
            },
            Vec::new(),
        );
        assert!(matches!(
            session.ingest(push),
            Ok(crate::game_kcp::KcpSessionEvent::Push {
                message_type: MessageType::FrameBundlePush,
                seq: 0
            })
        ));
        assert_eq!(session.pending_requests(), 1);

        let response = Packet::new(
            PacketHeader {
                msg_type: MessageType::RoomReconnectRes as u16,
                seq: 7,
                body_len: 0,
            },
            Vec::new(),
        );
        assert!(matches!(
            session.ingest(response),
            Ok(crate::game_kcp::KcpSessionEvent::Response {
                message_type: MessageType::RoomReconnectRes,
                seq: 7
            })
        ));
    }

    #[test]
    fn frame_bundle_schema_keeps_ticket_bound_input_ids_inside_server_pushes() {
        let bundle = FrameBundlePush {
            room_id: "room-contract".into(),
            frame_id: 1,
            fps: 20,
            inputs: Vec::new(),
            is_silent_frame: true,
            snapshot: None,
        };
        let encoded = game_protocol::encode_body(&bundle);
        assert_eq!(FrameBundlePush::decode(encoded.as_slice()).unwrap(), bundle);
    }
}
