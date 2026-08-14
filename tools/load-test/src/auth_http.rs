//! `auth-http` request boundary shared by preparation, dry-runs, and guarded
//! live execution. Request values may carry secrets in memory, but no request
//! or success payload type implements `Serialize` or `Debug`.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::time::{Duration, Instant};

use reqwest::blocking::Client;
use reqwest::header::{CONNECTION, RETRY_AFTER};
use serde::Serialize;
use serde_json::{Value, json};

use crate::auth_budget::{
    RuntimeAuthQuota, auth_operation_potential_writes, register_potential_writes,
};
use crate::config::AuthOperation;
use crate::config::HardBudget;
use crate::metrics::HistogramSnapshot;
use crate::side_services::AuthServicesPayload;

const MAX_IDEMPOTENT_RETRIES: u8 = 2;

/// Deterministic admission clock for login-producing requests. It reserves a
/// future slot before a request is sent; callers must wait until that slot and
/// may abort instead of sending when the run deadline or protection changes.
#[derive(Debug, Clone)]
struct RateAdmissionLimiter {
    interval_us: u64,
    next_admission_us: u64,
    admitted: u64,
}

impl RateAdmissionLimiter {
    pub fn new(max_login_qps: f64) -> Result<Self, String> {
        if !max_login_qps.is_finite() || max_login_qps <= 0.0 {
            return Err("max_login_qps must be finite and positive for admission control".into());
        }
        let interval_us = (1_000_000.0 / max_login_qps).ceil();
        if !interval_us.is_finite() || interval_us > u64::MAX as f64 {
            return Err("login admission interval is not representable".into());
        }
        Ok(Self {
            interval_us: (interval_us as u64).max(1),
            next_admission_us: 0,
            admitted: 0,
        })
    }

    pub fn reserve(&mut self, now_us: u64) -> u64 {
        let admitted_at = now_us.max(self.next_admission_us);
        self.next_admission_us = admitted_at.saturating_add(self.interval_us);
        self.admitted = self.admitted.saturating_add(1);
        admitted_at
    }

    pub fn admitted(&self) -> u64 {
        self.admitted
    }
}

#[derive(Debug, Clone)]
pub struct LoginAdmissionLimiter {
    limiter: RateAdmissionLimiter,
}

impl LoginAdmissionLimiter {
    pub fn new(max_login_qps: f64) -> Result<Self, String> {
        Ok(Self {
            limiter: RateAdmissionLimiter::new(max_login_qps)?,
        })
    }

    pub fn reserve(&mut self, now_us: u64) -> u64 {
        self.limiter.reserve(now_us)
    }

    pub fn admitted(&self) -> u64 {
        self.limiter.admitted()
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthHttpStatusCategory {
    Http2xx,
    Http3xx,
    Http400,
    Http401,
    Http403,
    Http404,
    Http409,
    Http429,
    Http4xxOther,
    Http5xx,
    HttpOther,
    NoResponse,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthBusinessCodeCategory {
    LoginNameExists,
    IpRateLimited,
    InvalidLoginCredentials,
    MissingBearerToken,
    InvalidAccessToken,
    CharacterNotLoginable,
    InvalidCharacterName,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthOutcomeCategory {
    Success,
    BusinessError,
    RateLimited,
    Timeout,
    InvalidJson,
    Disconnect,
    HttpError,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VirtualPlayerState {
    AwaitingLogin,
    LoggedIn,
    CharacterReady,
    TicketIssued,
    LoggedOut,
    Failed,
}

/// Request body values live only for the duration of a transport call.
#[derive(Clone)]
pub enum AuthHttpRequest {
    Register {
        login_name: String,
        password: String,
        display_name: Option<String>,
    },
    Login {
        login_name: String,
        password: String,
    },
    Me {
        access_token: String,
    },
    ListCharacters {
        access_token: String,
    },
    CreateCharacter {
        access_token: String,
        name: String,
    },
    SelectCharacter {
        access_token: String,
        character_id: String,
    },
    IssueTicket {
        access_token: String,
        character_id: String,
    },
    Logout {
        access_token: String,
    },
}

impl AuthHttpRequest {
    pub fn operation(&self) -> &'static str {
        match self {
            Self::Register { .. } => "register",
            Self::Login { .. } => "login",
            Self::Me { .. } => "me",
            Self::ListCharacters { .. } => "list_characters",
            Self::CreateCharacter { .. } => "create_character",
            Self::SelectCharacter { .. } => "select_character",
            Self::IssueTicket { .. } => "issue_ticket",
            Self::Logout { .. } => "logout",
        }
    }

    /// Server contracts currently only make these read-only endpoints safe for
    /// bounded automatic retry. Ticket issue, logout, registration, and
    /// character creation all have side effects or no idempotency key.
    pub fn is_explicitly_idempotent(&self) -> bool {
        matches!(self, Self::Me { .. } | Self::ListCharacters { .. })
    }

    fn potential_data_writes(&self) -> u64 {
        match self {
            Self::Register { .. } => register_potential_writes(),
            Self::Login { .. } => auth_operation_potential_writes(AuthOperation::Login),
            Self::Me { .. } => auth_operation_potential_writes(AuthOperation::Me),
            Self::ListCharacters { .. } => {
                auth_operation_potential_writes(AuthOperation::ListCharacters)
            }
            Self::CreateCharacter { .. } => {
                auth_operation_potential_writes(AuthOperation::CreateCharacter)
            }
            Self::SelectCharacter { .. } => {
                auth_operation_potential_writes(AuthOperation::SelectCharacter)
            }
            Self::IssueTicket { .. } => auth_operation_potential_writes(AuthOperation::IssueTicket),
            Self::Logout { .. } => auth_operation_potential_writes(AuthOperation::Logout),
        }
    }

    fn is_login_attempt(&self) -> bool {
        matches!(self, Self::Login { .. })
    }
}

/// A conservative, shared admission boundary for every outbound auth-http
/// attempt. The reqwest transport uses HTTP/1.1 `Connection: close`, while
/// admission nevertheless treats every attempt as a new connection, one
/// business message, and one message on a single worst-case connection.
/// That conservative mapping makes all auth HTTP traffic obey the configured
/// connection and message budgets even when transport internals are opaque.
#[derive(Debug)]
pub struct AuthDispatchAdmission {
    started: Instant,
    quota: RuntimeAuthQuota,
    login: RateAdmissionLimiter,
    connections: RateAdmissionLimiter,
    business_messages: RateAdmissionLimiter,
    messages_per_connection: RateAdmissionLimiter,
}

#[derive(Debug)]
pub enum AuthAdmissionError {
    BudgetExceeded(String),
    DeadlineExceeded,
    Stopped(String),
}

impl fmt::Display for AuthAdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BudgetExceeded(error) | Self::Stopped(error) => formatter.write_str(error),
            Self::DeadlineExceeded => formatter.write_str("auth admission deadline elapsed"),
        }
    }
}

impl std::error::Error for AuthAdmissionError {}

impl AuthDispatchAdmission {
    pub fn new(budget: &HardBudget) -> Result<Self, String> {
        Ok(Self {
            started: Instant::now(),
            quota: RuntimeAuthQuota::new(budget),
            login: RateAdmissionLimiter::new(budget.max_login_qps)?,
            connections: RateAdmissionLimiter::new(budget.max_new_connections_per_second)?,
            business_messages: RateAdmissionLimiter::new(budget.max_business_messages_per_second)?,
            messages_per_connection: RateAdmissionLimiter::new(
                budget.max_messages_per_connection_per_second,
            )?,
        })
    }

