use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Default)]
pub struct ConnectionLimitConfig {
    pub ip_denylist: IpDenyList,
    pub max_connections_per_ip: u64,
    pub max_connections_per_player: u64,
}

#[derive(Clone, Debug, Default)]
pub struct IpDenyList {
    entries: Vec<IpDenyEntry>,
}

impl IpDenyList {
    pub fn parse_csv(value: &str) -> Result<Self, String> {
        let mut entries = Vec::new();

        for raw_entry in value.split(',') {
            let entry = raw_entry.trim();
            if entry.is_empty() {
                continue;
            }

            entries.push(IpDenyEntry::parse(entry)?);
        }

        Ok(Self { entries })
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        self.entries.iter().any(|entry| entry.contains(ip))
    }
}

#[derive(Clone, Debug)]
enum IpDenyEntry {
    Exact(IpAddr),
    Cidr(IpCidr),
}

impl IpDenyEntry {
    fn parse(value: &str) -> Result<Self, String> {
        let Some((ip, prefix_len)) = value.split_once('/') else {
            return value
                .parse::<IpAddr>()
                .map(Self::Exact)
                .map_err(|_| format!("invalid PROXY_IP_DENYLIST entry: {value}"));
        };

        let ip = ip
            .trim()
            .parse::<IpAddr>()
            .map_err(|_| format!("invalid PROXY_IP_DENYLIST CIDR address: {value}"))?;
        let prefix_len = prefix_len
            .trim()
            .parse::<u8>()
            .map_err(|_| format!("invalid PROXY_IP_DENYLIST CIDR prefix: {value}"))?;

        Ok(Self::Cidr(IpCidr::new(ip, prefix_len).ok_or_else(
            || format!("invalid PROXY_IP_DENYLIST CIDR prefix: {value}"),
        )?))
    }

    fn contains(&self, ip: IpAddr) -> bool {
        match self {
            Self::Exact(denied_ip) => *denied_ip == ip,
            Self::Cidr(cidr) => cidr.contains(ip),
        }
    }
}

#[derive(Clone, Debug)]
enum IpCidr {
    V4 { network: u32, prefix_len: u8 },
    V6 { network: u128, prefix_len: u8 },
}

impl IpCidr {
    fn new(ip: IpAddr, prefix_len: u8) -> Option<Self> {
        match ip {
            IpAddr::V4(ip) if prefix_len <= 32 => Some(Self::V4 {
                network: mask_v4(ip, prefix_len),
                prefix_len,
            }),
            IpAddr::V6(ip) if prefix_len <= 128 => Some(Self::V6 {
                network: mask_v6(ip, prefix_len),
                prefix_len,
            }),
            _ => None,
        }
    }

    fn contains(&self, ip: IpAddr) -> bool {
        match (self, ip) {
            (
                Self::V4 {
                    network,
                    prefix_len,
                },
                IpAddr::V4(ip),
            ) => mask_v4(ip, *prefix_len) == *network,
            (
                Self::V6 {
                    network,
                    prefix_len,
                },
                IpAddr::V6(ip),
            ) => mask_v6(ip, *prefix_len) == *network,
            _ => false,
        }
    }
}

fn mask_v4(ip: Ipv4Addr, prefix_len: u8) -> u32 {
    let value = u32::from(ip);
    if prefix_len == 0 {
        0
    } else {
        value & (!0u32 << (32 - prefix_len))
    }
}

fn mask_v6(ip: Ipv6Addr, prefix_len: u8) -> u128 {
    let value = u128::from(ip);
    if prefix_len == 0 {
        0
    } else {
        value & (!0u128 << (128 - prefix_len))
    }
}

#[derive(Clone)]
pub struct ConnectionLimiter {
    inner: Arc<ConnectionLimiterInner>,
}

struct ConnectionLimiterInner {
    config: ConnectionLimitConfig,
    state: Mutex<ConnectionLimitState>,
}

#[derive(Default)]
struct ConnectionLimitState {
    ip_connections: HashMap<IpAddr, u64>,
    player_connections: HashMap<String, u64>,
}

impl ConnectionLimiter {
    pub fn new(config: ConnectionLimitConfig) -> Self {
        Self {
            inner: Arc::new(ConnectionLimiterInner {
                config,
                state: Mutex::new(ConnectionLimitState::default()),
            }),
        }
    }

