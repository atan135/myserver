use std::collections::VecDeque;
use std::time::{Duration, Instant};

use reqwest::blocking::Client;
use serde_json::Value;

use crate::config::EnvironmentKind;
use crate::side_services::{
    PlannedSideServiceStep, ServiceDescriptor, SideFakeOutcome, SideServiceConfig, SideServiceKind,
    SideServiceMetrics, SideServiceOperation, SideServicesScenario, SideTransportKind,
};

const MAX_BURST_READS: usize = 8;
const SLOW_HTTP_RESPONSE_MS: u64 = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SideHttpError {
    LiveTransportForbidden,
    LiveTransportNotEnabled,
    DescriptorRejected,
    InvalidPlan,
    WriteGateRejected,
    MailNotifyUnsupported,
    Timeout,
    Disconnect,
    RateLimited,
    HttpStatus(u16),
    Business(String),
    Admission(String),
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
    pub announce_writes: u64,
    pub notifications: u64,
}

impl SideHttpMetrics {
    pub fn merge_into_metrics(&self, metrics: &mut crate::metrics::Metrics) {
        self.side.merge_into_metrics(metrics);
        metrics.increment("side_mail_writes", self.mail_writes);
        metrics.increment("side_announce_writes", self.announce_writes);
        metrics.increment("side_http_writes", self.writes);
        metrics.increment("side_mail_notifications", self.notifications);
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
        let mut request = self
            .client
            .request(method, format!("{}{}", self.base_url, path))
            .header("connection", "close")
            .header("x-game-ticket", &self.ticket)
            .timeout(self.timeout);
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
        if !(200..300).contains(&status) {
            return Err(SideHttpError::Business(error_code(&body, status)));
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

fn announce_id_from(body: &Value) -> Option<String> {
    body.get("announcements")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("announce_id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
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
    let mut metrics = SideHttpMetrics::default();
    let mut mail_id = None;
    let mut announce_id = None;
    let mut transports: Vec<(SideServiceKind, ReqwestSideHttpTransport)> = Vec::new();
    for step in steps {
        let config = descriptor_for(scenario, step.service)?;
        if step.operation.is_write() {
            check_write_gate(config, environment, selected_batch)?;
        }
        if matches!(step.operation, SideServiceOperation::MailNotify) {
            return Err(SideHttpError::MailNotifyUnsupported);
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
            SideServiceOperation::MailClaim => transport.send(
                reqwest::Method::POST,
                &format!(
                    "/api/v1/mails/{}/claim",
                    mail_id
                        .as_deref()
                        .ok_or(SideHttpError::Business("mail_missing".into()))?
                ),
                Some(serde_json::json!({})),
            ),
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
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test HTTP listener");
        let port = listener.local_addr().expect("listener address").port();
        let handle = thread::spawn(move || {
            for body in responses {
                let (mut stream, _) = listener.accept().expect("accept test HTTP request");
                respond_json(&mut stream, body);
            }
        });
        (port, handle)
    }

    fn respond_json(stream: &mut TcpStream, body: &str) {
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request);
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
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
