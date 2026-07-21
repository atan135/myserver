use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::auth::AdminAuthContext;
use crate::protocol::Packet;

const ASSERTION_VERSION: u32 = 1;
const MAX_IDENTIFIER_LEN: usize = 128;
const MAX_REPLAY_ENTRIES: usize = 10_000;

#[derive(Clone)]
pub(crate) struct AdminAssertionVerifier {
    issuer: String,
    public_keys: HashMap<String, VerifyingKey>,
    max_ttl_ms: i64,
    replay_cache: Arc<Mutex<HashMap<String, ReplayEntry>>>,
}

#[derive(Clone)]
struct ReplayEntry {
    payload_sha256: String,
    expires_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AdminOperationAssertion {
    version: u32,
    operation_id: String,
    request_id: String,
    trace_id: String,
    issuer: String,
    key_id: String,
    actor_id: String,
    permission: String,
    scope: Value,
    target: Value,
    service: String,
    instance_id: String,
    issued_at_ms: i64,
    expires_at_ms: i64,
    payload_sha256: String,
    signature: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AdminAssertionError {
    Unauthenticated,
    Expired,
    PermissionDenied,
    TargetDenied,
    PayloadMismatch,
    RequestReplay,
    RequestConflict,
}

impl AdminAssertionError {
    pub(super) fn error_code(self) -> &'static str {
        match self {
            Self::Unauthenticated => "ADMIN_ASSERTION_UNAUTHENTICATED",
            Self::Expired => "ADMIN_ASSERTION_EXPIRED",
            Self::PermissionDenied => "ADMIN_ASSERTION_PERMISSION_DENIED",
            Self::TargetDenied => "ADMIN_ASSERTION_TARGET_DENIED",
            Self::PayloadMismatch => "ADMIN_ASSERTION_PAYLOAD_MISMATCH",
            Self::RequestReplay => "ADMIN_REQUEST_REPLAY",
            Self::RequestConflict => "ADMIN_REQUEST_CONFLICT",
        }
    }

    pub(super) fn message(self) -> &'static str {
        match self {
            Self::Unauthenticated => "admin operation assertion is missing or invalid",
            Self::Expired => "admin operation assertion has expired",
            Self::PermissionDenied => "admin operation assertion permission does not match this operation",
            Self::TargetDenied => "admin operation assertion target is outside this service scope",
            Self::PayloadMismatch => "admin operation assertion does not match the request payload",
            Self::RequestReplay => "admin operation request has already been processed",
            Self::RequestConflict => "admin operation request id conflicts with an existing request",
        }
    }
}