    pub fn try_acquire_ip(&self, ip: IpAddr) -> Result<IpConnectionGuard, ConnectionLimitError> {
        if self.inner.config.ip_denylist.contains(ip) {
            return Err(ConnectionLimitError::IpDenied);
        }

        let max = self.inner.config.max_connections_per_ip;
        if max == 0 {
            return Ok(IpConnectionGuard {
                limiter: self.clone(),
                ip,
                tracked: false,
                released: false,
            });
        }

        let mut state = self
            .inner
            .state
            .lock()
            .expect("connection limiter poisoned");
        let current = state.ip_connections.get(&ip).copied().unwrap_or(0);
        if current >= max {
            return Err(ConnectionLimitError::IpLimitExceeded { current, max });
        }

        state.ip_connections.insert(ip, current + 1);
        Ok(IpConnectionGuard {
            limiter: self.clone(),
            ip,
            tracked: true,
            released: false,
        })
    }

    pub fn try_acquire_player(
        &self,
        player_id: &str,
    ) -> Result<PlayerConnectionGuard, ConnectionLimitError> {
        let max = self.inner.config.max_connections_per_player;
        if max == 0 {
            return Ok(PlayerConnectionGuard {
                limiter: self.clone(),
                player_id: player_id.to_string(),
                tracked: false,
                released: false,
            });
        }

        let mut state = self
            .inner
            .state
            .lock()
            .expect("connection limiter poisoned");
        let current = state
            .player_connections
            .get(player_id)
            .copied()
            .unwrap_or(0);
        if current >= max {
            return Err(ConnectionLimitError::PlayerLimitExceeded { current, max });
        }

        state
            .player_connections
            .insert(player_id.to_string(), current + 1);
        Ok(PlayerConnectionGuard {
            limiter: self.clone(),
            player_id: player_id.to_string(),
            tracked: true,
            released: false,
        })
    }

    #[cfg(test)]
    fn ip_connection_count(&self, ip: IpAddr) -> u64 {
        self.inner
            .state
            .lock()
            .expect("connection limiter poisoned")
            .ip_connections
            .get(&ip)
            .copied()
            .unwrap_or(0)
    }

    #[cfg(test)]
    fn player_connection_count(&self, player_id: &str) -> u64 {
        self.inner
            .state
            .lock()
            .expect("connection limiter poisoned")
            .player_connections
            .get(player_id)
            .copied()
            .unwrap_or(0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionLimitError {
    IpDenied,
    IpLimitExceeded { current: u64, max: u64 },
    PlayerLimitExceeded { current: u64, max: u64 },
}

impl ConnectionLimitError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::IpDenied => "IP_DENIED",
            Self::IpLimitExceeded { .. } => "IP_CONNECTION_LIMIT_EXCEEDED",
            Self::PlayerLimitExceeded { .. } => "PLAYER_CONNECTION_LIMIT_EXCEEDED",
        }
    }
}

pub struct IpConnectionGuard {
    limiter: ConnectionLimiter,
    ip: IpAddr,
    tracked: bool,
    released: bool,
}

impl IpConnectionGuard {
    fn release(&mut self) {
        if self.released || !self.tracked {
            self.released = true;
            return;
        }

        let mut state = self
            .limiter
            .inner
            .state
            .lock()
            .expect("connection limiter poisoned");
        decrement_or_remove(&mut state.ip_connections, &self.ip);
        self.released = true;
    }
}

impl Drop for IpConnectionGuard {
    fn drop(&mut self) {
        self.release();
    }
}

pub struct PlayerConnectionGuard {
    limiter: ConnectionLimiter,
    player_id: String,
    tracked: bool,
    released: bool,
}

impl PlayerConnectionGuard {
    fn player_id(&self) -> &str {
        &self.player_id
    }

    fn release(&mut self) {
        if self.released || !self.tracked {
            self.released = true;
            return;
        }

        let mut state = self
            .limiter
            .inner
            .state
            .lock()
            .expect("connection limiter poisoned");
        decrement_or_remove(&mut state.player_connections, &self.player_id);
        self.released = true;
    }
}

impl Drop for PlayerConnectionGuard {
    fn drop(&mut self) {
        self.release();
    }
}

#[derive(Default)]
pub struct PlayerConnectionTracker {
    guard: Option<PlayerConnectionGuard>,
}

impl PlayerConnectionTracker {
    pub fn replace_authenticated_player(
        &mut self,
        limiter: &ConnectionLimiter,
        player_id: &str,
    ) -> Result<(), ConnectionLimitError> {
        if self
            .guard
            .as_ref()
            .is_some_and(|guard| guard.player_id() == player_id)
        {
            return Ok(());
        }

        let next_guard = limiter.try_acquire_player(player_id)?;
        self.guard = Some(next_guard);
        Ok(())
    }

