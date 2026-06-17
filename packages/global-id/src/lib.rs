use std::env;
use std::fmt;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub const EPOCH_MS: u64 = 1_767_225_600_000;
pub const TIME_BITS: u8 = 41;
pub const ORIGIN_BITS: u8 = 10;
pub const WORKER_BITS: u8 = 6;
pub const SEQUENCE_BITS: u8 = 6;
pub const MAX_ORIGIN_ID: u16 = (1 << ORIGIN_BITS) - 1;
pub const MAX_WORKER_ID: u8 = (1 << WORKER_BITS) - 1;
pub const MAX_SEQUENCE: u8 = (1 << SEQUENCE_BITS) - 1;
pub const WORKER_SHIFT: u8 = SEQUENCE_BITS;
pub const ORIGIN_SHIFT: u8 = WORKER_BITS + SEQUENCE_BITS;
pub const TIME_SHIFT: u8 = ORIGIN_BITS + WORKER_BITS + SEQUENCE_BITS;
pub const MAX_CLOCK_BACKWARD_MS: u64 = 5;
pub const DEFAULT_WORKER_LEASE_TTL_SECONDS: u64 = 30;

const BASE32_ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlobalIdError {
    InvalidOriginId(String),
    InvalidWorkerId(String),
    ClockBeforeEpoch,
    ClockMovedBackward { last_ms: u64, now_ms: u64 },
    InvalidInput(String),
    InvalidPrefix(String),
    InvalidBase32(String),
    WorkerLeaseUnavailable(String),
}

impl fmt::Display for GlobalIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GlobalIdError::InvalidOriginId(value) => write!(f, "invalid origin id: {}", value),
            GlobalIdError::InvalidWorkerId(value) => write!(f, "invalid worker id: {}", value),
            GlobalIdError::ClockBeforeEpoch => write!(f, "system clock is before global id epoch"),
            GlobalIdError::ClockMovedBackward { last_ms, now_ms } => write!(
                f,
                "system clock moved backward beyond tolerance: last={}, now={}",
                last_ms, now_ms
            ),
            GlobalIdError::InvalidInput(value) => write!(f, "invalid global id input: {}", value),
            GlobalIdError::InvalidPrefix(value) => write!(f, "invalid global id prefix: {}", value),
            GlobalIdError::InvalidBase32(value) => write!(f, "invalid global id base32: {}", value),
            GlobalIdError::WorkerLeaseUnavailable(value) => {
                write!(f, "worker lease unavailable: {}", value)
            }
        }
    }
}

impl std::error::Error for GlobalIdError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedGlobalId {
    pub numeric_id: u64,
    pub time_ms: u64,
    pub unix_ms: u64,
    pub origin_id: u16,
    pub worker_id: u8,
    pub sequence: u8,
}

#[derive(Debug)]
struct GeneratorState {
    last_time_ms: u64,
    sequence: u8,
}

#[derive(Debug)]
pub struct GlobalIdGenerator {
    origin_id: u16,
    worker_id: u8,
    state: Mutex<GeneratorState>,
    lease_active: Option<Arc<AtomicBool>>,
    lease_key: Option<String>,
}

#[derive(Clone, Debug)]
pub struct WorkerLease {
    pub origin_id: u16,
    pub worker_id: u8,
    pub key: String,
    pub value: String,
    active: Arc<AtomicBool>,
    #[cfg(feature = "redis")]
    ttl_seconds: u64,
}

impl GlobalIdGenerator {
    pub fn new(origin_id: u16, worker_id: u8) -> Result<Self, GlobalIdError> {
        if origin_id > MAX_ORIGIN_ID {
            return Err(GlobalIdError::InvalidOriginId(origin_id.to_string()));
        }
        if worker_id > MAX_WORKER_ID {
            return Err(GlobalIdError::InvalidWorkerId(worker_id.to_string()));
        }

        Ok(Self {
            origin_id,
            worker_id,
            state: Mutex::new(GeneratorState {
                last_time_ms: 0,
                sequence: 0,
            }),
            lease_active: None,
            lease_key: None,
        })
    }

