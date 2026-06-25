#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxySessionState {
    Connected,
    Authenticating,
    Authenticated,
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
    pub account_player_id: Option<String>,
    pub character_id: Option<String>,
    pub player_id: Option<String>,
    pub room_id: Option<String>,
    pub upstream_server_id: Option<String>,
}

impl ProxySession {
    pub fn new(id: u64) -> Self {
        Self {
            id,
            state: ProxySessionState::Connected,
            account_player_id: None,
            character_id: None,
            player_id: None,
            room_id: None,
            upstream_server_id: None,
        }
    }
}
