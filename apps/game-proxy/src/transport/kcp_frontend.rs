use std::io;
use std::net::SocketAddr;

use tokio_kcp::{KcpConfig, KcpListener, KcpNoDelayConfig, KcpStream};

#[cfg(windows)]
use std::os::windows::io::AsRawSocket;
#[cfg(windows)]
use windows_sys::Win32::Networking::WinSock::{SIO_UDP_CONNRESET, SOCKET_ERROR, WSAIoctl};

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
        disable_windows_udp_connreset(&listener)?;
        Ok(Self { listener })
    }

    pub async fn accept(&mut self) -> io::Result<(KcpStream, SocketAddr)> {
        self.listener
            .accept()
            .await
            .map_err(|error| io::Error::other(error.to_string()))
    }
}

#[cfg(windows)]
fn disable_windows_udp_connreset(listener: &KcpListener) -> io::Result<()> {
    let mut enabled = 0u32;
    let mut bytes_returned = 0u32;
    let result = unsafe {
        WSAIoctl(
            listener.as_raw_socket() as usize,
            SIO_UDP_CONNRESET,
            (&mut enabled as *mut u32).cast(),
            std::mem::size_of_val(&enabled) as u32,
            std::ptr::null_mut(),
            0,
            &mut bytes_returned,
            std::ptr::null_mut(),
            None,
        )
    };

    if result == SOCKET_ERROR {
        return Err(io::Error::last_os_error());
    }

    Ok(())
}

#[cfg(not(windows))]
fn disable_windows_udp_connreset(_listener: &KcpListener) -> io::Result<()> {
    Ok(())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn listener_disables_windows_udp_connreset() {
        let listener = KcpListener::bind(KcpConfig::default(), "127.0.0.1:0")
            .await
            .unwrap();

        disable_windows_udp_connreset(&listener).unwrap();
    }
}