    fn with_worker_lease(worker_lease: &WorkerLease) -> Result<Self, GlobalIdError> {
        if worker_lease.origin_id > MAX_ORIGIN_ID {
            return Err(GlobalIdError::InvalidOriginId(
                worker_lease.origin_id.to_string(),
            ));
        }
        if worker_lease.worker_id > MAX_WORKER_ID {
            return Err(GlobalIdError::InvalidWorkerId(
                worker_lease.worker_id.to_string(),
            ));
        }

        Ok(Self {
            origin_id: worker_lease.origin_id,
            worker_id: worker_lease.worker_id,
            state: Mutex::new(GeneratorState {
                last_time_ms: 0,
                sequence: 0,
            }),
            lease_active: Some(worker_lease.active.clone()),
            lease_key: Some(worker_lease.key.clone()),
        })
    }

    pub fn from_env() -> Result<Self, GlobalIdError> {
        let origin_id = parse_origin_id_env()?;
        let worker_id = parse_worker_id_env()?;
        Self::new(origin_id, worker_id)
    }

    pub fn origin_id(&self) -> u16 {
        self.origin_id
    }

    pub fn worker_id(&self) -> u8 {
        self.worker_id
    }

    pub fn generate(&self) -> Result<u64, GlobalIdError> {
        self.ensure_lease_active()?;
        let mut state = self.state.lock().expect("global id mutex poisoned");
        loop {
            self.ensure_lease_active()?;
            let mut now_ms = current_relative_ms()?;
            if now_ms < state.last_time_ms {
                let drift = state.last_time_ms - now_ms;
                if drift <= MAX_CLOCK_BACKWARD_MS {
                    now_ms = state.last_time_ms;
                } else {
                    return Err(GlobalIdError::ClockMovedBackward {
                        last_ms: state.last_time_ms,
                        now_ms,
                    });
                }
            }

            if now_ms == state.last_time_ms {
                if state.sequence < MAX_SEQUENCE {
                    state.sequence += 1;
                } else {
                    wait_next_millis(now_ms)?;
                    continue;
                }
            } else {
                state.sequence = 0;
            }

            state.last_time_ms = now_ms;
            return Ok(compose_id(
                now_ms,
                self.origin_id,
                self.worker_id,
                state.sequence,
            ));
        }
    }

    pub fn generate_string(&self, prefix: &str) -> Result<String, GlobalIdError> {
        let id = self.generate()?;
        encode_with_prefix(prefix, id)
    }

    fn ensure_lease_active(&self) -> Result<(), GlobalIdError> {
        if let Some(active) = &self.lease_active {
            if !active.load(Ordering::SeqCst) {
                return Err(GlobalIdError::WorkerLeaseUnavailable(format!(
                    "worker lease is no longer active: {}",
                    self.lease_key.as_deref().unwrap_or("<unknown>")
                )));
            }
        }
        Ok(())
    }
}

pub fn worker_lease_key(origin_id: u16, worker_id: u8) -> String {
    format!("id:worker:{}:{}", origin_id, worker_id)
}

impl WorkerLease {
    pub fn generator(&self) -> Result<GlobalIdGenerator, GlobalIdError> {
        GlobalIdGenerator::with_worker_lease(self)
    }

    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    pub fn deactivate(&self) {
        self.mark_inactive();
    }

    fn mark_inactive(&self) {
        self.active.store(false, Ordering::SeqCst);
    }

