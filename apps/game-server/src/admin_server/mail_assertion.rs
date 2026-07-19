use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::auth::AdminAuthContext;
use crate::admin_pb::GrantItemsResultQueryReq;
use crate::protocol::Packet;

const VERSION: u32 = 1;
const MAX_IDENTIFIER_LEN: usize = 128;
const MAX_REPLAY_ENTRIES: usize = 10_000;

#[derive(Clone)]
pub(crate) struct MailGrantAssertionVerifier {
    issuer: String,
    public_keys: HashMap<String, VerifyingKey>,
    max_ttl_ms: i64,
    seen: Arc<Mutex<HashMap<String, ReplayEntry>>>,
}

#[derive(Clone)]
struct ReplayEntry {
    payload_sha256: String,
    expires_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct MailGrantAssertion {
    version: u32,
    operation_id: String,
    request_id: String,
    mail_id: String,
    character_id: String,
    attachment_fingerprint: String,
    issuer: String,
    key_id: String,
    service: String,
    service_instance_id: String,
    target_service: String,
    target_instance_id: String,
    issued_at_ms: i64,
    expires_at_ms: i64,
    payload_sha256: String,
    signature: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MailGrantAssertionError {
    Unauthenticated,
    Expired,
    TargetDenied,
    PayloadMismatch,
    WorkflowMismatch,
    Replay,
    Conflict,
}

impl MailGrantAssertionError {
    pub(super) fn code(self) -> &'static str {
        match self {
            Self::Unauthenticated => "MAIL_GRANT_ASSERTION_UNAUTHENTICATED",
            Self::Expired => "MAIL_GRANT_ASSERTION_EXPIRED",
            Self::TargetDenied => "MAIL_GRANT_ASSERTION_TARGET_DENIED",
            Self::PayloadMismatch => "MAIL_GRANT_ASSERTION_PAYLOAD_MISMATCH",
            Self::WorkflowMismatch => "MAIL_GRANT_ASSERTION_WORKFLOW_MISMATCH",
            Self::Replay => "MAIL_GRANT_REQUEST_REPLAY",
            Self::Conflict => "MAIL_GRANT_REQUEST_CONFLICT",
        }
    }
}

impl MailGrantAssertionVerifier {
    pub(crate) fn new(issuer: String, keys: &HashMap<String, String>, max_ttl_ms: u64) -> Self {
        Self {
            issuer,
            public_keys: keys
                .iter()
                .filter_map(|(key_id, key)| decode_public_key(key).map(|key| (key_id.clone(), key)))
                .collect(),
            max_ttl_ms: i64::try_from(max_ttl_ms).unwrap_or(60_000),
            seen: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(super) fn parse(&self, body: &[u8]) -> Result<MailGrantAssertion, MailGrantAssertionError> {
        let assertion: MailGrantAssertion = serde_json::from_slice(body)
            .map_err(|_| MailGrantAssertionError::Unauthenticated)?;
        if ![
            &assertion.operation_id,
            &assertion.request_id,
            &assertion.mail_id,
            &assertion.character_id,
            &assertion.issuer,
            &assertion.key_id,
            &assertion.service,
            &assertion.service_instance_id,
            &assertion.target_service,
            &assertion.target_instance_id,
        ]
        .iter()
        .all(|value| identifier(value))
            || !attachment_fingerprint(&assertion.attachment_fingerprint)
        {
            return Err(MailGrantAssertionError::Unauthenticated);
        }
        Ok(assertion)
    }

    pub(super) fn verify_grant(
        &self,
        assertion: &MailGrantAssertion,
        packet: &Packet,
        game_instance_id: &str,
    ) -> Result<AdminAuthContext, MailGrantAssertionError> {
        self.verify_common(assertion, packet, game_instance_id)?;
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct GrantPayload {
            request_id: String,
            mail_id: String,
            character_id: String,
            request_fingerprint: String,
            source: String,
        }
        let payload: GrantPayload = serde_json::from_slice(&packet.body)
            .map_err(|_| MailGrantAssertionError::WorkflowMismatch)?;
        if payload.request_id != assertion.request_id
            || payload.mail_id != assertion.mail_id
            || payload.character_id != assertion.character_id
            || payload.request_fingerprint != assertion.attachment_fingerprint
            || payload.source != "mail-claim"
            || payload.request_id != format!("mail_claim:{}", payload.mail_id)
        {
            return Err(MailGrantAssertionError::WorkflowMismatch);
        }
        self.reserve_grant_request(assertion)?;
        Ok(mail_context(&assertion.service_instance_id))
    }

    pub(super) fn verify_query(
        &self,
        assertion: &MailGrantAssertion,
        packet: &Packet,
        game_instance_id: &str,
    ) -> Result<AdminAuthContext, MailGrantAssertionError> {
        self.verify_common(assertion, packet, game_instance_id)?;
        let query = packet
            .decode_body::<GrantItemsResultQueryReq>("INVALID_MAIL_GRANT_QUERY")
            .map_err(|_| MailGrantAssertionError::WorkflowMismatch)?;
        if query.request_id != assertion.request_id
            || query.request_fingerprint != assertion.attachment_fingerprint
            || query.request_id != format!("mail_claim:{}", assertion.mail_id)
        {
            return Err(MailGrantAssertionError::WorkflowMismatch);
        }
        Ok(mail_context(&assertion.service_instance_id))
    }

    fn verify_common(
        &self,
        assertion: &MailGrantAssertion,
        packet: &Packet,
        game_instance_id: &str,
    ) -> Result<(), MailGrantAssertionError> {
        if assertion.version != VERSION
            || assertion.issuer != self.issuer
            || assertion.service != "mail-service"
        {
            return Err(MailGrantAssertionError::Unauthenticated);
        }
        if assertion.target_service != "game-server"
            || assertion.target_instance_id != game_instance_id
        {
            return Err(MailGrantAssertionError::TargetDenied);
        }
        let now = unix_ms();
        if assertion.expires_at_ms <= now
            || assertion.expires_at_ms <= assertion.issued_at_ms
            || assertion.issued_at_ms > now.saturating_add(5_000)
            || assertion.expires_at_ms.saturating_sub(assertion.issued_at_ms) > self.max_ttl_ms
        {
            return Err(MailGrantAssertionError::Expired);
        }
        let key = self.public_keys.get(&assertion.key_id).ok_or(MailGrantAssertionError::Unauthenticated)?;
        let signature = Signature::from_slice(&decode_b64(&assertion.signature).ok_or(MailGrantAssertionError::Unauthenticated)?)
            .map_err(|_| MailGrantAssertionError::Unauthenticated)?;
        key.verify(canonical(assertion).as_bytes(), &signature)
            .map_err(|_| MailGrantAssertionError::Unauthenticated)?;
        if payload_hash(&packet.body) != assertion.payload_sha256 {
            return Err(MailGrantAssertionError::PayloadMismatch);
        }
        Ok(())
    }

    fn reserve_grant_request(
        &self,
        assertion: &MailGrantAssertion,
    ) -> Result<(), MailGrantAssertionError> {
        let now = unix_ms();
        let mut seen = self.seen.lock().map_err(|_| MailGrantAssertionError::Unauthenticated)?;
        seen.retain(|_, entry| entry.expires_at_ms > now);
        if let Some(existing) = seen.get(&assertion.request_id) {
            return if existing.payload_sha256 == assertion.payload_sha256 {
                Err(MailGrantAssertionError::Replay)
            } else {
                Err(MailGrantAssertionError::Conflict)
            };
        }
        if seen.len() >= MAX_REPLAY_ENTRIES {
            return Err(MailGrantAssertionError::Conflict);
        }
        seen.insert(
            assertion.request_id.clone(),
            ReplayEntry {
                payload_sha256: assertion.payload_sha256.clone(),
                expires_at_ms: assertion.expires_at_ms,
            },
        );
        Ok(())
    }
}

fn mail_context(instance: &str) -> AdminAuthContext {
    AdminAuthContext { actor: format!("mail-service.{}", instance), actor_missing: false }
}

fn canonical(value: &MailGrantAssertion) -> String {
    let fields = [
        value.version.to_string(), json(&value.operation_id), json(&value.request_id), json(&value.mail_id),
        json(&value.character_id), json(&value.attachment_fingerprint), json(&value.issuer), json(&value.key_id),
        json(&value.service), json(&value.service_instance_id), json(&value.target_service), json(&value.target_instance_id),
        value.issued_at_ms.to_string(), value.expires_at_ms.to_string(), json(&value.payload_sha256),
    ];
    format!("[{}]", fields.join(","))
}

fn json(value: &str) -> String { serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string()) }
fn payload_hash(payload: &[u8]) -> String { base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(payload)) }
fn decode_b64(value: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(value)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(value))
        .or_else(|_| base64::engine::general_purpose::STANDARD.decode(value)).ok()
}
fn decode_public_key(value: &str) -> Option<VerifyingKey> { VerifyingKey::from_bytes(&decode_b64(value)?.try_into().ok()?).ok() }
fn attachment_fingerprint(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
}
fn identifier(value: &str) -> bool { !value.is_empty() && value.len() <= MAX_IDENTIFIER_LEN && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'@' | b'-')) }
fn unix_ms() -> i64 { SystemTime::now().duration_since(UNIX_EPOCH).map(|value| value.as_millis() as i64).unwrap_or_default() }

