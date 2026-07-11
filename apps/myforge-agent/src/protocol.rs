use std::cmp::Ordering;
use std::collections::HashSet;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

pub const PROTOCOL_VERSION: i64 = 1;
pub const SUBPROTOCOL: &str = "myserver.myforge.v1";
pub const SIGNING_PREFIX: &[u8] = b"MYFORGE-WS-V1\n";
pub const QUEUE_CAPACITY: usize = 64;
pub const RESULT_FIXED_RESERVE_BYTES: u64 = 262_144;
pub const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtocolError {
    code: &'static str,
    message: String,
    safe_to_respond: bool,
    request_id: Option<String>,
}

impl ProtocolError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            safe_to_respond: true,
            request_id: None,
        }
    }

    pub fn unsafe_response(mut self) -> Self {
        self.safe_to_respond = false;
        self
    }

    pub fn with_request_id(mut self, request_id: Option<String>) -> Self {
        self.request_id = request_id;
        self
    }

    pub const fn code(&self) -> &'static str {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub const fn safe_to_respond(&self) -> bool {
        self.safe_to_respond
    }

    pub fn request_id(&self) -> Option<&str> {
        self.request_id.as_deref()
    }

    fn ijson(message: impl Into<String>) -> Self {
        Self::new("MYFORGE_MESSAGE_IJSON_INVALID", message).unsafe_response()
    }

    fn schema(message: impl Into<String>) -> Self {
        Self::new("MYFORGE_MESSAGE_SCHEMA_INVALID", message)
    }
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ProtocolError {}

#[derive(Clone, Eq, PartialEq)]
pub enum JsonValue {
    Null,
    Bool(bool),
    Integer(i64),
    String(String),
    Array(Vec<JsonValue>),
    Object(Vec<(String, JsonValue)>),
}

impl std::fmt::Debug for JsonValue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Null => formatter.write_str("JsonValue::Null"),
            Self::Bool(_) => formatter.write_str("JsonValue::Bool([REDACTED])"),
            Self::Integer(_) => formatter.write_str("JsonValue::Integer([REDACTED])"),
            Self::String(value) => formatter
                .debug_struct("JsonValue::String")
                .field("utf8_bytes", &value.len())
                .finish(),
            Self::Array(values) => formatter
                .debug_struct("JsonValue::Array")
                .field("length", &values.len())
                .finish(),
            Self::Object(fields) => formatter
                .debug_struct("JsonValue::Object")
                .field(
                    "fields",
                    &fields.iter().map(|(key, _)| key).collect::<Vec<_>>(),
                )
                .finish(),
        }
    }
}

impl JsonValue {
    pub fn object_field(&self, name: &str) -> Option<&JsonValue> {
        let Self::Object(fields) = self else {
            return None;
        };
        fields
            .iter()
            .find_map(|(key, value)| (key == name).then_some(value))
    }

    pub fn string_field(&self, name: &str) -> Option<&str> {
        match self.object_field(name) {
            Some(Self::String(value)) => Some(value),
            _ => None,
        }
    }

    pub fn has_exact_object_fields(&self, expected: &[&str]) -> bool {
        let Self::Object(fields) = self else {
            return false;
        };
        fields.len() == expected.len()
            && expected
                .iter()
                .all(|expected| fields.iter().any(|(actual, _)| actual == expected))
    }

    pub fn remove_top_level(&self, names: &HashSet<&str>) -> Result<Self, ProtocolError> {
        let Self::Object(fields) = self else {
            return Err(ProtocolError::schema("message must be an object"));
        };
        Ok(Self::Object(
            fields
                .iter()
                .filter(|(key, _)| !names.contains(key.as_str()))
                .cloned()
                .collect(),
        ))
    }

    pub fn insert_top_level(
        &mut self,
        name: impl Into<String>,
        value: JsonValue,
    ) -> Result<(), ProtocolError> {
        let Self::Object(fields) = self else {
            return Err(ProtocolError::schema("message must be an object"));
        };
        let name = name.into();
        if fields.iter().any(|(key, _)| key == &name) {
            return Err(ProtocolError::schema("message field already exists"));
        }
        fields.push((name, value));
        Ok(())
    }