    #[cfg(feature = "redis")]
    pub async fn acquire_redis(
        redis: &mut redis::aio::MultiplexedConnection,
        redis_key_prefix: &str,
        origin_id: u16,
        worker_id: Option<u8>,
        service_name: &str,
        service_instance_id: &str,
        ttl_seconds: u64,
    ) -> Result<Self, GlobalIdError> {
        if origin_id > MAX_ORIGIN_ID {
            return Err(GlobalIdError::InvalidOriginId(origin_id.to_string()));
        }
        if let Some(value) = worker_id {
            if value > MAX_WORKER_ID {
                return Err(GlobalIdError::InvalidWorkerId(value.to_string()));
            }
        }

        let candidates: Vec<u8> = match worker_id {
            Some(value) => vec![value],
            None => (0..=MAX_WORKER_ID).collect(),
        };
        let ttl = ttl_seconds.max(1);
        let token = format!(
            "{}:{}:{}:{}",
            service_name,
            service_instance_id,
            std::process::id(),
            current_unix_ms()
        );

        for candidate in candidates {
            let key = format!(
                "{}{}",
                redis_key_prefix,
                worker_lease_key(origin_id, candidate)
            );
            let value = format!(
                "{{\"token\":\"{}\",\"serviceName\":\"{}\",\"serviceInstanceId\":\"{}\",\"originId\":{},\"workerId\":{},\"pid\":{}}}",
                token,
                service_name,
                service_instance_id,
                origin_id,
                candidate,
                std::process::id()
            );
            let result: redis::RedisResult<Option<String>> = redis::cmd("SET")
                .arg(&key)
                .arg(&value)
                .arg("EX")
                .arg(ttl)
                .arg("NX")
                .query_async(redis)
                .await;

            match result {
                Ok(Some(_)) => {
                    return Ok(Self {
                        origin_id,
                        worker_id: candidate,
                        key,
                        value,
                        active: Arc::new(AtomicBool::new(true)),
                        #[cfg(feature = "redis")]
                        ttl_seconds: ttl,
                    });
                }
                Ok(None) => {}
                Err(error) => {
                    return Err(GlobalIdError::WorkerLeaseUnavailable(error.to_string()));
                }
            }
        }

        Err(GlobalIdError::WorkerLeaseUnavailable(format!(
            "origin={} worker={}",
            origin_id,
            worker_id
                .map(|value| value.to_string())
                .unwrap_or_else(|| "*".to_string())
        )))
    }

    #[cfg(feature = "redis")]
    pub async fn renew_redis(
        &self,
        redis: &mut redis::aio::MultiplexedConnection,
    ) -> redis::RedisResult<bool> {
        let script = r#"
if redis.call("GET", KEYS[1]) == ARGV[1] then
  redis.call("SET", KEYS[1], ARGV[1], "EX", tonumber(ARGV[2]))
  return 1
end
return 0
"#;
        let renewed: redis::RedisResult<i32> = redis::cmd("EVAL")
            .arg(script)
            .arg(1)
            .arg(&self.key)
            .arg(&self.value)
            .arg(self.ttl_seconds)
            .query_async(redis)
            .await;

        match renewed {
            Ok(1) => Ok(true),
            Ok(_) => {
                self.mark_inactive();
                Ok(false)
            }
            Err(error) => {
                self.mark_inactive();
                Err(error)
            }
        }
    }

    #[cfg(feature = "redis")]
    pub async fn release_redis(
        &self,
        redis: &mut redis::aio::MultiplexedConnection,
    ) -> redis::RedisResult<bool> {
        self.mark_inactive();
        let script = r#"
if redis.call("GET", KEYS[1]) == ARGV[1] then
  return redis.call("DEL", KEYS[1])
end
return 0
"#;
        let released: i32 = redis::cmd("EVAL")
            .arg(script)
            .arg(1)
            .arg(&self.key)
            .arg(&self.value)
            .query_async(redis)
            .await?;
        Ok(released == 1)
    }
}

pub fn last_timestamp_key(origin_id: u16, worker_id: u8) -> String {
    format!("id:last-ts:{}:{}", origin_id, worker_id)
}

pub fn origin_metadata_key(origin_id: u16) -> String {
    format!("id:origin:{}", origin_id)
}

pub fn parse_origin_id_env() -> Result<u16, GlobalIdError> {
    parse_origin_id(env::var("GLOBAL_ID_ORIGIN_ID").unwrap_or_else(|_| "0".to_string()))
}

pub fn parse_worker_id_env() -> Result<u8, GlobalIdError> {
    parse_worker_id(env::var("GLOBAL_ID_WORKER_ID").unwrap_or_else(|_| "0".to_string()))
}

pub fn parse_origin_id(value: impl AsRef<str>) -> Result<u16, GlobalIdError> {
    let raw = value.as_ref().trim();
    let parsed = raw
        .parse::<u16>()
        .map_err(|_| GlobalIdError::InvalidOriginId(raw.to_string()))?;
    if parsed > MAX_ORIGIN_ID {
        return Err(GlobalIdError::InvalidOriginId(raw.to_string()));
    }
    Ok(parsed)
}

