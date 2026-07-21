mod admin_server;
mod auth;
mod blocklist;
mod config;
mod connection_limits;
mod local_socket;
mod maintenance;
mod metrics;
mod proto;
mod protocol;
mod protocol_version_policy {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../packages/proto/compatibility/version-policy.rs"
    ));
}
mod proxy_server;
mod rollout_drain_status;
mod route_store;
mod session;
mod transport;
mod upstream;

use std::fs;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use auth::ProxyAuthService;
use blocklist::RedisBlocklistChecker;
use config::Config;
use maintenance::GlobalMaintenanceChecker;
pub use proto::myserver::game as pb;
use rollout_drain_status::{HttpOldServerDrainStatusChecker, OldServerDrainStatusChecker};
use route_store::{
    ProxyRouteStore, RedisRouteStorePersistence, UpstreamHealthState, UpstreamOperationState,
    UpstreamRoute, run_redis_route_store_update_listener,
};
use service_registry::{RegistryClient, ServiceEndpoint, ServiceInstance};
use tokio::sync::RwLock;
use tracing_appender::rolling;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

fn init_logging(config: &Config) {
    let env_filter = EnvFilter::new(config.log_level.clone());
    let mut layers = Vec::new();

    if config.log_enable_console {
        layers.push(
            fmt::layer()
                .with_target(false)
                .with_ansi(true)
                .compact()
                .boxed(),
        );
    }

    if config.log_enable_file {
        fs::create_dir_all(&config.log_dir).expect("failed to create log dir");
        let file_appender = rolling::daily(&config.log_dir, "game-proxy.log");
        layers.push(
            fmt::layer()
                .with_ansi(false)
                .with_target(false)
                .with_writer(file_appender)
                .compact()
                .boxed(),
        );
    }

    if layers.is_empty() {
        layers.push(
            fmt::layer()
                .with_target(false)
                .with_ansi(true)
                .compact()
                .boxed(),
        );
    }

    tracing_subscriber::registry()
        .with(env_filter)
        .with(layers)
        .init();
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv();
    let config = Config::from_env();
    init_logging(&config);
    config
        .validate_upstream_discovery()
        .map_err(std::io::Error::other)?;

    // 启动 metrics 上报任务
    let metrics_nats_url = config.nats_url.clone();
    let metrics_instance_id = config.service_instance_id.clone();
    tokio::spawn(async move {
        metrics::METRICS
            .start_reporting(&metrics_nats_url, metrics_instance_id, 5)
            .await;
    });

    let mut route_store_update_task = None;
    let route_store = match &config.route_store_backend {
        config::RouteStoreBackend::Memory => {
            tracing::info!("using in-memory proxy route store");
            ProxyRouteStore::default()
        }
        config::RouteStoreBackend::Redis => {
            tracing::info!(
                redis_url = %config.route_store_redis_url,
                key_prefix = %config.route_store_key_prefix,
                "using redis proxy route store"
            );
            let persistence = Arc::new(RedisRouteStorePersistence::new(
                &config.route_store_redis_url,
                config.route_store_key_prefix.clone(),
            )?);
            let update_channel = persistence.update_channel().to_string();
            let update_pubsub = persistence.subscribe_updates().await.map_err(|error| {
                tracing::warn!(
                    redis_channel = %update_channel,
                    error = %error,
                    "failed to subscribe proxy route store update channel"
                );
                error
            })?;
            let route_store = ProxyRouteStore::with_persistence(persistence.clone());
            let listener_route_store = route_store.clone();
            let listener_persistence = persistence.clone();
            route_store_update_task = Some(tokio::spawn(async move {
                let mut next_pubsub = Some(update_pubsub);
                loop {
                    let pubsub = match next_pubsub.take() {
                        Some(pubsub) => pubsub,
                        None => match listener_persistence.subscribe_updates().await {
                            Ok(pubsub) => pubsub,
                            Err(error) => {
                                tracing::warn!(
                                    redis_channel = %update_channel,
                                    error = %error,
                                    "failed to resubscribe proxy route store update channel"
                                );
                                tokio::time::sleep(Duration::from_secs(5)).await;
                                continue;
                            }
                        },
                    };

                    if let Err(error) = run_redis_route_store_update_listener(
                        listener_route_store.clone(),
                        update_channel.clone(),
                        pubsub,
                    )
                    .await
                    {
                        tracing::warn!(
                            redis_channel = %update_channel,
                            error = %error,
                            "proxy route store update listener stopped with error"
                        );
                    }

                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }));
            route_store
        }
    };
    route_store.load_persisted_state().await?;
    if !config.registry_enabled && config.static_upstream_fallback_allowed() {
        route_store
            .set_static_routes(vec![UpstreamRoute {
                server_id: config.upstream_server_id.clone(),
                local_socket_name: config.upstream_local_socket_name.clone(),
                operation_state: UpstreamOperationState::Active,
                health_state: UpstreamHealthState::Healthy,
            }])
            .await;
    }
    let auth_service = Arc::new(ProxyAuthService::new(
        &config.redis_url,
        config.redis_key_prefix.clone(),
        config.ticket_secret.clone(),
    )?);
    let global_maintenance = Arc::new(GlobalMaintenanceChecker::new(
        &config.redis_url,
        config.redis_key_prefix.clone(),
        std::time::Duration::from_millis(config.maintenance_cache_ttl_ms),
    )?);
    let blocklist_checker = Arc::new(RedisBlocklistChecker::new(
        config.redis_blocklist_enabled,
        &config.redis_url,
        config.redis_key_prefix.clone(),
        std::time::Duration::from_millis(config.redis_blocklist_cache_ttl_ms),
    )?);

    let connection_count = Arc::new(AtomicU64::new(0));
    let maintenance = Arc::new(RwLock::new(false));

    let registry_client: Option<RegistryClient> = if config.registry_enabled {
        match RegistryClient::new(
            &config.registry_url,
            &config.service_name,
            &config.service_instance_id,
        )
        .await
        {
            Ok(client) => {
                let client = client.with_key_prefix(config.registry_key_prefix.clone());
                let route_store_backend = config.route_store_backend_name();
                let instance = build_service_instance(&config);

                if let Err(error) = client.register(&instance).await {
                    tracing::error!(error = %error, "failed to register game-proxy service");
                    if registry_failure_is_fatal() {
                        return Err(std::io::Error::other(error.to_string()).into());
                    }
                } else {
                    tracing::info!(
                        service = %config.service_name,
                        instance = %config.service_instance_id,
                        route_store_backend,
                        "game-proxy service registered to registry"
                    );
                }

                Some(client)
            }
            Err(error) => {
                tracing::error!(error = %error, "failed to create registry client");
                if registry_failure_is_fatal() {
                    return Err(std::io::Error::other(error.to_string()).into());
                }
                None
            }
        }
    } else {
        tracing::info!("service registry disabled; using local static upstream fallback");
        None
    };
    let heartbeat_handle = registry_client
        .as_ref()
        .map(|client| client.start_heartbeat_task());

    let admin_bind_addr = config.admin_bind_addr();
    let admin_auth_config = admin_server::AdminAuthConfig::with_scoped_tokens(
        config.admin_token.clone(),
        config.admin_read_token.clone(),
        config.admin_scoped_tokens.clone(),
    );
    let admin_assertion_verifier = admin_server::AdminAssertionVerifier::new(
        config.admin_assertion_issuer.clone(),
        &config.admin_assertion_public_keys,
        config.admin_assertion_max_ttl_ms,
    );
    let admin_audit_logger =
        admin_server::AdminAuditLogger::new(admin_server::AdminAuditConfig::new(
            config.admin_audit_enabled,
            config.admin_audit_path.clone(),
            config.admin_audit_require_actor,
        ));
    let rollout_drain_status_checker: Option<Arc<dyn OldServerDrainStatusChecker>> =
        if config.rollout_drain_status_check.enabled {
            Some(Arc::new(HttpOldServerDrainStatusChecker::new(
                config.rollout_drain_status_check.clone(),
            )))
        } else {
            None
        };
    let admin_route_store = route_store.clone();
    let admin_connection_count = connection_count.clone();
    let admin_maintenance = maintenance.clone();
    let admin_service_instance_id = config.service_instance_id.clone();
    let admin_task = tokio::spawn(async move {
        if let Err(error) = admin_server::run(
            &admin_bind_addr,
            admin_route_store,
            admin_connection_count,
            admin_maintenance,
            admin_auth_config,
            admin_assertion_verifier,
            admin_service_instance_id,
            admin_audit_logger,
            rollout_drain_status_checker,
        )
        .await
        {
            tracing::warn!(error = %error, "proxy admin server stopped");
        }
    });

    let result = proxy_server::run(
        &config,
        route_store,
        auth_service,
        global_maintenance,
        blocklist_checker,
        connection_count,
        maintenance,
    )
    .await;

    admin_task.abort();
    let _ = admin_task.await;
    if let Some(route_store_update_task) = route_store_update_task {
        route_store_update_task.abort();
        let _ = route_store_update_task.await;
    }
    if let Some(client) = registry_client {
        if let Some(handle) = heartbeat_handle {
            handle.abort();
        }
        if let Err(error) = client.deregister().await {
            tracing::error!(error = %error, "failed to deregister game-proxy service");
        } else {
            tracing::info!(
                service = %config.service_name,
                instance = %config.service_instance_id,
                "game-proxy service deregistered from registry"
            );
        }
    }
    result
}

fn build_service_instance(config: &Config) -> ServiceInstance {
    let client_host = published_host(&config.public_host);
    let tcp_fallback_host = published_host(&config.tcp_fallback_advertised_host);
    let admin_host = published_host(&config.admin_advertised_host);

    ServiceInstance::new(
        config.service_instance_id.clone(),
        config.service_name.clone(),
        client_host.clone(),
        config.port,
    )
    .with_endpoints(vec![
        ServiceEndpoint {
            name: "client".to_string(),
            protocol: "kcp".to_string(),
            host: client_host,
            port: config.port,
            socket: String::new(),
            visibility: "public".to_string(),
            metadata: serde_json::json!({
                "service_name": config.service_name.clone(),
                "service_instance_id": config.service_instance_id.clone(),
                "instance_id": config.service_instance_id.clone(),
                "build_version": config.service_build_version.clone(),
                "zone": config.service_zone.clone()
            }),
            healthy: true,
        },
        ServiceEndpoint {
            name: "client-tcp-fallback".to_string(),
            protocol: "tcp".to_string(),
            host: tcp_fallback_host,
            port: config.tcp_fallback_port,
            socket: String::new(),
            visibility: "public".to_string(),
            metadata: serde_json::json!({
                "service_name": config.service_name.clone(),
                "service_instance_id": config.service_instance_id.clone(),
                "instance_id": config.service_instance_id.clone(),
                "build_version": config.service_build_version.clone(),
                "zone": config.service_zone.clone()
            }),
            healthy: true,
        },
        ServiceEndpoint {
            name: "admin".to_string(),
            protocol: "http".to_string(),
            host: admin_host,
            port: config.admin_port,
            socket: String::new(),
            visibility: "admin".to_string(),
            metadata: serde_json::json!({
                "service_name": config.service_name.clone(),
                "service_instance_id": config.service_instance_id.clone(),
                "instance_id": config.service_instance_id.clone(),
                "build_version": config.service_build_version.clone(),
                "zone": config.service_zone.clone()
            }),
            healthy: true,
        },
    ])
    .with_tags(vec!["proxy".to_string(), "kcp".to_string()])
    .with_metadata(config.service_instance_metadata())
}

fn published_host(host: &str) -> String {
    let host = host.trim();
    if matches!(host, "" | "0.0.0.0" | "::" | "[::]") {
        "127.0.0.1".to_string()
    } else {
        host.to_string()
    }
}

fn registry_failure_is_fatal() -> bool {
    config::discovery_required_from_env()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RouteStoreBackend;
    use crate::connection_limits::{ConnectionLimitConfig, IpDenyList};
    use crate::rollout_drain_status::RolloutDrainStatusCheckConfig;

    fn test_config() -> Config {
        Config {
            host: "127.0.0.1".to_string(),
            public_host: "127.0.0.1".to_string(),
            port: 4000,
            admin_host: "127.0.0.1".to_string(),
            admin_advertised_host: "127.0.0.1".to_string(),
            admin_port: 7101,
            admin_token: "admin-token".to_string(),
            admin_read_token: None,
            admin_scoped_tokens: Vec::new(),
            admin_assertion_issuer: "admin-api".to_string(),
            admin_assertion_public_keys: std::collections::HashMap::new(),
            admin_assertion_max_ttl_ms: 60_000,
            admin_audit_enabled: true,
            admin_audit_path: "logs/game-proxy/admin-audit.jsonl".to_string(),
            admin_audit_require_actor: false,
            tcp_fallback_host: "127.0.0.1".to_string(),
            tcp_fallback_advertised_host: "127.0.0.1".to_string(),
            tcp_fallback_port: 14000,
            log_level: "info".to_string(),
            log_enable_console: true,
            log_enable_file: false,
            log_dir: "logs/game-proxy".to_string(),
            local_socket_name: "myserver-game-proxy.sock".to_string(),
            redis_url: "redis://127.0.0.1:6379".to_string(),
            redis_key_prefix: String::new(),
            route_store_backend: RouteStoreBackend::Redis,
            route_store_redis_url: "redis://127.0.0.1:6379".to_string(),
            route_store_key_prefix: String::new(),
            nats_url: "nats://127.0.0.1:4222".to_string(),
            ticket_secret: "ticket-secret".to_string(),
            proxy_max_connections: 0,
            proxy_max_preauth_failures: 3,
            proxy_msg_rate_window_ms: 1000,
            proxy_msg_rate_max: 0,
            maintenance_cache_ttl_ms: 2000,
            redis_blocklist_enabled: false,
            redis_blocklist_cache_ttl_ms: 2000,
            connection_limits: ConnectionLimitConfig {
                ip_denylist: IpDenyList::parse_csv("").unwrap(),
                max_connections_per_ip: 0,
                max_connections_per_player: 0,
            },
            rollout_drain_status_check: RolloutDrainStatusCheckConfig {
                enabled: false,
                url: "http://127.0.0.1:3000/api/v1/internal/game-server/rollout-drain-status"
                    .to_string(),
                token: None,
                connect_timeout_ms: 3000,
                read_timeout_ms: 3000,
                overall_timeout_ms: 3000,
                max_body_bytes: 1048576,
            },
            registry_enabled: true,
            registry_url: "redis://127.0.0.1:6379".to_string(),
            registry_key_prefix: String::new(),
            registry_discover_interval_secs: 5,
            upstream_service_name: "game-server".to_string(),
            service_name: "edge-proxy".to_string(),
            service_instance_id: "edge-proxy-a".to_string(),
            service_build_version: "2026.06.18".to_string(),
            service_zone: "zone-a".to_string(),
            local_discovery_fallback_enabled: true,
            upstream_server_id: "game-server-1".to_string(),
            upstream_local_socket_name: "myserver-game-server.sock".to_string(),
            legacy_direct_config_warnings: Vec::new(),
        }
    }

    #[test]
    fn service_instance_uses_configured_name_and_instance_metadata() {
        let mut config = test_config();
        config.host = "0.0.0.0".to_string();
        config.public_host = "10.0.0.40".to_string();
        config.tcp_fallback_host = "0.0.0.0".to_string();
        config.tcp_fallback_advertised_host = "10.0.0.42".to_string();
        config.admin_host = "0.0.0.0".to_string();
        config.admin_advertised_host = "10.0.0.41".to_string();
        let instance = build_service_instance(&config);

        assert_eq!(instance.id, "edge-proxy-a");
        assert_eq!(instance.name, "edge-proxy");
        assert_eq!(instance.host, "10.0.0.40");
        assert_eq!(instance.metadata["service_name"], "edge-proxy");
        assert_eq!(instance.metadata["service_instance_id"], "edge-proxy-a");
        assert_eq!(instance.metadata["instance_id"], "edge-proxy-a");
        assert_eq!(instance.metadata["route_store_backend"], "redis");
        assert_eq!(instance.metadata["build_version"], "2026.06.18");
        assert_eq!(instance.metadata["zone"], "zone-a");
        assert_eq!(instance.endpoints.len(), 3);
        assert_eq!(instance.endpoints[0].name, "client");
        assert_eq!(instance.endpoints[0].protocol, "kcp");
        assert_eq!(instance.endpoints[0].host, "10.0.0.40");
        assert_eq!(instance.endpoints[0].visibility, "public");
        assert_eq!(instance.endpoints[0].metadata["service_name"], "edge-proxy");
        assert_eq!(
            instance.endpoints[0].metadata["service_instance_id"],
            "edge-proxy-a"
        );
        assert_eq!(
            instance.endpoints[0].metadata["build_version"],
            "2026.06.18"
        );
        assert_eq!(instance.endpoints[0].metadata["zone"], "zone-a");
        assert_eq!(instance.endpoints[1].name, "client-tcp-fallback");
        assert_eq!(instance.endpoints[1].protocol, "tcp");
        assert_eq!(instance.endpoints[1].host, "10.0.0.42");
        assert_eq!(instance.endpoints[1].visibility, "public");
        assert_eq!(instance.endpoints[2].name, "admin");
        assert_eq!(instance.endpoints[2].protocol, "http");
        assert_eq!(instance.endpoints[2].host, "10.0.0.41");
        assert_eq!(instance.endpoints[2].visibility, "admin");
    }

    #[test]
    fn service_instance_never_publishes_wildcard_network_hosts() {
        let mut config = test_config();
        config.public_host = "0.0.0.0".to_string();
        config.tcp_fallback_advertised_host = "::".to_string();
        config.admin_advertised_host = "[::]".to_string();

        let instance = build_service_instance(&config);

        assert_eq!(instance.host, "127.0.0.1");
        assert_eq!(instance.endpoints[0].host, "127.0.0.1");
        assert_eq!(instance.endpoints[1].host, "127.0.0.1");
        assert_eq!(instance.endpoints[2].host, "127.0.0.1");
    }
}