    fn to_serde(&self) -> serde_json::Value {
        match self {
            Self::Null => serde_json::Value::Null,
            Self::Bool(value) => serde_json::Value::Bool(*value),
            Self::Integer(value) => serde_json::Value::Number((*value).into()),
            Self::String(value) => serde_json::Value::String(value.clone()),
            Self::Array(values) => {
                serde_json::Value::Array(values.iter().map(Self::to_serde).collect())
            }
            Self::Object(fields) => serde_json::Value::Object(
                fields
                    .iter()
                    .map(|(key, value)| (key.clone(), value.to_serde()))
                    .collect(),
            ),
        }
    }

    fn from_serde(value: serde_json::Value) -> Result<Self, ProtocolError> {
        match value {
            serde_json::Value::Null => Ok(Self::Null),
            serde_json::Value::Bool(value) => Ok(Self::Bool(value)),
            serde_json::Value::String(value) => Ok(Self::String(value)),
            serde_json::Value::Array(values) => values
                .into_iter()
                .map(Self::from_serde)
                .collect::<Result<_, _>>()
                .map(Self::Array),
            serde_json::Value::Object(fields) => fields
                .into_iter()
                .map(|(key, value)| Ok((key, Self::from_serde(value)?)))
                .collect::<Result<_, _>>()
                .map(Self::Object),
            serde_json::Value::Number(number) => {
                let value = number
                    .as_i64()
                    .ok_or_else(|| ProtocolError::ijson("JSON numbers must be safe integers"))?;
                if value.unsigned_abs() > MAX_SAFE_INTEGER as u64 {
                    return Err(ProtocolError::ijson(
                        "JSON integer is outside the interoperable range",
                    ));
                }
                Ok(Self::Integer(value))
            }
        }
    }
}

struct StrictJsonParser<'a> {
    source: &'a str,
    bytes: &'a [u8],
    index: usize,
}

