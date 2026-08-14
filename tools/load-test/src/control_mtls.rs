//! Private mTLS controller/worker control plane.
//!
//! This module intentionally owns a protocol and TLS material that are
//! separate from every player-facing HTTP/KCP/WSS/gRPC transport. It accepts
//! only an existing `WorkerAssignment`, `MetricBatch`, `WorkerHeartbeat`, and
//! `AbortSignal` serialized from the load-test contracts; account pools,
//! tickets, target descriptors, and target credentials have no representation
//! in this boundary.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::{
    Certificate, Channel, ClientTlsConfig, Endpoint, Identity, Server, ServerTlsConfig,
};
use tonic::{Request, Response, Status};

use crate::SCHEMA_VERSION;
use crate::contracts::{AbortSignal, MetricBatch, RunPlan, WorkerAssignment, WorkerHeartbeat};
use crate::distributed::{
    BatchDisposition, DistributedError, DistributedMetricsAggregator, WorkerControlState,
    validate_assignment, validate_run_plan, validate_schema_version, worker_state_after_disconnect,
};

pub const CONTROL_PROTOCOL_VERSION: u32 = 1;
pub const MAX_CONTROL_CREDENTIAL_TTL_MS: u64 = 15 * 60 * 1_000;
const MAX_CONTROL_JSON_BYTES: usize = 1_048_576;
const MAX_REGISTRATION_NONCE_BYTES: usize = 128;

type ControlNow = Arc<dyn Fn() -> u64 + Send + Sync>;

#[derive(Debug, Error)]
pub enum ControlMtlsError {
    #[error("control endpoint must use an https URL")]
    InsecureEndpoint,
    #[error("control endpoint is invalid")]
    InvalidEndpoint,
    #[error("control TLS material is incomplete")]
    IncompleteTlsMaterial,
    #[error("controller state rejected: {0}")]
    State(String),
    #[error("control transport failed: {0}")]
    Transport(#[from] tonic::transport::Error),
    #[error("control RPC failed: {0}")]
    Rpc(#[from] tonic::Status),
    #[error("control contract JSON failed: {0}")]
    ContractJson(#[from] serde_json::Error),
    #[error("control listener failed: {0}")]
    Listener(#[from] std::io::Error),
}

/// An intentionally separate endpoint type. It does not accept player
/// endpoints or expose a conversion from `LoadTestConfig::targets`.
#[derive(Clone, PartialEq, Eq)]
pub struct ControlEndpoint {
    uri: String,
}

impl fmt::Debug for ControlEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ControlEndpoint")
            .field(&self.uri)
            .finish()
    }
}

impl ControlEndpoint {
    pub fn parse(uri: impl Into<String>) -> Result<Self, ControlMtlsError> {
        let uri = uri.into();
        let parsed: tonic::codegen::http::Uri =
            uri.parse().map_err(|_| ControlMtlsError::InvalidEndpoint)?;
        if parsed.scheme_str() != Some("https") {
            return Err(ControlMtlsError::InsecureEndpoint);
        }
        if parsed.authority().is_none() || parsed.path_and_query().is_some_and(|path| path != "/") {
            return Err(ControlMtlsError::InvalidEndpoint);
        }
        Ok(Self { uri })
    }

    pub fn as_str(&self) -> &str {
        &self.uri
    }
}

/// Controller-side TLS input. It is deliberately distinct from worker TLS
/// input so one role cannot accidentally reuse the other role's private key.
pub struct ControlServerTlsMaterial {
    pub server_certificate_pem: Vec<u8>,
    pub server_private_key_pem: Vec<u8>,
    pub worker_ca_certificate_pem: Vec<u8>,
}

impl fmt::Debug for ControlServerTlsMaterial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ControlServerTlsMaterial")
            .field("server_certificate_pem", &"<redacted>")
            .field("server_private_key_pem", &"<redacted>")
            .field("worker_ca_certificate_pem", &"<redacted>")
            .finish()
    }
}

impl ControlServerTlsMaterial {
    fn tls_config(&self) -> Result<ServerTlsConfig, ControlMtlsError> {
        require_tls_material(
            &self.server_certificate_pem,
            &self.server_private_key_pem,
            &self.worker_ca_certificate_pem,
        )?;
        Ok(ServerTlsConfig::new()
            .identity(Identity::from_pem(
                self.server_certificate_pem.clone(),
                self.server_private_key_pem.clone(),
            ))
            .client_ca_root(Certificate::from_pem(
                self.worker_ca_certificate_pem.clone(),
            )))
    }
}

/// Worker-side TLS input. It contains only the controller CA and this worker's
/// own client identity, never player target credentials or account secrets.
pub struct ControlWorkerTlsMaterial {
    pub controller_ca_certificate_pem: Vec<u8>,
    pub worker_certificate_pem: Vec<u8>,
    pub worker_private_key_pem: Vec<u8>,
}

impl fmt::Debug for ControlWorkerTlsMaterial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ControlWorkerTlsMaterial")
            .field("controller_ca_certificate_pem", &"<redacted>")
            .field("worker_certificate_pem", &"<redacted>")
            .field("worker_private_key_pem", &"<redacted>")
            .finish()
    }
}

impl ControlWorkerTlsMaterial {
    fn tls_config(&self) -> Result<ClientTlsConfig, ControlMtlsError> {
        require_tls_material(
            &self.worker_certificate_pem,
            &self.worker_private_key_pem,
            &self.controller_ca_certificate_pem,
        )?;
        Ok(ClientTlsConfig::new()
            .domain_name("localhost")
            .ca_certificate(Certificate::from_pem(
                self.controller_ca_certificate_pem.clone(),
            ))
            .identity(Identity::from_pem(
                self.worker_certificate_pem.clone(),
                self.worker_private_key_pem.clone(),
            )))
    }
}

fn require_tls_material(
    certificate: &[u8],
    private_key: &[u8],
    ca_certificate: &[u8],
) -> Result<(), ControlMtlsError> {
    if certificate.is_empty() || private_key.is_empty() || ca_certificate.is_empty() {
        return Err(ControlMtlsError::IncompleteTlsMaterial);
    }
    Ok(())
}

#[derive(Clone, PartialEq, Eq)]
pub struct WorkerControlCredential {
    pub credential_id: String,
    pub run_id: String,
    pub worker_id: String,
    pub certificate_fingerprint: String,
    pub issued_unix_ms: u64,
    pub expires_unix_ms: u64,
    registration_nonce: Vec<u8>,
}

impl fmt::Debug for WorkerControlCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkerControlCredential")
            .field("credential_id", &self.credential_id)
            .field("run_id", &self.run_id)
            .field("worker_id", &self.worker_id)
            .field("certificate_fingerprint", &self.certificate_fingerprint)
            .field("issued_unix_ms", &self.issued_unix_ms)
            .field("expires_unix_ms", &self.expires_unix_ms)
            .field("registration_nonce", &"<redacted>")
            .finish()
    }
}

