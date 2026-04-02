use std::io;

use interprocess::local_socket::tokio::Stream as LocalSocketStream;

use crate::route_store::UpstreamRoute;

pub async fn connect_upstream(route: &UpstreamRoute) -> io::Result<LocalSocketStream> {
    crate::local_socket::connect(&route.local_socket_name).await
}
