use std::collections::HashMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::config::ConfigError;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScenarioStep {
    pub name: String,
    pub timeout_ms: u64,
    pub think_time_ms: u64,
    pub expected: ExpectedResponse,
    pub idempotency: Idempotency,
    pub retry: RetryPolicy,
}

impl ScenarioStep {
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.name.trim().is_empty() {
            return Err(ConfigError::Rejected(
                "scenario step name must not be empty".into(),
            ));
        }
        if self.timeout_ms == 0 {
            return Err(ConfigError::Rejected(format!(
                "step {} has zero timeout",
                self.name
            )));
        }
        if matches!(self.idempotency, Idempotency::Write)
            && !matches!(self.retry, RetryPolicy::Never)
        {
            return Err(ConfigError::Rejected(format!(
                "write step {} must not retry automatically",
                self.name
            )));
        }
        if let RetryPolicy::Bounded { attempts } = self.retry {
            if attempts < 2 || attempts > 3 {
                return Err(ConfigError::Rejected(format!(
                    "step {} retry attempts must be 2..=3",
                    self.name
                )));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpectedResponse {
    Success,
    HttpStatus { code: u16 },
    BusinessCode { code: String },
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ExpectedResponseRef<'a> {
    Success {},
    HttpStatus { code: u16 },
    BusinessCode { code: &'a str },
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ExpectedResponseWire {
    Success {},
    HttpStatus { code: u16 },
    BusinessCode { code: String },
}

impl Serialize for ExpectedResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Success => ExpectedResponseRef::Success {}.serialize(serializer),
            Self::HttpStatus { code } => {
                ExpectedResponseRef::HttpStatus { code: *code }.serialize(serializer)
            }
            Self::BusinessCode { code } => {
                ExpectedResponseRef::BusinessCode { code }.serialize(serializer)
            }
        }
    }
}

impl<'de> Deserialize<'de> for ExpectedResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(match ExpectedResponseWire::deserialize(deserializer)? {
            ExpectedResponseWire::Success {} => Self::Success,
            ExpectedResponseWire::HttpStatus { code } => Self::HttpStatus { code },
            ExpectedResponseWire::BusinessCode { code } => Self::BusinessCode { code },
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Idempotency {
    ReadOnly,
    IdempotentWrite,
    Write,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryPolicy {
    Never,
    Bounded { attempts: u8 },
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum RetryPolicyRef {
    Never {},
    Bounded { attempts: u8 },
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum RetryPolicyWire {
    Never {},
    Bounded { attempts: u8 },
}

impl Serialize for RetryPolicy {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Never => RetryPolicyRef::Never {}.serialize(serializer),
            Self::Bounded { attempts } => RetryPolicyRef::Bounded {
                attempts: *attempts,
            }
            .serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for RetryPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(match RetryPolicyWire::deserialize(deserializer)? {
            RetryPolicyWire::Never {} => Self::Never,
            RetryPolicyWire::Bounded { attempts } => Self::Bounded { attempts },
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseClassification {
    Matched,
    Unexpected,
    Timeout,
    LateResponse,
}

#[derive(Debug, Default)]
pub struct InFlightRequests {
    requests: HashMap<u32, u64>,
}

impl InFlightRequests {
    pub fn begin(&mut self, sequence: u32, deadline_monotonic_ms: u64) -> bool {
        self.requests
            .insert(sequence, deadline_monotonic_ms)
            .is_none()
    }

    pub fn expire(&mut self, now_monotonic_ms: u64) -> Vec<u32> {
        let expired: Vec<u32> = self
            .requests
            .iter()
            .filter_map(|(sequence, deadline)| (*deadline <= now_monotonic_ms).then_some(*sequence))
            .collect();
        for sequence in &expired {
            self.requests.remove(sequence);
        }
        expired
    }

    pub fn respond(&mut self, sequence: u32, response_matches: bool) -> ResponseClassification {
        if self.requests.remove(&sequence).is_none() {
            ResponseClassification::LateResponse
        } else if response_matches {
            ResponseClassification::Matched
        } else {
            ResponseClassification::Unexpected
        }
    }

    pub fn len(&self) -> usize {
        self.requests.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_request_cannot_be_matched_by_a_later_response() {
        let mut requests = InFlightRequests::default();
        assert!(requests.begin(7, 10));
        assert_eq!(requests.expire(10), vec![7]);
        assert_eq!(
            requests.respond(7, true),
            ResponseClassification::LateResponse
        );
        assert_eq!(requests.len(), 0);
    }
}