impl WorkerControlCredential {
    pub fn new(
        credential_id: impl Into<String>,
        run_id: impl Into<String>,
        worker_id: impl Into<String>,
        certificate_fingerprint: impl Into<String>,
        issued_unix_ms: u64,
        expires_unix_ms: u64,
        registration_nonce: Vec<u8>,
    ) -> Self {
        Self {
            credential_id: credential_id.into(),
            run_id: run_id.into(),
            worker_id: worker_id.into(),
            certificate_fingerprint: certificate_fingerprint.into(),
            issued_unix_ms,
            expires_unix_ms,
            registration_nonce,
        }
    }

    fn validate(&self, now_unix_ms: u64) -> Result<(), String> {
        if self.credential_id.trim().is_empty()
            || self.run_id.trim().is_empty()
            || self.worker_id.trim().is_empty()
            || self.certificate_fingerprint.trim().is_empty()
            || self.registration_nonce.is_empty()
            || self.registration_nonce.len() > MAX_REGISTRATION_NONCE_BYTES
            || self.issued_unix_ms >= self.expires_unix_ms
            || self.expires_unix_ms.saturating_sub(self.issued_unix_ms)
                > MAX_CONTROL_CREDENTIAL_TTL_MS
            || now_unix_ms < self.issued_unix_ms
            || now_unix_ms >= self.expires_unix_ms
        {
            return Err("credential is empty, invalid, expired, or exceeds the short TTL".into());
        }
        Ok(())
    }
}

pub fn certificate_fingerprint_from_der(certificate_der: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(certificate_der))
}

pub fn certificate_fingerprint_from_pem(
    certificate_pem: &[u8],
) -> Result<String, ControlMtlsError> {
    let text = std::str::from_utf8(certificate_pem)
        .map_err(|_| ControlMtlsError::State("certificate PEM is not UTF-8".into()))?;
    let encoded = text
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect::<String>();
    let der = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| ControlMtlsError::State("certificate PEM cannot be decoded".into()))?;
    if der.is_empty() {
        return Err(ControlMtlsError::State("certificate PEM is empty".into()));
    }
    Ok(certificate_fingerprint_from_der(&der))
}

#[derive(Debug, Clone)]
struct ControlSession {
    run_id: String,
    worker_id: String,
    credential_id: String,
    certificate_fingerprint: String,
    next_sequence: u64,
}

/// Controller-owned state. `WorkerAssignment` is the only worker payload held
/// here; account shards and all player-facing secrets stay outside this module.
#[derive(Debug)]
pub struct ControlRunState {
    plan: RunPlan,
    assignments: BTreeMap<String, WorkerAssignment>,
    credentials: BTreeMap<String, WorkerControlCredential>,
    consumed_credentials: BTreeSet<String>,
    sessions: BTreeMap<String, ControlSession>,
    heartbeats: BTreeMap<String, WorkerHeartbeat>,
    abort: Option<AbortSignal>,
    metrics: DistributedMetricsAggregator,
    next_session_id: u64,
}

impl ControlRunState {
    pub fn new(
        plan: RunPlan,
        assignments: impl IntoIterator<Item = WorkerAssignment>,
    ) -> Result<Self, ControlMtlsError> {
        validate_run_plan(&plan).map_err(distributed_state_error)?;
        let mut by_worker = BTreeMap::new();
        for assignment in assignments {
            validate_assignment(&assignment, &plan).map_err(distributed_state_error)?;
            if by_worker
                .insert(assignment.worker_id.clone(), assignment)
                .is_some()
            {
                return Err(ControlMtlsError::State(
                    "each worker may receive exactly one assignment".into(),
                ));
            }
        }
        if by_worker.is_empty() {
            return Err(ControlMtlsError::State(
                "controller requires at least one assignment".into(),
            ));
        }
        Ok(Self {
            plan,
            assignments: by_worker,
            credentials: BTreeMap::new(),
            consumed_credentials: BTreeSet::new(),
            sessions: BTreeMap::new(),
            heartbeats: BTreeMap::new(),
            abort: None,
            metrics: DistributedMetricsAggregator::default(),
            next_session_id: 0,
        })
    }

    pub fn add_credential(
        &mut self,
        credential: WorkerControlCredential,
        now_unix_ms: u64,
    ) -> Result<(), ControlMtlsError> {
        self.validate_credential_insert(&credential, now_unix_ms, false)?;
        self.credentials
            .insert(credential.credential_id.clone(), credential);
        Ok(())
    }

    /// Rotating an identity revokes all prior credentials for this exact
    /// run/worker tuple, invalidating their existing control sessions.
    pub fn rotate_credential(
        &mut self,
        replacement: WorkerControlCredential,
        now_unix_ms: u64,
    ) -> Result<(), ControlMtlsError> {
        self.validate_credential_insert(&replacement, now_unix_ms, true)?;
        let run_id = replacement.run_id.clone();
        let worker_id = replacement.worker_id.clone();
        self.credentials.retain(|_, credential| {
            credential.run_id != run_id || credential.worker_id != worker_id
        });
        self.consumed_credentials
            .retain(|credential_id| self.credentials.contains_key(credential_id));
        self.sessions
            .retain(|_, session| session.run_id != run_id || session.worker_id != worker_id);
        self.add_credential(replacement, now_unix_ms)
    }

    fn validate_credential_insert(
        &self,
        credential: &WorkerControlCredential,
        now_unix_ms: u64,
        replacing_same_worker: bool,
    ) -> Result<(), ControlMtlsError> {
        credential
            .validate(now_unix_ms)
            .map_err(ControlMtlsError::State)?;
        if credential.run_id != self.plan.run_id
            || !self.assignments.contains_key(&credential.worker_id)
        {
            return Err(ControlMtlsError::State(
                "credential identity does not match a controller assignment".into(),
            ));
        }
        if self.credentials.contains_key(&credential.credential_id) {
            return Err(ControlMtlsError::State(
                "credential identifier must be unique".into(),
            ));
        }
        if !replacing_same_worker
            && self.credentials.values().any(|existing| {
                existing.run_id == credential.run_id && existing.worker_id == credential.worker_id
            })
        {
            return Err(ControlMtlsError::State(
                "worker already has an active control credential; use explicit rotation".into(),
            ));
        }
        if self.credentials.values().any(|existing| {
            existing.run_id == credential.run_id
                && existing.certificate_fingerprint == credential.certificate_fingerprint
                && existing.worker_id != credential.worker_id
        }) {
            return Err(ControlMtlsError::State(
                "certificate fingerprint is already bound to another worker in this run".into(),
            ));
        }
        Ok(())
    }

    pub fn issue_abort(&mut self, signal: AbortSignal) -> Result<(), ControlMtlsError> {
        validate_schema_version(signal.schema_version).map_err(distributed_state_error)?;
        if signal.run_id != self.plan.run_id || signal.reason.trim().is_empty() {
            return Err(ControlMtlsError::State(
                "abort signal identity is invalid".into(),
            ));
        }
        self.abort = Some(signal);
        Ok(())
    }

    pub fn metric_snapshot(&self) -> crate::metrics::MetricsSnapshot {
        self.metrics.snapshot()
    }
}

fn distributed_state_error(error: DistributedError) -> ControlMtlsError {
    ControlMtlsError::State(error.to_string())
}

#[derive(Clone)]
pub struct ControlMtlsService {
    state: Arc<Mutex<ControlRunState>>,
    now_unix_ms: ControlNow,
}

