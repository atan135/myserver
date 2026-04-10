use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

#[derive(Deserialize)]
struct TicketPayload {
    #[serde(rename = "playerId")]
    player_id: String,
    exp: String,
}

/// 验证票据，返回 player_id
/// 票据格式: base64(payload).base64(signature)
/// payload 是 JSON: {"playerId":"xxx","exp":"2024-01-01T00:00:00Z"}
pub fn verify_ticket(secret: &str, ticket: &str) -> Result<String, &'static str> {
    let (payload_b64, signature_b64) = ticket.split_once('.').ok_or("INVALID_TICKET_FORMAT")?;

    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).map_err(|_| "INVALID_TICKET_SECRET")?;
    mac.update(payload_b64.as_bytes());

    let signature = URL_SAFE_NO_PAD
        .decode(signature_b64)
        .map_err(|_| "INVALID_TICKET_SIGNATURE")?;

    mac.verify_slice(&signature)
        .map_err(|_| "INVALID_TICKET_SIGNATURE")?;

    let payload_json = URL_SAFE_NO_PAD
        .decode(payload_b64)
        .map_err(|_| "INVALID_TICKET_PAYLOAD")?;
    let payload: TicketPayload =
        serde_json::from_slice(&payload_json).map_err(|_| "INVALID_TICKET_PAYLOAD")?;

    let expires_at = DateTime::parse_from_rfc3339(&payload.exp)
        .map_err(|_| "INVALID_TICKET_EXP")?
        .with_timezone(&Utc);

    if expires_at <= Utc::now() {
        return Err("TICKET_EXPIRED");
    }

    Ok(payload.player_id)
}
