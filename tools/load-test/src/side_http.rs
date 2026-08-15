use std::collections::VecDeque;
use std::time::{Duration, Instant};

use reqwest::blocking::Client;
use serde_json::Value;

use crate::chat_wss::LiveMailNotificationListener;
use crate::config::EnvironmentKind;
use crate::side_services::{
    PlannedSideServiceStep, ServiceDescriptor, SideFakeOutcome, SideServiceConfig, SideServiceKind,
    SideServiceMetrics, SideServiceOperation, SideServicesScenario, SideTransportKind,
};

const MAX_BURST_READS: usize = 8;
const SLOW_HTTP_RESPONSE_MS: u64 = 500;
const MAIL_LOAD_TEST_TOKEN_ENV: &str = "MYSERVER_LOADTEST_MAIL_NOTIFICATION_TOKEN";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SideHttpError {
    LiveTransportForbidden,
    LiveTransportNotEnabled,
    DescriptorRejected,
    InvalidPlan,
    WriteGateRejected,
    MailNotifyUnsupported,
    MailNotificationUnavailable,
    Timeout,
    Disconnect,
    RateLimited,
    HttpStatus(u16),
    Business(String),
    MailClaimFailed(MailClaimFailure),
    Admission(String),
}

/// Public mail-claim result semantics retained when a claim is not complete.
/// These fields intentionally mirror only the player-safe HTTP contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailClaimFailure {
    pub http_status: u16,
    pub claim_status: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SideHttpAdmission {
    Connection,
    Message { writes: bool },
}

impl std::fmt::Display for SideHttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SideHttpMetrics {
    pub side: SideServiceMetrics,
    pub writes: u64,
    pub mail_writes: u64,
    /// Internal mail creation is separate from player claim writes so the
    /// report can prove the dedicated send-volume cap.
    pub mail_internal_writes: u64,
    pub announce_writes: u64,
    pub notifications: u64,
    pub mail_notification_outbox_published: u64,
    pub mail_notification_delivery_ms: Vec<u64>,
    pub mail_claim_successes: u64,
    pub mail_claim_idempotent_replays: u64,
    pub mail_claim_processing: u64,
    pub mail_claim_reconciliation_pending: u64,
    pub mail_claim_retryable_failures: u64,
    /// Public mail responses do not carry an outbox/NATS correlation
    /// timestamp. Keep the absence explicit instead of deriving a latency
    /// from the HTTP request duration.
    pub mail_notification_observation_holes: u64,
}

impl SideHttpMetrics {
    pub fn merge_into_metrics(&self, metrics: &mut crate::metrics::Metrics) {
        self.side.merge_into_metrics(metrics);
        metrics.increment("side_mail_writes", self.mail_writes);
        metrics.increment("side_mail_internal_writes", self.mail_internal_writes);
        metrics.increment("side_announce_writes", self.announce_writes);
        metrics.increment("side_http_writes", self.writes);
        metrics.increment("side_mail_notifications", self.notifications);
        metrics.increment(
            "side_mail_notification_outbox_published",
            self.mail_notification_outbox_published,
        );
        for latency_ms in &self.mail_notification_delivery_ms {
            metrics.observe_latency("side_mail_notification_delivery_ms", *latency_ms);
        }
        metrics.increment("side_mail_claim_successes", self.mail_claim_successes);
        metrics.increment(
            "side_mail_claim_idempotent_replays",
            self.mail_claim_idempotent_replays,
        );
        metrics.increment("side_mail_claim_processing", self.mail_claim_processing);
        metrics.increment(
            "side_mail_claim_reconciliation_pending",
            self.mail_claim_reconciliation_pending,
        );
        metrics.increment(
            "side_mail_claim_retryable_failures",
            self.mail_claim_retryable_failures,
        );
        metrics.increment(
            "side_mail_notification_observation_holes",
            self.mail_notification_observation_holes,
        );
    }
}

#[derive(Debug, Clone)]
struct HttpResponse {
    status: u16,
    body: Value,
}

struct ReqwestSideHttpTransport {
    client: Client,
    base_url: String,
    ticket: String,
    timeout: Duration,
}

impl ReqwestSideHttpTransport {
    fn new(
        descriptor: &ServiceDescriptor,
        ticket: &str,
        timeout_ms: u64,
    ) -> Result<Self, SideHttpError> {
        descriptor
            .validate(SideServiceKind::Mail)
            .or_else(|_| descriptor.validate(SideServiceKind::Announce))
            .map_err(|_| SideHttpError::DescriptorRejected)?;
        let scheme = match descriptor.protocol {
            SideTransportKind::Http => "http",
            SideTransportKind::Https => "https",
            _ => return Err(SideHttpError::DescriptorRejected),
        };
        let timeout = Duration::from_millis(timeout_ms.max(1));
        let client = Client::builder()
            .timeout(timeout)
            .http1_only()
            .pool_max_idle_per_host(0)
            .build()
            .map_err(|_| SideHttpError::Disconnect)?;
        Ok(Self {
            client,
            base_url: format!("{scheme}://{}:{}", descriptor.host, descriptor.port),
            ticket: ticket.to_owned(),
            timeout,
        })
    }

    fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<HttpResponse, SideHttpError> {
        let response = self.send_response(method, path, body, None)?;
        if !(200..300).contains(&response.status) {
            return Err(SideHttpError::Business(error_code(
                &response.body,
                response.status,
            )));
        }
        Ok(response)
    }

    fn send_with_extra_header(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
        extra_header: Option<(&str, &str)>,
    ) -> Result<HttpResponse, SideHttpError> {
        let response = self.send_response(method, path, body, extra_header)?;
        if !(200..300).contains(&response.status) {
            return Err(SideHttpError::Business(error_code(
                &response.body,
                response.status,
            )));
        }
        Ok(response)
    }

    /// Mail-claim results use structured HTTP bodies for both completed and
    /// incomplete workflows. Keep those public semantics available to the
    /// caller instead of collapsing a 409/422/503 into a generic code.
    fn send_mail_claim(
        &self,
        path: &str,
        body: Option<Value>,
    ) -> Result<HttpResponse, SideHttpError> {
        self.send_response(reqwest::Method::POST, path, body, None)
    }

    fn send_response(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
        extra_header: Option<(&str, &str)>,
    ) -> Result<HttpResponse, SideHttpError> {
        let mut request = self
            .client
            .request(method, format!("{}{}", self.base_url, path))
            .header("connection", "close")
            .header("x-game-ticket", &self.ticket)
            .timeout(self.timeout);
        if let Some((name, value)) = extra_header {
            request = request.header(name, value);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().map_err(|error| {
            if error.is_timeout() {
                SideHttpError::Timeout
            } else {
                SideHttpError::Disconnect
            }
        })?;
        let status = response.status().as_u16();
        let body = response
            .json::<Value>()
            .unwrap_or_else(|_| Value::Object(Default::default()));
        if status == 429 {
            return Err(SideHttpError::RateLimited);
        }
        if status == 408 || status == 504 {
            return Err(SideHttpError::Timeout);
        }
        Ok(HttpResponse { status, body })
    }
}

fn error_code(body: &Value, status: u16) -> String {
    body.get("error_code")
        .or_else(|| body.get("code"))
        .or_else(|| body.get("errorCode"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("http_{status}"))
}

fn descriptor_for<'a>(
    scenario: &'a SideServicesScenario,
    service: SideServiceKind,
) -> Result<&'a SideServiceConfig, SideHttpError> {
    let config = match service {
        SideServiceKind::Mail => scenario.mail.as_ref(),
        SideServiceKind::Announce => scenario.announce.as_ref(),
        _ => None,
    }
    .ok_or(SideHttpError::InvalidPlan)?;
    if !config.live_http {
        return Err(SideHttpError::LiveTransportNotEnabled);
    }
    if config.descriptor.is_none() {
        return Err(SideHttpError::DescriptorRejected);
    }
    Ok(config)
}

fn check_write_gate(
    config: &SideServiceConfig,
    environment: EnvironmentKind,
    selected_batch: &str,
) -> Result<(), SideHttpError> {
    if !config.writes {
        return Err(SideHttpError::WriteGateRejected);
    }
    if !matches!(environment, EnvironmentKind::Local | EnvironmentKind::Test) {
        return Err(SideHttpError::LiveTransportForbidden);
    }
    let Some(required_batch) = config.write_batch.as_deref() else {
        return Err(SideHttpError::WriteGateRejected);
    };
    if required_batch != selected_batch || selected_batch.trim().is_empty() {
        return Err(SideHttpError::WriteGateRejected);
    }
    Ok(())
}

fn mail_id_from(body: &Value) -> Option<String> {
    body.get("mails")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("mail_id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn mail_notification_from(body: &Value) -> Result<String, SideHttpError> {
    if body
        .get("idempotent_replay")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(SideHttpError::MailNotificationUnavailable);
    }
    let mail_id = body
        .get("mail_id")
        .and_then(Value::as_str)
        .filter(|mail_id| !mail_id.is_empty())
        .ok_or(SideHttpError::MailNotificationUnavailable)?;
    let notification = body
        .get("notification")
        .and_then(Value::as_object)
        .ok_or(SideHttpError::MailNotificationUnavailable)?;
    let expected_event_id = format!("mail.notify:{mail_id}");
    if notification.get("event_id").and_then(Value::as_str) != Some(expected_event_id.as_str())
        || notification
            .get("outbox_published")
            .and_then(Value::as_bool)
            != Some(true)
    {
        return Err(SideHttpError::MailNotificationUnavailable);
    }
    Ok(mail_id.to_owned())
}

fn mail_notification_chat_config(
    scenario: &SideServicesScenario,
) -> Result<&SideServiceConfig, SideHttpError> {
    let chat = scenario
        .chat
        .as_ref()
        .ok_or(SideHttpError::MailNotificationUnavailable)?;
    if !chat.live_websocket {
        return Err(SideHttpError::LiveTransportNotEnabled);
    }
    chat.descriptor
        .as_ref()
        .ok_or(SideHttpError::DescriptorRejected)?
        .validate(SideServiceKind::Chat)
        .map_err(|_| SideHttpError::DescriptorRejected)?;
    Ok(chat)
}