pub fn parse_worker_id(value: impl AsRef<str>) -> Result<u8, GlobalIdError> {
    let raw = value.as_ref().trim();
    let parsed = raw
        .parse::<u8>()
        .map_err(|_| GlobalIdError::InvalidWorkerId(raw.to_string()))?;
    if parsed > MAX_WORKER_ID {
        return Err(GlobalIdError::InvalidWorkerId(raw.to_string()));
    }
    Ok(parsed)
}

pub fn compose_id(time_ms: u64, origin_id: u16, worker_id: u8, sequence: u8) -> u64 {
    (time_ms << TIME_SHIFT)
        | ((origin_id as u64) << ORIGIN_SHIFT)
        | ((worker_id as u64) << WORKER_SHIFT)
        | sequence as u64
}

pub fn decode_numeric(id: u64) -> DecodedGlobalId {
    let sequence = (id & ((1u64 << SEQUENCE_BITS) - 1)) as u8;
    let worker_id = ((id >> WORKER_SHIFT) & ((1u64 << WORKER_BITS) - 1)) as u8;
    let origin_id = ((id >> ORIGIN_SHIFT) & ((1u64 << ORIGIN_BITS) - 1)) as u16;
    let time_ms = id >> TIME_SHIFT;

    DecodedGlobalId {
        numeric_id: id,
        time_ms,
        unix_ms: EPOCH_MS + time_ms,
        origin_id,
        worker_id,
        sequence,
    }
}

pub fn encode_base32(mut value: u64) -> String {
    if value == 0 {
        return "0".to_string();
    }

    let mut chars = Vec::new();
    while value > 0 {
        let idx = (value & 31) as usize;
        chars.push(BASE32_ALPHABET[idx] as char);
        value >>= 5;
    }
    chars.iter().rev().collect()
}

pub fn decode_base32(value: &str) -> Result<u64, GlobalIdError> {
    let raw = value.trim().to_ascii_lowercase();
    if raw.is_empty() {
        return Err(GlobalIdError::InvalidBase32(value.to_string()));
    }

    let mut result = 0u64;
    for ch in raw.bytes() {
        let idx = match ch {
            b'0'..=b'9' => ch - b'0',
            b'a' => 10,
            b'b' => 11,
            b'c' => 12,
            b'd' => 13,
            b'e' => 14,
            b'f' => 15,
            b'g' => 16,
            b'h' => 17,
            b'j' => 18,
            b'k' => 19,
            b'm' => 20,
            b'n' => 21,
            b'p' => 22,
            b'q' => 23,
            b'r' => 24,
            b's' => 25,
            b't' => 26,
            b'v' => 27,
            b'w' => 28,
            b'x' => 29,
            b'y' => 30,
            b'z' => 31,
            _ => return Err(GlobalIdError::InvalidBase32(value.to_string())),
        } as u64;
        result = result
            .checked_mul(32)
            .and_then(|current| current.checked_add(idx))
            .ok_or_else(|| GlobalIdError::InvalidBase32(value.to_string()))?;
    }

    Ok(result)
}

pub fn encode_with_prefix(prefix: &str, id: u64) -> Result<String, GlobalIdError> {
    validate_prefix(prefix)?;
    Ok(format!("{}_{}", prefix, encode_base32(id)))
}

pub fn decode_string_id(value: &str) -> Result<(String, u64), GlobalIdError> {
    let raw = value.trim();
    let (prefix, encoded) = raw
        .split_once('_')
        .ok_or_else(|| GlobalIdError::InvalidInput(value.to_string()))?;
    validate_prefix(prefix)?;
    let id = decode_base32(encoded)?;
    Ok((prefix.to_string(), id))
}

fn validate_prefix(prefix: &str) -> Result<(), GlobalIdError> {
    if prefix.is_empty()
        || !prefix
            .bytes()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
    {
        return Err(GlobalIdError::InvalidPrefix(prefix.to_string()));
    }
    Ok(())
}

