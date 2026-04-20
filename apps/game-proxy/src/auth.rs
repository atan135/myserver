use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use redis::AsyncCommands;
use serde::Deserialize;
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
pub struct ProxyAuthService {
    redis_client: redis::Client,
    redis_key_prefix: String,
    ticket_secret: String,
}

#[derive(Deserialize)]
struct TicketPayload {
    #[serde(rename = "playerId")]
    player_id: String,
    exp: String,
}

impl ProxyAuthService {
    pub fn new(
        redis_url: &str,
        redis_key_prefix: impl Into<String>,
        ticket_secret: impl Into<String>,
    ) -> Result<Self, redis::RedisError> {
        Ok(Self {
            redis_client: redis::Client::open(redis_url)?,
            redis_key_prefix: redis_key_prefix.into(),
            ticket_secret: ticket_secret.into(),
        })
    }

    pub async fn authenticate_ticket(&self, ticket: &str) -> Result<String, &'static str> {
        let player_id = verify_ticket(&self.ticket_secret, ticket)?;
        let ticket_key = format!("{}ticket:{}", self.redis_key_prefix, hash_ticket(ticket));
        let mut conn = self
            .redis_client
            .get_multiplexed_async_connection()
            .await
            .map_err(|_| "AUTH_BACKEND_UNAVAILABLE")?;
        let ticket_owner: Option<String> = conn
            .get(ticket_key)
            .await
            .map_err(|_| "AUTH_BACKEND_UNAVAILABLE")?;

        if ticket_owner.as_deref() != Some(player_id.as_str()) {
            return Err("TICKET_NOT_FOUND");
        }

        Ok(player_id)
    }
}

fn hash_ticket(ticket: &str) -> String {
    let digest = Sha256::digest(ticket.as_bytes());
    format!("{:x}", digest)
}

fn verify_ticket(secret: &str, ticket: &str) -> Result<String, &'static str> {
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