impl fmt::Debug for ControlMtlsService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("ControlMtlsService").finish()
    }
}

impl ControlMtlsService {
    pub fn new(state: Arc<Mutex<ControlRunState>>) -> Self {
        Self::with_clock(state, system_unix_ms)
    }

    pub fn with_clock(
        state: Arc<Mutex<ControlRunState>>,
        now_unix_ms: impl Fn() -> u64 + Send + Sync + 'static,
    ) -> Self {
        Self {
            state,
            now_unix_ms: Arc::new(now_unix_ms),
        }
    }

    fn now(&self) -> u64 {
        (self.now_unix_ms)()
    }

    fn peer_fingerprint<T>(&self, request: &Request<T>) -> Result<String, Status> {
        let certificate = request
            .peer_certs()
            .and_then(|certificates| certificates.first().cloned())
            .ok_or_else(|| Status::unauthenticated("mTLS client certificate is required"))?;
        Ok(certificate_fingerprint_from_der(certificate.as_ref()))
    }

    fn register_worker(
        &self,
        request: crate::control_pb::RegisterWorkerRequest,
        peer_fingerprint: &str,
    ) -> Result<crate::control_pb::RegisterWorkerResponse, Status> {
        if request.protocol_version != CONTROL_PROTOCOL_VERSION {
            return Err(Status::failed_precondition(
                "control protocol version is unsupported",
            ));
        }
        if request.registration_nonce.is_empty()
            || request.registration_nonce.len() > MAX_REGISTRATION_NONCE_BYTES
        {
            return Err(Status::invalid_argument("registration nonce is invalid"));
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| Status::internal("controller state lock is poisoned"))?;
        let now = self.now();
        let credential = state
            .credentials
            .get(&request.credential_id)
            .cloned()
            .ok_or_else(|| Status::unauthenticated("worker credential is unknown"))?;
        credential
            .validate(now)
            .map_err(|_| Status::unauthenticated("worker credential is expired or invalid"))?;
        if credential.run_id != request.run_id
            || credential.worker_id != request.worker_id
            || credential.certificate_fingerprint != peer_fingerprint
            || credential.registration_nonce != request.registration_nonce
        {
            return Err(Status::permission_denied(
                "mTLS identity does not match the registered worker credential",
            ));
        }
        if !state
            .consumed_credentials
            .insert(credential.credential_id.clone())
        {
            return Err(Status::already_exists(
                "worker registration credential was already consumed",
            ));
        }
        let assignment = state
            .assignments
            .get(&credential.worker_id)
            .cloned()
            .ok_or_else(|| Status::permission_denied("worker assignment is unavailable"))?;
        state.next_session_id = state.next_session_id.saturating_add(1);
        let session_id = session_id_for(&credential, state.next_session_id);
        state.sessions.insert(
            session_id.clone(),
            ControlSession {
                run_id: credential.run_id.clone(),
                worker_id: credential.worker_id.clone(),
                credential_id: credential.credential_id,
                certificate_fingerprint: peer_fingerprint.to_string(),
                next_sequence: 0,
            },
        );
        Ok(crate::control_pb::RegisterWorkerResponse {
            session_id,
            credential_expires_unix_ms: credential.expires_unix_ms,
            assignment_json: serialize_contract(&assignment)?,
        })
    }

    fn authorize(
        &self,
        request: &crate::control_pb::SessionRequest,
        peer_fingerprint: &str,
    ) -> Result<(String, String), Status> {
        if request.protocol_version != CONTROL_PROTOCOL_VERSION {
            return Err(Status::failed_precondition(
                "control protocol version is unsupported",
            ));
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| Status::internal("controller state lock is poisoned"))?;
        let session = state
            .sessions
            .get(&request.session_id)
            .cloned()
            .ok_or_else(|| Status::unauthenticated("control session is unknown"))?;
        if session.run_id != request.run_id
            || session.worker_id != request.worker_id
            || session.certificate_fingerprint != peer_fingerprint
        {
            return Err(Status::permission_denied(
                "control session is not authorized for this worker identity",
            ));
        }
        let credential = state
            .credentials
            .get(&session.credential_id)
            .ok_or_else(|| Status::unauthenticated("control credential was rotated or revoked"))?;
        credential
            .validate(self.now())
            .map_err(|_| Status::unauthenticated("control credential expired"))?;
        if credential.certificate_fingerprint != peer_fingerprint {
            return Err(Status::permission_denied(
                "mTLS certificate fingerprint changed",
            ));
        }
        let live_session = state
            .sessions
            .get_mut(&request.session_id)
            .expect("session was cloned from this map");
        if request.sequence != live_session.next_sequence {
            return Err(Status::already_exists(
                "control message sequence is replayed or out of order",
            ));
        }
        live_session.next_sequence = live_session.next_sequence.saturating_add(1);
        Ok((request.run_id.clone(), request.worker_id.clone()))
    }

    fn assignment_for(&self, run_id: &str, worker_id: &str) -> Result<WorkerAssignment, Status> {
        let state = self
            .state
            .lock()
            .map_err(|_| Status::internal("controller state lock is poisoned"))?;
        if run_id != state.plan.run_id {
            return Err(Status::permission_denied("run identity is not authorized"));
        }
        state
            .assignments
            .get(worker_id)
            .cloned()
            .ok_or_else(|| Status::permission_denied("worker assignment is not authorized"))
    }

    fn ingest_metric_batch(
        &self,
        run_id: &str,
        worker_id: &str,
        batch_json: &[u8],
    ) -> Result<(), Status> {
        let batch: MetricBatch = deserialize_contract(batch_json)?;
        if batch.run_id != run_id || batch.worker_id != worker_id {
            return Err(Status::permission_denied(
                "worker may submit only its own metric batch",
            ));
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| Status::internal("controller state lock is poisoned"))?;
        let plan = state.plan.clone();
        match state
            .metrics
            .ingest(&batch, &plan)
            .map_err(distributed_status)?
        {
            BatchDisposition::Accepted => Ok(()),
            BatchDisposition::Duplicate
            | BatchDisposition::OutOfOrder
            | BatchDisposition::MissingGap => Err(Status::already_exists(
                "metric batch is replayed, out of order, or has a sequence gap",
            )),
        }
    }

    fn ingest_heartbeat(
        &self,
        run_id: &str,
        worker_id: &str,
        heartbeat_json: &[u8],
    ) -> Result<(), Status> {
        let heartbeat: WorkerHeartbeat = deserialize_contract(heartbeat_json)?;
        validate_schema_version(heartbeat.schema_version).map_err(distributed_status)?;
        if heartbeat.run_id != run_id || heartbeat.worker_id != worker_id {
            return Err(Status::permission_denied(
                "worker may submit only its own heartbeat",
            ));
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| Status::internal("controller state lock is poisoned"))?;
        if heartbeat.run_id != state.plan.run_id {
            return Err(Status::permission_denied("run identity is not authorized"));
        }
        if state
            .heartbeats
            .get(worker_id)
            .is_some_and(|previous| heartbeat.sequence <= previous.sequence)
        {
            return Err(Status::already_exists("heartbeat sequence is replayed"));
        }
        state.heartbeats.insert(worker_id.to_string(), heartbeat);
        Ok(())
    }