fn announce_id_from(body: &Value) -> Option<String> {
    body.get("announcements")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("announce_id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn record_mail_claim_result(metrics: &mut SideHttpMetrics, body: &Value) {
    let status = body
        .get("claim_status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let already_claimed = body
        .get("already_claimed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    match (status, already_claimed) {
        ("claimed", true) => {
            metrics.mail_claim_idempotent_replays =
                metrics.mail_claim_idempotent_replays.saturating_add(1);
        }
        ("claimed", false) => {
            metrics.mail_claim_successes = metrics.mail_claim_successes.saturating_add(1);
        }
        ("processing", _) => {
            metrics.mail_claim_processing = metrics.mail_claim_processing.saturating_add(1);
        }
        ("reconciliation_pending", _) => {
            metrics.mail_claim_reconciliation_pending =
                metrics.mail_claim_reconciliation_pending.saturating_add(1);
        }
        ("retryable_failure", _) => {
            metrics.mail_claim_retryable_failures =
                metrics.mail_claim_retryable_failures.saturating_add(1);
        }
        _ => {}
    }
    metrics.mail_notification_observation_holes = metrics
        .mail_notification_observation_holes
        .saturating_add(1);
}

fn mail_claim_failure(response: &HttpResponse) -> Option<MailClaimFailure> {
    let claim_status = response
        .body
        .get("claim_status")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown")
        .to_owned();
    if (200..300).contains(&response.status) && claim_status == "claimed" {
        return None;
    }
    let error = response
        .body
        .get("error")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            response
                .body
                .get("error_code")
                .or_else(|| response.body.get("code"))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        });
    Some(MailClaimFailure {
        http_status: response.status,
        claim_status,
        error,
    })
}

fn mail_claim_failure_metric_code(failure: &MailClaimFailure) -> &'static str {
    match failure.claim_status.as_str() {
        "processing" => "mail_claim_processing",
        "reconciliation_pending" => "mail_claim_reconciliation_pending",
        "retryable_failure" => "mail_claim_retryable_failure",
        "blocked_capacity" => "mail_claim_blocked_capacity",
        "permanent_failure" => "mail_claim_permanent_failure",
        "manual_review" => "mail_claim_manual_review",
        _ => "mail_claim_rejected",
    }
}

fn record_success(
    metrics: &mut SideHttpMetrics,
    step: &PlannedSideServiceStep,
    started: Instant,
    writes: bool,
) {
    metrics.side.record(step, SideFakeOutcome::Success);
    let elapsed_ms = started.elapsed().as_millis() as u64;
    metrics.side.record_latency(step.service, elapsed_ms);
    if elapsed_ms >= SLOW_HTTP_RESPONSE_MS {
        let key = match step.service {
            SideServiceKind::Mail => "side_mail_slow",
            SideServiceKind::Announce => "side_announce_slow",
            _ => unreachable!("HTTP runner only accepts mail/announce steps"),
        };
        *metrics.side.counters.entry(key.into()).or_default() += 1;
    }
    if writes {
        metrics.writes = metrics.writes.saturating_add(1);
        match step.service {
            SideServiceKind::Mail => metrics.mail_writes = metrics.mail_writes.saturating_add(1),
            SideServiceKind::Announce => {
                metrics.announce_writes = metrics.announce_writes.saturating_add(1)
            }
            _ => {}
        }
    }
}

fn record_error(
    metrics: &mut SideHttpMetrics,
    step: &PlannedSideServiceStep,
    started: Instant,
    error: &SideHttpError,
) {
    let outcome = match error {
        SideHttpError::RateLimited => SideFakeOutcome::RateLimited,
        SideHttpError::Timeout => SideFakeOutcome::Timeout,
        SideHttpError::Disconnect => SideFakeOutcome::Disconnect,
        SideHttpError::Business(code) => SideFakeOutcome::BusinessError(code.clone()),
        SideHttpError::MailClaimFailed(failure) => {
            SideFakeOutcome::BusinessError(mail_claim_failure_metric_code(failure).into())
        }
        _ => SideFakeOutcome::BusinessError("request_rejected".into()),
    };
    metrics.side.record(step, outcome);
    metrics
        .side
        .record_latency(step.service, started.elapsed().as_millis() as u64);
}

pub fn execute_live_mail_announce_steps(
    scenario: &SideServicesScenario,
    environment: EnvironmentKind,
    selected_batch: &str,
    ticket: &str,
    steps: &[PlannedSideServiceStep],
    timeout_ms: u64,
    admit: impl FnMut(SideHttpAdmission) -> Result<(), SideHttpError>,
) -> Result<SideHttpMetrics, SideHttpError> {
    execute_live_mail_announce_steps_with_mail_notification_token(
        scenario,
        environment,
        selected_batch,
        ticket,
        steps,
        timeout_ms,
        std::env::var(MAIL_LOAD_TEST_TOKEN_ENV).ok(),
        admit,
    )
}

