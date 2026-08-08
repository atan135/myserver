//! gRPC 服务器

use std::sync::Arc;
use std::time::Duration;

use global_id::{
    DEFAULT_WORKER_LEASE_RENEW_INTERVAL_SECONDS, DEFAULT_WORKER_LEASE_TTL_SECONDS, WorkerLease,
};
use service_registry::HealthState;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tonic::transport::{Server, server::TcpIncoming};
use tracing::{info, warn};

use crate::config::Config;
use crate::matcher::{new_simple_matcher, new_simple_matcher_with_runtime_store};
use crate::proto::myserver::matchservice::{
    match_internal_server::MatchInternalServer, match_service_server::MatchServiceServer,
};
use crate::runtime_store::{
    RedisMatchRuntimeStore, SharedMatchRuntimeStore, new_memory_match_runtime_store,
};
use crate::service::{MatchInternalImpl, MatchServiceImpl};

pub async fn run(
    config: Config,
    health_state: HealthState,
) -> Result<(), Box<dyn std::error::Error>> {
    let redis_client = redis::Client::open(config.redis_url.clone())?;
    let mut global_id_redis = redis_client.get_multiplexed_async_connection().await?;
    let global_id_origin_id = u16::try_from(config.global_id_origin_id).map_err(|_| {
        format!(
            "GLOBAL_ID_ORIGIN_ID out of range: {}",
            config.global_id_origin_id
        )
    })?;
    let global_id_worker_id = config
        .global_id_worker_id
        .map(|worker_id| {
            u8::try_from(worker_id).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("GLOBAL_ID_WORKER_ID out of range: {worker_id}"),
                )
            })
        })
        .transpose()?;
    let worker_lease = WorkerLease::acquire_redis(
        &mut global_id_redis,
        &config.redis_key_prefix,
        global_id_origin_id,
        global_id_worker_id,
        &config.service_name,
        &config.service_instance_id,
        DEFAULT_WORKER_LEASE_TTL_SECONDS,
    )
    .await
    .map_err(|error| std::io::Error::other(error.to_string()))?;
    info!(
        origin_id = worker_lease.origin_id,
        worker_id = worker_lease.worker_id,
        lease_key = %worker_lease.key,
        "global id worker lease acquired"
    );
    health_state.mark_ready("local-runtime", "worker-lease");

    let (lease_loss_tx, mut lease_loss_rx) = watch::channel(false);
    let lease_renew_task =
        spawn_worker_lease_renewal(redis_client.clone(), worker_lease.clone(), lease_loss_tx);

    let result = async {
        let room_id_generator = Arc::new(worker_lease.generator()?);
        let matcher = if uses_memory_runtime_store(&config) {
            new_simple_matcher(config.clone(), room_id_generator, health_state.clone())
        } else {
            let runtime_store = build_runtime_store(&config)?;
            new_simple_matcher_with_runtime_store(
                config.clone(),
                runtime_store,
                room_id_generator,
                health_state.clone(),
            )
        };
        matcher.recover_runtime_state().await?;
        let cleanup_matcher = matcher.clone();
        let cleanup_interval_secs = config.match_cleanup_interval_secs.max(1);

        let match_service = MatchServiceImpl::new(matcher.clone());
        let match_internal = MatchInternalImpl::new(matcher);

        info!(addr = %config.bind_addr, "match-service gRPC server starting");

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(cleanup_interval_secs));
            loop {
                interval.tick().await;
                if let Err(error) = cleanup_matcher.cleanup_timeout().await {
                    tracing::error!(error = %error, "match cleanup task failed");
                }
            }
        });

        let addr = config.bind_addr.parse()?;
        let reflection = tonic_reflection::server::Builder::configure().build()?;
        let mut shutdown_lease_loss_rx = lease_loss_rx.clone();
        let incoming = bind_grpc_incoming(addr, &health_state).await?;

        Server::builder()
            .add_service(reflection)
            .add_service(MatchServiceServer::new(match_service))
            .add_service(MatchInternalServer::new(match_internal))
            .serve_with_incoming_shutdown(incoming, async {
                if *shutdown_lease_loss_rx.borrow_and_update() {
                    warn!("global id worker lease lost, stopping match-service gRPC server");
                } else {
                    tokio::select! {
                        _ = shutdown_signal() => {
                            info!("shutdown signal received, stopping match-service gRPC server");
                        }
                        changed = shutdown_lease_loss_rx.changed() => {
                            if changed.is_err() || *shutdown_lease_loss_rx.borrow_and_update() {
                                warn!("global id worker lease lost, stopping match-service gRPC server");
                            }
                        }
                    }
                }
            })
            .await?;

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    lease_renew_task.abort();
    let _ = lease_renew_task.await;
    release_worker_lease(redis_client, &worker_lease).await;

    if *lease_loss_rx.borrow_and_update() {
        Err(std::io::Error::other("global id worker lease lost").into())
    } else {
        result
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("failed to install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn bind_grpc_incoming(
    addr: std::net::SocketAddr,
    health_state: &HealthState,
) -> Result<TcpIncoming, std::io::Error> {
    let listener = TcpListener::bind(addr).await?;
    let incoming = TcpIncoming::from_listener(listener, false, None)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    health_state.mark_ready("local-runtime", "grpc-listener");
    Ok(incoming)
}

fn spawn_worker_lease_renewal(
    redis_client: redis::Client,
    worker_lease: WorkerLease,
    lease_loss_tx: watch::Sender<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(
                DEFAULT_WORKER_LEASE_RENEW_INTERVAL_SECONDS,
            ))
            .await;
            let lease_is_active = match redis_client.get_multiplexed_async_connection().await {
                Ok(mut redis) => match worker_lease.renew_redis(&mut redis).await {
                    Ok(active) => active,
                    Err(error) => {
                        warn!(
                            lease_key = %worker_lease.key,
                            error = %error,
                            "global id worker lease renewal failed"
                        );
                        false
                    }
                },
                Err(error) => {
                    worker_lease.deactivate();
                    warn!(
                        lease_key = %worker_lease.key,
                        error = %error,
                        "global id worker lease renewal failed"
                    );
                    false
                }
            };
            if !lease_is_active {
                warn!(
                    lease_key = %worker_lease.key,
                    "global id worker lease lost; requesting process shutdown"
                );
                let _ = lease_loss_tx.send(true);
                break;
            }
        }
    })
}