    fn abort_for(&self, run_id: &str) -> Result<Option<AbortSignal>, Status> {
        let state = self
            .state
            .lock()
            .map_err(|_| Status::internal("controller state lock is poisoned"))?;
        if run_id != state.plan.run_id {
            return Err(Status::permission_denied("run identity is not authorized"));
        }
        Ok(state.abort.clone())
    }
}

#[tonic::async_trait]
impl crate::control_pb::loadtest_control_server::LoadtestControl for ControlMtlsService {
    async fn register_worker(
        &self,
        request: Request<crate::control_pb::RegisterWorkerRequest>,
    ) -> Result<Response<crate::control_pb::RegisterWorkerResponse>, Status> {
        let peer_fingerprint = self.peer_fingerprint(&request)?;
        let response = self.register_worker(request.into_inner(), &peer_fingerprint)?;
        Ok(Response::new(response))
    }

    async fn get_assignment(
        &self,
        request: Request<crate::control_pb::SessionRequest>,
    ) -> Result<Response<crate::control_pb::AssignmentResponse>, Status> {
        let peer_fingerprint = self.peer_fingerprint(&request)?;
        let request = request.into_inner();
        let (run_id, worker_id) = self.authorize(&request, &peer_fingerprint)?;
        let assignment = self.assignment_for(&run_id, &worker_id)?;
        Ok(Response::new(crate::control_pb::AssignmentResponse {
            assignment_json: serialize_contract(&assignment)?,
        }))
    }

    async fn submit_metric_batch(
        &self,
        request: Request<crate::control_pb::MetricBatchRequest>,
    ) -> Result<Response<crate::control_pb::OperationAck>, Status> {
        let peer_fingerprint = self.peer_fingerprint(&request)?;
        let request = request.into_inner();
        let session = request
            .session
            .ok_or_else(|| Status::invalid_argument("control session is required"))?;
        let (run_id, worker_id) = self.authorize(&session, &peer_fingerprint)?;
        self.ingest_metric_batch(&run_id, &worker_id, &request.metric_batch_json)?;
        Ok(Response::new(crate::control_pb::OperationAck {
            accepted: true,
        }))
    }

    async fn submit_heartbeat(
        &self,
        request: Request<crate::control_pb::HeartbeatRequest>,
    ) -> Result<Response<crate::control_pb::OperationAck>, Status> {
        let peer_fingerprint = self.peer_fingerprint(&request)?;
        let request = request.into_inner();
        let session = request
            .session
            .ok_or_else(|| Status::invalid_argument("control session is required"))?;
        let (run_id, worker_id) = self.authorize(&session, &peer_fingerprint)?;
        self.ingest_heartbeat(&run_id, &worker_id, &request.heartbeat_json)?;
        Ok(Response::new(crate::control_pb::OperationAck {
            accepted: true,
        }))
    }

    async fn get_abort(
        &self,
        request: Request<crate::control_pb::SessionRequest>,
    ) -> Result<Response<crate::control_pb::AbortResponse>, Status> {
        let peer_fingerprint = self.peer_fingerprint(&request)?;
        let request = request.into_inner();
        let (run_id, _) = self.authorize(&request, &peer_fingerprint)?;
        let abort = self.abort_for(&run_id)?;
        Ok(Response::new(crate::control_pb::AbortResponse {
            present: abort.is_some(),
            abort_signal_json: abort
                .as_ref()
                .map(serialize_contract)
                .transpose()?
                .unwrap_or_default(),
        }))
    }
}

/// Serves only the private load-test controller service over mandatory mTLS.
/// Callers must supply a listener dedicated to the control plane; no target
/// HTTP/KCP/WSS/gRPC listener or player credential can be passed here.
pub async fn serve_control_listener(
    listener: TcpListener,
    service: ControlMtlsService,
    tls: &ControlServerTlsMaterial,
    shutdown: oneshot::Receiver<()>,
) -> Result<(), ControlMtlsError> {
    let tls_config = tls.tls_config()?;
    Server::builder()
        .tls_config(tls_config)?
        .add_service(
            crate::control_pb::loadtest_control_server::LoadtestControlServer::new(service),
        )
        .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
            let _ = shutdown.await;
        })
        .await?;
    Ok(())
}

pub struct ControlWorkerClient {
    client: crate::control_pb::loadtest_control_client::LoadtestControlClient<Channel>,
    credential: WorkerControlCredential,
    session_id: Option<String>,
    next_sequence: u64,
    state: WorkerControlState,
}

impl fmt::Debug for ControlWorkerClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ControlWorkerClient")
            .field("worker_id", &self.credential.worker_id)
            .field("run_id", &self.credential.run_id)
            .field("credential_id", &self.credential.credential_id)
            .field(
                "session_id",
                &self.session_id.as_ref().map(|_| "<redacted>"),
            )
            .field("state", &self.state)
            .finish()
    }
}

impl ControlWorkerClient {
    pub async fn connect(
        endpoint: ControlEndpoint,
        tls: &ControlWorkerTlsMaterial,
        credential: WorkerControlCredential,
    ) -> Result<Self, ControlMtlsError> {
        credential
            .validate(system_unix_ms())
            .map_err(ControlMtlsError::State)?;
        let tls_config = tls.tls_config()?;
        let channel = Endpoint::from_shared(endpoint.uri)
            .map_err(|_| ControlMtlsError::InvalidEndpoint)?
            .tls_config(tls_config)?
            .connect()
            .await?;
        Ok(Self {
            client: crate::control_pb::loadtest_control_client::LoadtestControlClient::new(channel),
            credential,
            session_id: None,
            next_sequence: 0,
            state: WorkerControlState::Running,
        })
    }

    pub fn state(&self) -> WorkerControlState {
        self.state
    }

    pub async fn register(&mut self) -> Result<WorkerAssignment, ControlMtlsError> {
        let response = self
            .client
            .register_worker(crate::control_pb::RegisterWorkerRequest {
                protocol_version: CONTROL_PROTOCOL_VERSION,
                run_id: self.credential.run_id.clone(),
                worker_id: self.credential.worker_id.clone(),
                credential_id: self.credential.credential_id.clone(),
                registration_nonce: self.credential.registration_nonce.clone(),
            })
            .await
            .map_err(|error| self.fail_closed(error))?
            .into_inner();
        let assignment: WorkerAssignment = deserialize_control_contract(&response.assignment_json)
            .map_err(|error| self.fail_closed_error(error))?;
        if assignment.run_id != self.credential.run_id
            || assignment.worker_id != self.credential.worker_id
            || response.credential_expires_unix_ms != self.credential.expires_unix_ms
        {
            self.state = WorkerControlState::DisconnectedFailClosed;
            return Err(ControlMtlsError::State(
                "controller registration response identity is invalid".into(),
            ));
        }
        self.session_id = Some(response.session_id);
        self.next_sequence = 0;
        Ok(assignment)
    }