impl AdminAssertionVerifier {
    pub(crate) fn new(
        issuer: String,
        public_keys: &HashMap<String, String>,
        max_ttl_ms: u64,
    ) -> Self {
        let public_keys = public_keys
            .iter()
            .filter_map(|(key_id, encoded)| decode_public_key(encoded).map(|key| (key_id.clone(), key)))
            .collect();
        Self {
            issuer,
            public_keys,
            max_ttl_ms: i64::try_from(max_ttl_ms).unwrap_or(60_000),
            replay_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    #[cfg(test)]
    pub(super) fn with_public_key(issuer: &str, key_id: &str, encoded_key: &str) -> Self {
        let mut keys = HashMap::new();
        keys.insert(key_id.to_string(), encoded_key.to_string());
        Self::new(issuer.to_string(), &keys, 60_000)
    }

    pub(super) fn parse(&self, body: &[u8]) -> Result<AdminOperationAssertion, AdminAssertionError> {
        let assertion: AdminOperationAssertion = serde_json::from_slice(body)
            .map_err(|_| AdminAssertionError::Unauthenticated)?;
        assertion.validate_identifiers()?;
        Ok(assertion)
    }

    pub(super) fn verify_for_packet(
        &self,
        assertion: &AdminOperationAssertion,
        packet: &Packet,
        expected_permission: &str,
        expected_target_type: &str,
        service: &str,
        instance_id: &str,
    ) -> Result<AdminAuthContext, AdminAssertionError> {
        self.verify_for_packet_at(
            assertion,
            packet,
            expected_permission,
            expected_target_type,
            service,
            instance_id,
            unix_time_ms(),
        )
    }

    fn verify_for_packet_at(
        &self,
        assertion: &AdminOperationAssertion,
        packet: &Packet,
        expected_permission: &str,
        expected_target_type: &str,
        service: &str,
        instance_id: &str,
        now: i64,
    ) -> Result<AdminAuthContext, AdminAssertionError> {
        if assertion.version != ASSERTION_VERSION || assertion.issuer != self.issuer {
            return Err(AdminAssertionError::Unauthenticated);
        }
        if assertion.expires_at_ms <= now || assertion.expires_at_ms <= assertion.issued_at_ms {
            return Err(AdminAssertionError::Expired);
        }
        if assertion.issued_at_ms > now.saturating_add(5_000)
            || assertion.expires_at_ms.saturating_sub(assertion.issued_at_ms) > self.max_ttl_ms
        {
            return Err(AdminAssertionError::Expired);
        }
        if assertion.permission != expected_permission {
            return Err(AdminAssertionError::PermissionDenied);
        }
        if assertion.service != service || assertion.instance_id != instance_id {
            return Err(AdminAssertionError::TargetDenied);
        }
        self.verify_signature(assertion)?;
        if payload_sha256(&packet.body) != assertion.payload_sha256 {
            return Err(AdminAssertionError::PayloadMismatch);
        }
        verify_target_and_scope(assertion, expected_target_type, service, instance_id)?;
        self.reserve_request(assertion, now)?;
        Ok(AdminAuthContext {
            actor: assertion.actor_id.clone(),
            actor_missing: false,
        })
    }

    fn verify_signature(&self, assertion: &AdminOperationAssertion) -> Result<(), AdminAssertionError> {
        let key = self
            .public_keys
            .get(&assertion.key_id)
            .ok_or(AdminAssertionError::Unauthenticated)?;
        let signature_bytes = decode_base64url(&assertion.signature)
            .ok_or(AdminAssertionError::Unauthenticated)?;
        let signature = Signature::from_slice(&signature_bytes)
            .map_err(|_| AdminAssertionError::Unauthenticated)?;
        key.verify(canonical_assertion_payload(assertion).as_bytes(), &signature)
            .map_err(|_| AdminAssertionError::Unauthenticated)
    }

    fn reserve_request(
        &self,
        assertion: &AdminOperationAssertion,
        now: i64,
    ) -> Result<(), AdminAssertionError> {
        let mut cache = self.replay_cache.lock().map_err(|_| AdminAssertionError::Unauthenticated)?;
        cache.retain(|_, entry| entry.expires_at_ms > now);
        if let Some(existing) = cache.get(&assertion.request_id) {
            return if existing.payload_sha256 == assertion.payload_sha256 {
                Err(AdminAssertionError::RequestReplay)
            } else {
                Err(AdminAssertionError::RequestConflict)
            };
        }
        if cache.len() >= MAX_REPLAY_ENTRIES {
            return Err(AdminAssertionError::RequestConflict);
        }
        cache.insert(
            assertion.request_id.clone(),
            ReplayEntry {
                payload_sha256: assertion.payload_sha256.clone(),
                expires_at_ms: assertion.expires_at_ms,
            },
        );
        Ok(())
    }
}

impl AdminOperationAssertion {
    fn validate_identifiers(&self) -> Result<(), AdminAssertionError> {
        for value in [
            &self.operation_id,
            &self.request_id,
            &self.trace_id,
            &self.issuer,
            &self.key_id,
            &self.actor_id,
            &self.permission,
            &self.service,
            &self.instance_id,
        ] {
            if !is_identifier(value) {
                return Err(AdminAssertionError::Unauthenticated);
            }
        }
        Ok(())
    }
}

fn verify_target_and_scope(
    assertion: &AdminOperationAssertion,
    expected_target_type: &str,
    service: &str,
    instance_id: &str,
) -> Result<(), AdminAssertionError> {
    let target = assertion.target.as_object().ok_or(AdminAssertionError::TargetDenied)?;
    let target_service = target.get("service").and_then(Value::as_str).unwrap_or_default();
    let target_instance = target.get("instanceId").and_then(Value::as_str).unwrap_or_default();
    let target_type = target.get("targetType").and_then(Value::as_str).unwrap_or_default();
    let target_world = target.get("worldId").and_then(Value::as_str).unwrap_or_default();
    let target_ids = string_array(target.get("targetIds")).ok_or(AdminAssertionError::TargetDenied)?;
    if target_service != service
        || target_instance != instance_id
        || target_type != expected_target_type
        || target_world.is_empty()
        || target_ids.is_empty()
    {
        return Err(AdminAssertionError::TargetDenied);
    }

    let scope = assertion.scope.as_object().ok_or(AdminAssertionError::TargetDenied)?;
    let matches_scope = scope_allows(scope, "worldIds", &[target_world])
        && scope_allows(scope, "serviceNames", &[target_service])
        && scope_allows(scope, "instanceIds", &[target_instance])
        && scope_allows(scope, "targetTypes", &[target_type])
        && scope_allows(scope, "targetIds", &target_ids)
        && scope
            .get("maxTargets")
            .and_then(Value::as_u64)
            .is_some_and(|limit| limit >= target_ids.len() as u64);
    matches_scope
        .then_some(())
        .ok_or(AdminAssertionError::TargetDenied)
}

fn scope_allows(scope: &serde_json::Map<String, Value>, key: &str, requested: &[&str]) -> bool {
    let Some(values) = string_array(scope.get(key)) else {
        return false;
    };
    requested
        .iter()
        .all(|value| values.contains(value) || values.contains(&"*"))
}

fn string_array(value: Option<&Value>) -> Option<Vec<&str>> {
    value?
        .as_array()?
        .iter()
        .map(Value::as_str)
        .collect::<Option<Vec<_>>>()
}

fn canonical_assertion_payload(assertion: &AdminOperationAssertion) -> String {
    let values = vec![
        assertion.version.to_string(),
        json_string(&assertion.operation_id),
        json_string(&assertion.request_id),
        json_string(&assertion.trace_id),
        json_string(&assertion.issuer),
        json_string(&assertion.key_id),
        json_string(&assertion.actor_id),
        json_string(&assertion.permission),
        canonical_json(&assertion.scope),
        canonical_json(&assertion.target),
        json_string(&assertion.service),
        json_string(&assertion.instance_id),
        assertion.issued_at_ms.to_string(),
        assertion.expires_at_ms.to_string(),
        json_string(&assertion.payload_sha256),
    ];
    format!("[{}]", values.join(","))
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => json_string(value),
        Value::Array(values) => format!(
            "[{}]",
            values.iter().map(canonical_json).collect::<Vec<_>>().join(",")
        ),
        Value::Object(values) => {
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            format!(
                "{{{}}}",
                entries
                    .into_iter()
                    .map(|(key, value)| format!("{}:{}", json_string(key), canonical_json(value)))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
    }
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

fn payload_sha256(payload: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(payload))
}

fn decode_public_key(encoded: &str) -> Option<VerifyingKey> {
    let bytes: [u8; 32] = decode_base64url(encoded)?.try_into().ok()?;
    VerifyingKey::from_bytes(&bytes).ok()
}

fn decode_base64url(value: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(value))
        .or_else(|_| base64::engine::general_purpose::STANDARD.decode(value))
        .ok()
}

fn is_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_IDENTIFIER_LEN
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'@' | b'-')
        })
}

