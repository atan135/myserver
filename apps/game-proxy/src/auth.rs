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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedIdentity {
    pub account_player_id: String,
    pub character_id: String,
    pub world_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TicketPayload {
    #[serde(rename = "playerId")]
    player_id: String,
    #[serde(default, rename = "characterId")]
    character_id: String,
    #[serde(rename = "worldId")]
    world_id: Option<u64>,
    ver: Option<u64>,
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

    pub async fn authenticate_ticket(
        &self,
        ticket: &str,
    ) -> Result<AuthenticatedIdentity, &'static str> {
        let ticket_payload = verify_ticket(&self.ticket_secret, ticket)?;
        let account_player_id = ticket_payload.player_id;
        let character_id = ticket_payload.character_id;
        let world_id = ticket_payload.world_id;
        let ticket_key = format!("{}ticket:{}", self.redis_key_prefix, hash_ticket(ticket));
        let ticket_version_key = format!(
            "{}player-ticket-version:{}",
            self.redis_key_prefix, account_player_id
        );
        let mut conn = self
            .redis_client
            .get_multiplexed_async_connection()
            .await
            .map_err(|_| "AUTH_BACKEND_UNAVAILABLE")?;
        let ticket_owner: Option<String> = conn
            .get(ticket_key)
            .await
            .map_err(|_| "AUTH_BACKEND_UNAVAILABLE")?;

        validate_ticket_owner(ticket_owner.as_deref(), &account_player_id)?;

        let current_ticket_version: Option<u64> = conn
            .get(ticket_version_key)
            .await
            .map_err(|_| "AUTH_BACKEND_UNAVAILABLE")?;
        validate_ticket_version(ticket_payload.ver, current_ticket_version)?;

        Ok(AuthenticatedIdentity {
            account_player_id,
            character_id,
            world_id,
        })
    }
}

fn validate_ticket_owner(
    ticket_owner: Option<&str>,
    expected_account_player_id: &str,
) -> Result<(), &'static str> {
    match ticket_owner {
        Some(owner) if owner == expected_account_player_id => Ok(()),
        Some(_) => Err("ACCOUNT_PLAYER_ID_MISMATCH"),
        None => Err("TICKET_NOT_FOUND"),
    }
}

fn validate_ticket_version(
    ticket_version: Option<u64>,
    current_ticket_version: Option<u64>,
) -> Result<(), &'static str> {
    if ticket_version.unwrap_or(1) != current_ticket_version.unwrap_or(1) {
        return Err("TICKET_REVOKED");
    }

    Ok(())
}

fn hash_ticket(ticket: &str) -> String {
    let digest = Sha256::digest(ticket.as_bytes());
    format!("{:x}", digest)
}

fn verify_ticket(secret: &str, ticket: &str) -> Result<TicketPayload, &'static str> {
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

    if !is_valid_character_id(&payload.character_id) {
        return Err("INVALID_CHARACTER_ID");
    }

    Ok(payload)
}

fn is_valid_character_id(character_id: &str) -> bool {
    character_id
        .strip_prefix("chr_")
        .is_some_and(|suffix| !suffix.is_empty() && suffix.chars().all(is_crockford_base32_char))
}

fn is_crockford_base32_char(value: char) -> bool {
    matches!(value, '0'..='9' | 'a'..='h' | 'j'..='k' | 'm'..='n' | 'p'..='t' | 'v'..='z')
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

    #[test]
    fn verify_ticket_rejects_invalid_character_id() {
        let ticket = create_ticket(
            json!({
                "playerId": "player-001",
                "characterId": "character-1",
                "ver": 1,
                "exp": (Utc::now() + chrono::Duration::minutes(5)).to_rfc3339()
            }),
            "test-secret",
        );

        assert_eq!(
            verify_ticket("test-secret", &ticket).unwrap_err(),
            "INVALID_CHARACTER_ID"
        );
    }

    #[test]
    fn verify_ticket_rejects_character_id_with_outer_whitespace() {
        let ticket = create_ticket(
            json!({
                "playerId": "player-001",
                "characterId": " chr_0000000000001 ",
                "ver": 1,
                "exp": (Utc::now() + chrono::Duration::minutes(5)).to_rfc3339()
            }),
            "test-secret",
        );

        assert_eq!(
            verify_ticket("test-secret", &ticket).unwrap_err(),
            "INVALID_CHARACTER_ID"
        );
    }

    #[tokio::test]
    async fn authenticate_ticket_rejects_missing_character_id_before_redis_lookup() {
        let auth_service = ProxyAuthService::new("redis://127.0.0.1:1", "", "test-secret")
            .expect("test redis url should parse");
        let ticket = create_ticket(
            json!({
                "playerId": "player-001",
                "ver": 1,
                "exp": (Utc::now() + chrono::Duration::minutes(5)).to_rfc3339()
            }),
            "test-secret",
        );

        assert_eq!(
            auth_service.authenticate_ticket(&ticket).await.unwrap_err(),
            "MISSING_CHARACTER_ID"
        );
    }

    #[tokio::test]
    async fn authenticate_ticket_rejects_invalid_character_id_before_redis_lookup() {
        let auth_service = ProxyAuthService::new("redis://127.0.0.1:1", "", "test-secret")
            .expect("test redis url should parse");
        let ticket = create_ticket(
            json!({
                "playerId": "player-001",
                "characterId": "invalid-character",
                "ver": 1,
                "exp": (Utc::now() + chrono::Duration::minutes(5)).to_rfc3339()
            }),
            "test-secret",
        );

        assert_eq!(
            auth_service.authenticate_ticket(&ticket).await.unwrap_err(),
            "INVALID_CHARACTER_ID"
        );
    }

    #[test]
    fn validate_ticket_owner_distinguishes_account_mismatch() {
        assert_eq!(
            validate_ticket_owner(Some("player-001"), "player-001"),
            Ok(())
        );
        assert_eq!(
            validate_ticket_owner(Some("player-002"), "player-001").unwrap_err(),
            "ACCOUNT_PLAYER_ID_MISMATCH"
        );
        assert_eq!(
            validate_ticket_owner(None, "player-001").unwrap_err(),
            "TICKET_NOT_FOUND"
        );
    }

    #[test]
    fn validate_ticket_version_keeps_account_level_revocation() {
        assert_eq!(validate_ticket_version(Some(2), Some(2)), Ok(()));
        assert_eq!(validate_ticket_version(None, None), Ok(()));
        assert_eq!(
            validate_ticket_version(Some(1), Some(2)).unwrap_err(),
            "TICKET_REVOKED"
        );
    }
}