#[cfg(test)]
mod tests {
    use base64::Engine;
    use ed25519_dalek::{Signer, SigningKey};

    use super::*;
    use crate::protocol::{MessageType, Packet, PacketHeader};

    fn packet(message_type: MessageType, payload: &[u8]) -> Packet {
        Packet::new(
            PacketHeader {
                msg_type: message_type as u16,
                seq: 1,
                body_len: payload.len() as u32,
            },
            payload.to_vec(),
        )
    }

    fn grant_payload(
        character_id: &str,
        fingerprint: &str,
        source: &str,
    ) -> Vec<u8> {
        format!(
            r#"{{"requestId":"mail_claim:mail-1","mailId":"mail-1","characterId":"{character_id}","requestFingerprint":"{fingerprint}","source":"{source}"}}"#
        )
        .into_bytes()
    }

    fn signed_assertion(
        signing_key: &SigningKey,
        payload: &[u8],
        fingerprint: &str,
    ) -> MailGrantAssertion {
        let now = unix_ms();
        let mut assertion = MailGrantAssertion {
            version: VERSION,
            operation_id: "mail-op-1".to_string(),
            request_id: "mail_claim:mail-1".to_string(),
            mail_id: "mail-1".to_string(),
            character_id: "chr-1".to_string(),
            attachment_fingerprint: fingerprint.to_string(),
            issuer: "mail-service".to_string(),
            key_id: "mail-v1".to_string(),
            service: "mail-service".to_string(),
            service_instance_id: "mail-service-1".to_string(),
            target_service: "game-server".to_string(),
            target_instance_id: "game-server-1".to_string(),
            issued_at_ms: now - 1,
            expires_at_ms: now + 30_000,
            payload_sha256: payload_hash(payload),
            signature: String::new(),
        };
        assertion.signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(signing_key.sign(canonical(&assertion).as_bytes()).to_bytes());
        assertion
    }

