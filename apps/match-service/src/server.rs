//! gRPC 服务器

use tonic::transport::Server;
use tracing::info;

use crate::config::Config;
use crate::matcher::new_simple_matcher;
use crate::proto::myserver::matchservice::{
    match_service_server::MatchServiceServer,
    match_internal_server::MatchInternalServer,
};
use crate::service::{MatchInternalImpl, MatchServiceImpl};

pub async fn run(config: Config) -> Result<(), Box<dyn std::error::Error>> {
    let matcher = new_simple_matcher(config.clone());

    let match_service = MatchServiceImpl::new(matcher.clone());
    let match_internal = MatchInternalImpl::new(matcher);

    info!(addr = %config.bind_addr, "match-service gRPC server starting");

    let addr = config.bind_addr.parse()?;
    let reflection = tonic_reflection::server::Builder::configure()
        .build()?;

    Server::builder()
        .add_service(reflection)
        .add_service(MatchServiceServer::new(match_service))
        .add_service(MatchInternalServer::new(match_internal))
        .serve(addr)
        .await?;

    Ok(())
}