    pub fn clear(&mut self) {
        self.guard = None;
    }
}

fn decrement_or_remove<K>(counts: &mut HashMap<K, u64>, key: &K)
where
    K: std::hash::Hash + Eq,
{
    let Some(count) = counts.get_mut(key) else {
        return;
    };

    *count = count.saturating_sub(1);
    if *count == 0 {
        counts.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use super::{
        ConnectionLimitConfig, ConnectionLimitError, ConnectionLimiter, IpDenyList,
        PlayerConnectionTracker,
    };

    fn ip(value: &str) -> IpAddr {
        value.parse().unwrap()
    }

    #[test]
    fn denylist_matches_exact_ip_and_cidr_entries() {
        let denylist = IpDenyList::parse_csv("192.0.2.1, 198.51.100.0/24, 2001:db8::/32")
            .expect("denylist should parse");

        assert!(denylist.contains(ip("192.0.2.1")));
        assert!(denylist.contains(ip("198.51.100.10")));
        assert!(denylist.contains(ip("2001:db8::1")));
        assert!(!denylist.contains(ip("192.0.2.2")));
        assert!(!denylist.contains(ip("198.51.101.10")));
        assert!(!denylist.contains(ip("2001:db9::1")));
    }

    #[test]
    fn denylist_rejects_invalid_entries() {
        assert!(IpDenyList::parse_csv("not-an-ip").is_err());
        assert!(IpDenyList::parse_csv("192.0.2.0/33").is_err());
        assert!(IpDenyList::parse_csv("2001:db8::/129").is_err());
    }

    #[test]
    fn single_ip_limit_is_released_when_guard_drops() {
        let limiter = ConnectionLimiter::new(ConnectionLimitConfig {
            max_connections_per_ip: 1,
            ..Default::default()
        });
        let client_ip = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10));

        let first = limiter.try_acquire_ip(client_ip).unwrap();
        assert_eq!(limiter.ip_connection_count(client_ip), 1);
        match limiter.try_acquire_ip(client_ip) {
            Ok(_) => panic!("second connection from same ip should be rejected"),
            Err(error) => assert_eq!(
                error,
                ConnectionLimitError::IpLimitExceeded { current: 1, max: 1 }
            ),
        }

        drop(first);

        assert_eq!(limiter.ip_connection_count(client_ip), 0);
        assert!(limiter.try_acquire_ip(client_ip).is_ok());
    }

    #[test]
    fn single_player_limit_releases_and_repeated_auth_does_not_double_count() {
        let limiter = ConnectionLimiter::new(ConnectionLimitConfig {
            max_connections_per_player: 1,
            ..Default::default()
        });
        let mut tracker = PlayerConnectionTracker::default();

        tracker
            .replace_authenticated_player(&limiter, "player-1")
            .unwrap();
        assert_eq!(limiter.player_connection_count("player-1"), 1);

        tracker
            .replace_authenticated_player(&limiter, "player-1")
            .unwrap();
        assert_eq!(limiter.player_connection_count("player-1"), 1);
        match limiter.try_acquire_player("player-1") {
            Ok(_) => panic!("second connection for same player should be rejected"),
            Err(error) => assert_eq!(
                error,
                ConnectionLimitError::PlayerLimitExceeded { current: 1, max: 1 }
            ),
        }

        tracker
            .replace_authenticated_player(&limiter, "player-2")
            .unwrap();
        assert_eq!(limiter.player_connection_count("player-1"), 0);
        assert_eq!(limiter.player_connection_count("player-2"), 1);

        tracker.clear();

        assert_eq!(limiter.player_connection_count("player-2"), 0);
        assert!(limiter.try_acquire_player("player-2").is_ok());
    }

    #[test]
    fn denylisted_ip_is_rejected_before_counting() {
        let denied_ip = IpAddr::V6(Ipv6Addr::LOCALHOST);
        let limiter = ConnectionLimiter::new(ConnectionLimitConfig {
            ip_denylist: IpDenyList::parse_csv("::1").unwrap(),
            max_connections_per_ip: 1,
            ..Default::default()
        });

        match limiter.try_acquire_ip(denied_ip) {
            Ok(_) => panic!("denylisted ip should be rejected"),
            Err(error) => assert_eq!(error, ConnectionLimitError::IpDenied),
        }
        assert_eq!(limiter.ip_connection_count(denied_ip), 0);
    }
}
