#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxySessionState {
    Connected,
    Authenticating,
    SelectingUpstream,
    ReplayingAuth,
    Proxying,
    Draining,
    Closed,
}

#[derive(Debug)]
pub struct ProxySession {
    pub id: u64,
    pub state: ProxySessionState,
    pub player_id: Option<String>,
    pub room_id: Option<String>,
    pub upstream_server_id: Option<String>,
}

impl ProxySession {
    pub fn new(id: u64) -> Self {
        Self {
            id,
            state: ProxySessionState::Connected,
            player_id: None,
            room_id: None,
            upstream_server_id: None,
        }
    }
}