    pub async fn get_assignment(&mut self) -> Result<WorkerAssignment, ControlMtlsError> {
        let session = self.next_session_request()?;
        let response = self
            .client
            .get_assignment(session)
            .await
            .map_err(|error| self.fail_closed(error))?
            .into_inner();
        let assignment: WorkerAssignment = deserialize_control_contract(&response.assignment_json)
            .map_err(|error| self.fail_closed_error(error))?;
        if assignment.run_id != self.credential.run_id
            || assignment.worker_id != self.credential.worker_id
        {
            self.state = WorkerControlState::DisconnectedFailClosed;
            return Err(ControlMtlsError::State(
                "controller assignment crossed a worker or run boundary".into(),
            ));
        }
        Ok(assignment)
    }

    pub async fn submit_metric_batch(
        &mut self,
        batch: MetricBatch,
    ) -> Result<(), ControlMtlsError> {
        self.require_own_identity(&batch.run_id, &batch.worker_id)?;
        let session = self.next_session_request()?;
        let request = crate::control_pb::MetricBatchRequest {
            session: Some(session),
            metric_batch_json: serialize_control_contract(&batch)?,
        };
        let response = self
            .client
            .submit_metric_batch(request)
            .await
            .map_err(|error| self.fail_closed(error))?
            .into_inner();
        if !response.accepted {
            self.state = WorkerControlState::DisconnectedFailClosed;
            return Err(ControlMtlsError::State(
                "metric batch was not accepted".into(),
            ));
        }
        Ok(())
    }

    pub async fn submit_heartbeat(
        &mut self,
        heartbeat: WorkerHeartbeat,
    ) -> Result<(), ControlMtlsError> {
        self.require_own_identity(&heartbeat.run_id, &heartbeat.worker_id)?;
        let session = self.next_session_request()?;
        let request = crate::control_pb::HeartbeatRequest {
            session: Some(session),
            heartbeat_json: serialize_control_contract(&heartbeat)?,
        };
        let response = self
            .client
            .submit_heartbeat(request)
            .await
            .map_err(|error| self.fail_closed(error))?
            .into_inner();
        if !response.accepted {
            self.state = WorkerControlState::DisconnectedFailClosed;
            return Err(ControlMtlsError::State("heartbeat was not accepted".into()));
        }
        Ok(())
    }

    pub async fn get_abort(&mut self) -> Result<Option<AbortSignal>, ControlMtlsError> {
        let request = self.next_session_request()?;
        let response = self
            .client
            .get_abort(request)
            .await
            .map_err(|error| self.fail_closed(error))?
            .into_inner();
        if !response.present {
            if !response.abort_signal_json.is_empty() {
                self.state = WorkerControlState::DisconnectedFailClosed;
                return Err(ControlMtlsError::State(
                    "empty abort response contained a payload".into(),
                ));
            }
            return Ok(None);
        }
        let signal: AbortSignal = deserialize_control_contract(&response.abort_signal_json)
            .map_err(|error| self.fail_closed_error(error))?;
        if signal.run_id != self.credential.run_id || signal.schema_version != SCHEMA_VERSION {
            self.state = WorkerControlState::DisconnectedFailClosed;
            return Err(ControlMtlsError::State(
                "abort signal crossed a worker run boundary".into(),
            ));
        }
        self.state = WorkerControlState::Aborting;
        Ok(Some(signal))
    }

    fn require_own_identity(
        &mut self,
        run_id: &str,
        worker_id: &str,
    ) -> Result<(), ControlMtlsError> {
        if run_id != self.credential.run_id || worker_id != self.credential.worker_id {
            self.state = WorkerControlState::DisconnectedFailClosed;
            return Err(ControlMtlsError::State(
                "worker cannot submit another worker or run identity".into(),
            ));
        }
        Ok(())
    }

    fn next_session_request(
        &mut self,
    ) -> Result<crate::control_pb::SessionRequest, ControlMtlsError> {
        if self.state != WorkerControlState::Running && self.state != WorkerControlState::Aborting {
            return Err(ControlMtlsError::State(
                "worker control client is fail-closed or completed".into(),
            ));
        }
        let session_id = self.session_id.clone().ok_or_else(|| {
            ControlMtlsError::State("worker must register before control operations".into())
        })?;
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        Ok(crate::control_pb::SessionRequest {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            run_id: self.credential.run_id.clone(),
            worker_id: self.credential.worker_id.clone(),
            session_id,
            sequence,
        })
    }

    fn fail_closed(&mut self, error: Status) -> ControlMtlsError {
        self.state = worker_state_after_disconnect(self.state);
        ControlMtlsError::Rpc(error)
    }

    fn fail_closed_error(&mut self, error: ControlMtlsError) -> ControlMtlsError {
        self.state = worker_state_after_disconnect(self.state);
        error
    }
}

fn serialize_contract<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, Status> {
    serde_json::to_vec(value).map_err(|_| Status::internal("control contract cannot serialize"))
}

fn deserialize_contract<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, Status> {
    if bytes.is_empty() || bytes.len() > MAX_CONTROL_JSON_BYTES {
        return Err(Status::invalid_argument(
            "control contract payload is invalid",
        ));
    }
    serde_json::from_slice(bytes)
        .map_err(|_| Status::invalid_argument("control contract JSON is invalid"))
}

fn serialize_control_contract<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, ControlMtlsError> {
    serde_json::to_vec(value).map_err(ControlMtlsError::ContractJson)
}

fn deserialize_control_contract<T: serde::de::DeserializeOwned>(
    bytes: &[u8],
) -> Result<T, ControlMtlsError> {
    if bytes.is_empty() || bytes.len() > MAX_CONTROL_JSON_BYTES {
        return Err(ControlMtlsError::State(
            "control contract payload is invalid".into(),
        ));
    }
    serde_json::from_slice(bytes).map_err(ControlMtlsError::ContractJson)
}

fn distributed_status(error: DistributedError) -> Status {
    match error {
        DistributedError::UnsupportedSchema(_) => {
            Status::failed_precondition("contract schema version is unsupported")
        }
        DistributedError::InvalidPlan(_)
        | DistributedError::InvalidAssignment(_)
        | DistributedError::InvalidBatch(_) => {
            Status::invalid_argument("control contract is invalid")
        }
        DistributedError::PendingBatchLimit => {
            Status::resource_exhausted("worker metric queue reached its bound")
        }
        DistributedError::CredentialRejected(_) => Status::permission_denied("credential rejected"),
        DistributedError::ControllerDisconnected => Status::unavailable("controller disconnected"),
    }
}

fn session_id_for(credential: &WorkerControlCredential, counter: u64) -> String {
    let mut hasher = Sha256::new();
    for value in [
        credential.credential_id.as_bytes(),
        credential.run_id.as_bytes(),
        credential.worker_id.as_bytes(),
        &counter.to_be_bytes(),
    ] {
        hasher.update(value);
        hasher.update([0]);
    }
    format!("session-{:x}", hasher.finalize())
}

