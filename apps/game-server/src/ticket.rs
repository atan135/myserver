use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Deserialize)]
pub struct TicketPayload {
    #[serde(rename = "playerId")]
    pub player_id: String,
    #[serde(default, rename = "characterId")]
    pub character_id: String,
    #[serde(rename = "worldId")]
    pub world_id: Option<u64>,
    pub ver: Option<u64>,
    pub exp: String,
}

pub fn hash_ticket(ticket: &str) -> String {
    let digest = Sha256::digest(ticket.as_bytes());
    format!("{:x}", digest)
}

pub fn verify_ticket(secret: &str, ticket: &str) -> Result<TicketPayload, &'static str> {
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

    if payload.player_id.trim().is_empty() {
        return Err("INVALID_TICKET_PAYLOAD");
    }

    if payload.character_id.trim().is_empty() {
        return Err("MISSING_CHARACTER_ID");
    }

    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn create_ticket(payload: serde_json::Value, secret: &str) -> String {
        let payload_b64 = URL_SAFE_NO_PAD.encode(payload.to_string());
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(payload_b64.as_bytes());
        let signature = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
        format!("{}.{}", payload_b64, signature)
    }

    #[test]
    fn verify_ticket_accepts_character_bound_payload() {
        let ticket = create_ticket(
            json!({
                "playerId": "player-001",
                "characterId": "chr_0000000000001",
                "worldId": 9,
                "ver": 1,
                "exp": (Utc::now() + chrono::Duration::minutes(5)).to_rfc3339()
            }),
            "test-secret",
        );

        let payload = verify_ticket("test-secret", &ticket).unwrap();

        assert_eq!(payload.player_id, "player-001");
        assert_eq!(payload.character_id, "chr_0000000000001");
        assert_eq!(payload.world_id, Some(9));
        assert_eq!(payload.ver, Some(1));
    }

    #[test]
    fn verify_ticket_rejects_missing_character_id() {
        let ticket = create_ticket(
            json!({
                "playerId": "player-001",
                "ver": 1,
                "exp": (Utc::now() + chrono::Duration::minutes(5)).to_rfc3339()
            }),
            "test-secret",
        );

        assert_eq!(
            verify_ticket("test-secret", &ticket).unwrap_err(),
            "MISSING_CHARACTER_ID"
        );
    }
}