    fn fixture(payload: &[u8], fingerprint: &str) -> (MailGrantAssertionVerifier, SigningKey, MailGrantAssertion) {
        let signing_key = SigningKey::from_bytes(&[23u8; 32]);
        let public_key = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(signing_key.verifying_key().as_bytes());
        let mut keys = HashMap::new();
        keys.insert("mail-v1".to_string(), public_key);
        let verifier = MailGrantAssertionVerifier::new("mail-service".to_string(), &keys, 60_000);
        let assertion = signed_assertion(&signing_key, payload, fingerprint);
        (verifier, signing_key, assertion)
    }

    #[test]
    fn accepts_only_a_valid_frozen_mail_claim_grant() {
        let fingerprint = format!("sha256:{}", "a".repeat(64));
        let payload = grant_payload("chr-1", &fingerprint, "mail-claim");
        let (verifier, _, assertion) = fixture(&payload, &fingerprint);

        let context = verifier
            .verify_grant(
                &assertion,
                &packet(MessageType::MailAttachmentGrantReq, &payload),
                "game-server-1",
            )
            .unwrap();

        assert_eq!(context.actor, "mail-service.mail-service-1");
        assert!(!context.actor_missing);
    }

    #[test]
    fn rejects_wrong_key_expired_and_payload_or_workflow_tampering() {
        let fingerprint = format!("sha256:{}", "b".repeat(64));
        let payload = grant_payload("chr-1", &fingerprint, "mail-claim");
        let (verifier, signing_key, assertion) = fixture(&payload, &fingerprint);

        let mut wrong_issuer = assertion.clone();
        wrong_issuer.issuer = "admin-api".to_string();
        assert_eq!(
            verifier.verify_grant(&wrong_issuer, &packet(MessageType::MailAttachmentGrantReq, &payload), "game-server-1"),
            Err(MailGrantAssertionError::Unauthenticated)
        );

        let mut wrong_key = assertion.clone();
        wrong_key.key_id = "admin-api-v1".to_string();
        assert_eq!(
            verifier.verify_grant(&wrong_key, &packet(MessageType::MailAttachmentGrantReq, &payload), "game-server-1"),
            Err(MailGrantAssertionError::Unauthenticated)
        );

        let mut expired = assertion.clone();
        expired.expires_at_ms = unix_ms() - 1;
        assert_eq!(
            verifier.verify_grant(&expired, &packet(MessageType::MailAttachmentGrantReq, &payload), "game-server-1"),
            Err(MailGrantAssertionError::Expired)
        );

        let tampered_payload = grant_payload("chr-2", &fingerprint, "mail-claim");
        assert_eq!(
            verifier.verify_grant(&assertion, &packet(MessageType::MailAttachmentGrantReq, &tampered_payload), "game-server-1"),
            Err(MailGrantAssertionError::PayloadMismatch)
        );

        let invalid_workflow = grant_payload("chr-1", &fingerprint, "gm-emergency-correction");
        let invalid_assertion = signed_assertion(&signing_key, &invalid_workflow, &fingerprint);
        assert_eq!(
            verifier.verify_grant(&invalid_assertion, &packet(MessageType::MailAttachmentGrantReq, &invalid_workflow), "game-server-1"),
            Err(MailGrantAssertionError::WorkflowMismatch)
        );
    }

    #[test]
    fn reserves_grants_against_replay_and_conflicting_fingerprints() {
        let fingerprint = format!("sha256:{}", "c".repeat(64));
        let payload = grant_payload("chr-1", &fingerprint, "mail-claim");
        let (verifier, signing_key, assertion) = fixture(&payload, &fingerprint);
        let grant_packet = packet(MessageType::MailAttachmentGrantReq, &payload);

        verifier.verify_grant(&assertion, &grant_packet, "game-server-1").unwrap();
        assert_eq!(
            verifier.verify_grant(&assertion, &grant_packet, "game-server-1"),
            Err(MailGrantAssertionError::Replay)
        );

        let conflicting_fingerprint = format!("sha256:{}", "d".repeat(64));
        let conflicting_payload = grant_payload("chr-1", &conflicting_fingerprint, "mail-claim");
        let conflicting_assertion = signed_assertion(&signing_key, &conflicting_payload, &conflicting_fingerprint);
        assert_eq!(
            verifier.verify_grant(
                &conflicting_assertion,
                &packet(MessageType::MailAttachmentGrantReq, &conflicting_payload),
                "game-server-1",
            ),
            Err(MailGrantAssertionError::Conflict)
        );
    }
}