impl<'a> StrictJsonParser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            bytes: source.as_bytes(),
            index: 0,
        }
    }

    fn parse(mut self) -> Result<JsonValue, ProtocolError> {
        self.skip_whitespace();
        let value = self.parse_value(0)?;
        self.skip_whitespace();
        if self.index != self.bytes.len() {
            return Err(ProtocolError::ijson("JSON contains trailing data"));
        }
        Ok(value)
    }

    fn parse_value(&mut self, depth: usize) -> Result<JsonValue, ProtocolError> {
        if depth > 64 {
            return Err(ProtocolError::ijson(
                "JSON nesting exceeds the protocol limit",
            ));
        }
        match self.peek() {
            Some(b'{') => self.parse_object(depth),
            Some(b'[') => self.parse_array(depth),
            Some(b'"') => self.parse_string().map(JsonValue::String),
            Some(b't') => self.parse_literal(b"true", JsonValue::Bool(true)),
            Some(b'f') => self.parse_literal(b"false", JsonValue::Bool(false)),
            Some(b'n') => self.parse_literal(b"null", JsonValue::Null),
            Some(b'-' | b'0'..=b'9') => self.parse_number().map(JsonValue::Integer),
            _ => Err(ProtocolError::ijson("JSON contains an invalid value")),
        }
    }

    fn parse_literal(
        &mut self,
        literal: &[u8],
        value: JsonValue,
    ) -> Result<JsonValue, ProtocolError> {
        if self.bytes.get(self.index..self.index + literal.len()) == Some(literal) {
            self.index += literal.len();
            Ok(value)
        } else {
            Err(ProtocolError::ijson("JSON contains an invalid value"))
        }
    }

    fn parse_object(&mut self, depth: usize) -> Result<JsonValue, ProtocolError> {
        self.index += 1;
        self.skip_whitespace();
        let mut fields = Vec::new();
        let mut keys = HashSet::new();
        if self.consume(b'}') {
            return Ok(JsonValue::Object(fields));
        }
        loop {
            if self.peek() != Some(b'"') {
                return Err(ProtocolError::ijson("JSON object key must be a string"));
            }
            let key = self.parse_string()?;
            if !keys.insert(key.clone()) {
                return Err(ProtocolError::ijson("JSON contains a duplicate object key"));
            }
            self.skip_whitespace();
            if !self.consume(b':') {
                return Err(ProtocolError::ijson("JSON object is missing ':'"));
            }
            self.skip_whitespace();
            let value = self.parse_value(depth + 1)?;
            fields.push((key, value));
            self.skip_whitespace();
            if self.consume(b'}') {
                return Ok(JsonValue::Object(fields));
            }
            if !self.consume(b',') {
                return Err(ProtocolError::ijson("JSON object is missing ','"));
            }
            self.skip_whitespace();
        }
    }

    fn parse_array(&mut self, depth: usize) -> Result<JsonValue, ProtocolError> {
        self.index += 1;
        self.skip_whitespace();
        let mut values = Vec::new();
        if self.consume(b']') {
            return Ok(JsonValue::Array(values));
        }
        loop {
            values.push(self.parse_value(depth + 1)?);
            self.skip_whitespace();
            if self.consume(b']') {
                return Ok(JsonValue::Array(values));
            }
            if !self.consume(b',') {
                return Err(ProtocolError::ijson("JSON array is missing ','"));
            }
            self.skip_whitespace();
        }
    }

    fn parse_string(&mut self) -> Result<String, ProtocolError> {
        self.index += 1;
        let mut result = String::new();
        while let Some(byte) = self.peek() {
            match byte {
                b'"' => {
                    self.index += 1;
                    return Ok(result);
                }
                b'\\' => {
                    self.index += 1;
                    self.parse_escape(&mut result)?;
                }
                0..=0x1f => {
                    return Err(ProtocolError::ijson(
                        "JSON string contains an unescaped control character",
                    ));
                }
                0x20..=0x7f => {
                    result.push(byte as char);
                    self.index += 1;
                }
                _ => {
                    let character = self.source[self.index..]
                        .chars()
                        .next()
                        .ok_or_else(|| ProtocolError::ijson("JSON string is invalid"))?;
                    result.push(character);
                    self.index += character.len_utf8();
                }
            }
        }
        Err(ProtocolError::ijson("JSON string is not terminated"))
    }

    fn parse_escape(&mut self, output: &mut String) -> Result<(), ProtocolError> {
        let escaped = self
            .peek()
            .ok_or_else(|| ProtocolError::ijson("JSON string contains an invalid escape"))?;
        self.index += 1;
        match escaped {
            b'"' => output.push('"'),
            b'\\' => output.push('\\'),
            b'/' => output.push('/'),
            b'b' => output.push('\u{0008}'),
            b'f' => output.push('\u{000c}'),
            b'n' => output.push('\n'),
            b'r' => output.push('\r'),
            b't' => output.push('\t'),
            b'u' => self.parse_unicode_escape(output)?,
            _ => {
                return Err(ProtocolError::ijson(
                    "JSON string contains an invalid escape",
                ));
            }
        }
        Ok(())
    }

    fn parse_unicode_escape(&mut self, output: &mut String) -> Result<(), ProtocolError> {
        let first = self.parse_hex_quad()?;
        let scalar = if (0xd800..=0xdbff).contains(&first) {
            if self.bytes.get(self.index..self.index + 2) != Some(b"\\u") {
                return Err(ProtocolError::ijson("JSON contains a lone surrogate"));
            }
            self.index += 2;
            let second = self.parse_hex_quad()?;
            if !(0xdc00..=0xdfff).contains(&second) {
                return Err(ProtocolError::ijson("JSON contains a lone surrogate"));
            }
            0x1_0000 + (((first - 0xd800) as u32) << 10) + (second - 0xdc00) as u32
        } else if (0xdc00..=0xdfff).contains(&first) {
            return Err(ProtocolError::ijson("JSON contains a lone surrogate"));
        } else {
            first as u32
        };
        output.push(
            char::from_u32(scalar)
                .ok_or_else(|| ProtocolError::ijson("JSON contains an invalid scalar"))?,
        );
        Ok(())
    }

    fn parse_hex_quad(&mut self) -> Result<u16, ProtocolError> {
        let bytes = self
            .bytes
            .get(self.index..self.index + 4)
            .ok_or_else(|| ProtocolError::ijson("JSON string has an invalid Unicode escape"))?;
        let mut value = 0_u16;
        for byte in bytes {
            let digit = match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                b'A'..=b'F' => byte - b'A' + 10,
                _ => {
                    return Err(ProtocolError::ijson(
                        "JSON string has an invalid Unicode escape",
                    ));
                }
            };
            value = (value << 4) | u16::from(digit);
        }
        self.index += 4;
        Ok(value)
    }

    fn parse_number(&mut self) -> Result<i64, ProtocolError> {
        let start = self.index;
        while let Some(byte) = self.peek() {
            if matches!(byte, b'\t' | b'\n' | b'\r' | b' ' | b',' | b']' | b'}') {
                break;
            }
            self.index += 1;
        }
        let token = &self.source[start..self.index];
        let bytes = token.as_bytes();
        let canonical = bytes == b"0"
            || (bytes.first() == Some(&b'-')
                && bytes.get(1).is_some_and(|byte| matches!(byte, b'1'..=b'9'))
                && bytes[2..].iter().all(u8::is_ascii_digit))
            || (bytes
                .first()
                .is_some_and(|byte| matches!(byte, b'1'..=b'9'))
                && bytes[1..].iter().all(u8::is_ascii_digit));
        if !canonical {
            return Err(ProtocolError::ijson(
                "JSON numbers must be canonical safe integers",
            ));
        }
        let value = token
            .parse::<i64>()
            .map_err(|_| ProtocolError::ijson("JSON integer is outside the interoperable range"))?;
        if value.unsigned_abs() > MAX_SAFE_INTEGER as u64 {
            return Err(ProtocolError::ijson(
                "JSON integer is outside the interoperable range",
            ));
        }
        Ok(value)
    }

    fn skip_whitespace(&mut self) {
        while self
            .peek()
            .is_some_and(|byte| matches!(byte, b'\t' | b'\n' | b'\r' | b' '))
        {
            self.index += 1;
        }
    }

    fn consume(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.index).copied()
    }
}

