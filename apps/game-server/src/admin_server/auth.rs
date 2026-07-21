use serde::Deserialize;

use crate::protocol::{MessageType, Packet};

const MAX_ADMIN_ACTOR_LEN: usize = 128;
const DEFAULT_ADMIN_ACTOR: &str = "unknown";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AdminAuthContext {
    pub(super) actor: String,
    pub(super) actor_missing: bool,
}

pub(super) enum AdminConnectionAuth {
    ReadOnly(AdminAuthContext),
    Assertion,
}

#[derive(Deserialize)]
struct AdminAuthEnvelope {
    token: String,
    actor: Option<String>,
}

pub(super) fn authenticate_admin_packet(
    packet: &Packet,
    admin_token: &str,
) -> Option<AdminAuthContext> {
    if packet.message_type() != Some(MessageType::AdminAuthReq) {
        return None;
    }

    let body = std::str::from_utf8(&packet.body).ok()?;
    if body == admin_token {
        return Some(AdminAuthContext {
            actor: DEFAULT_ADMIN_ACTOR.to_string(),
            actor_missing: true,
        });
    }

    let envelope: AdminAuthEnvelope = serde_json::from_str(body).ok()?;
    if envelope.token != admin_token {
        return None;
    }

    Some(normalize_admin_auth_context(envelope.actor))
}

pub(super) fn authenticate_admin_connection(
    packet: &Packet,
    admin_token: &str,
) -> Option<AdminConnectionAuth> {
    if packet.message_type() != Some(MessageType::AdminAuthReq) {
        return None;
    }
    if packet.body.as_slice() == br#"{"mode":"assertion"}"# {
        return Some(AdminConnectionAuth::Assertion);
    }
    authenticate_admin_packet(packet, admin_token).map(AdminConnectionAuth::ReadOnly)
}

fn normalize_admin_auth_context(actor: Option<String>) -> AdminAuthContext {
    let Some(actor) = actor
        .as_deref()
        .map(str::trim)
        .and_then(normalize_admin_actor)
    else {
        return AdminAuthContext {
            actor: DEFAULT_ADMIN_ACTOR.to_string(),
            actor_missing: true,
        };
    };

    AdminAuthContext {
        actor,
        actor_missing: false,
    }
}

fn normalize_admin_actor(value: &str) -> Option<String> {
    if value.is_empty() || value.len() > MAX_ADMIN_ACTOR_LEN {
        return None;
    }

    value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'@'))
        .then(|| value.to_string())
}