async fn release_worker_lease(redis_client: redis::Client, worker_lease: &WorkerLease) {
    match redis_client.get_multiplexed_async_connection().await {
        Ok(mut redis) => {
            if let Err(error) = worker_lease.release_redis(&mut redis).await {
                warn!(
                    lease_key = %worker_lease.key,
                    error = %error,
                    "failed to release global id worker lease"
                );
            }
        }
        Err(error) => {
            warn!(
                lease_key = %worker_lease.key,
                error = %error,
                "failed to connect redis for global id worker lease release"
            );
        }
    }
}

fn build_runtime_store(
    config: &Config,
) -> Result<SharedMatchRuntimeStore, Box<dyn std::error::Error>> {
    match config.match_runtime_store.as_str() {
        "redis" => Ok(std::sync::Arc::new(RedisMatchRuntimeStore::new(
            &config.redis_url,
            config.match_runtime_key_prefix.clone(),
        )?)),
        "memory" | "" => Ok(new_memory_match_runtime_store()),
        other => Err(format!("unsupported MATCH_RUNTIME_STORE: {other}").into()),
    }
}

fn uses_memory_runtime_store(config: &Config) -> bool {
    matches!(config.match_runtime_store.as_str(), "memory" | "")
}

#[cfg(test)]
mod health_tests {
    use super::*;
    use service_registry::{DependencySpec, DependencyStatus, HealthConfig};

    fn listener_health() -> HealthState {
        HealthState::new(
            "match-service",
            "match-test",
            HealthConfig::for_tests(100, 0, 100),
            [DependencySpec::local_required("grpc-listener")],
        )
    }

    #[tokio::test]
    async fn bind_failure_does_not_mark_grpc_listener_ready() {
        let occupied = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = occupied.local_addr().unwrap();
        let health = listener_health();

        assert!(bind_grpc_incoming(addr, &health).await.is_err());
        assert_eq!(
            health.snapshot().dependencies[0].status,
            DependencyStatus::Pending
        );
    }

    #[tokio::test]
    async fn successful_bind_marks_grpc_listener_ready() {
        let health = listener_health();
        let incoming = bind_grpc_incoming("127.0.0.1:0".parse().unwrap(), &health)
            .await
            .unwrap();

        assert_eq!(
            health.snapshot().dependencies[0].status,
            DependencyStatus::Ready
        );
        drop(incoming);
    }
}