pub fn parse_strict_json(frame: &[u8], max_bytes: usize) -> Result<JsonValue, ProtocolError> {
    if frame.len() > max_bytes {
        return Err(ProtocolError::new(
            "MYFORGE_OUTPUT_TOO_LARGE",
            "WebSocket frame exceeds the configured limit",
        )
        .unsafe_response());
    }
    let source = std::str::from_utf8(frame)
        .map_err(|_| ProtocolError::ijson("WebSocket frame is not valid UTF-8"))?;
    StrictJsonParser::new(source).parse()
}

pub fn parse_canonical_frame(frame: &[u8], max_bytes: usize) -> Result<JsonValue, ProtocolError> {
    let value = parse_strict_json(frame, max_bytes)?;
    if canonicalize(&value).as_bytes() != frame {
        return Err(ProtocolError::ijson(
            "WebSocket text frame must use canonical JCS encoding",
        ));
    }
    Ok(value)
}

fn utf16_cmp(left: &str, right: &str) -> Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}

pub fn canonicalize(value: &JsonValue) -> String {
    match value {
        JsonValue::Null => "null".to_string(),
        JsonValue::Bool(true) => "true".to_string(),
        JsonValue::Bool(false) => "false".to_string(),
        JsonValue::Integer(value) => value.to_string(),
        JsonValue::String(value) => canonical_string(value),
        JsonValue::Array(values) => {
            let values = values.iter().map(canonicalize).collect::<Vec<_>>();
            format!("[{}]", values.join(","))
        }
        JsonValue::Object(fields) => {
            let mut fields = fields.iter().collect::<Vec<_>>();
            fields.sort_by(|(left, _), (right, _)| utf16_cmp(left, right));
            let fields = fields
                .into_iter()
                .map(|(key, value)| format!("{}:{}", canonical_string(key), canonicalize(value)))
                .collect::<Vec<_>>();
            format!("{{{}}}", fields.join(","))
        }
    }
}

