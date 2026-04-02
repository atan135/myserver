use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::io::copy_bidirectional;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::config::Config;
use crate::route_store::{ProxyRouteStore, UpstreamState};
use crate::session::{ProxySession, ProxySessionState};
use crate::upstream::connect_upstream;

pub type SharedConnectionCount = Arc<AtomicU64>;
pub type SharedMaintenanceFlag = Arc<RwLock<bool>>;

pub async fn run(
    config: &Config,
    route_store: ProxyRouteStore,
    connection_count: SharedConnectionCount,
    maintenance: SharedMaintenanceFlag,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut frontend = crate::transport::kcp_frontend::KcpFrontend::bind(&config.bind_addr()).await?;
    let mut next_session_id = 1u64;

    info!(addr = %config.bind_addr(), "game-proxy kcp frontend listening");

    loop {
        let (client_stream, client_addr) = frontend.accept().await?;
        let route_store = route_store.clone();
        let connection_count = connection_count.clone();
        let maintenance = maintenance.clone();
        let session_id = next_session_id;
        next_session_id = next_session_id.saturating_add(1);

        tokio::spawn(async move {
            if let Err(error) = handle_session(
                session_id,
                client_addr,
                client_stream,
                route_store,
                connection_count,
                maintenance,
            )
            .await
            {
                warn!(session_id = session_id, error = %error, "proxy session failed");
            }
        });
    }
}

async fn handle_session(
    session_id: u64,
    client_addr: std::net::SocketAddr,
    mut client_stream: tokio_kcp::KcpStream,
    route_store: ProxyRouteStore,
    connection_count: SharedConnectionCount,
    maintenance: SharedMaintenanceFlag,
) -> Result<(), Box<dyn std::error::Error>> {
    if *maintenance.read().await {
        return Err(Box::new(std::io::Error::other("proxy is in maintenance")));
    }

    let mut session = ProxySession::new(session_id);
    session.state = ProxySessionState::SelectingUpstream;

    let route = route_store
        .select_active()
        .await
        .ok_or_else(|| std::io::Error::other("no active upstream"))?;
    if route.state != UpstreamState::Active {
        return Err(Box::new(std::io::Error::other("selected upstream is not active")));
    }

    let mut upstream = connect_upstream(&route).await?;
    session.upstream_server_id = Some(route.server_id.clone());
    session.state = ProxySessionState::Proxying;
    connection_count.fetch_add(1, Ordering::Relaxed);

    info!(
        session_id = session.id,
        client_addr = %client_addr,
        upstream_server_id = %route.server_id,
        upstream_local_socket_name = %route.local_socket_name,
        "proxy session established"
    );

    let result = copy_bidirectional(&mut client_stream, &mut upstream).await;
    connection_count.fetch_sub(1, Ordering::Relaxed);
    session.state = ProxySessionState::Closed;

    match result {
        Ok((from_client, from_upstream)) => {
            info!(
                session_id = session.id,
                bytes_from_client = from_client,
                bytes_from_upstream = from_upstream,
                "proxy session closed"
            );
            Ok(())
        }
        Err(error) => Err(Box::new(error)),
    }
}