fn unix_time_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use base64::Engine;
    use ed25519_dalek::{Signer, SigningKey};
    use serde_json::json;

    use super::*;
    use crate::protocol::{MessageType, Packet, PacketHeader};

    fn packet(payload: &[u8]) -> Packet {
        Packet::new(
            PacketHeader {
                msg_type: MessageType::GmSendItemReq as u16,
                seq: 1,
                body_len: payload.len() as u32,
            },
            payload.to_vec(),
        )
    }

    fn golden_fixture() -> serde_json::Value {
        serde_json::from_str(include_str!(
            "../../../../tests/fixtures/admin-operation-assertion-v1.json"
        ))
        .unwrap()
    }

    fn golden_verifier(fixture: &serde_json::Value) -> AdminAssertionVerifier {
        let mut keys = HashMap::new();
        keys.insert(
            fixture["key"]["keyId"].as_str().unwrap().to_string(),
            fixture["key"]["publicKeyBase64url"]
                .as_str()
                .unwrap()
                .to_string(),
        );
        AdminAssertionVerifier::new(
            fixture["key"]["issuer"].as_str().unwrap().to_string(),
            &keys,
            60_000,
        )
    }

    fn golden_assertion(case: &serde_json::Value) -> AdminOperationAssertion {
        serde_json::from_value(case["assertion"].clone()).unwrap()
    }

    fn fixture(payload: &[u8]) -> (AdminAssertionVerifier, AdminOperationAssertion) {
        let signing_key = SigningKey::from_bytes(&[17u8; 32]);
        let public_key = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(signing_key.verifying_key().as_bytes());
        let verifier = AdminAssertionVerifier::with_public_key("admin-api", "test-v1", &public_key);
        let now = unix_time_ms();
        let mut assertion = AdminOperationAssertion {
            version: ASSERTION_VERSION,
            operation_id: "op-test-1".to_string(),
            request_id: "req-test-1".to_string(),
            trace_id: "trace-test-1".to_string(),
            issuer: "admin-api".to_string(),
            key_id: "test-v1".to_string(),
            actor_id: "admin-7".to_string(),
            permission: "gm.send_item".to_string(),
            scope: json!({
                "worldIds": ["world-1"],
                "serviceNames": ["game-server"],
                "instanceIds": ["game-server-test"],
                "fieldAllowlist": ["*"],
                "targetTypes": ["character"],
                "targetIds": ["chr-1"],
                "maxTargets": 1
            }),
            target: json!({
                "service": "game-server",
                "instanceId": "game-server-test",
                "worldId": "world-1",
                "targetType": "character",
                "targetIds": ["chr-1"]
            }),
            service: "game-server".to_string(),
            instance_id: "game-server-test".to_string(),
            issued_at_ms: now - 1,
            expires_at_ms: now + 30_000,
            payload_sha256: payload_sha256(payload),
            signature: String::new(),
        };
        assertion.signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(signing_key.sign(canonical_assertion_payload(&assertion).as_bytes()).to_bytes());
        (verifier, assertion)
    }

    #[test]
    fn accepts_a_valid_signed_assertion_bound_to_the_payload_and_instance() {
        let payload = br#"{"characterId":"chr-1","itemId":"1001","itemCount":1}"#;
        let (verifier, assertion) = fixture(payload);

        let context = verifier
            .verify_for_packet(
                &assertion,
                &packet(payload),
                "gm.send_item",
                "character",
                "game-server",
                "game-server-test",
            )
            .unwrap();

        assert_eq!(context.actor, "admin-7");
        assert!(!context.actor_missing);
    }

    #[test]
    fn accepts_shared_node_signed_assertion_fixture_for_the_bound_packet() {
        let fixture = golden_fixture();
        let case = &fixture["cases"]["gameServerPacket"];
        let assertion = golden_assertion(case);
        let payload = case["packet"]["bodyUtf8"].as_str().unwrap().as_bytes();
        let verifier = golden_verifier(&fixture);

        assert_eq!(
            canonical_assertion_payload(&assertion),
            case["expectedCanonicalPayloadUtf8"].as_str().unwrap()
        );
        let context = verifier
            .verify_for_packet_at(
                &assertion,
                &packet(payload),
                "gm.send_item",
                "character",
                "game-server",
                "game-server-fixture",
                fixture["verificationNowMs"].as_i64().unwrap(),
            )
            .unwrap();

        assert_eq!(context.actor, "admin-7");
        assert!(!context.actor_missing);
    }

    #[test]
    fn shared_node_signed_assertion_fixture_rejects_payload_tampering_and_replay() {
        let fixture = golden_fixture();
        let case = &fixture["cases"]["gameServerPacket"];
        let assertion = golden_assertion(case);
        let payload = case["packet"]["bodyUtf8"].as_str().unwrap();
        let now = fixture["verificationNowMs"].as_i64().unwrap();

        assert_eq!(
            golden_verifier(&fixture).verify_for_packet_at(
                &assertion,
                &packet(br#"{\"characterId\":\"chr_fixture_002\",\"itemId\":\"1001\",\"itemCount\":2,\"reason\":\"fixture\"}"#),
                "gm.send_item",
                "character",
                "game-server",
                "game-server-fixture",
                now,
            ),
            Err(AdminAssertionError::PayloadMismatch)
        );

        let verifier = golden_verifier(&fixture);
        verifier
            .verify_for_packet_at(
                &assertion,
                &packet(payload.as_bytes()),
                "gm.send_item",
                "character",
                "game-server",
                "game-server-fixture",
                now,
            )
            .unwrap();
        assert_eq!(
            verifier.verify_for_packet_at(
                &assertion,
                &packet(payload.as_bytes()),
                "gm.send_item",
                "character",
                "game-server",
                "game-server-fixture",
                now,
            ),
            Err(AdminAssertionError::RequestReplay)
        );
    }

    #[test]
    fn rejects_expired_and_wrong_permission_assertions() {
        let payload = br#"{"characterId":"chr-1"}"#;
        let (verifier, mut expired) = fixture(payload);
        expired.expires_at_ms = unix_time_ms() - 1;
        assert_eq!(
            verifier.verify_for_packet(
                &expired,
                &packet(payload),
                "gm.send_item",
                "character",
                "game-server",
                "game-server-test",
            ),
            Err(AdminAssertionError::Expired)
        );

        let (verifier, assertion) = fixture(payload);
        assert_eq!(
            verifier.verify_for_packet(
                &assertion,
                &packet(payload),
                "gm.ban_player",
                "player",
                "game-server",
                "game-server-test",
            ),
            Err(AdminAssertionError::PermissionDenied)
        );
    }

    #[test]
    fn rejects_payload_or_target_tampering() {
        let payload = br#"{"characterId":"chr-1"}"#;
        let (verifier, assertion) = fixture(payload);
        assert_eq!(
            verifier.verify_for_packet(
                &assertion,
                &packet(br#"{"characterId":"chr-2"}"#),
                "gm.send_item",
                "character",
                "game-server",
                "game-server-test",
            ),
            Err(AdminAssertionError::PayloadMismatch)
        );

        let (verifier, assertion) = fixture(payload);
        assert_eq!(
            verifier.verify_for_packet(
                &assertion,
                &packet(payload),
                "gm.send_item",
                "character",
                "game-server",
                "game-server-other",
            ),
            Err(AdminAssertionError::TargetDenied)
        );
    }

    #[test]
    fn reserves_request_ids_against_replay_and_conflict() {
        let payload = br#"{"characterId":"chr-1"}"#;
        let (verifier, assertion) = fixture(payload);
        verifier
            .verify_for_packet(
                &assertion,
                &packet(payload),
                "gm.send_item",
                "character",
                "game-server",
                "game-server-test",
            )
            .unwrap();
        assert_eq!(
            verifier.verify_for_packet(
                &assertion,
                &packet(payload),
                "gm.send_item",
                "character",
                "game-server",
                "game-server-test",
            ),
            Err(AdminAssertionError::RequestReplay)
        );
    }
}