fn canonical_string(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{0008}' => output.push_str("\\b"),
            '\u{0009}' => output.push_str("\\t"),
            '\u{000a}' => output.push_str("\\n"),
            '\u{000c}' => output.push_str("\\f"),
            '\u{000d}' => output.push_str("\\r"),
            '\u{0000}'..='\u{001f}' => {
                use std::fmt::Write;
                let _ = write!(output, "\\u{:04x}", character as u32);
            }
            _ => output.push(character),
        }
    }
    output.push('"');
    output
}

pub fn from_serializable(value: &impl Serialize) -> Result<JsonValue, ProtocolError> {
    let value = serde_json::to_value(value)
        .map_err(|_| ProtocolError::schema("message cannot be serialized"))?;
    JsonValue::from_serde(value)
}

pub fn deserialize<T: DeserializeOwned>(value: &JsonValue) -> Result<T, ProtocolError> {
    serde_json::from_value(value.to_serde())
        .map_err(|_| ProtocolError::schema("message schema is invalid"))
}

pub fn signing_bytes(message: &JsonValue) -> Result<Vec<u8>, ProtocolError> {
    let unsigned = message.remove_top_level(&HashSet::from(["signature"]))?;
    let canonical = canonicalize(&unsigned);
    let mut bytes = Vec::with_capacity(SIGNING_PREFIX.len() + canonical.len());
    bytes.extend_from_slice(SIGNING_PREFIX);
    bytes.extend_from_slice(canonical.as_bytes());
    Ok(bytes)
}

pub fn sign_message(
    unsigned: &impl Serialize,
    signing_key: &SigningKey,
) -> Result<String, ProtocolError> {
    let mut value = from_serializable(unsigned)?;
    if value.object_field("signature").is_some() {
        return Err(ProtocolError::schema(
            "unsigned message must not include signature",
        ));
    }
    let signature = signing_key.sign(&signing_bytes(&value)?);
    value.insert_top_level(
        "signature",
        JsonValue::String(URL_SAFE_NO_PAD.encode(signature.to_bytes())),
    )?;
    Ok(canonicalize(&value))
}

pub fn verify_message_signature(
    message: &JsonValue,
    verifying_key: &VerifyingKey,
) -> Result<(), ProtocolError> {
    let encoded = message.string_field("signature").ok_or_else(|| {
        ProtocolError::new(
            "MYFORGE_SERVER_SIGNATURE_INVALID",
            "server message signature is invalid",
        )
    })?;
    if encoded.is_empty()
        || encoded.contains('=')
        || !encoded
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(ProtocolError::new(
            "MYFORGE_SERVER_SIGNATURE_INVALID",
            "server message signature is invalid",
        ));
    }
    let decoded = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| {
        ProtocolError::new(
            "MYFORGE_SERVER_SIGNATURE_INVALID",
            "server message signature is invalid",
        )
    })?;
    if decoded.len() != 64 || URL_SAFE_NO_PAD.encode(&decoded) != encoded {
        return Err(ProtocolError::new(
            "MYFORGE_SERVER_SIGNATURE_INVALID",
            "server message signature is invalid",
        ));
    }
    let signature = Signature::from_slice(&decoded).map_err(|_| {
        ProtocolError::new(
            "MYFORGE_SERVER_SIGNATURE_INVALID",
            "server message signature is invalid",
        )
    })?;
    verifying_key
        .verify(&signing_bytes(message)?, &signature)
        .map_err(|_| {
            ProtocolError::new(
                "MYFORGE_SERVER_SIGNATURE_INVALID",
                "server message signature is invalid",
            )
        })
}

pub fn strict_base64url(
    value: &str,
    expected_bytes: usize,
    field: &str,
) -> Result<Vec<u8>, ProtocolError> {
    if value.is_empty()
        || value.contains('=')
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(ProtocolError::schema(format!(
            "{field} must be unpadded base64url"
        )));
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| ProtocolError::schema(format!("{field} has an invalid encoding")))?;
    if decoded.len() != expected_bytes || URL_SAFE_NO_PAD.encode(&decoded) != value {
        return Err(ProtocolError::schema(format!(
            "{field} has an invalid length or encoding"
        )));
    }
    Ok(decoded)
}

