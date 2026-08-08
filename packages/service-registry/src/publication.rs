use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::{
    ConvergenceAttempt, ConvergenceConfig, ConvergenceTask, HealthState, RegistryClient,
    ServiceInstance, StartupErrorCode, spawn_convergence,
};

const REGISTRY_DEPENDENCY: &str = "service-registry";
const REGISTRY_ENDPOINT: &str = "self-registration";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PublicationAction {
    Register(bool),
    Heartbeat,
    Poll,
}

#[derive(Debug, Default)]
struct PublicationState {
    registered: bool,
    published_healthy: Option<bool>,
    last_refresh: Option<Instant>,
}

impl PublicationState {
    fn next_action(
        &self,
        desired_healthy: bool,
        now: Instant,
        heartbeat_interval: Duration,
    ) -> PublicationAction {
        if !self.registered {
            PublicationAction::Register(false)
        } else if self.published_healthy != Some(desired_healthy) {
            PublicationAction::Register(desired_healthy)
        } else if self
            .last_refresh
            .is_none_or(|last_refresh| now.duration_since(last_refresh) >= heartbeat_interval)
        {
            PublicationAction::Heartbeat
        } else {
            PublicationAction::Poll
        }
    }

    fn record_success(&mut self, action: PublicationAction, now: Instant) {
        if action == PublicationAction::Poll {
            return;
        }
        self.registered = true;
        self.last_refresh = Some(now);
        if let PublicationAction::Register(healthy) = action {
            self.published_healthy = Some(healthy);
        }
    }

    fn record_failure(&mut self) {
        self.registered = false;
        self.published_healthy = None;
        self.last_refresh = None;
    }
}

/// Starts registry publication and heartbeat convergence.
///
/// The task always publishes an unhealthy instance first. Once the shared health state is
/// stable-ready it republishes the original instance as healthy. Any required dependency loss or
/// registry failure causes the next successful publication to be unhealthy again.
pub fn spawn_registry_publication(
    client: Arc<RegistryClient>,
    instance: ServiceInstance,
    health_state: HealthState,
    config: ConvergenceConfig,
) -> ConvergenceTask {
    let state = Arc::new(Mutex::new(PublicationState::default()));
    spawn_convergence(config, move || {
        let client = Arc::clone(&client);
        let instance = instance.clone();
        let health_state = health_state.clone();
        let state = Arc::clone(&state);
        async move {
            let desired_healthy = health_state.snapshot().ready;
            let now = Instant::now();
            let action =
                state
                    .lock()
                    .await
                    .next_action(desired_healthy, now, client.heartbeat_interval());
            let result = match action {
                PublicationAction::Register(healthy) => {
                    let published = instance_with_health(&instance, healthy);
                    client.register(&published).await
                }
                PublicationAction::Heartbeat => client.heartbeat().await,
                PublicationAction::Poll => Ok(()),
            };

            match result {
                Ok(()) => {
                    state.lock().await.record_success(action, now);
                    if action != PublicationAction::Poll {
                        health_state.mark_ready(REGISTRY_DEPENDENCY, REGISTRY_ENDPOINT);
                    }
                    ConvergenceAttempt::Converged
                }
                Err(_) => {
                    state.lock().await.record_failure();
                    health_state.mark_degraded(
                        REGISTRY_DEPENDENCY,
                        REGISTRY_ENDPOINT,
                        StartupErrorCode::RegistryUnavailable,
                    );
                    ConvergenceAttempt::Retry(StartupErrorCode::RegistryUnavailable)
                }
            }
        }
    })
}

fn instance_with_health(instance: &ServiceInstance, healthy: bool) -> ServiceInstance {
    let mut published = instance.clone();
    published.healthy = healthy && instance.healthy;
    for (endpoint, original) in published.endpoints.iter_mut().zip(&instance.endpoints) {
        endpoint.healthy = healthy && original.healthy;
    }
    published
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ServiceEndpoint;

    #[test]
    fn publication_always_registers_unhealthy_first() {
        let mut state = PublicationState::default();
        let now = Instant::now();
        let heartbeat_interval = Duration::from_secs(10);
        assert_eq!(
            state.next_action(true, now, heartbeat_interval),
            PublicationAction::Register(false)
        );
        state.record_success(PublicationAction::Register(false), now);
        assert_eq!(
            state.next_action(true, now, heartbeat_interval),
            PublicationAction::Register(true)
        );
        state.record_success(PublicationAction::Register(true), now);
        assert_eq!(
            state.next_action(true, now + Duration::from_secs(1), heartbeat_interval),
            PublicationAction::Poll
        );
        assert_eq!(
            state.next_action(true, now + heartbeat_interval, heartbeat_interval),
            PublicationAction::Heartbeat
        );
    }

    #[test]
    fn publication_failure_requires_unhealthy_reregistration() {
        let mut state = PublicationState {
            registered: true,
            published_healthy: Some(true),
            last_refresh: Some(Instant::now()),
        };
        state.record_failure();
        assert_eq!(
            state.next_action(false, Instant::now(), Duration::from_secs(10)),
            PublicationAction::Register(false)
        );
    }

    #[test]
    fn health_projection_preserves_explicitly_disabled_endpoints() {
        let mut instance = ServiceInstance::new(
            "game-1".to_string(),
            "game-server".to_string(),
            "127.0.0.1".to_string(),
            7000,
        )
        .with_endpoints(vec![
            ServiceEndpoint::tcp("client", "127.0.0.1", 7000, "public"),
            ServiceEndpoint::tcp("admin", "127.0.0.1", 7500, "admin"),
        ]);
        instance.endpoints[1].healthy = false;

        let pending = instance_with_health(&instance, false);
        assert!(!pending.healthy);
        assert!(pending.endpoints.iter().all(|endpoint| !endpoint.healthy));

        let ready = instance_with_health(&instance, true);
        assert!(ready.healthy);
        assert!(ready.endpoints[0].healthy);
        assert!(!ready.endpoints[1].healthy);
    }
}