    /// Waits in short bounded slices and re-runs `checkpoint` before every
    /// sleep and immediately before quota consumption. The callback is where
    /// callers recheck Ctrl+C, stop files, and target protection.
    pub fn admit<F>(
        &mut self,
        request: &AuthHttpRequest,
        deadline: Instant,
        checkpoint: F,
    ) -> Result<Duration, AuthAdmissionError>
    where
        F: FnMut() -> Result<(), String>,
    {
        self.admit_outbound(
            request.is_login_attempt(),
            true,
            true,
            true,
            request.potential_data_writes(),
            deadline,
            checkpoint,
        )
    }

    /// Reserve an auth operation for an execution plan before its credential
    /// material is available to the transport. This preserves the same login,
    /// connection, message, and potential-write accounting as `admit` without
    /// constructing placeholder secrets solely for admission.
    pub fn admit_auth_operation<F>(
        &mut self,
        operation: AuthOperation,
        deadline: Instant,
        checkpoint: F,
    ) -> Result<Duration, AuthAdmissionError>
    where
        F: FnMut() -> Result<(), String>,
    {
        self.admit_outbound(
            operation == AuthOperation::Login,
            true,
            true,
            true,
            auth_operation_potential_writes(operation),
            deadline,
            checkpoint,
        )
    }

    /// Accounts for a formal KCP connection in the same hard operation and
    /// connection budgets as auth-http. It has no business payload yet.
    pub fn admit_game_connection<F>(
        &mut self,
        deadline: Instant,
        checkpoint: F,
    ) -> Result<Duration, AuthAdmissionError>
    where
        F: FnMut() -> Result<(), String>,
    {
        self.admit_outbound(false, true, false, false, 0, deadline, checkpoint)
    }

    /// Accounts for a player-protocol request after its KCP connection exists.
    /// The minimal game runner currently emits `AuthReq` and `PingReq`; both
    /// have zero declared data writes and use this common rate boundary.
    pub fn admit_game_message<F>(
        &mut self,
        deadline: Instant,
        checkpoint: F,
    ) -> Result<Duration, AuthAdmissionError>
    where
        F: FnMut() -> Result<(), String>,
    {
        self.admit_outbound(false, false, true, true, 0, deadline, checkpoint)
    }

    /// Gameplay room/input operations are mutable by contract. Reserve their
    /// conservative effect before dispatching the KCP packet.
    pub fn admit_gameplay_message<F>(
        &mut self,
        potential_data_writes: u64,
        deadline: Instant,
        checkpoint: F,
    ) -> Result<Duration, AuthAdmissionError>
    where
        F: FnMut() -> Result<(), String>,
    {
        self.admit_outbound(
            false,
            false,
            true,
            true,
            potential_data_writes,
            deadline,
            checkpoint,
        )
    }

    pub fn admit_side_connection<F>(
        &mut self,
        deadline: Instant,
        checkpoint: F,
    ) -> Result<Duration, AuthAdmissionError>
    where
        F: FnMut() -> Result<(), String>,
    {
        self.admit_outbound(false, true, false, false, 0, deadline, checkpoint)
    }

    pub fn admit_side_message<F>(
        &mut self,
        deadline: Instant,
        checkpoint: F,
    ) -> Result<Duration, AuthAdmissionError>
    where
        F: FnMut() -> Result<(), String>,
    {
        self.admit_outbound(false, false, true, true, 0, deadline, checkpoint)
    }

    pub fn admit_side_message_with_writes<F>(
        &mut self,
        potential_data_writes: u64,
        deadline: Instant,
        checkpoint: F,
    ) -> Result<Duration, AuthAdmissionError>
    where
        F: FnMut() -> Result<(), String>,
    {
        self.admit_outbound(
            false,
            false,
            true,
            true,
            potential_data_writes,
            deadline,
            checkpoint,
        )
    }

    fn admit_outbound<F>(
        &mut self,
        is_login: bool,
        is_new_connection: bool,
        is_business_message: bool,
        is_per_connection_message: bool,
        potential_data_writes: u64,
        deadline: Instant,
        mut checkpoint: F,
    ) -> Result<Duration, AuthAdmissionError>
    where
        F: FnMut() -> Result<(), String>,
    {
        self.checkpoint(deadline, &mut checkpoint)?;
        let now_us = self.started.elapsed().as_micros() as u64;
        let mut admitted_at_us = now_us;
        if is_new_connection {
            admitted_at_us = admitted_at_us.max(self.connections.reserve(now_us));
        }
        if is_business_message {
            admitted_at_us = admitted_at_us.max(self.business_messages.reserve(now_us));
        }
        if is_per_connection_message {
            admitted_at_us = admitted_at_us.max(self.messages_per_connection.reserve(now_us));
        }
        if is_login {
            admitted_at_us = admitted_at_us.max(self.login.reserve(now_us));
        }

        while (self.started.elapsed().as_micros() as u64) < admitted_at_us {
            self.checkpoint(deadline, &mut checkpoint)?;
            let now_us = self.started.elapsed().as_micros() as u64;
            let wait_us = admitted_at_us.saturating_sub(now_us).min(10_000);
            if wait_us > 0 {
                std::thread::sleep(Duration::from_micros(wait_us));
            }
        }
        self.checkpoint(deadline, &mut checkpoint)?;
        self.quota
            .admit_potential_writes(potential_data_writes)
            .map_err(AuthAdmissionError::BudgetExceeded)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(AuthAdmissionError::DeadlineExceeded);
        }
        Ok(remaining)
    }

    fn checkpoint<F>(&self, deadline: Instant, checkpoint: &mut F) -> Result<(), AuthAdmissionError>
    where
        F: FnMut() -> Result<(), String>,
    {
        if Instant::now() >= deadline {
            return Err(AuthAdmissionError::DeadlineExceeded);
        }
        checkpoint().map_err(AuthAdmissionError::Stopped)
    }

    pub fn used_operations(&self) -> u64 {
        self.quota.used_operations()
    }

    pub fn used_data_writes(&self) -> u64 {
        self.quota.used_data_writes()
    }
}

#[derive(Clone)]
pub struct AuthSuccess {
    pub access_token: Option<String>,
    pub ticket: Option<String>,
    pub character_id: Option<String>,
    pub services: Option<AuthServicesPayload>,
}

#[derive(Clone)]
pub enum AuthResponseBody {
    Success(AuthSuccess),
    BusinessError(String),
    InvalidJson,
    Timeout,
    Disconnect,
}

#[derive(Clone)]
pub struct AuthHttpResponse {
    pub status: Option<u16>,
    pub retry_after_secs: Option<u64>,
    pub body: AuthResponseBody,
}

impl AuthHttpResponse {
    pub fn outcome(&self) -> AuthOutcomeCategory {
        match &self.body {
            AuthResponseBody::Success(_) if self.status.is_none_or(|status| status < 400) => {
                AuthOutcomeCategory::Success
            }
            AuthResponseBody::BusinessError(code) if code == "IP_RATE_LIMITED" => {
                AuthOutcomeCategory::RateLimited
            }
            AuthResponseBody::BusinessError(_) if self.status == Some(429) => {
                AuthOutcomeCategory::RateLimited
            }
            AuthResponseBody::BusinessError(_) => AuthOutcomeCategory::BusinessError,
            AuthResponseBody::InvalidJson => AuthOutcomeCategory::InvalidJson,
            AuthResponseBody::Timeout => AuthOutcomeCategory::Timeout,
            AuthResponseBody::Disconnect => AuthOutcomeCategory::Disconnect,
            AuthResponseBody::Success(_) => AuthOutcomeCategory::HttpError,
        }
    }