pub fn random_base64url<const N: usize>() -> String {
    URL_SAFE_NO_PAD.encode(rand::random::<[u8; N]>())
}

pub fn semantic_digest(message: &JsonValue) -> Result<String, ProtocolError> {
    let semantic = message.remove_top_level(&HashSet::from([
        "signature",
        "timestampMs",
        "expiresAtMs",
        "nonce",
    ]))?;
    Ok(format!("{:x}", Sha256::digest(canonicalize(&semantic))))
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::pkcs8::{DecodePrivateKey, DecodePublicKey};

    use super::*;

    const PRIVATE_KEY: &str = "-----BEGIN PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEIJ1hsZ3v/VpguoRK9JLsLMREScVpezJpGXA7rAMcrn9g\n-----END PRIVATE KEY-----\n";
    const PUBLIC_KEY: &str = "-----BEGIN PUBLIC KEY-----\nMCowBQYDK2VwAyEA11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo=\n-----END PUBLIC KEY-----\n";
    const UNSIGNED: &str = r#"{"protocolVersion":1,"type":"protocol.error","connectionId":"67da7da9-a653-4d6e-9e81-f5f8baf874bb","agentId":"dev-pc-001","projectId":"myforge-local","requestId":null,"errorCode":"MYFORGE_TEST_VECTOR","errorMessage":"中文😀","fatal":true,"meta":{"z":null,"a":["escaped",7]},"timestampMs":1783694421000,"expiresAtMs":1783694481000,"nonce":"AAECAwQFBgcICQoLDA0ODw"}"#;
    const SIGNING_HEX: &str = "4d59464f5247452d57532d56310a7b226167656e744964223a226465762d70632d303031222c22636f6e6e656374696f6e4964223a2236376461376461392d613635332d346436652d396538312d663566386261663837346262222c226572726f72436f6465223a224d59464f5247455f544553545f564543544f52222c226572726f724d657373616765223a22e4b8ade69687f09f9880222c226578706972657341744d73223a313738333639343438313030302c22666174616c223a747275652c226d657461223a7b2261223a5b2265736361706564222c375d2c227a223a6e756c6c7d2c226e6f6e6365223a2241414543417751464267634943516f4c4441304f4477222c2270726f6a6563744964223a226d79666f7267652d6c6f63616c222c2270726f746f636f6c56657273696f6e223a312c22726571756573744964223a6e756c6c2c2274696d657374616d704d73223a313738333639343432313030302c2274797065223a2270726f746f636f6c2e6572726f72227d";
    const SIGNATURE: &str =
        "Z821v68j79iokjA0Pgj8_UkU3A53Py2BDfnXkcP-lxxygukCeA7L6yuoHpdZ6VpV5MXk93ecNoV5AqnHnb_BAw";

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn matches_node_signing_bytes_and_signature_vector_exactly() {
        let unsigned = parse_strict_json(UNSIGNED.as_bytes(), usize::MAX).unwrap();
        assert_eq!(hex(&signing_bytes(&unsigned).unwrap()), SIGNING_HEX);

        let key = SigningKey::from_pkcs8_pem(PRIVATE_KEY).unwrap();
        let frame = sign_message(&unsigned.to_serde(), &key).unwrap();
        let signed = parse_canonical_frame(frame.as_bytes(), usize::MAX).unwrap();
        assert_eq!(signed.string_field("signature"), Some(SIGNATURE));

        let public = VerifyingKey::from_public_key_pem(PUBLIC_KEY).unwrap();
        verify_message_signature(&signed, &public).unwrap();
    }

    #[test]
    fn rejects_duplicate_keys_invalid_numbers_surrogates_and_noncanonical_frames() {
        for source in [
            r#"{"a":1,"a":2}"#,
            r#"{"a":{"b":1,"b":2}}"#,
            r#"{"a":-0}"#,
            r#"{"a":1.0}"#,
            r#"{"a":1e2}"#,
            r#"{"a":9007199254740992}"#,
            r#"{"a":"\ud800"}"#,
            r#"{"a":"\udc00"}"#,
        ] {
            assert_eq!(
                parse_strict_json(source.as_bytes(), usize::MAX)
                    .unwrap_err()
                    .code(),
                "MYFORGE_MESSAGE_IJSON_INVALID"
            );
        }

        let canonical = r#"{"a":"😀","b":1}"#;
        parse_canonical_frame(canonical.as_bytes(), usize::MAX).unwrap();
        assert!(parse_canonical_frame(br#" {"a":"\ud83d\ude00","b":1}"#, usize::MAX).is_err());
        assert!(parse_strict_json(&[0xff], usize::MAX).is_err());
    }

    #[test]
    fn jcs_orders_property_names_by_utf16_code_units() {
        let value = JsonValue::Object(vec![
            ("\u{1f600}".to_string(), JsonValue::Integer(1)),
            ("\u{e000}".to_string(), JsonValue::Integer(2)),
        ]);
        assert_eq!(canonicalize(&value), "{\"😀\":1,\"\":2}");
    }

    #[test]
    fn escaped_and_unescaped_unicode_have_identical_jcs() {
        let escaped = parse_strict_json(br#"{"value":"\u4e2d\ud83d\ude00"}"#, usize::MAX).unwrap();
        let unescaped = parse_strict_json("{\"value\":\"中😀\"}".as_bytes(), usize::MAX).unwrap();
        assert_eq!(canonicalize(&escaped), canonicalize(&unescaped));
        assert_eq!(canonicalize(&escaped), "{\"value\":\"中😀\"}");
    }

    #[test]
    fn debug_projection_never_contains_wire_values() {
        let value = parse_strict_json(
            br#"{"renderedPrompt":"secret-prompt","signature":"secret-signature","nested":{"input":"secret-input"}}"#,
            usize::MAX,
        )
        .unwrap();
        let debug = format!("{value:?}");
        assert!(debug.contains("renderedPrompt"));
        assert!(!debug.contains("secret-prompt"));
        assert!(!debug.contains("secret-signature"));
        assert!(!debug.contains("secret-input"));
    }

    #[test]
    fn malformed_signature_is_classified_as_server_signature_failure() {
        let message = parse_canonical_frame(br#"{"signature":"AAAA="}"#, usize::MAX).unwrap();
        let public = VerifyingKey::from_public_key_pem(PUBLIC_KEY).unwrap();
        assert_eq!(
            verify_message_signature(&message, &public)
                .unwrap_err()
                .code(),
            "MYFORGE_SERVER_SIGNATURE_INVALID"
        );
    }

    #[test]
    fn signature_covers_identity_business_time_and_nonce_fields() {
        let key = SigningKey::from_pkcs8_pem(PRIVATE_KEY).unwrap();
        let public = VerifyingKey::from_public_key_pem(PUBLIC_KEY).unwrap();
        let unsigned = parse_strict_json(UNSIGNED.as_bytes(), usize::MAX).unwrap();
        let frame = sign_message(&unsigned.to_serde(), &key).unwrap();
        let signed = parse_canonical_frame(frame.as_bytes(), usize::MAX).unwrap();

        for (field, replacement) in [
            (
                "connectionId",
                JsonValue::String("2d0465b1-dc92-46d2-bc45-c90ed9724f5a".to_string()),
            ),
            ("timestampMs", JsonValue::Integer(1_783_694_421_001)),
            (
                "nonce",
                JsonValue::String("AQECAwQFBgcICQoLDA0ODw".to_string()),
            ),
            ("errorMessage", JsonValue::String("modified".to_string())),
        ] {
            let mut modified = signed.clone();
            let JsonValue::Object(fields) = &mut modified else {
                unreachable!();
            };
            fields.iter_mut().find(|(name, _)| name == field).unwrap().1 = replacement;
            assert_eq!(
                verify_message_signature(&modified, &public)
                    .unwrap_err()
                    .code(),
                "MYFORGE_SERVER_SIGNATURE_INVALID"
            );
        }
    }
}
