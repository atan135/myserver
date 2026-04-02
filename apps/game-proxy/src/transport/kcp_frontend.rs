use std::io;
use std::net::SocketAddr;

use tokio_kcp::{KcpConfig, KcpListener, KcpNoDelayConfig, KcpStream};

pub struct KcpFrontend {
    listener: KcpListener,
}

impl KcpFrontend {
    pub async fn bind(addr: &str) -> io::Result<Self> {
        let mut config = KcpConfig::default();
        config.nodelay = KcpNoDelayConfig::fastest();
        config.stream = true;
        let listener = KcpListener::bind(config, addr)
            .await
            .map_err(|error| io::Error::other(error.to_string()))?;
        Ok(Self { listener })
    }

    pub async fn accept(&mut self) -> io::Result<(KcpStream, SocketAddr)> {
        self.listener
            .accept()
            .await
            .map_err(|error| io::Error::other(error.to_string()))
    }
}