    pub fn status_category(&self) -> AuthHttpStatusCategory {
        match self.status {
            Some(200..=299) => AuthHttpStatusCategory::Http2xx,
            Some(300..=399) => AuthHttpStatusCategory::Http3xx,
            Some(400) => AuthHttpStatusCategory::Http400,
            Some(401) => AuthHttpStatusCategory::Http401,
            Some(403) => AuthHttpStatusCategory::Http403,
            Some(404) => AuthHttpStatusCategory::Http404,
            Some(409) => AuthHttpStatusCategory::Http409,
            Some(429) => AuthHttpStatusCategory::Http429,
            Some(400..=499) => AuthHttpStatusCategory::Http4xxOther,
            Some(500..=599) => AuthHttpStatusCategory::Http5xx,
            Some(_) => AuthHttpStatusCategory::HttpOther,
            None => AuthHttpStatusCategory::NoResponse,
        }
    }

    pub fn business_code_category(&self) -> Option<AuthBusinessCodeCategory> {
        let AuthResponseBody::BusinessError(code) = &self.body else {
            return None;
        };
        Some(match code.as_str() {
            "LOGIN_NAME_EXISTS" => AuthBusinessCodeCategory::LoginNameExists,
            "IP_RATE_LIMITED" => AuthBusinessCodeCategory::IpRateLimited,
            "INVALID_LOGIN_CREDENTIALS" => AuthBusinessCodeCategory::InvalidLoginCredentials,
            "MISSING_BEARER_TOKEN" => AuthBusinessCodeCategory::MissingBearerToken,
            "INVALID_ACCESS_TOKEN" => AuthBusinessCodeCategory::InvalidAccessToken,
            "CHARACTER_NOT_LOGINABLE" => AuthBusinessCodeCategory::CharacterNotLoginable,
            "INVALID_CHARACTER_NAME" => AuthBusinessCodeCategory::InvalidCharacterName,
            _ => AuthBusinessCodeCategory::Other,
        })
    }

    pub fn should_retry(&self) -> bool {
        matches!(
            self.outcome(),
            AuthOutcomeCategory::Timeout | AuthOutcomeCategory::Disconnect
        ) || matches!(self.status, Some(500..=599))
    }
}

pub trait AuthHttpTransport {
    fn send(&mut self, request: AuthHttpRequest) -> AuthHttpResponse;

    /// The caller sets the deadline remainder before every actual attempt.
    /// Fakes can ignore it; the reqwest transport applies it to the request.
    fn set_attempt_timeout(&mut self, _timeout: Duration) {}
}

/// Minimal mature-client implementation. Construction alone makes no network
/// call; command gates decide whether this transport may be created and used.
pub struct ReqwestAuthHttpTransport {
    base_url: String,
    client: Client,
    attempt_timeout: Duration,
}

