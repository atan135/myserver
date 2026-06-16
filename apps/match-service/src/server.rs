//! gRPC 服务器

use std::time::Duration;

use tonic::transport::Server;
use tracing::info;

use crate::config::Config;
use crate::matcher::{new_simple_matcher, new_simple_matcher_with_runtime_store};
use crate::proto::myserver::matchservice::{
    match_internal_server::MatchInternalServer, match_service_server::MatchServiceServer,
};
use crate::runtime_store::{
    RedisMatchRuntimeStore, SharedMatchRuntimeStore, new_memory_match_runtime_store,
};
use crate::service::{MatchInternalImpl, MatchServiceImpl};

pub async fn run(config: Config) -> Result<(), Box<dyn std::error::Error>> {
    let matcher = if uses_memory_runtime_store(&config) {
        new_simple_matcher(config.clone())
    } else {
        let runtime_store = build_runtime_store(&config)?;
        new_simple_matcher_with_runtime_store(config.clone(), runtime_store)
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

    Server::builder()
        .add_service(reflection)
        .add_service(MatchServiceServer::new(match_service))
        .add_service(MatchInternalServer::new(match_internal))
        .serve(addr)
        .await?;

    Ok(())
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
