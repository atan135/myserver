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
mod proxy_server;
mod route_store;
mod session;
mod transport;
mod upstream;

use std::fs;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use auth::ProxyAuthService;
use blocklist::RedisBlocklistChecker;
use config::Config;
use maintenance::GlobalMaintenanceChecker;
pub use proto::myserver::game as pb;
use route_store::{
    ProxyRouteStore, RedisRouteStorePersistence, UpstreamHealthState, UpstreamOperationState,
    UpstreamRoute,
};
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

    // 启动 metrics 上报任务
    let metrics_nats_url = config.nats_url.clone();
    let metrics_instance_id = config.service_instance_id.clone();
    tokio::spawn(async move {
        metrics::METRICS
            .start_reporting(&metrics_nats_url, metrics_instance_id, 5)
            .await;
    });

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
            ProxyRouteStore::with_persistence(Arc::new(RedisRouteStorePersistence::new(
                &config.route_store_redis_url,
                config.route_store_key_prefix.clone(),
            )?))
        }
    };
    route_store.load_persisted_state().await?;
    route_store
        .set_static_routes(vec![UpstreamRoute {
            server_id: config.upstream_server_id.clone(),
            local_socket_name: config.upstream_local_socket_name.clone(),
            operation_state: UpstreamOperationState::Active,
            health_state: UpstreamHealthState::Healthy,
        }])
        .await;
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

    let admin_bind_addr = config.admin_bind_addr();
    let admin_auth_config = admin_server::AdminAuthConfig::new(
        config.admin_token.clone(),
        config.admin_read_token.clone(),
    );
    let admin_audit_logger =
        admin_server::AdminAuditLogger::new(admin_server::AdminAuditConfig::new(
            config.admin_audit_enabled,
            config.admin_audit_path.clone(),
            config.admin_audit_require_actor,
        ));
    let admin_route_store = route_store.clone();
    let admin_connection_count = connection_count.clone();
    let admin_maintenance = maintenance.clone();
    let admin_task = tokio::spawn(async move {
        if let Err(error) = admin_server::run(
            &admin_bind_addr,
            admin_route_store,
            admin_connection_count,
            admin_maintenance,
            admin_auth_config,
            admin_audit_logger,
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
    result
}