fn execute_live_mail_announce_steps_with_mail_notification_token(
    scenario: &SideServicesScenario,
    environment: EnvironmentKind,
    selected_batch: &str,
    ticket: &str,
    steps: &[PlannedSideServiceStep],
    timeout_ms: u64,
    runtime_mail_notification_token: Option<String>,
    mut admit: impl FnMut(SideHttpAdmission) -> Result<(), SideHttpError>,
) -> Result<SideHttpMetrics, SideHttpError> {
    if !matches!(environment, EnvironmentKind::Local | EnvironmentKind::Test) {
        return Err(SideHttpError::LiveTransportForbidden);
    }
    if ticket.is_empty() {
        return Err(SideHttpError::Business("ticket_missing".into()));
    }
    if steps.iter().any(|step| {
        !matches!(
            step.service,
            SideServiceKind::Mail | SideServiceKind::Announce
        )
    }) {
        return Err(SideHttpError::InvalidPlan);
    }
    // Validate the runtime-only secret before any descriptor lookup,
    // admission, or transport setup. A missing secret must be side-effect
    // free even for a live mail-notification plan.
    let mail_notification_token = if steps
        .iter()
        .any(|step| step.operation == SideServiceOperation::MailNotify)
    {
        Some(
            runtime_mail_notification_token
                .filter(|token| !token.is_empty())
                .ok_or(SideHttpError::MailNotificationUnavailable)?,
        )
    } else {
        None
    };
    let mut metrics = SideHttpMetrics::default();
    let mut mail_id = None;
    let mut announce_id = None;
    let mut transports: Vec<(SideServiceKind, ReqwestSideHttpTransport)> = Vec::new();
    for step in steps {
        let config = descriptor_for(scenario, step.service)?;
        if step.operation.is_write() {
            check_write_gate(config, environment, selected_batch)?;
        }
        let descriptor = config
            .descriptor
            .as_ref()
            .ok_or(SideHttpError::DescriptorRejected)?;
        if step.think_time_ms > 0 {
            if step.think_time_ms > timeout_ms.max(1) {
                return Err(SideHttpError::Timeout);
            }
            std::thread::sleep(Duration::from_millis(step.think_time_ms));
        }
        let mut mail_listener = if step.operation == SideServiceOperation::MailNotify {
            let chat = mail_notification_chat_config(scenario)?;
            let descriptor = chat.descriptor.as_ref().expect("validated chat descriptor");
            admit(SideHttpAdmission::Connection)?;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .enable_time()
                .build()
                .map_err(|_| SideHttpError::MailNotificationUnavailable)?;
            let listener = runtime
                .block_on(LiveMailNotificationListener::connect(
                    descriptor,
                    environment,
                    ticket.to_owned(),
                    timeout_ms,
                ))
                .map_err(|_| SideHttpError::MailNotificationUnavailable)?;
            Some((runtime, listener))
        } else {
            None
        };
        let index = transports
            .iter()
            .position(|(service, _)| *service == step.service);
        if index.is_none() {
            admit(SideHttpAdmission::Connection)?;
            transports.push((
                step.service,
                ReqwestSideHttpTransport::new(descriptor, ticket, timeout_ms)?,
            ));
        }
        admit(SideHttpAdmission::Message {
            writes: step.operation.is_write(),
        })?;
        let transport = &transports
            .iter()
            .find(|(service, _)| *service == step.service)
            .expect("transport was inserted")
            .1;
        let started = Instant::now();
        let result = match step.operation {
            SideServiceOperation::MailList => transport.send(
                reqwest::Method::GET,
                "/api/v1/mails?limit=20&offset=0",
                None,
            ),
            SideServiceOperation::MailDetail => transport.send(
                reqwest::Method::GET,
                &format!(
                    "/api/v1/mails/{}",
                    mail_id
                        .as_deref()
                        .ok_or(SideHttpError::Business("mail_missing".into()))?
                ),
                None,
            ),
            SideServiceOperation::MailRead => transport.send(
                reqwest::Method::PUT,
                &format!(
                    "/api/v1/mails/{}/read",
                    mail_id
                        .as_deref()
                        .ok_or(SideHttpError::Business("mail_missing".into()))?
                ),
                Some(serde_json::json!({})),
            ),
            SideServiceOperation::MailClaim => transport.send_mail_claim(
                &format!(
                    "/api/v1/mails/{}/claim",
                    mail_id
                        .as_deref()
                        .ok_or(SideHttpError::Business("mail_missing".into()))?
                ),
                Some(serde_json::json!({})),
            ),
            SideServiceOperation::MailNotify => {
                let token = mail_notification_token
                    .as_deref()
                    .expect("mail notification token was validated before transport setup");
                transport.send_with_extra_header(
                    reqwest::Method::POST,
                    "/api/v1/mails/load-test/notification",
                    Some(serde_json::json!({ "batch": selected_batch })),
                    Some(("x-mail-load-test-token", &token)),
                )
            }
            SideServiceOperation::AnnounceList | SideServiceOperation::AnnounceBurstRead => {
                transport.send(
                    reqwest::Method::GET,
                    "/api/v1/announcements?limit=20&offset=0&active_only=true",
                    None,
                )
            }
            SideServiceOperation::AnnounceDetail => transport.send(
                reqwest::Method::GET,
                &format!(
                    "/api/v1/announcements/{}",
                    announce_id
                        .as_deref()
                        .ok_or(SideHttpError::Business("announce_missing".into()))?
                ),
                None,
            ),
            _ => return Err(SideHttpError::InvalidPlan),
        };
        match result {
            Ok(response) => {
                if step.operation == SideServiceOperation::MailList {
                    mail_id = mail_id_from(&response.body);
                }
                if step.operation == SideServiceOperation::MailClaim {
                    record_mail_claim_result(&mut metrics, &response.body);
                    if let Some(failure) = mail_claim_failure(&response) {
                        let error = SideHttpError::MailClaimFailed(failure);
                        record_error(&mut metrics, step, started, &error);
                        return Err(error);
                    }
                }
                if step.operation == SideServiceOperation::MailNotify {
                    let observed_mail_id = mail_notification_from(&response.body)?;
                    let (runtime, listener) = mail_listener
                        .as_mut()
                        .expect("mail notification listener was connected");
                    let delivery_started = started;
                    runtime
                        .block_on(
                            listener.wait_for_mail(
                                &observed_mail_id,
                                timeout_ms
                                    .saturating_sub(delivery_started.elapsed().as_millis() as u64),
                            ),
                        )
                        .map_err(|_| SideHttpError::MailNotificationUnavailable)?;
                    metrics.notifications = metrics.notifications.saturating_add(1);
                    metrics.mail_internal_writes = metrics.mail_internal_writes.saturating_add(1);
                    metrics.mail_notification_outbox_published =
                        metrics.mail_notification_outbox_published.saturating_add(1);
                    metrics
                        .mail_notification_delivery_ms
                        .push(delivery_started.elapsed().as_millis() as u64);
                    mail_id = Some(observed_mail_id);
                }
                if matches!(
                    step.operation,
                    SideServiceOperation::AnnounceList | SideServiceOperation::AnnounceBurstRead
                ) {
                    announce_id = announce_id_from(&response.body);
                }
                record_success(&mut metrics, step, started, step.operation.is_write());
                let _ = response.status;
            }
            Err(error) => {
                record_error(&mut metrics, step, started, &error);
                return Err(error);
            }
        }
        if step.operation == SideServiceOperation::AnnounceBurstRead {
            for _ in 1..MAX_BURST_READS {
                if step.think_time_ms > 0 {
                    if step.think_time_ms > timeout_ms.max(1) {
                        return Err(SideHttpError::Timeout);
                    }
                    std::thread::sleep(Duration::from_millis(step.think_time_ms));
                }
                admit(SideHttpAdmission::Message { writes: false })?;
                let started = Instant::now();
                match transport.send(
                    reqwest::Method::GET,
                    "/api/v1/announcements?limit=20&offset=0&active_only=true",
                    None,
                ) {
                    Ok(response) => {
                        announce_id = announce_id_from(&response.body).or(announce_id);
                        record_success(&mut metrics, step, started, false);
                    }
                    Err(error) => {
                        record_error(&mut metrics, step, started, &error);
                        return Err(error);
                    }
                }
            }
        }
    }
    Ok(metrics)
}

