use std::io;
use std::net::SocketAddr;

use tokio::net::TcpListener;
use tokio::net::TcpStream;

pub struct TcpFrontend {
    listener: TcpListener,
}

impl TcpFrontend {
    pub async fn bind(addr: &str) -> io::Result<Self> {
        let listener = TcpListener::bind(addr).await?;
        Ok(Self { listener })
    }

    pub async fn accept(&mut self) -> io::Result<(TcpStream, SocketAddr)> {
        self.listener.accept().await
    }
}
