use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, copy_bidirectional, ReadBuf};
use tokio::net::TcpStream as TokioTcpStream;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::config::Config;
use crate::route_store::{ProxyRouteStore, UpstreamRoute, UpstreamState};
use crate::session::{ProxySession, ProxySessionState};
use crate::upstream::connect_upstream;
use service_registry::RegistryClient;

pub type SharedConnectionCount = Arc<AtomicU64>;
pub type SharedMaintenanceFlag = Arc<RwLock<bool>>;

/// Wrapper for either KcpStream or TcpStream that implements AsyncRead + AsyncWrite
enum ProxyStream {
    Kcp(tokio_kcp::KcpStream),
    Tcp(TokioTcpStream),
}

impl AsyncRead for ProxyStream {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            ProxyStream::Kcp(stream) => Pin::new(stream).poll_read(cx, buf),
            ProxyStream::Tcp(stream) => Pin::new(stream).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for ProxyStream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            ProxyStream::Kcp(stream) => Pin::new(stream).poll_write(cx, buf),
            ProxyStream::Tcp(stream) => Pin::new(stream).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            ProxyStream::Kcp(stream) => Pin::new(stream).poll_flush(cx),
            ProxyStream::Tcp(stream) => Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            ProxyStream::Kcp(stream) => Pin::new(stream).poll_shutdown(cx),
            ProxyStream::Tcp(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}

pub async fn run(
    config: &Config,
    route_store: ProxyRouteStore,
    connection_count: SharedConnectionCount,
    maintenance: SharedMaintenanceFlag,
) -> Result<(), Box<dyn std::error::Error>> {
    // 启动 KCP 前端
    let mut kcp_frontend = crate::transport::kcp_frontend::KcpFrontend::bind(&config.bind_addr()).await?;
    info!(addr = %config.bind_addr(), protocol = "kcp", "game-proxy frontend listening");

    // 启动 TCP 前端（fallback 测试端口）
    let tcp_addr = config.tcp_fallback_addr();
    let mut tcp_frontend = crate::transport::tcp_frontend::TcpFrontend::bind(&tcp_addr).await?;
    info!(addr = %tcp_addr, protocol = "tcp", "game-proxy tcp fallback frontend listening");

    // 如果启用了 registry，启动动态发现任务
    if config.registry_enabled {
        let registry_url = config.registry_url.clone();
        let service_name = config.upstream_service_name.clone();
        let discover_interval = config.registry_discover_interval_secs;
        let route_store_clone = route_store.clone();

        tokio::spawn(async move {
            if let Err(e) = run_upstream_discovery(registry_url, service_name, discover_interval, route_store_clone).await {
                tracing::error!(error = %e, "upstream discovery stopped");
            }
        });
    } else {
        // 使用静态配置（向后兼容）
        route_store
            .set_routes(vec![UpstreamRoute {
                server_id: config.upstream_server_id.clone(),
                local_socket_name: config.upstream_local_socket_name.clone(),
                state: UpstreamState::Active,
            }])
            .await;
        tracing::info!(
            upstream_server_id = %config.upstream_server_id,
            upstream_local_socket_name = %config.upstream_local_socket_name,
            "using static upstream config"
        );
    }

    let mut next_session_id = 1u64;

    loop {
        tokio::select! {
            // KCP 连接
            kcp_result = kcp_frontend.accept() => {
                match kcp_result {
                    Ok((client_stream, client_addr)) => {
                        let route_store = route_store.clone();
                        let connection_count = connection_count.clone();
                        let maintenance = maintenance.clone();
                        let session_id = next_session_id;
                        next_session_id = next_session_id.saturating_add(1);

                        tokio::spawn(async move {
                            let stream = ProxyStream::Kcp(client_stream);
                            if let Err(error) = handle_session(
                                session_id,
                                client_addr,
                                stream,
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
                    Err(e) => {
                        warn!(error = %e, "kcp accept failed");
                    }
                }
            }

            // TCP 连接
            tcp_result = tcp_frontend.accept() => {
                match tcp_result {
                    Ok((client_stream, client_addr)) => {
                        let route_store = route_store.clone();
                        let connection_count = connection_count.clone();
                        let maintenance = maintenance.clone();
                        let session_id = next_session_id;
                        next_session_id = next_session_id.saturating_add(1);

                        tokio::spawn(async move {
                            let stream = ProxyStream::Tcp(client_stream);
                            if let Err(error) = handle_session(
                                session_id,
                                client_addr,
                                stream,
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
                    Err(e) => {
                        warn!(error = %e, "tcp accept failed");
                    }
                }
            }
        }
    }
}

/// 从服务注册中心动态发现上游服务器
async fn run_upstream_discovery(
    registry_url: String,
    service_name: String,
    discover_interval_secs: u64,
    route_store: ProxyRouteStore,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let client = RegistryClient::new(&registry_url, "proxy", "proxy-static").await?;
    let interval = discover_interval_secs;

    // 立即执行一次发现
    discover_and_update_routes(&client, &service_name, &route_store).await?;

    // 定时刷新
    let mut ticker = tokio::time::interval(tokio::time::Duration::from_secs(interval));
    loop {
        ticker.tick().await;
        if let Err(e) = discover_and_update_routes(&client, &service_name, &route_store).await {
            tracing::warn!(error = %e, "failed to discover upstream");
        }
    }
}

/// 发现服务并更新路由
async fn discover_and_update_routes(
    client: &RegistryClient,
    service_name: &str,
    route_store: &ProxyRouteStore,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match client.discover(service_name).await? {
        instances if !instances.is_empty() => {
            let routes: Vec<UpstreamRoute> = instances
                .into_iter()
                .map(|instance| UpstreamRoute {
                    server_id: instance.id.clone(),
                    local_socket_name: instance.local_socket.clone(),
                    state: UpstreamState::Active,
                })
                .collect();

            route_store.set_routes(routes.clone()).await;

            tracing::info!(
                service = %service_name,
                count = routes.len(),
                "upstream discovered"
            );
        }
        _ => {
            // 清空路由，表示没有可用上游
            route_store.set_routes(vec![]).await;
            tracing::warn!(service = %service_name, "no healthy upstream found");
        }
    }

    Ok(())
}

async fn handle_session<S: AsyncRead + AsyncWrite + Send + Unpin + 'static>(
    session_id: u64,
    client_addr: std::net::SocketAddr,
    mut client_stream: S,
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