#[derive(Debug, Clone)]
pub struct DeterministicSideHttpFake {
    outcomes: VecDeque<SideFakeOutcome>,
    pub requests: u64,
}

impl DeterministicSideHttpFake {
    pub fn scripted(outcomes: impl IntoIterator<Item = SideFakeOutcome>) -> Self {
        Self {
            outcomes: outcomes.into_iter().collect(),
            requests: 0,
        }
    }

    pub fn execute(
        &mut self,
        step: &PlannedSideServiceStep,
        metrics: &mut SideHttpMetrics,
    ) -> Result<(), SideHttpError> {
        self.requests = self.requests.saturating_add(1);
        let outcome = self
            .outcomes
            .pop_front()
            .unwrap_or(SideFakeOutcome::Success);
        let started = Instant::now();
        if matches!(outcome, SideFakeOutcome::Success | SideFakeOutcome::Slow) {
            record_success(metrics, step, started, step.operation.is_write());
            if matches!(outcome, SideFakeOutcome::Slow) {
                let key = match step.service {
                    SideServiceKind::Mail => "side_mail_slow",
                    SideServiceKind::Announce => "side_announce_slow",
                    _ => unreachable!("HTTP fake only accepts mail/announce steps"),
                };
                *metrics.side.counters.entry(key.into()).or_default() += 1;
            }
            Ok(())
        } else {
            let error = match outcome {
                SideFakeOutcome::RateLimited => SideHttpError::RateLimited,
                SideFakeOutcome::Slow | SideFakeOutcome::Timeout => SideHttpError::Timeout,
                SideFakeOutcome::Disconnect => SideHttpError::Disconnect,
                SideFakeOutcome::BusinessError(code) => SideHttpError::Business(code),
                _ => SideHttpError::Business("push_outcome_not_applicable".into()),
            };
            record_error(metrics, step, started, &error);
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::side_services::{ServiceDescriptor, SideServiceStep};
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    fn step(service: SideServiceKind, operation: SideServiceOperation) -> PlannedSideServiceStep {
        PlannedSideServiceStep {
            service,
            operation,
            weight: 1,
            think_time_ms: 0,
        }
    }

    fn scenario(writes: bool) -> SideServicesScenario {
        scenario_with_port(writes, 1)
    }

    fn scenario_with_port(writes: bool, port: u16) -> SideServicesScenario {
        let descriptor = |service| SideServiceConfig {
            descriptor: Some(ServiceDescriptor {
                host: "127.0.0.1".into(),
                port,
                protocol: SideTransportKind::Http,
            }),
            steps: vec![SideServiceStep {
                operation: if service == SideServiceKind::Mail {
                    SideServiceOperation::MailList
                } else {
                    SideServiceOperation::AnnounceList
                },
                weight: 1,
                think_time_ms: 0,
            }],
            writes,
            live_http: true,
            write_batch: writes.then(|| "loadtest-local".into()),
            ..Default::default()
        };
        SideServicesScenario {
            mail: Some(descriptor(SideServiceKind::Mail)),
            announce: Some(descriptor(SideServiceKind::Announce)),
            ..Default::default()
        }
    }

    fn spawn_json_server(responses: Vec<&'static str>) -> (u16, thread::JoinHandle<()>) {
        spawn_json_server_with_status(responses.into_iter().map(|body| (200, body)).collect())
    }

    fn spawn_json_server_with_status(
        responses: Vec<(u16, &'static str)>,
    ) -> (u16, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test HTTP listener");
        let port = listener.local_addr().expect("listener address").port();
        let handle = thread::spawn(move || {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().expect("accept test HTTP request");
                respond_json(&mut stream, status, body);
            }
        });
        (port, handle)
    }

    fn respond_json(stream: &mut TcpStream, status: u16, body: &str) {
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request);
        let response = format!(
            "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write test HTTP response");
    }

    #[test]
    fn fake_covers_success_rate_limit_timeout_disconnect_business_error() {
        let mut fake = DeterministicSideHttpFake::scripted([
            SideFakeOutcome::Success,
            SideFakeOutcome::Slow,
            SideFakeOutcome::RateLimited,
            SideFakeOutcome::Timeout,
            SideFakeOutcome::Disconnect,
            SideFakeOutcome::BusinessError("MAIL_BUSY".into()),
        ]);
        let mut metrics = SideHttpMetrics::default();
        let operation = SideServiceOperation::MailList;
        for expected in [
            Ok(()),
            Ok(()),
            Err(SideHttpError::RateLimited),
            Err(SideHttpError::Timeout),
            Err(SideHttpError::Disconnect),
            Err(SideHttpError::Business("MAIL_BUSY".into())),
        ] {
            assert_eq!(
                fake.execute(
                    &step(SideServiceKind::Mail, operation.clone()),
                    &mut metrics
                ),
                expected
            );
        }
        assert_eq!(fake.requests, 6);
        assert_eq!(metrics.side.counters["side_mail_operations"], 6);
        assert_eq!(metrics.side.counters["side_mail_slow"], 1);
    }

    #[test]
    fn mail_claim_classification_preserves_idempotency_and_observation_holes() {
        let mut metrics = SideHttpMetrics::default();
        for body in [
            serde_json::json!({"claim_status":"claimed", "already_claimed":false}),
            serde_json::json!({"claim_status":"claimed", "already_claimed":true}),
            serde_json::json!({"claim_status":"processing", "already_claimed":false}),
            serde_json::json!({"claim_status":"reconciliation_pending"}),
            serde_json::json!({"claim_status":"retryable_failure"}),
        ] {
            record_mail_claim_result(&mut metrics, &body);
        }
        assert_eq!(metrics.mail_claim_successes, 1);
        assert_eq!(metrics.mail_claim_idempotent_replays, 1);
        assert_eq!(metrics.mail_claim_processing, 1);
        assert_eq!(metrics.mail_claim_reconciliation_pending, 1);
        assert_eq!(metrics.mail_claim_retryable_failures, 1);
        assert_eq!(metrics.mail_notification_observation_holes, 5);

        let mut projected = crate::metrics::Metrics::default();
        metrics.merge_into_metrics(&mut projected);
        let snapshot = projected.snapshot();
        assert_eq!(snapshot.counters["side_mail_claim_idempotent_replays"], 1);
        assert_eq!(
            snapshot.counters["side_mail_notification_observation_holes"],
            5
        );
    }

    #[test]
    fn live_mail_claim_preserves_incomplete_public_contracts() {
        for (status, body, expected) in [
            (
                409,
                r#"{"claim_status":"manual_review","error":"MAIL_CLAIM_ROUTE_UNAVAILABLE"}"#,
                MailClaimFailure {
                    http_status: 409,
                    claim_status: "manual_review".into(),
                    error: Some("MAIL_CLAIM_ROUTE_UNAVAILABLE".into()),
                },
            ),
            (
                202,
                r#"{"claim_status":"processing"}"#,
                MailClaimFailure {
                    http_status: 202,
                    claim_status: "processing".into(),
                    error: None,
                },
            ),
            (
                202,
                r#"{"claim_status":"reconciliation_pending","error":"MAIL_CLAIM_RECONCILIATION_PENDING"}"#,
                MailClaimFailure {
                    http_status: 202,
                    claim_status: "reconciliation_pending".into(),
                    error: Some("MAIL_CLAIM_RECONCILIATION_PENDING".into()),
                },
            ),
        ] {
            let (port, server) = spawn_json_server_with_status(vec![
                (200, r#"{"mails":[{"mail_id":"mail-1"}]}"#),
                (status, body),
            ]);
            let scenario = scenario_with_port(true, port);
            let result = execute_live_mail_announce_steps(
                &scenario,
                EnvironmentKind::Local,
                "loadtest-local",
                "ticket",
                &[
                    step(SideServiceKind::Mail, SideServiceOperation::MailList),
                    step(SideServiceKind::Mail, SideServiceOperation::MailClaim),
                ],
                1_000,
                |_| Ok(()),
            );
            assert_eq!(result, Err(SideHttpError::MailClaimFailed(expected)));
            server.join().expect("test HTTP server thread");
        }
    }

    #[test]
    fn live_mail_claim_accepts_claimed_and_only_counts_claimed_replay_as_idempotent() {
        let (port, server) = spawn_json_server(vec![
            r#"{"mails":[{"mail_id":"mail-1"}]}"#,
            r#"{"claim_status":"claimed","already_claimed":false}"#,
            r#"{"claim_status":"claimed","already_claimed":true}"#,
        ]);
        let scenario = scenario_with_port(true, port);
        let metrics = execute_live_mail_announce_steps(
            &scenario,
            EnvironmentKind::Local,
            "loadtest-local",
            "ticket",
            &[
                step(SideServiceKind::Mail, SideServiceOperation::MailList),
                step(SideServiceKind::Mail, SideServiceOperation::MailClaim),
                step(SideServiceKind::Mail, SideServiceOperation::MailClaim),
            ],
            1_000,
            |_| Ok(()),
        )
        .expect("claimed responses should complete the mail-claim phase");
        assert_eq!(metrics.mail_claim_successes, 1);
        assert_eq!(metrics.mail_claim_idempotent_replays, 1);
        assert_eq!(metrics.writes, 2);
        server.join().expect("test HTTP server thread");
    }

    #[test]
    fn mail_notification_requires_a_fresh_outbox_publish_correlation() {
        let ready = serde_json::json!({
            "mail_id": "loadtest-mail-1",
            "idempotent_replay": false,
            "notification": {
                "event_id": "mail.notify:loadtest-mail-1",
                "outbox_published": true
            }
        });
        assert_eq!(mail_notification_from(&ready).unwrap(), "loadtest-mail-1");

        for rejected in [
            serde_json::json!({
                "mail_id": "loadtest-mail-1",
                "idempotent_replay": true,
                "notification": { "event_id": "mail.notify:loadtest-mail-1", "outbox_published": true }
            }),
            serde_json::json!({
                "mail_id": "loadtest-mail-1",
                "notification": { "event_id": "mail.notify:other", "outbox_published": true }
            }),
            serde_json::json!({
                "mail_id": "loadtest-mail-1",
                "notification": { "event_id": "mail.notify:loadtest-mail-1", "outbox_published": false }
            }),
        ] {
            assert_eq!(
                mail_notification_from(&rejected),
                Err(SideHttpError::MailNotificationUnavailable)
            );
        }
    }

    #[test]
    fn missing_mail_notification_token_rejects_before_admission_or_transport() {
        let steps = [step(
            SideServiceKind::Mail,
            SideServiceOperation::MailNotify,
        )];
        let mut admissions = 0;
        let result = execute_live_mail_announce_steps_with_mail_notification_token(
            &SideServicesScenario::default(),
            EnvironmentKind::Local,
            "loadtest-local",
            "ticket",
            &steps,
            10,
            None,
            |_| {
                admissions += 1;
                Ok(())
            },
        );

        assert_eq!(result, Err(SideHttpError::MailNotificationUnavailable));
        assert_eq!(admissions, 0, "missing token must not admit a connection");
    }

    #[test]
    fn production_and_missing_batch_writes_fail_before_transport() {
        let scenario = scenario(true);
        let steps = [step(
            SideServiceKind::Announce,
            SideServiceOperation::AnnounceCreate,
        )];
        let result = execute_live_mail_announce_steps(
            &scenario,
            EnvironmentKind::Production,
            "loadtest-prod",
            "ticket",
            &steps,
            10,
            |_| panic!("admission must not run"),
        );
        assert!(matches!(result, Err(SideHttpError::LiveTransportForbidden)));
    }

    #[test]
    fn service_live_http_gate_rejects_mail_writes_before_admission_or_transport() {
        let (port, server) = spawn_json_server(vec![
            r#"{"mails":[{"mail_id":"mail-1"}]}"#,
            r#"{"claim_status":"claimed","already_claimed":false}"#,
        ]);
        let mut scenario = scenario_with_port(true, port);
        scenario.mail.as_mut().unwrap().live_http = false;
        let steps = [
            step(SideServiceKind::Mail, SideServiceOperation::MailList),
            step(SideServiceKind::Mail, SideServiceOperation::MailClaim),
        ];
        let mut admissions = Vec::new();
        let result = execute_live_mail_announce_steps(
            &scenario,
            EnvironmentKind::Local,
            "loadtest-local",
            "ticket",
            &steps,
            20,
            |admission| {
                admissions.push(admission);
                Ok(())
            },
        );
        assert!(matches!(
            result,
            Err(SideHttpError::LiveTransportNotEnabled)
        ));
        assert!(admissions.is_empty(), "mail write must not be admitted");

        scenario.mail.as_mut().unwrap().live_http = true;
        let metrics = execute_live_mail_announce_steps(
            &scenario,
            EnvironmentKind::Local,
            "loadtest-local",
            "ticket",
            &steps,
            1_000,
            |admission| {
                admissions.push(admission);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(metrics.writes, 1);
        assert_eq!(metrics.mail_writes, 1);
        assert_eq!(
            admissions,
            vec![
                SideHttpAdmission::Connection,
                SideHttpAdmission::Message { writes: false },
                SideHttpAdmission::Message { writes: true },
            ]
        );
        server.join().expect("test HTTP server thread");
    }

    #[test]
    fn admission_rejection_happens_before_http_request() {
        let scenario = scenario(false);
        let steps = [step(SideServiceKind::Mail, SideServiceOperation::MailList)];
        let result = execute_live_mail_announce_steps(
            &scenario,
            EnvironmentKind::Local,
            "loadtest-local",
            "ticket",
            &steps,
            10,
            |_| Err(SideHttpError::Admission("stopped".into())),
        );
        assert!(matches!(result, Err(SideHttpError::Admission(_))));
    }

    #[test]
    fn missing_mail_id_fails_closed_without_an_extra_http_request() {
        for operation in [
            SideServiceOperation::MailDetail,
            SideServiceOperation::MailRead,
            SideServiceOperation::MailClaim,
        ] {
            let writes = operation.is_write();
            let (port, server) = spawn_json_server(vec![r#"{"mails":[]}"#]);
            let scenario = scenario_with_port(writes, port);
            let mut admissions = Vec::new();
            let result = execute_live_mail_announce_steps(
                &scenario,
                EnvironmentKind::Local,
                "loadtest-local",
                "ticket",
                &[
                    step(SideServiceKind::Mail, SideServiceOperation::MailList),
                    step(SideServiceKind::Mail, operation),
                ],
                1000,
                |admission| {
                    admissions.push(admission);
                    Ok(())
                },
            );
            assert_eq!(result, Err(SideHttpError::Business("mail_missing".into())));
            assert_eq!(
                admissions,
                vec![
                    SideHttpAdmission::Connection,
                    SideHttpAdmission::Message { writes: false },
                    SideHttpAdmission::Message { writes },
                ]
            );
            server.join().expect("test HTTP server thread");
        }
    }

    #[test]
    fn missing_announce_id_fails_closed_without_an_extra_http_request() {
        let (port, server) = spawn_json_server(vec![r#"{"announcements":[]}"#]);
        let scenario = scenario_with_port(false, port);
        let mut admissions = Vec::new();
        let result = execute_live_mail_announce_steps(
            &scenario,
            EnvironmentKind::Local,
            "loadtest-local",
            "ticket",
            &[
                step(
                    SideServiceKind::Announce,
                    SideServiceOperation::AnnounceList,
                ),
                step(
                    SideServiceKind::Announce,
                    SideServiceOperation::AnnounceDetail,
                ),
            ],
            1000,
            |admission| {
                admissions.push(admission);
                Ok(())
            },
        );
        assert_eq!(
            result,
            Err(SideHttpError::Business("announce_missing".into()))
        );
        assert_eq!(
            admissions,
            vec![
                SideHttpAdmission::Connection,
                SideHttpAdmission::Message { writes: false },
                SideHttpAdmission::Message { writes: false },
            ]
        );
        server.join().expect("test HTTP server thread");
    }

    #[test]
    fn live_http_admission_orders_connections_messages_and_burst_reads() {
        let (port, server) = spawn_json_server(vec![
            r#"{"mails":[{"mail_id":"mail-1"}]}"#,
            r#"{"announcements":[{"announce_id":"announce-1"}]}"#,
        ]);
        let scenario = scenario_with_port(false, port);
        let mut admissions = Vec::new();
        let result = execute_live_mail_announce_steps(
            &scenario,
            EnvironmentKind::Local,
            "loadtest-local",
            "ticket",
            &[
                step(SideServiceKind::Mail, SideServiceOperation::MailList),
                step(
                    SideServiceKind::Announce,
                    SideServiceOperation::AnnounceList,
                ),
            ],
            1000,
            |admission| {
                admissions.push(admission);
                Ok(())
            },
        );
        assert!(result.is_ok());
        assert_eq!(
            admissions,
            vec![
                SideHttpAdmission::Connection,
                SideHttpAdmission::Message { writes: false },
                SideHttpAdmission::Connection,
                SideHttpAdmission::Message { writes: false },
            ]
        );
        server.join().expect("test HTTP server thread");

        let (port, server) = spawn_json_server(vec![
            r#"{"announcements":[{"announce_id":"announce-1"}]}"#;
            MAX_BURST_READS
        ]);
        let scenario = scenario_with_port(false, port);
        let mut admissions = Vec::new();
        let result = execute_live_mail_announce_steps(
            &scenario,
            EnvironmentKind::Local,
            "loadtest-local",
            "ticket",
            &[step(
                SideServiceKind::Announce,
                SideServiceOperation::AnnounceBurstRead,
            )],
            1000,
            |admission| {
                admissions.push(admission);
                Ok(())
            },
        );
        assert!(result.is_ok());
        assert_eq!(admissions.len(), 1 + MAX_BURST_READS);
        assert_eq!(admissions[0], SideHttpAdmission::Connection);
        assert!(
            admissions[1..]
                .iter()
                .all(|admission| *admission == SideHttpAdmission::Message { writes: false })
        );
        server.join().expect("test HTTP server thread");
    }
}