impl ReqwestAuthHttpTransport {
    pub fn new(base_url: &str, timeout: Duration) -> Result<Self, String> {
        let base_url = base_url.trim_end_matches('/');
        if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
            return Err("auth-http target must use http or https".into());
        }
        let client = Client::builder()
            .timeout(timeout)
            .http1_only()
            .pool_max_idle_per_host(0)
            .build()
            .map_err(|error| format!("could not build auth-http client: {error}"))?;
        Ok(Self {
            base_url: base_url.to_string(),
            client,
            attempt_timeout: timeout,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    fn with_bearer(
        &self,
        request: reqwest::blocking::RequestBuilder,
        access_token: &str,
    ) -> reqwest::blocking::RequestBuilder {
        // Keep the opaque token in the request builder only. `reqwest` handles
        // header validation without an application-level panic or logging path.
        self.with_connection_close(request)
            .bearer_auth(access_token)
    }

    fn with_connection_close(
        &self,
        request: reqwest::blocking::RequestBuilder,
    ) -> reqwest::blocking::RequestBuilder {
        request
            .header(CONNECTION, "close")
            .timeout(self.attempt_timeout)
    }

    fn parse_response(response: reqwest::blocking::Response) -> AuthHttpResponse {
        let status = response.status().as_u16();
        let retry_after_secs = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        let body = match response.text() {
            Ok(body) => match serde_json::from_str::<Value>(&body) {
                Ok(value) => parse_json_body(value),
                Err(_) => AuthResponseBody::InvalidJson,
            },
            Err(_) => AuthResponseBody::Disconnect,
        };
        AuthHttpResponse {
            status: Some(status),
            retry_after_secs,
            body,
        }
    }
}

impl AuthHttpTransport for ReqwestAuthHttpTransport {
    fn set_attempt_timeout(&mut self, timeout: Duration) {
        self.attempt_timeout = timeout;
    }

    fn send(&mut self, request: AuthHttpRequest) -> AuthHttpResponse {
        let response = match request {
            AuthHttpRequest::Register {
                login_name,
                password,
                display_name,
            } => self
                .with_connection_close(self.client.post(self.url("/api/v1/auth/register")))
                .json(&json!({
                    "loginName": login_name,
                    "password": password,
                    "displayName": display_name,
                }))
                .send(),
            AuthHttpRequest::Login {
                login_name,
                password,
            } => self
                .with_connection_close(self.client.post(self.url("/api/v1/auth/login")))
                .json(&json!({
                    "loginName": login_name,
                    "password": password,
                }))
                .send(),
            AuthHttpRequest::Me { access_token } => self
                .with_bearer(self.client.get(self.url("/api/v1/auth/me")), &access_token)
                .send(),
            AuthHttpRequest::ListCharacters { access_token } => self
                .with_bearer(
                    self.client.get(self.url("/api/v1/characters")),
                    &access_token,
                )
                .send(),
            AuthHttpRequest::CreateCharacter { access_token, name } => self
                .with_bearer(
                    self.client
                        .post(self.url("/api/v1/characters"))
                        .json(&json!({ "name": name })),
                    &access_token,
                )
                .send(),
            AuthHttpRequest::SelectCharacter {
                access_token,
                character_id,
            } => self
                .with_bearer(
                    self.client
                        .post(self.url("/api/v1/characters/select"))
                        .json(&json!({ "character_id": character_id })),
                    &access_token,
                )
                .send(),
            AuthHttpRequest::IssueTicket {
                access_token,
                character_id,
            } => self
                .with_bearer(
                    self.client
                        .post(self.url("/api/v1/game-ticket/issue"))
                        .json(&json!({ "character_id": character_id })),
                    &access_token,
                )
                .send(),
            AuthHttpRequest::Logout { access_token } => self
                .with_bearer(
                    self.client
                        .post(self.url("/api/v1/auth/logout"))
                        .json(&json!({})),
                    &access_token,
                )
                .send(),
        };
        match response {
            Ok(response) => Self::parse_response(response),
            Err(error) if error.is_timeout() => AuthHttpResponse {
                status: None,
                retry_after_secs: None,
                body: AuthResponseBody::Timeout,
            },
            Err(_) => AuthHttpResponse {
                status: None,
                retry_after_secs: None,
                body: AuthResponseBody::Disconnect,
            },
        }
    }
}

fn parse_json_body(value: Value) -> AuthResponseBody {
    if value.get("ok") != Some(&Value::Bool(true)) {
        return AuthResponseBody::BusinessError(
            value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("UNCLASSIFIED_BUSINESS_ERROR")
                .to_string(),
        );
    }
    let access_token = value
        .get("accessToken")
        .and_then(Value::as_str)
        .map(str::to_string);
    let ticket = value
        .get("ticket")
        .and_then(Value::as_str)
        .map(str::to_string);
    let character_id = value
        .get("character_id")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("character")
                .and_then(|character| character.get("character_id"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            value
                .get("characters")
                .and_then(Value::as_array)
                .and_then(|characters| characters.first())
                .and_then(|character| character.get("character_id"))
                .and_then(Value::as_str)
        })
        .map(str::to_string);
    let services = match value.get("services") {
        Some(services) => match serde_json::from_value(services.clone()) {
            Ok(services) => Some(services),
            Err(_) => return AuthResponseBody::InvalidJson,
        },
        None => None,
    };
    AuthResponseBody::Success(AuthSuccess {
        access_token,
        ticket,
        character_id,
        services,
    })
}

#[derive(Clone, Debug, Serialize)]
pub struct AuthRunMetrics {
    pub requests: u64,
    pub login_requests: u64,
    pub login_successes: u64,
    pub connection_failures: u64,
    pub ticket_attempts: u64,
    pub ticket_successes: u64,
    pub rate_limited: u64,
    pub http_statuses: BTreeMap<AuthHttpStatusCategory, u64>,
    pub business_codes: BTreeMap<AuthBusinessCodeCategory, u64>,
    pub outcomes: BTreeMap<AuthOutcomeCategory, u64>,
    pub virtual_player_states: BTreeMap<VirtualPlayerState, u64>,
    pub latency_ms: HistogramSnapshot,
    pub login_latency_ms: HistogramSnapshot,
    pub ticket_latency_ms: HistogramSnapshot,
    /// Monotonic wall-clock duration supplied by the controller. It includes
    /// admission waits and excludes summed request latency.
    pub wall_clock_window_ms: u64,
}

impl Default for AuthRunMetrics {
    fn default() -> Self {
        Self {
            requests: 0,
            login_requests: 0,
            login_successes: 0,
            connection_failures: 0,
            ticket_attempts: 0,
            ticket_successes: 0,
            rate_limited: 0,
            http_statuses: BTreeMap::new(),
            business_codes: BTreeMap::new(),
            outcomes: BTreeMap::new(),
            virtual_player_states: BTreeMap::new(),
            latency_ms: HistogramSnapshot::default(),
            login_latency_ms: HistogramSnapshot::default(),
            ticket_latency_ms: HistogramSnapshot::default(),
            wall_clock_window_ms: 0,
        }
    }
}

impl AuthRunMetrics {
    pub fn merge(&mut self, other: &Self) {
        self.requests = self.requests.saturating_add(other.requests);
        self.login_requests = self.login_requests.saturating_add(other.login_requests);
        self.login_successes = self.login_successes.saturating_add(other.login_successes);
        self.connection_failures = self
            .connection_failures
            .saturating_add(other.connection_failures);
        self.ticket_attempts = self.ticket_attempts.saturating_add(other.ticket_attempts);
        self.ticket_successes = self.ticket_successes.saturating_add(other.ticket_successes);
        self.rate_limited = self.rate_limited.saturating_add(other.rate_limited);
        self.wall_clock_window_ms = self.wall_clock_window_ms.max(other.wall_clock_window_ms);
        self.latency_ms.merge(&other.latency_ms);
        self.login_latency_ms.merge(&other.login_latency_ms);
        self.ticket_latency_ms.merge(&other.ticket_latency_ms);
        for (key, value) in &other.http_statuses {
            *self.http_statuses.entry(*key).or_default() += value;
        }
        for (key, value) in &other.business_codes {
            *self.business_codes.entry(*key).or_default() += value;
        }
        for (key, value) in &other.outcomes {
            *self.outcomes.entry(*key).or_default() += value;
        }
        for (key, value) in &other.virtual_player_states {
            *self.virtual_player_states.entry(*key).or_default() += value;
        }
    }

    pub fn login_qps(&self) -> f64 {
        if self.wall_clock_window_ms == 0 {
            return 0.0;
        }
        self.login_requests as f64 / (self.wall_clock_window_ms as f64 / 1_000.0)
    }

    pub fn login_success_rate(&self) -> f64 {
        if self.login_requests == 0 {
            return 0.0;
        }
        self.login_successes as f64 / self.login_requests as f64
    }

    pub fn connection_failure_rate(&self) -> f64 {
        if self.requests == 0 {
            return 0.0;
        }
        self.connection_failures as f64 / self.requests as f64
    }

    pub fn set_wall_clock_window_ms(&mut self, window_ms: u64) {
        self.wall_clock_window_ms = window_ms.max(1);
    }

    pub fn ticket_success_rate(&self) -> f64 {
        if self.ticket_attempts == 0 {
            return 0.0;
        }
        self.ticket_successes as f64 / self.ticket_attempts as f64
    }

    pub fn rate_limit_rate(&self) -> f64 {
        if self.requests == 0 {
            return 0.0;
        }
        self.rate_limited as f64 / self.requests as f64
    }

    pub fn p50_ms(&self) -> u64 {
        self.latency_ms.percentile(0.50)
    }

    pub fn p95_ms(&self) -> u64 {
        self.latency_ms.percentile(0.95)
    }

    pub fn p99_ms(&self) -> u64 {
        self.latency_ms.percentile(0.99)
    }

    fn record(&mut self, request: &AuthHttpRequest, response: &AuthHttpResponse, elapsed_ms: u64) {
        self.requests += 1;
        self.latency_ms.record(elapsed_ms);
        *self
            .http_statuses
            .entry(response.status_category())
            .or_default() += 1;
        let outcome = response.outcome();
        *self.outcomes.entry(outcome).or_default() += 1;
        if let Some(code) = response.business_code_category() {
            *self.business_codes.entry(code).or_default() += 1;
        }
        if outcome == AuthOutcomeCategory::RateLimited {
            self.rate_limited += 1;
        }
        if matches!(
            outcome,
            AuthOutcomeCategory::Timeout | AuthOutcomeCategory::Disconnect
        ) {
            self.connection_failures += 1;
        }
        if matches!(request, AuthHttpRequest::Login { .. }) {
            self.login_latency_ms.record(elapsed_ms);
            self.login_requests += 1;
            if outcome == AuthOutcomeCategory::Success {
                self.login_successes += 1;
            }
        }
        if matches!(request, AuthHttpRequest::IssueTicket { .. }) {
            self.ticket_latency_ms.record(elapsed_ms);
            self.ticket_attempts += 1;
            if outcome == AuthOutcomeCategory::Success {
                self.ticket_successes += 1;
            }
        }
    }

    fn mark_state(&mut self, state: VirtualPlayerState) {
        *self.virtual_player_states.entry(state).or_default() += 1;
    }
}

pub fn send_with_bounded_retry<T: AuthHttpTransport>(
    transport: &mut T,
    request: AuthHttpRequest,
    configured_retries: u8,
    metrics: &mut AuthRunMetrics,
) -> AuthHttpResponse {
    send_with_bounded_retry_after_admission(transport, request, configured_retries, metrics, || {
        Ok(Duration::MAX)
    })
    .expect("the no-op admission callback cannot fail")
}

fn send_with_bounded_retry_after_admission<T, F>(
    transport: &mut T,
    request: AuthHttpRequest,
    configured_retries: u8,
    metrics: &mut AuthRunMetrics,
    mut before_dispatch: F,
) -> Result<AuthHttpResponse, String>
where
    T: AuthHttpTransport,
    F: FnMut() -> Result<Duration, String>,
{
    let retry_count = if request.is_explicitly_idempotent() {
        configured_retries.min(MAX_IDEMPOTENT_RETRIES)
    } else {
        0
    };
    for attempt in 0..=retry_count {
        let timeout = before_dispatch()?;
        transport.set_attempt_timeout(timeout);
        let started = Instant::now();
        let response = transport.send(request.clone());
        metrics.record(&request, &response, started.elapsed().as_millis() as u64);
        if attempt == retry_count || !response.should_retry() {
            return Ok(response);
        }
    }
    unreachable!("retry loop returns on its final attempt")
}

pub struct AuthExecution {
    pub metrics: AuthRunMetrics,
    pub error: Option<String>,
    // These values are intentionally neither serializable nor debuggable.
    // The caller must consume them before starting a game session.
    ticket: Option<String>,
    character_id: Option<String>,
    access_token: Option<String>,
    side_services: Option<AuthServicesPayload>,
}

impl AuthExecution {
    fn completed(metrics: AuthRunMetrics) -> Self {
        Self {
            metrics,
            error: None,
            ticket: None,
            character_id: None,
            access_token: None,
            side_services: None,
        }
    }

    fn failed(metrics: AuthRunMetrics, error: impl Into<String>) -> Self {
        Self {
            metrics,
            error: Some(error.into()),
            ticket: None,
            character_id: None,
            access_token: None,
            side_services: None,
        }
    }

    /// Transfers ephemeral game credentials to the next runner stage without
    /// making them serializable or including them in reports.
    pub fn take_game_credentials(&mut self) -> Option<(String, String)> {
        self.ticket.take().zip(self.character_id.take())
    }

    /// Transfers the latest auth-discovered public service descriptors. These
    /// endpoint values stay in memory and are never included in reports.
    pub fn take_side_services(&mut self) -> Option<AuthServicesPayload> {
        self.side_services.take()
    }

    fn take_logout_request(&mut self) -> Option<AuthHttpRequest> {
        self.access_token
            .take()
            .map(|access_token| AuthHttpRequest::Logout { access_token })
    }
}

/// Splits a scenario's terminal logout from the ticket-producing auth flow.
/// `auth-http` invalidates every player ticket on logout, so a game runner must
/// complete its KCP session before it may dispatch this operation.
pub fn split_game_auth_operations(
    operations: &[AuthOperation],
) -> Result<(Vec<AuthOperation>, bool), String> {
    let Some(logout_index) = operations
        .iter()
        .position(|operation| matches!(operation, AuthOperation::Logout))
    else {
        return Ok((operations.to_vec(), false));
    };
    if logout_index + 1 != operations.len() {
        return Err("game scenarios may only place logout as the final auth operation".into());
    }
    let before_logout = operations[..logout_index].to_vec();
    if !before_logout.iter().any(|operation| {
        matches!(
            operation,
            AuthOperation::IssueTicket | AuthOperation::SelectCharacter
        )
    }) {
        return Err(
            "game scenarios require a ticket-producing auth operation before logout".into(),
        );
    }
    Ok((before_logout, true))
}

/// Sends the terminal logout after the caller has closed its ticket-authenticated
/// game session. Access tokens stay in this non-debug, non-serializable object
/// until this one-shot operation consumes them.
pub fn execute_deferred_logout<T, F>(
    transport: &mut T,
    execution: &mut AuthExecution,
    mut before_request: F,
) -> Result<(), String>
where
    T: AuthHttpTransport,
    F: FnMut(&AuthHttpRequest) -> Result<Duration, String>,
{
    let request = execution
        .take_logout_request()
        .ok_or("deferred logout requires an authenticated access token")?;
    let response = send_with_bounded_retry_after_admission(
        transport,
        request.clone(),
        0,
        &mut execution.metrics,
        || before_request(&request),
    )?;
    if !matches!(response.body, AuthResponseBody::Success(_)) {
        return Err(format!(
            "deferred logout ended with {:?}",
            response.outcome()
        ));
    }
    execution.ticket = None;
    execution.character_id = None;
    execution.metrics.mark_state(VirtualPlayerState::LoggedOut);
    Ok(())
}

pub fn execute_auth_operations<T, F>(
    transport: &mut T,
    operations: &[AuthOperation],
    character_name: &str,
    login_name: &str,
    password: &str,
    mut before_request: F,
) -> AuthExecution
where
    T: AuthHttpTransport,
    F: FnMut(AuthOperation, &AuthHttpRequest) -> Result<Duration, String>,
{
    let mut metrics = AuthRunMetrics::default();
    metrics.mark_state(VirtualPlayerState::AwaitingLogin);
    let mut access_token: Option<String> = None;
    let mut character_id: Option<String> = None;
    let mut game_credentials: Option<(String, String)> = None;
    let mut side_services: Option<AuthServicesPayload> = None;
    let mut state = VirtualPlayerState::AwaitingLogin;

    for operation in operations {
        let request = match operation {
            AuthOperation::Login | AuthOperation::DuplicateLogin => AuthHttpRequest::Login {
                login_name: login_name.to_string(),
                password: password.to_string(),
            },
            AuthOperation::FailedLogin => AuthHttpRequest::Login {
                login_name: login_name.to_string(),
                password: "offline-invalid-password".into(),
            },
            AuthOperation::Me => match access_token.clone() {
                Some(access_token) => AuthHttpRequest::Me { access_token },
                None => {
                    return AuthExecution::failed(
                        metrics,
                        "auth operation me requires a preceding successful login",
                    );
                }
            },
            AuthOperation::ListCharacters => match access_token.clone() {
                Some(access_token) => AuthHttpRequest::ListCharacters { access_token },
                None => {
                    return AuthExecution::failed(
                        metrics,
                        "auth operation list_characters requires a preceding successful login",
                    );
                }
            },
            AuthOperation::CreateCharacter => match access_token.clone() {
                Some(access_token) => AuthHttpRequest::CreateCharacter {
                    access_token,
                    name: character_name.to_string(),
                },
                None => {
                    return AuthExecution::failed(
                        metrics,
                        "auth operation create_character requires a preceding successful login",
                    );
                }
            },
            AuthOperation::SelectCharacter => match (access_token.clone(), character_id.clone()) {
                (Some(access_token), Some(character_id)) => AuthHttpRequest::SelectCharacter {
                    access_token,
                    character_id,
                },
                (None, _) => {
                    return AuthExecution::failed(
                        metrics,
                        "auth operation select_character requires a preceding successful login",
                    );
                }
                (_, None) => {
                    return AuthExecution::failed(
                        metrics,
                        "auth operation select_character requires a prepared character",
                    );
                }
            },
            AuthOperation::IssueTicket => match (access_token.clone(), character_id.clone()) {
                (Some(access_token), Some(character_id)) => AuthHttpRequest::IssueTicket {
                    access_token,
                    character_id,
                },
                (None, _) => {
                    return AuthExecution::failed(
                        metrics,
                        "auth operation issue_ticket requires a preceding successful login",
                    );
                }
                (_, None) => {
                    return AuthExecution::failed(
                        metrics,
                        "auth operation issue_ticket requires a prepared character",
                    );
                }
            },
            AuthOperation::Logout => match access_token.clone() {
                Some(access_token) => AuthHttpRequest::Logout { access_token },
                None => {
                    return AuthExecution::failed(
                        metrics,
                        "auth operation logout requires a preceding successful login",
                    );
                }
            },
        };
        let request_for_admission = request.clone();
        let response = send_with_bounded_retry_after_admission(
            transport,
            request,
            MAX_IDEMPOTENT_RETRIES,
            &mut metrics,
            || before_request(*operation, &request_for_admission),
        );
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                metrics.mark_state(VirtualPlayerState::Failed);
                return AuthExecution::failed(metrics, error);
            }
        };

        if matches!(operation, AuthOperation::FailedLogin) {
            if matches!(response.body, AuthResponseBody::BusinessError(_)) {
                continue;
            }
            metrics.mark_state(VirtualPlayerState::Failed);
            return AuthExecution::failed(
                metrics,
                "failed_login scenario did not receive an expected business rejection",
            );
        }
        let AuthResponseBody::Success(success) = response.body else {
            metrics.mark_state(VirtualPlayerState::Failed);
            return AuthExecution::failed(
                metrics,
                format!(
                    "auth operation {} ended with {:?}",
                    operation_name(*operation),
                    response.outcome()
                ),
            );
        };
        if success.services.is_some() {
            side_services = success.services.clone();
        }
        match operation {
            AuthOperation::Login | AuthOperation::DuplicateLogin => {
                access_token = success.access_token;
                if access_token.is_none() {
                    metrics.mark_state(VirtualPlayerState::Failed);
                    return AuthExecution::failed(
                        metrics,
                        "auth login response did not provide an access token",
                    );
                }
                state = VirtualPlayerState::LoggedIn;
                metrics.mark_state(state);
            }
            AuthOperation::ListCharacters | AuthOperation::CreateCharacter => {
                if let Some(id) = success.character_id {
                    character_id = Some(id);
                    state = VirtualPlayerState::CharacterReady;
                    metrics.mark_state(state);
                }
            }
            AuthOperation::SelectCharacter | AuthOperation::IssueTicket => {
                let Some(ticket) = success.ticket else {
                    metrics.mark_state(VirtualPlayerState::Failed);
                    return AuthExecution::failed(
                        metrics,
                        "ticket operation did not return a ticket",
                    );
                };
                let Some(selected_character_id) = character_id.clone() else {
                    metrics.mark_state(VirtualPlayerState::Failed);
                    return AuthExecution::failed(
                        metrics,
                        "ticket operation requires a prepared character",
                    );
                };
                // A select response may already contain a character-bound
                // ticket; an explicit issue response replaces it. Neither
                // opaque value enters metrics, diagnostics, or reports.
                game_credentials = Some((ticket, selected_character_id));
                state = VirtualPlayerState::TicketIssued;
                metrics.mark_state(state);
            }
            AuthOperation::Logout => {
                // The production logout invalidates every ticket for the
                // player. Do not leave a now-invalid opaque ticket reachable
                // from this execution object.
                access_token = None;
                game_credentials = None;
                state = VirtualPlayerState::LoggedOut;
                metrics.mark_state(state);
            }
            AuthOperation::Me | AuthOperation::FailedLogin => {}
        }
    }
    metrics.mark_state(state);
    let mut execution = AuthExecution::completed(metrics);
    if let Some((ticket, character_id)) = game_credentials {
        execution.ticket = Some(ticket);
        execution.character_id = Some(character_id);
    }
    execution.access_token = access_token;
    execution.side_services = side_services;
    execution
}