fn current_relative_ms() -> Result<u64, GlobalIdError> {
    let unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| GlobalIdError::ClockBeforeEpoch)?
        .as_millis() as u64;
    unix_ms
        .checked_sub(EPOCH_MS)
        .ok_or(GlobalIdError::ClockBeforeEpoch)
}

#[cfg(feature = "redis")]
fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as u64)
        .unwrap_or_default()
}

fn wait_next_millis(current_ms: u64) -> Result<(), GlobalIdError> {
    while current_relative_ms()? <= current_ms {
        std::thread::yield_now();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_and_decode_round_trip() {
        let id = compose_id(123, 45, 6, 7);
        let decoded = decode_numeric(id);
        assert_eq!(decoded.time_ms, 123);
        assert_eq!(decoded.origin_id, 45);
        assert_eq!(decoded.worker_id, 6);
        assert_eq!(decoded.sequence, 7);
    }

    #[test]
    fn base32_round_trip() {
        let id = compose_id(987_654, 12, 3, 4);
        let encoded = encode_base32(id);
        assert_eq!(decode_base32(&encoded).unwrap(), id);
    }

    #[test]
    fn prefixed_round_trip() {
        let id = compose_id(1, 2, 3, 4);
        let encoded = encode_with_prefix("plr", id).unwrap();
        let (prefix, decoded) = decode_string_id(&encoded).unwrap();
        assert_eq!(prefix, "plr");
        assert_eq!(decoded, id);
    }

    #[test]
    fn generator_increments_sequence() {
        let generator = GlobalIdGenerator::new(0, 1).unwrap();
        let first = decode_numeric(generator.generate().unwrap());
        let second = decode_numeric(generator.generate().unwrap());
        assert!(second.numeric_id > first.numeric_id);
        assert_eq!(second.origin_id, 0);
        assert_eq!(second.worker_id, 1);
    }

    #[test]
    fn redis_key_helpers_use_ids() {
        assert_eq!(worker_lease_key(1, 2), "id:worker:1:2");
        assert_eq!(last_timestamp_key(1, 2), "id:last-ts:1:2");
        assert_eq!(origin_metadata_key(1), "id:origin:1");
    }

    #[test]
    fn worker_lease_generator_rejects_out_of_range_claims() {
        let lease = WorkerLease {
            origin_id: MAX_ORIGIN_ID,
            worker_id: MAX_WORKER_ID.saturating_add(1),
            key: "lease".to_string(),
            value: "value".to_string(),
            active: Arc::new(AtomicBool::new(true)),
            #[cfg(feature = "redis")]
            ttl_seconds: DEFAULT_WORKER_LEASE_TTL_SECONDS,
        };

        assert!(matches!(
            lease.generator(),
            Err(GlobalIdError::InvalidWorkerId(_))
        ));
    }

    #[test]
    fn worker_lease_builds_generator_with_claimed_ids() {
        let lease = WorkerLease {
            origin_id: 7,
            worker_id: 8,
            key: worker_lease_key(7, 8),
            value: "lease-token".to_string(),
            active: Arc::new(AtomicBool::new(true)),
            #[cfg(feature = "redis")]
            ttl_seconds: DEFAULT_WORKER_LEASE_TTL_SECONDS,
        };
        let generated = decode_numeric(lease.generator().unwrap().generate().unwrap());

        assert_eq!(generated.origin_id, 7);
        assert_eq!(generated.worker_id, 8);
    }

    #[test]
    fn worker_lease_generator_rejects_after_lease_becomes_inactive() {
        let lease = WorkerLease {
            origin_id: 7,
            worker_id: 8,
            key: worker_lease_key(7, 8),
            value: "lease-token".to_string(),
            active: Arc::new(AtomicBool::new(true)),
            #[cfg(feature = "redis")]
            ttl_seconds: DEFAULT_WORKER_LEASE_TTL_SECONDS,
        };
        let generator = lease.generator().unwrap();

        assert!(generator.generate().is_ok());
        lease.mark_inactive();
        assert!(matches!(
            generator.generate(),
            Err(GlobalIdError::WorkerLeaseUnavailable(_))
        ));
    }
}