fn system_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HardBudget;
    use crate::metrics::Metrics;
    use rcgen::{
        BasicConstraints, Certificate, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa,
        KeyPair, KeyUsagePurpose,
    };
    use tonic::Code;

    fn test_now_unix_ms() -> u64 {
        system_unix_ms()
    }

    struct TestPki {
        ca_certificate_pem: String,
        server_certificate_pem: String,
        server_private_key_pem: String,
        worker_a_certificate_pem: String,
        worker_a_private_key_pem: String,
        worker_b_certificate_pem: String,
        worker_b_private_key_pem: String,
    }

    impl TestPki {
        fn server_tls(&self) -> ControlServerTlsMaterial {
            ControlServerTlsMaterial {
                server_certificate_pem: self.server_certificate_pem.as_bytes().to_vec(),
                server_private_key_pem: self.server_private_key_pem.as_bytes().to_vec(),
                worker_ca_certificate_pem: self.ca_certificate_pem.as_bytes().to_vec(),
            }
        }

        fn worker_tls(&self, worker_id: &str) -> ControlWorkerTlsMaterial {
            let (certificate, private_key) = match worker_id {
                "worker-a" => (
                    &self.worker_a_certificate_pem,
                    &self.worker_a_private_key_pem,
                ),
                "worker-b" => (
                    &self.worker_b_certificate_pem,
                    &self.worker_b_private_key_pem,
                ),
                _ => panic!("unknown test worker"),
            };
            ControlWorkerTlsMaterial {
                controller_ca_certificate_pem: self.ca_certificate_pem.as_bytes().to_vec(),
                worker_certificate_pem: certificate.as_bytes().to_vec(),
                worker_private_key_pem: private_key.as_bytes().to_vec(),
            }
        }

        fn worker_fingerprint(&self, worker_id: &str) -> String {
            let certificate = match worker_id {
                "worker-a" => &self.worker_a_certificate_pem,
                "worker-b" => &self.worker_b_certificate_pem,
                _ => panic!("unknown test worker"),
            };
            certificate_fingerprint_from_pem(certificate.as_bytes()).unwrap()
        }
    }

    fn test_pki() -> TestPki {
        let mut ca_params = CertificateParams::default();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "load-test-control-ca");
        let ca_key = KeyPair::generate().unwrap();
        let ca_certificate = ca_params.self_signed(&ca_key).unwrap();

        let (server_certificate_pem, server_private_key_pem) = issue_test_certificate(
            &ca_certificate,
            &ca_key,
            vec!["localhost".to_string()],
            ExtendedKeyUsagePurpose::ServerAuth,
        );
        let (worker_a_certificate_pem, worker_a_private_key_pem) = issue_test_certificate(
            &ca_certificate,
            &ca_key,
            Vec::new(),
            ExtendedKeyUsagePurpose::ClientAuth,
        );
        let (worker_b_certificate_pem, worker_b_private_key_pem) = issue_test_certificate(
            &ca_certificate,
            &ca_key,
            Vec::new(),
            ExtendedKeyUsagePurpose::ClientAuth,
        );

        TestPki {
            ca_certificate_pem: ca_certificate.pem(),
            server_certificate_pem,
            server_private_key_pem,
            worker_a_certificate_pem,
            worker_a_private_key_pem,
            worker_b_certificate_pem,
            worker_b_private_key_pem,
        }
    }

    fn issue_test_certificate(
        issuer: &Certificate,
        issuer_key: &KeyPair,
        subject_alt_names: Vec<String>,
        usage: ExtendedKeyUsagePurpose,
    ) -> (String, String) {
        let mut params = CertificateParams::new(subject_alt_names).unwrap();
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyEncipherment,
        ];
        params.extended_key_usages = vec![usage];
        let key = KeyPair::generate().unwrap();
        let certificate = params.signed_by(&key, issuer, issuer_key).unwrap();
        (certificate.pem(), key.serialize_pem())
    }

    fn plan() -> RunPlan {
        RunPlan {
            schema_version: SCHEMA_VERSION,
            run_id: "control-run".into(),
            environment: "local".into(),
            scenario_name: "mtls-control".into(),
            budget: HardBudget {
                max_virtual_players: 2,
                max_login_qps: 1.0,
                max_new_connections_per_second: 1.0,
                max_business_messages_per_second: 2.0,
                max_messages_per_connection_per_second: 2.0,
                max_duration_secs: 60,
                max_total_operations: 100,
                max_error_rate: 0.1,
                max_connection_failure_rate: 0.1,
                max_p99_ms: 1_000,
                max_data_writes: 0,
            },
            planned_start_unix_ms: test_now_unix_ms(),
        }
    }

    fn assignment(worker_id: &str, start: u32) -> WorkerAssignment {
        WorkerAssignment {
            schema_version: SCHEMA_VERSION,
            run_id: "control-run".into(),
            worker_id: worker_id.into(),
            virtual_player_start: start,
            virtual_player_count: 1,
            lease_expires_unix_ms: 2_000_000_000_000,
        }
    }

    fn test_state() -> Arc<Mutex<ControlRunState>> {
        Arc::new(Mutex::new(
            ControlRunState::new(
                plan(),
                [assignment("worker-a", 0), assignment("worker-b", 1)],
            )
            .unwrap(),
        ))
    }

    fn test_credential(
        pki: &TestPki,
        worker_id: &str,
        credential_id: &str,
        issued_unix_ms: u64,
        expires_unix_ms: u64,
    ) -> WorkerControlCredential {
        WorkerControlCredential::new(
            credential_id,
            "control-run",
            worker_id,
            pki.worker_fingerprint(worker_id),
            issued_unix_ms,
            expires_unix_ms,
            format!("nonce-{credential_id}").into_bytes(),
        )
    }

    async fn start_server(
        state: Arc<Mutex<ControlRunState>>,
        pki: &TestPki,
    ) -> (
        ControlEndpoint,
        oneshot::Sender<()>,
        tokio::task::JoinHandle<Result<(), ControlMtlsError>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let tls = pki.server_tls();
        let service = ControlMtlsService::new(state);
        let task = tokio::spawn(async move {
            serve_control_listener(listener, service, &tls, shutdown_rx).await
        });
        (
            ControlEndpoint::parse(format!("https://localhost:{port}")).unwrap(),
            shutdown_tx,
            task,
        )
    }

    async fn stop_server(
        shutdown: oneshot::Sender<()>,
        task: tokio::task::JoinHandle<Result<(), ControlMtlsError>>,
    ) {
        let _ = shutdown.send(());
        task.await.unwrap().unwrap();
    }

    async fn connect_worker(
        endpoint: ControlEndpoint,
        pki: &TestPki,
        credential: WorkerControlCredential,
    ) -> ControlWorkerClient {
        let tls = pki.worker_tls(&credential.worker_id);
        ControlWorkerClient::connect(endpoint, &tls, credential)
            .await
            .unwrap()
    }

    fn metric_batch(worker_id: &str, sequence: u64) -> MetricBatch {
        let mut metrics = Metrics::default();
        metrics.increment("requests", 1);
        MetricBatch::new(
            "control-run",
            worker_id,
            sequence,
            0,
            100,
            metrics.snapshot(),
        )
    }

    fn heartbeat(worker_id: &str, sequence: u64) -> WorkerHeartbeat {
        WorkerHeartbeat {
            schema_version: SCHEMA_VERSION,
            run_id: "control-run".into(),
            worker_id: worker_id.into(),
            sequence,
            monotonic_ms: 100,
            wall_clock_unix_ms: test_now_unix_ms(),
            active_virtual_players: 1,
        }
    }

    #[test]
    fn endpoint_and_tls_material_stay_separate_from_player_transports() {
        assert!(matches!(
            ControlEndpoint::parse("http://localhost:9000"),
            Err(ControlMtlsError::InsecureEndpoint)
        ));
        assert!(matches!(
            ControlEndpoint::parse("kcp://localhost:4000"),
            Err(ControlMtlsError::InsecureEndpoint)
        ));
        assert!(matches!(
            ControlEndpoint::parse("https://localhost:9000/control"),
            Err(ControlMtlsError::InvalidEndpoint)
        ));
        assert_eq!(
            ControlEndpoint::parse("https://localhost:9000")
                .unwrap()
                .as_str(),
            "https://localhost:9000"
        );

        let server_debug = format!(
            "{:?}",
            ControlServerTlsMaterial {
                server_certificate_pem: b"server-cert".to_vec(),
                server_private_key_pem: b"server-key".to_vec(),
                worker_ca_certificate_pem: b"worker-ca".to_vec(),
            }
        );
        assert!(!server_debug.contains("server-key"));
        let worker_debug = format!(
            "{:?}",
            ControlWorkerTlsMaterial {
                controller_ca_certificate_pem: b"controller-ca".to_vec(),
                worker_certificate_pem: b"worker-cert".to_vec(),
                worker_private_key_pem: b"worker-key".to_vec(),
            }
        );
        assert!(!worker_debug.contains("worker-key"));
    }

    #[test]
    fn active_credentials_are_unique_per_worker_and_certificate() {
        let pki = test_pki();
        let state = test_state();
        let first = test_credential(
            &pki,
            "worker-a",
            "worker-a-v1",
            test_now_unix_ms() - 1,
            test_now_unix_ms() + 60_000,
        );
        state
            .lock()
            .unwrap()
            .add_credential(first.clone(), test_now_unix_ms())
            .unwrap();

        let duplicate_worker = test_credential(
            &pki,
            "worker-a",
            "worker-a-v2",
            test_now_unix_ms() - 1,
            test_now_unix_ms() + 60_000,
        );
        assert!(matches!(
            state
                .lock()
                .unwrap()
                .add_credential(duplicate_worker, test_now_unix_ms()),
            Err(ControlMtlsError::State(reason)) if reason.contains("explicit rotation")
        ));

        let mut shared_fingerprint = test_credential(
            &pki,
            "worker-b",
            "worker-b-shared-fingerprint",
            test_now_unix_ms() - 1,
            test_now_unix_ms() + 60_000,
        );
        shared_fingerprint.certificate_fingerprint = pki.worker_fingerprint("worker-a");
        assert!(matches!(
            state
                .lock()
                .unwrap()
                .add_credential(shared_fingerprint, test_now_unix_ms()),
            Err(ControlMtlsError::State(reason)) if reason.contains("another worker")
        ));

        let worker_b = test_credential(
            &pki,
            "worker-b",
            "worker-b-v1",
            test_now_unix_ms() - 1,
            test_now_unix_ms() + 60_000,
        );
        state
            .lock()
            .unwrap()
            .add_credential(worker_b, test_now_unix_ms())
            .unwrap();
        let mut conflicting_rotation = test_credential(
            &pki,
            "worker-a",
            "worker-a-v3",
            test_now_unix_ms() - 1,
            test_now_unix_ms() + 60_000,
        );
        conflicting_rotation.certificate_fingerprint = pki.worker_fingerprint("worker-b");
        assert!(matches!(
            state
                .lock()
                .unwrap()
                .rotate_credential(conflicting_rotation, test_now_unix_ms()),
            Err(ControlMtlsError::State(reason)) if reason.contains("another worker")
        ));
        let state = state.lock().unwrap();
        assert!(state.credentials.contains_key(&first.credential_id));
        assert!(state.credentials.contains_key("worker-b-v1"));
        assert!(!state.credentials.contains_key("worker-a-v3"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn mtls_worker_can_access_only_its_control_flow_and_receives_abort() {
        let pki = test_pki();
        let state = test_state();
        let credential = test_credential(
            &pki,
            "worker-a",
            "worker-a-v1",
            test_now_unix_ms() - 1,
            test_now_unix_ms() + 60_000,
        );
        state
            .lock()
            .unwrap()
            .add_credential(credential.clone(), test_now_unix_ms())
            .unwrap();
        let (endpoint, shutdown, task) = start_server(state.clone(), &pki).await;
        let mut worker = connect_worker(endpoint, &pki, credential).await;

        assert_eq!(worker.register().await.unwrap(), assignment("worker-a", 0));
        assert_eq!(
            worker.get_assignment().await.unwrap(),
            assignment("worker-a", 0)
        );
        worker
            .submit_metric_batch(metric_batch("worker-a", 0))
            .await
            .unwrap();
        worker
            .submit_heartbeat(heartbeat("worker-a", 0))
            .await
            .unwrap();
        state
            .lock()
            .unwrap()
            .issue_abort(AbortSignal {
                schema_version: SCHEMA_VERSION,
                run_id: "control-run".into(),
                reason: "budget threshold".into(),
                issued_unix_ms: test_now_unix_ms(),
                graceful_shutdown_ms: 500,
            })
            .unwrap();
        assert!(worker.get_abort().await.unwrap().is_some());
        assert_eq!(worker.state(), WorkerControlState::Aborting);
        assert_eq!(
            state.lock().unwrap().metric_snapshot().counters["requests"],
            1
        );
        stop_server(shutdown, task).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn certificate_mismatch_and_expired_credentials_fail_closed() {
        let pki = test_pki();
        let state = test_state();
        let mismatched = test_credential(
            &pki,
            "worker-b",
            "worker-b-v1",
            test_now_unix_ms() - 1,
            test_now_unix_ms() + 60_000,
        );
        state
            .lock()
            .unwrap()
            .add_credential(mismatched.clone(), test_now_unix_ms())
            .unwrap();
        let (endpoint, shutdown, task) = start_server(state.clone(), &pki).await;
        let worker_a_tls = pki.worker_tls("worker-a");
        let mut mismatched_client =
            ControlWorkerClient::connect(endpoint, &worker_a_tls, mismatched)
                .await
                .unwrap();
        let error = mismatched_client.register().await.unwrap_err();
        assert!(matches!(
            error,
            ControlMtlsError::Rpc(status) if status.code() == Code::PermissionDenied
        ));
        assert_eq!(
            mismatched_client.state(),
            WorkerControlState::DisconnectedFailClosed
        );
        stop_server(shutdown, task).await;

        let expired = test_credential(
            &pki,
            "worker-a",
            "worker-a-expired",
            test_now_unix_ms() - 100,
            test_now_unix_ms() - 1,
        );
        let tls = pki.worker_tls("worker-a");
        let error = ControlWorkerClient::connect(
            ControlEndpoint::parse("https://localhost:1").unwrap(),
            &tls,
            expired,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, ControlMtlsError::State(_)));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn credential_rotation_and_one_time_registration_revoke_old_access() {
        let pki = test_pki();
        let state = test_state();
        let first = test_credential(
            &pki,
            "worker-a",
            "worker-a-v1",
            test_now_unix_ms() - 1,
            test_now_unix_ms() + 60_000,
        );
        state
            .lock()
            .unwrap()
            .add_credential(first.clone(), test_now_unix_ms())
            .unwrap();
        let (endpoint, shutdown, task) = start_server(state.clone(), &pki).await;
        let mut first_client = connect_worker(endpoint.clone(), &pki, first.clone()).await;
        first_client.register().await.unwrap();

        let mut reused_client = connect_worker(endpoint.clone(), &pki, first).await;
        let error = reused_client.register().await.unwrap_err();
        assert!(matches!(
            error,
            ControlMtlsError::Rpc(status) if status.code() == Code::AlreadyExists
        ));

        let mut replacement = test_credential(
            &pki,
            "worker-a",
            "worker-a-v2",
            test_now_unix_ms() - 1,
            test_now_unix_ms() + 60_000,
        );
        replacement.certificate_fingerprint = pki.worker_fingerprint("worker-b");
        state
            .lock()
            .unwrap()
            .rotate_credential(replacement, test_now_unix_ms())
            .unwrap();
        let error = first_client.get_assignment().await.unwrap_err();
        assert!(matches!(
            error,
            ControlMtlsError::Rpc(status) if status.code() == Code::Unauthenticated
        ));
        assert_eq!(
            first_client.state(),
            WorkerControlState::DisconnectedFailClosed
        );
        let worker_b_tls = pki.worker_tls("worker-b");
        let mut rotated_client = ControlWorkerClient::connect(
            endpoint,
            &worker_b_tls,
            state
                .lock()
                .unwrap()
                .credentials
                .get("worker-a-v2")
                .cloned()
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            rotated_client.register().await.unwrap(),
            assignment("worker-a", 0)
        );
        stop_server(shutdown, task).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cross_worker_and_protocol_replay_requests_are_rejected() {
        let pki = test_pki();
        let state = test_state();
        let credential = test_credential(
            &pki,
            "worker-a",
            "worker-a-v1",
            test_now_unix_ms() - 1,
            test_now_unix_ms() + 60_000,
        );
        state
            .lock()
            .unwrap()
            .add_credential(credential.clone(), test_now_unix_ms())
            .unwrap();
        let (endpoint, shutdown, task) = start_server(state, &pki).await;
        let mut worker = connect_worker(endpoint, &pki, credential).await;
        worker.register().await.unwrap();
        let session_id = worker.session_id.clone().unwrap();

        let cross_worker = worker
            .client
            .get_assignment(crate::control_pb::SessionRequest {
                protocol_version: CONTROL_PROTOCOL_VERSION,
                run_id: "control-run".into(),
                worker_id: "worker-b".into(),
                session_id: session_id.clone(),
                sequence: 0,
            })
            .await
            .unwrap_err();
        assert_eq!(cross_worker.code(), Code::PermissionDenied);

        let cross_run = worker
            .client
            .get_assignment(crate::control_pb::SessionRequest {
                protocol_version: CONTROL_PROTOCOL_VERSION,
                run_id: "other-run".into(),
                worker_id: "worker-a".into(),
                session_id: session_id.clone(),
                sequence: 0,
            })
            .await
            .unwrap_err();
        assert_eq!(cross_run.code(), Code::PermissionDenied);

        let bad_protocol = worker
            .client
            .get_assignment(crate::control_pb::SessionRequest {
                protocol_version: CONTROL_PROTOCOL_VERSION + 1,
                run_id: "control-run".into(),
                worker_id: "worker-a".into(),
                session_id: session_id.clone(),
                sequence: 0,
            })
            .await
            .unwrap_err();
        assert_eq!(bad_protocol.code(), Code::FailedPrecondition);

        worker.get_assignment().await.unwrap();
        let replay = worker
            .client
            .get_assignment(crate::control_pb::SessionRequest {
                protocol_version: CONTROL_PROTOCOL_VERSION,
                run_id: "control-run".into(),
                worker_id: "worker-a".into(),
                session_id,
                sequence: 0,
            })
            .await
            .unwrap_err();
        assert_eq!(replay.code(), Code::AlreadyExists);
        stop_server(shutdown, task).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn untrusted_client_certificate_cannot_open_a_control_session() {
        let pki = test_pki();
        let state = test_state();
        let untrusted = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let untrusted_certificate_pem = untrusted.cert.pem();
        let credential = WorkerControlCredential::new(
            "untrusted-worker-a",
            "control-run",
            "worker-a",
            certificate_fingerprint_from_pem(untrusted_certificate_pem.as_bytes()).unwrap(),
            test_now_unix_ms() - 1,
            test_now_unix_ms() + 60_000,
            b"untrusted-nonce".to_vec(),
        );
        state
            .lock()
            .unwrap()
            .add_credential(credential.clone(), test_now_unix_ms())
            .unwrap();
        let (endpoint, shutdown, task) = start_server(state.clone(), &pki).await;
        let untrusted_tls = ControlWorkerTlsMaterial {
            controller_ca_certificate_pem: pki.ca_certificate_pem.as_bytes().to_vec(),
            worker_certificate_pem: untrusted_certificate_pem.into_bytes(),
            worker_private_key_pem: untrusted.key_pair.serialize_pem().into_bytes(),
        };
        let mut worker = ControlWorkerClient::connect(endpoint, &untrusted_tls, credential)
            .await
            .unwrap();
        assert!(worker.register().await.is_err());
        assert_eq!(worker.state(), WorkerControlState::DisconnectedFailClosed);
        assert!(state.lock().unwrap().sessions.is_empty());
        stop_server(shutdown, task).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn duplicate_metric_batches_and_server_loss_fail_closed() {
        let pki = test_pki();
        let state = test_state();
        let credential = test_credential(
            &pki,
            "worker-a",
            "worker-a-v1",
            test_now_unix_ms() - 1,
            test_now_unix_ms() + 60_000,
        );
        state
            .lock()
            .unwrap()
            .add_credential(credential.clone(), test_now_unix_ms())
            .unwrap();
        let (endpoint, shutdown, task) = start_server(state, &pki).await;
        let mut worker = connect_worker(endpoint, &pki, credential).await;
        worker.register().await.unwrap();
        worker
            .submit_metric_batch(metric_batch("worker-a", 0))
            .await
            .unwrap();
        let duplicate = worker
            .submit_metric_batch(metric_batch("worker-a", 0))
            .await
            .unwrap_err();
        assert!(matches!(
            duplicate,
            ControlMtlsError::Rpc(status) if status.code() == Code::AlreadyExists
        ));
        assert_eq!(worker.state(), WorkerControlState::DisconnectedFailClosed);
        stop_server(shutdown, task).await;

        let second_pki = test_pki();
        let second_state = test_state();
        let second_credential = test_credential(
            &second_pki,
            "worker-a",
            "worker-a-v2",
            test_now_unix_ms() - 1,
            test_now_unix_ms() + 60_000,
        );
        second_state
            .lock()
            .unwrap()
            .add_credential(second_credential.clone(), test_now_unix_ms())
            .unwrap();
        let (endpoint, shutdown, task) = start_server(second_state, &second_pki).await;
        let mut worker = connect_worker(endpoint, &second_pki, second_credential).await;
        worker.register().await.unwrap();
        stop_server(shutdown, task).await;
        assert!(worker.get_assignment().await.is_err());
        assert_eq!(worker.state(), WorkerControlState::DisconnectedFailClosed);
    }
}