pub fn operation_name(operation: AuthOperation) -> &'static str {
    match operation {
        AuthOperation::Login => "login",
        AuthOperation::Me => "me",
        AuthOperation::ListCharacters => "list_characters",
        AuthOperation::CreateCharacter => "create_character",
        AuthOperation::SelectCharacter => "select_character",
        AuthOperation::IssueTicket => "issue_ticket",
        AuthOperation::Logout => "logout",
        AuthOperation::DuplicateLogin => "duplicate_login",
        AuthOperation::FailedLogin => "failed_login",
    }
}

#[derive(Clone, Copy)]
pub enum FakeAuthOutcome {
    Success,
    RateLimited,
    BusinessError,
    Timeout,
    InvalidJson,
    Disconnect,
}

pub struct FakeAuthHttpService {
    outcomes: VecDeque<FakeAuthOutcome>,
    request_count: u64,
}

impl FakeAuthHttpService {
    pub fn scripted(outcomes: impl IntoIterator<Item = FakeAuthOutcome>) -> Self {
        Self {
            outcomes: outcomes.into_iter().collect(),
            request_count: 0,
        }
    }

    pub fn request_count(&self) -> u64 {
        self.request_count
    }
}

impl AuthHttpTransport for FakeAuthHttpService {
    fn send(&mut self, request: AuthHttpRequest) -> AuthHttpResponse {
        self.request_count = self.request_count.saturating_add(1);
        let outcome = self
            .outcomes
            .pop_front()
            .unwrap_or(FakeAuthOutcome::Success);
        match outcome {
            FakeAuthOutcome::Success => AuthHttpResponse {
                status: Some(if matches!(request, AuthHttpRequest::Login { .. }) {
                    201
                } else {
                    200
                }),
                retry_after_secs: None,
                body: AuthResponseBody::Success(AuthSuccess {
                    access_token: matches!(request, AuthHttpRequest::Login { .. })
                        .then(|| "fake-access-token".into()),
                    ticket: matches!(
                        request,
                        AuthHttpRequest::SelectCharacter { .. }
                            | AuthHttpRequest::IssueTicket { .. }
                    )
                    .then(|| "fake-ticket".into()),
                    character_id: matches!(
                        request,
                        AuthHttpRequest::ListCharacters { .. }
                            | AuthHttpRequest::CreateCharacter { .. }
                    )
                    .then(|| "fake-character".into()),
                    services: None,
                }),
            },
            FakeAuthOutcome::RateLimited => AuthHttpResponse {
                status: Some(429),
                retry_after_secs: Some(1),
                body: AuthResponseBody::BusinessError("IP_RATE_LIMITED".into()),
            },
            FakeAuthOutcome::BusinessError => AuthHttpResponse {
                status: Some(401),
                retry_after_secs: None,
                body: AuthResponseBody::BusinessError("INVALID_LOGIN_CREDENTIALS".into()),
            },
            FakeAuthOutcome::Timeout => AuthHttpResponse {
                status: None,
                retry_after_secs: None,
                body: AuthResponseBody::Timeout,
            },
            FakeAuthOutcome::InvalidJson => AuthHttpResponse {
                status: Some(200),
                retry_after_secs: None,
                body: AuthResponseBody::InvalidJson,
            },
            FakeAuthOutcome::Disconnect => AuthHttpResponse {
                status: None,
                retry_after_secs: None,
                body: AuthResponseBody::Disconnect,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abort::{AbortController, AbortReason};

    fn budget() -> HardBudget {
        HardBudget {
            max_virtual_players: 4,
            max_login_qps: 2.0,
            max_new_connections_per_second: 2.0,
            max_business_messages_per_second: 2.0,
            max_messages_per_connection_per_second: 2.0,
            max_duration_secs: 10,
            max_total_operations: 10,
            max_error_rate: 0.1,
            max_connection_failure_rate: 0.1,
            max_p99_ms: 1_000,
            max_data_writes: 20,
        }
    }

    struct TimeoutRecordingTransport {
        timeout: Option<Duration>,
    }

    impl AuthHttpTransport for TimeoutRecordingTransport {
        fn send(&mut self, _request: AuthHttpRequest) -> AuthHttpResponse {
            AuthHttpResponse {
                status: Some(200),
                retry_after_secs: None,
                body: AuthResponseBody::Success(AuthSuccess {
                    access_token: None,
                    ticket: None,
                    character_id: None,
                    services: None,
                }),
            }
        }

        fn set_attempt_timeout(&mut self, timeout: Duration) {
            self.timeout = Some(timeout);
        }
    }

    #[test]
    fn fake_auth_outcomes_have_stable_classification_without_secrets() {
        let mut service = FakeAuthHttpService::scripted([
            FakeAuthOutcome::Success,
            FakeAuthOutcome::RateLimited,
            FakeAuthOutcome::BusinessError,
            FakeAuthOutcome::Timeout,
            FakeAuthOutcome::InvalidJson,
            FakeAuthOutcome::Disconnect,
        ]);
        let request = || AuthHttpRequest::Login {
            login_name: "loadtest_local_default_000001".into(),
            password: "local-only-password".into(),
        };
        assert_eq!(
            service.send(request()).outcome(),
            AuthOutcomeCategory::Success
        );
        assert_eq!(
            service.send(request()).outcome(),
            AuthOutcomeCategory::RateLimited
        );
        assert_eq!(
            service.send(request()).outcome(),
            AuthOutcomeCategory::BusinessError
        );
        assert_eq!(
            service.send(request()).outcome(),
            AuthOutcomeCategory::Timeout
        );
        assert_eq!(
            service.send(request()).outcome(),
            AuthOutcomeCategory::InvalidJson
        );
        assert_eq!(
            service.send(request()).outcome(),
            AuthOutcomeCategory::Disconnect
        );
    }

    #[test]
    fn parses_current_auth_http_character_create_and_list_contracts() {
        let create = parse_json_body(serde_json::json!({
            "ok": true,
            "character": { "character_id": "chr_0000000000001" }
        }));
        let list = parse_json_body(serde_json::json!({
            "ok": true,
            "characters": [{ "character_id": "chr_0000000000002" }]
        }));

        let AuthResponseBody::Success(create) = create else {
            panic!("create contract must parse as success");
        };
        let AuthResponseBody::Success(list) = list else {
            panic!("list contract must parse as success");
        };
        assert_eq!(create.character_id.as_deref(), Some("chr_0000000000001"));
        assert_eq!(list.character_id.as_deref(), Some("chr_0000000000002"));
    }

    #[test]
    fn auth_success_retains_service_descriptors_without_serializing_credentials() {
        let response = parse_json_body(serde_json::json!({
            "ok": true,
            "accessToken": "test-access-token",
            "services": {
                "game": { "host": "game.example", "port": 4000, "protocol": "kcp" },
                "chat": { "host": "chat.example", "port": 443, "protocol": "wss" },
                "mail": null,
                "announce": null
            }
        }));
        let AuthResponseBody::Success(success) = response else {
            panic!("auth response with descriptors must parse as success");
        };
        assert_eq!(
            success
                .services
                .as_ref()
                .unwrap()
                .chat
                .as_ref()
                .unwrap()
                .host,
            "chat.example"
        );
        assert_eq!(success.access_token.as_deref(), Some("test-access-token"));
    }

    #[test]
    fn only_explicitly_idempotent_requests_retry() {
        let mut metrics = AuthRunMetrics::default();
        let mut read_service =
            FakeAuthHttpService::scripted([FakeAuthOutcome::Timeout, FakeAuthOutcome::Success]);
        let response = send_with_bounded_retry(
            &mut read_service,
            AuthHttpRequest::Me {
                access_token: "in-memory-only".into(),
            },
            2,
            &mut metrics,
        );
        assert_eq!(response.outcome(), AuthOutcomeCategory::Success);
        assert_eq!(metrics.requests, 2);

        let mut metrics = AuthRunMetrics::default();
        let mut write_service =
            FakeAuthHttpService::scripted([FakeAuthOutcome::Timeout, FakeAuthOutcome::Success]);
        let response = send_with_bounded_retry(
            &mut write_service,
            AuthHttpRequest::CreateCharacter {
                access_token: "in-memory-only".into(),
                name: "loadtest_000001".into(),
            },
            2,
            &mut metrics,
        );
        assert_eq!(response.outcome(), AuthOutcomeCategory::Timeout);
        assert_eq!(metrics.requests, 1);
    }

    #[test]
    fn retry_admission_runs_before_each_transport_attempt() {
        let mut metrics = AuthRunMetrics::default();
        let mut service =
            FakeAuthHttpService::scripted([FakeAuthOutcome::Timeout, FakeAuthOutcome::Success]);
        let mut admissions = 0;
        let result = send_with_bounded_retry_after_admission(
            &mut service,
            AuthHttpRequest::Me {
                access_token: "in-memory-only".into(),
            },
            2,
            &mut metrics,
            || {
                admissions += 1;
                (admissions < 2)
                    .then_some(Duration::MAX)
                    .ok_or_else(|| "operation budget exhausted".into())
            },
        );
        assert!(result.is_err());
        assert_eq!(admissions, 2);
        assert_eq!(metrics.requests, 1);
    }

    #[test]
    fn admission_conservatively_applies_all_http_rate_budgets_and_write_quota() {
        let mut admission = AuthDispatchAdmission::new(&budget()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let login = AuthHttpRequest::Login {
            login_name: "loadtest_local_default_000001".into(),
            password: "in-memory-only".into(),
        };
        admission.admit(&login, deadline, || Ok(())).unwrap();
        let started = Instant::now();
        admission
            .admit(
                &AuthHttpRequest::Me {
                    access_token: "in-memory-only".into(),
                },
                deadline,
                || Ok(()),
            )
            .unwrap();
        assert!(started.elapsed() >= Duration::from_millis(450));
        assert_eq!(admission.used_operations(), 2);
        assert_eq!(admission.used_data_writes(), 3);

        let mut write_limited = budget();
        write_limited.max_data_writes = 3;
        let mut admission = AuthDispatchAdmission::new(&write_limited).unwrap();
        let error = admission
            .admit(
                &AuthHttpRequest::Register {
                    login_name: "loadtest_local_default_000002".into(),
                    password: "in-memory-only".into(),
                    display_name: None,
                },
                Instant::now() + Duration::from_secs(1),
                || Ok(()),
            )
            .unwrap_err();
        assert!(matches!(error, AuthAdmissionError::BudgetExceeded(_)));
        assert_eq!(admission.used_operations(), 0);
    }

    #[test]
    fn admission_rechecks_control_state_while_waiting_and_sets_attempt_timeout() {
        let mut admission = AuthDispatchAdmission::new(&budget()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let request = AuthHttpRequest::Login {
            login_name: "loadtest_local_default_000001".into(),
            password: "in-memory-only".into(),
        };
        admission.admit(&request, deadline, || Ok(())).unwrap();
        let mut checks = 0;
        let error = admission
            .admit(&request, deadline, || {
                checks += 1;
                (checks < 2)
                    .then_some(())
                    .ok_or_else(|| "protection changed".into())
            })
            .unwrap_err();
        assert!(matches!(error, AuthAdmissionError::Stopped(_)));

        let mut transport = TimeoutRecordingTransport { timeout: None };
        let mut metrics = AuthRunMetrics::default();
        let _ = send_with_bounded_retry_after_admission(
            &mut transport,
            AuthHttpRequest::Me {
                access_token: "in-memory-only".into(),
            },
            0,
            &mut metrics,
            || Ok(Duration::from_millis(17)),
        )
        .unwrap();
        assert_eq!(transport.timeout, Some(Duration::from_millis(17)));
    }

    #[test]
    fn login_qps_uses_attempts_and_monotonic_wall_clock_window() {
        let mut limiter = LoginAdmissionLimiter::new(2.0).unwrap();
        assert_eq!(limiter.reserve(0), 0);
        assert_eq!(limiter.reserve(0), 500_000);
        let mut metrics = AuthRunMetrics {
            login_requests: 2,
            login_successes: 1,
            ..Default::default()
        };
        metrics.set_wall_clock_window_ms(500);
        assert_eq!(metrics.login_qps(), 4.0);
        assert_eq!(metrics.login_success_rate(), 0.5);

        let mut service = FakeAuthHttpService::scripted([FakeAuthOutcome::BusinessError]);
        let mut execution = execute_auth_operations(
            &mut service,
            &[AuthOperation::FailedLogin],
            "loadtest_000001",
            "loadtest_local_default_000001",
            "in-memory-only",
            |_, _| Ok(Duration::MAX),
        );
        assert!(execution.error.is_none());
        execution.metrics.set_wall_clock_window_ms(500);
        assert_eq!(execution.metrics.login_requests, 1);
        assert_eq!(execution.metrics.login_successes, 0);
        assert_eq!(execution.metrics.login_qps(), 2.0);
        assert_eq!(execution.metrics.login_success_rate(), 0.0);
    }

    #[test]
    fn failed_auth_operations_keep_all_observed_categories() {
        let cases = [
            (
                FakeAuthOutcome::RateLimited,
                AuthOutcomeCategory::RateLimited,
                AuthHttpStatusCategory::Http429,
                Some(AuthBusinessCodeCategory::IpRateLimited),
                0,
            ),
            (
                FakeAuthOutcome::BusinessError,
                AuthOutcomeCategory::BusinessError,
                AuthHttpStatusCategory::Http401,
                Some(AuthBusinessCodeCategory::InvalidLoginCredentials),
                0,
            ),
            (
                FakeAuthOutcome::Timeout,
                AuthOutcomeCategory::Timeout,
                AuthHttpStatusCategory::NoResponse,
                None,
                1,
            ),
            (
                FakeAuthOutcome::InvalidJson,
                AuthOutcomeCategory::InvalidJson,
                AuthHttpStatusCategory::Http2xx,
                None,
                0,
            ),
            (
                FakeAuthOutcome::Disconnect,
                AuthOutcomeCategory::Disconnect,
                AuthHttpStatusCategory::NoResponse,
                None,
                1,
            ),
        ];
        for (
            fake,
            expected_outcome,
            expected_status,
            expected_business_code,
            expected_connection_failures,
        ) in cases
        {
            let mut service = FakeAuthHttpService::scripted([fake]);
            let execution = execute_auth_operations(
                &mut service,
                &[AuthOperation::Login],
                "loadtest_000001",
                "loadtest_local_default_000001",
                "in-memory-only",
                |_, _| Ok(Duration::MAX),
            );
            assert!(execution.error.is_some());
            assert_eq!(execution.metrics.requests, 1);
            assert_eq!(
                execution.metrics.connection_failures,
                expected_connection_failures
            );
            assert_eq!(execution.metrics.outcomes.get(&expected_outcome), Some(&1),);
            assert_eq!(
                execution.metrics.http_statuses.get(&expected_status),
                Some(&1),
            );
            if let Some(expected_business_code) = expected_business_code {
                assert_eq!(
                    execution
                        .metrics
                        .business_codes
                        .get(&expected_business_code),
                    Some(&1),
                );
            }
            assert!(
                execution
                    .metrics
                    .virtual_player_states
                    .contains_key(&VirtualPlayerState::Failed)
            );
        }
    }

    #[test]
    fn disconnect_failure_rate_triggers_connection_threshold() {
        let mut service = FakeAuthHttpService::scripted([FakeAuthOutcome::Disconnect]);
        let execution = execute_auth_operations(
            &mut service,
            &[AuthOperation::Login],
            "loadtest_000001",
            "loadtest_local_default_000001",
            "in-memory-only",
            |_, _| Ok(Duration::MAX),
        );
        assert_eq!(execution.metrics.connection_failure_rate(), 1.0);

        let mut abort = AbortController::default();
        abort.check_thresholds(
            1.0,
            execution.metrics.connection_failure_rate(),
            execution.metrics.p99_ms(),
            1.0,
            0.5,
            1_000,
            true,
        );
        assert_eq!(abort.reason(), Some(&AbortReason::ConnectionFailureRate));
    }

    #[test]
    fn auth_flow_records_login_ticket_and_virtual_player_state() {
        let mut service = FakeAuthHttpService::scripted([FakeAuthOutcome::Success; 8]);
        let execution = execute_auth_operations(
            &mut service,
            &[
                AuthOperation::Login,
                AuthOperation::ListCharacters,
                AuthOperation::SelectCharacter,
                AuthOperation::IssueTicket,
                AuthOperation::Logout,
            ],
            "loadtest_000001",
            "loadtest_local_default_000001",
            "in-memory-only",
            |_, _| Ok(Duration::MAX),
        );
        assert!(execution.error.is_none());
        let metrics = execution.metrics;
        assert_eq!(metrics.login_requests, 1);
        assert_eq!(metrics.ticket_attempts, 1);
        assert_eq!(metrics.ticket_successes, 1);
        assert_eq!(metrics.login_latency_ms.count(), 1);
        assert_eq!(metrics.ticket_latency_ms.count(), 1);
        assert!(metrics.p99_ms() >= 1);
        assert!(
            metrics
                .virtual_player_states
                .contains_key(&VirtualPlayerState::LoggedOut)
        );
        let output = serde_json::to_string(&metrics).unwrap();
        assert!(!output.contains("in-memory-only"));
        assert!(!output.contains("fake-access-token"));
        assert!(!output.contains("fake-ticket"));
    }

    #[test]
    fn game_credentials_transfer_once_before_deferred_logout_without_metrics_or_debug_exposure() {
        let mut service = FakeAuthHttpService::scripted([FakeAuthOutcome::Success; 4]);
        let mut execution = execute_auth_operations(
            &mut service,
            &[
                AuthOperation::Login,
                AuthOperation::ListCharacters,
                AuthOperation::IssueTicket,
            ],
            "loadtest_000001",
            "loadtest_local_default_000001",
            "in-memory-only",
            |_, _| Ok(Duration::MAX),
        );

        assert!(execution.error.is_none());
        assert_eq!(
            execution.take_game_credentials(),
            Some(("fake-ticket".into(), "fake-character".into()))
        );
        assert_eq!(execution.take_game_credentials(), None);
        let output = serde_json::to_string(&execution.metrics).unwrap();
        assert!(!output.contains("fake-ticket"));
        assert!(!output.contains("fake-character"));
    }

    #[test]
    fn game_mode_defers_final_logout_until_game_cleanup_and_only_sends_once() {
        let operations = [
            AuthOperation::Login,
            AuthOperation::ListCharacters,
            AuthOperation::IssueTicket,
            AuthOperation::Logout,
        ];
        assert_eq!(
            split_game_auth_operations(&operations).unwrap(),
            (
                vec![
                    AuthOperation::Login,
                    AuthOperation::ListCharacters,
                    AuthOperation::IssueTicket,
                ],
                true,
            )
        );
        assert!(
            split_game_auth_operations(&[
                AuthOperation::Login,
                AuthOperation::Logout,
                AuthOperation::IssueTicket,
            ])
            .is_err()
        );

        let mut transport = FakeAuthHttpService::scripted([FakeAuthOutcome::Success; 4]);
        let mut execution = execute_auth_operations(
            &mut transport,
            &operations[..3],
            "loadtest_000001",
            "loadtest_local_default_000001",
            "in-memory-only",
            |_, _| Ok(Duration::MAX),
        );
        assert!(execution.take_game_credentials().is_some());
        // The game follow-up may fail after it consumes the ticket. Cleanup
        // still owns the authenticated access token and must send logout.
        execute_deferred_logout(&mut transport, &mut execution, |_| Ok(Duration::MAX)).unwrap();
        assert_eq!(transport.request_count(), 4);
        assert_eq!(execution.take_game_credentials(), None);
        assert!(
            execute_deferred_logout(&mut transport, &mut execution, |_| Ok(Duration::MAX)).is_err()
        );
        assert_eq!(transport.request_count(), 4);

        let mut inline_transport = FakeAuthHttpService::scripted([FakeAuthOutcome::Success; 4]);
        let mut inline_execution = execute_auth_operations(
            &mut inline_transport,
            &operations,
            "loadtest_000001",
            "loadtest_local_default_000001",
            "in-memory-only",
            |_, _| Ok(Duration::MAX),
        );
        assert_eq!(inline_execution.take_game_credentials(), None);
    }

    #[test]
    fn login_admission_reserves_spaced_slots_before_requests_are_sent() {
        let mut limiter = LoginAdmissionLimiter::new(2.0).unwrap();
        assert_eq!(limiter.reserve(0), 0);
        assert_eq!(limiter.reserve(0), 500_000);
        assert_eq!(limiter.reserve(10), 1_000_000);
        assert_eq!(limiter.admitted(), 3);
    }
}
